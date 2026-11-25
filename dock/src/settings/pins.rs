//! The pinned apps page: the dock's pins as a list (icon, name, path) to
//! drag into order, "×" on every row (and the icon buttons on the row under
//! the mouse or the keyboard), and buttons to add an app: a file, or one
//! running now. Every change shows on the dock right away, like any other
//! setting.
//!
//! With a dock on every display the list is the picked dock's. A dock that
//! takes the main dock's apps shows them read only, with "Customize this
//! dock" (editing them there would change the main dock and every dock
//! following it); the main dock's list says that its changes reach those.

use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ROUNDED_RECT};

use super::rows::Mirror;
use super::{clip_to, dialogs, model, rgba, App, Hit, FOOTER, win_h};
use crate::config::PinEntry;
use crate::glyphs::Glyph;
use crate::i18n::t;

/// A pin row's height, and from one row to the next.
const ROW_H: f32 = 52.0;
/// The rows follow each other in their card (no gap).
pub(super) const PITCH: f32 = ROW_H;

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// A pin being dragged within the list.
pub(super) struct PinDrag {
    /// Where the pin is now (it moves along as it passes others).
    pub index: usize,
    pub start_y: f32,
    /// Mouse y minus the row's top when it was taken.
    pub grab: f32,
    pub y: f32,
    /// Past the few pixels that tell a drag from a click.
    pub moving: bool,
}

/// "Add from running apps": (name, path) of each app, the highlighted one
/// and the first one shown. `rules`: the Input page's "Add app…" (the
/// second of each pair is then the rule's app). `web`: by row, what a web
/// app's pin keeps (empty for the rules).
pub(super) struct AppsMenu {
    pub items: Vec<(String, String)>,
    pub web: Vec<Option<crate::webapps::WebPin>>,
    pub hi: usize,
    pub first: usize,
    pub rules: bool,
}

/// One app with a window open now, as "Add from running apps" lists it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Running {
    pub name: String,
    pub path: String,
    pub web: Option<crate::webapps::WebPin>,
}

/// The running apps were read (`App::running_apps_ready`). (WM_APP + 19
/// was WM_PICK_ACCENT's too: this one was never handled. Every settings
/// window message is checked to be its own: settings_messages_are_distinct.)
pub(super) const WM_RUNNING_APPS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 20;

/// A list read this recently is shown at once, not read again.
const SCAN_FRESH: std::time::Duration = std::time::Duration::from_secs(2);

/// Which list of the apps running now was asked for: the Pins page's
/// "Add from running apps" or the Input page's "Add app…".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Purpose {
    Pins,
    Rules,
}

/// Who asked for a list: what for, on which page, for which display's
/// list (model::editing). A reading back is shown only to the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Want {
    pub purpose: Purpose,
    pub page: usize,
    pub display: Option<String>,
}

/// The apps running now, as both lists show them (one reading).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Reading {
    /// Not pinned (the Pins page).
    pub pins: Vec<Running>,
    /// (name, the rule's app) without a rule (the Input page).
    pub rules: Vec<(String, String)>,
}

/// What a press of "Add from running apps" or "Add app…" does
/// (`Scan::ask`).
#[derive(Debug, PartialEq)]
pub(super) enum Ask {
    /// A list read a moment ago: shown now.
    Show(Reading),
    /// Read one (off this thread); shown when it is back.
    Start,
    /// One is being read already: that one is shown.
    Wait,
}

/// The running apps are read from windows, version resources and the
/// Start menus: on a thread of its own, one reading at a time (presses
/// meanwhile wait for it), shown only to the last press still waiting
/// (`wanted`: not if pressed again, the page or display changed, or
/// another list or menu opened meanwhile).
#[derive(Default, Debug)]
pub(super) struct Scan {
    busy: bool,
    wanted: Option<Want>,
    last: Option<(std::time::Instant, Reading)>,
}

impl Scan {
    pub(super) fn ask(&mut self, want: Want, now: std::time::Instant) -> Ask {
        if !self.busy {
            if let Some((_, r)) = self.last.as_ref().filter(|(at, _)| now.saturating_duration_since(*at) < SCAN_FRESH) {
                self.wanted = None;
                return Ask::Show(r.clone());
            }
        }
        self.wanted = Some(want);
        if self.busy {
            return Ask::Wait;
        }
        self.busy = true;
        Ask::Start
    }

    /// Waiting for a list for `purpose` (its button shows pressed).
    pub(super) fn waiting(&self, purpose: Purpose) -> bool {
        self.wanted.as_ref().is_some_and(|w| w.purpose == purpose)
    }

    /// Pressed again (or the page left, another menu opened) while
    /// waiting: not shown when back.
    pub(super) fn cancel(&mut self) {
        self.wanted = None;
    }

    /// A reading is back: it, and who still waits for it (to be shown
    /// only if that is where the window is now: App::scan_want).
    pub(super) fn done(&mut self, r: Reading, now: std::time::Instant) -> Option<(Want, Reading)> {
        self.busy = false;
        self.last = Some((now, r.clone()));
        self.wanted.take().map(|w| (w, r))
    }
}

thread_local! {
    /// The settings window's (its UI thread's) reading.
    static SCAN: std::cell::RefCell<Scan> = std::cell::RefCell::new(Scan::default());
}

/// A list of running apps is not to be shown any more (page left,
/// another menu opened, the window resized or closing).
pub(super) fn cancel_scan() {
    SCAN.with(|s| s.borrow_mut().cancel());
}

/// Waiting for a list for `purpose` (its button shows pressed).
pub(super) fn scan_waiting(purpose: Purpose) -> bool {
    SCAN.with(|s| s.borrow().waiting(purpose))
}

/// The list a reading thread made, for the UI thread.
static SCANNED: std::sync::Mutex<Option<Reading>> = std::sync::Mutex::new(None);

/// The apps with a window open now (`skip`: the settings window), as both
/// lists show them: those `pins` do not have, those `rules` have none for.
/// Reads windows, version resources and the Start menus (web apps): never
/// on the UI thread, with COM.
pub(super) fn read_running(skip: isize, pins: &[PinEntry], rules: &[crate::config::InputRule]) -> Reading {
    let wins = crate::apps::enumerate(HWND(skip as *mut _));
    // Web apps' shortcuts and names: settings is its own process, the
    // dock's tracker read them into the dock's cache, not this one.
    crate::apps::warm_names(&wins);
    Reading { rules: super::input::rule_apps_from(&wins, rules), pins: running_apps(&wins, pins) }
}

/// Of `wins`, the apps that `pins` do not have: by name, each once.
/// Reads version resources: the reading thread's (read_running).
fn running_apps(wins: &[crate::apps::AppWindow], pins: &[PinEntry]) -> Vec<Running> {
    let pinned: Vec<crate::apps::AppId> = pins.iter().map(|p| crate::apps::identify_pin(&p.path, p.web.as_ref())).collect();
    let me = std::env::current_exe().ok().map(|p| p.to_string_lossy().to_lowercase());
    let mut out: Vec<Running> = Vec::new();
    for w in wins {
        let path = crate::apps::launch_path(w);
        if path.is_empty() || out.iter().any(|r| r.path.eq_ignore_ascii_case(&path)) || (w.exe.is_some() && w.exe == me) {
            continue;
        }
        if pinned.iter().any(|id| id.matches(w)) || pins.iter().any(|p| p.path.eq_ignore_ascii_case(&path)) {
            continue;
        }
        let name = crate::apps::display_name(w);
        let mut web = crate::apps::web_pin(w);
        if let Some(wp) = web.as_mut() {
            // The name it is pinned with is the one the dock gave it.
            wp.name = Some(name.clone());
        }
        out.push(Running { name, path, web });
    }
    out.sort_by_key(|r| r.name.to_lowercase());
    out
}

/// Pure: what a pin's icon is drawn from: its path (and the icon chosen for
/// it, `chosen`), or for a web app's AppsFolder path (no shortcut to draw)
/// the window's icon or its browser's, kept with the pin.
pub(super) fn pin_icon(p: &PinEntry, chosen: bool) -> String {
    let web_path = crate::webapps::apps_folder_aumid(&p.path).is_some_and(crate::webapps::is_web_app);
    let kept = p.web.as_ref().and_then(|w| w.icon.clone().or_else(|| (!w.exe.is_empty()).then(|| w.exe.clone())));
    match kept {
        Some(k) if web_path && !chosen => k,
        _ => p.path.clone(),
    }
}

impl App {
    /// The picked display's key when the list shown is its own apps (a
    /// dock on every display, that display not "Same as Dock 1").
    fn own_pins(&self) -> Option<String> {
        model::editing().map(|e| e.key).filter(|k| self.cfg.dock.per_display.get(k).is_some_and(|d| d.pins.is_some()))
    }

    /// The list shown is the main dock's, seen from a dock that takes it:
    /// read only here.
    pub(super) fn pins_locked(&self) -> bool {
        model::editing().is_some() && self.own_pins().is_none()
    }

    /// Other docks show the main dock's apps (a dock on every display):
    /// editing the main list changes theirs too.
    fn pins_shared(&self) -> bool {
        self.per_display() && model::editing().is_none() && self.screens.iter().any(|d| !d.primary && self.cfg.dock.per_display.get(&d.key).is_none_or(|x| x.pins.is_none()))
    }

    /// The apps the page shows and edits: the picked display's own, else
    /// the main dock's (also for a display that takes the main dock's).
    pub(super) fn pins(&self) -> &Vec<PinEntry> {
        match self.own_pins().and_then(|k| self.cfg.dock.per_display.get(&k)).and_then(|d| d.pins.as_ref()) {
            Some(p) => p,
            None => &self.cfg.dock.pins,
        }
    }

    fn pins_mut(&mut self) -> &mut Vec<PinEntry> {
        if let Some(k) = self.own_pins() {
            if self.cfg.dock.per_display.get(&k).is_some_and(|d| d.pins.is_some()) {
                return self.cfg.dock.per_display.get_mut(&k).and_then(|d| d.pins.as_mut()).expect("checked above");
            }
        }
        &mut self.cfg.dock.pins
    }

    /// Marks an edit of the list shown: "dock.pins", "+pin:x", "-pin:x",
    /// or for a display's own list the per-display map and
    /// "+pin@<display>:x" / "-pin@<display>:x" (model::merge_displays).
    fn touch_pins(&mut self, key: &str) {
        let k = match self.own_pins() {
            None => key.to_string(),
            Some(d) => match key {
                "dock.pins" => "dock.per_display".to_string(),
                k => match (k.strip_prefix("+pin:"), k.strip_prefix("-pin:")) {
                    (Some(p), _) => format!("+pin@{d}:{p}"),
                    (_, Some(p)) => format!("-pin@{d}:{p}"),
                    _ => k.to_string(),
                },
            },
        };
        self.touch(&k);
    }

    /// The list shown, edited here (a pin removed, one moved), without
    /// the dividers that left redundant: saved so (each as removed, so the
    /// merge with the file drops it there too), and shown so at once. Not
    /// while a row is held (only once it is dropped).
    fn settle_dividers(&mut self) {
        let gone = crate::config::redundant_divider_paths(self.pins());
        if gone.is_empty() {
            return;
        }
        let settled = crate::config::normalize_dividers(self.pins());
        *self.pins_mut() = settled;
        self.touch_pins("dock.pins");
        for g in gone {
            self.touch_pins(&format!("-pin:{g}"));
        }
        let n = self.pins().len();
        if let Some(Hit::PinRow(k)) = self.focus {
            self.focus = (n > 0).then(|| Hit::PinRow(k.min(n - 1)));
        }
    }

    /// The pin row the mouse or keyboard is on (its buttons show).
    fn pin_active(&self, i: usize) -> bool {
        let on = |h: Option<Hit>| matches!(h, Some(Hit::PinRow(k) | Hit::PinIcon(k) | Hit::PinReset(k) | Hit::PinRemove(k)) if k == i);
        self.pin_drag.as_ref().is_none_or(|d| !d.moving) && (on(self.hover) || (self.keyboard && on(self.focus)))
    }

    /// The page's own part: "Pinned apps" (its "i": how pinning works, or
    /// whose list it is) over one card - the pins, or "None" - its last
    /// row Add app… and Add from running apps.
    pub(super) unsafe fn paint_pins(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        use super::rows::tok;
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left, right };
        // Another dock's view of the main dock's apps: read only.
        let locked = self.pins_locked();
        let mut y = y;
        if locked {
            let s = crate::i18n::tf("settings.pins.same_as_main", &[("dock", &self.dock_name(self.display_sel))]);
            y = self.notice_card(dc, hits, y, left, right, clip, &s, t("settings.pins.customize"), Hit::PinsCustomize)?;
        }
        let tip = super::tooltip::card_tip(if self.pins_shared() { "settings.pins.shared" } else { "settings.pins.tip" });
        let y = self.heading(dc, hits, y, left, right, "pins", t("settings.pins.title"), Some(tip), clip)?;
        let y = self.paint_pin_list(dc, hits, y, m, clip, locked)?;
        if locked {
            return Ok(y);
        }
        // The card's last row: Add app… and Add from running apps ▾ (at
        // the text's inset, as a list's own buttons are).
        let (add, run) = (t("settings.pins.add"), t("settings.pins.add_running"));
        let x0 = left + tok::ROW_PAD;
        let room = right - tok::ROW_PAD - x0;
        let aw = (self.text_w(add, tok::BODY, false) + 32.0).min(room);
        let rw = (self.text_w(run, tok::BODY, false) + 44.0).min(room);
        // Side by side, or (a long translation) one under the other.
        let two = aw + 8.0 + rw > room;
        let line = tok::CONTROL_H + 8.0;
        let r = self.card_row(dc, "pins.add", y, if two { tok::ROW_MIN + line } else { tok::ROW_MIN }, left, right, false, true)?;
        let cy = r.top + tok::ROW_MIN / 2.0;
        let h = tok::CONTROL_H / 2.0;
        let a = m.r(rect(x0, cy - h, x0 + aw, cy + h));
        let (rx, cy) = if two { (x0, cy + line) } else { (x0 + aw + 8.0, cy) };
        let rr = m.r(rect(rx, cy - h, rx + rw, cy + h));
        self.button(dc, a, add, self.hover == Some(Hit::PinAdd))?;
        let open = self.apps_menu.as_ref().is_some_and(|m| !m.rules) || scan_waiting(Purpose::Pins);
        self.note(super::layout::Kind::Button, run, rr);
        self.fill_round(dc, self.ctl_fill(if open || self.hover == Some(Hit::PinAddRunning) { crate::theme::State::Hover } else { crate::theme::State::Rest }), rr, self.ctl_r())?;
        let (tx, chev) = if rtl { (rr.left + 30.0, rr.left + 16.0) } else { (rr.left + 14.0, rr.right - 16.0) };
        self.text_box(dc, run, tx, self.text_top(cy, tok::BODY), tok::BODY, false, rw - 44.0, self.ink(), rtl)?;
        self.chevron(dc, chev, cy, self.dim())?;
        if let Some(c) = clip_to(a, clip) {
            hits.push((c, Hit::PinAdd));
        }
        if let Some(c) = clip_to(rr, clip) {
            hits.push((c, Hit::PinAddRunning));
        }
        self.apps_anchor.set(Some(rr));
        Ok(r.bottom)
    }

    /// The list's rows from `y`, the card's first ones (`locked`: read
    /// only, no buttons, no dragging; its last row then). Empty: "None".
    unsafe fn paint_pin_list(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, m: Mirror, clip: D2D_RECT_F, locked: bool) -> Result<f32> {
        use super::rows::tok;
        let (left, right, rtl) = (m.left, m.right, m.on);
        let pins = self.pins();
        if pins.is_empty() {
            let r = self.card_row(dc, "pins.none", y, tok::ROW_MIN, left, right, true, locked)?;
            self.text_box(dc, t("settings.list.none"), m.span(left + tok::ROW_PAD, right - left - tok::ROW_PAD * 2.0), self.text_top(r.top + tok::ROW_MIN / 2.0, tok::BODY), tok::BODY, false, right - left - tok::ROW_PAD * 2.0, self.faint(), rtl)?;
            self.pin_list.set((r.bottom, PITCH));
            return Ok(r.bottom);
        }
        let y = self.block_top(y, super::layout::Block::Card);
        self.pin_list.set((y, PITCH));
        let drag = self.pin_drag.as_ref().filter(|d| d.moving);
        for (i, p) in pins.iter().enumerate() {
            let top = y + i as f32 * PITCH;
            let (first, last) = (i == 0, locked && i + 1 == pins.len());
            let seg = rect(left, top, right, top + ROW_H);
            self.note(super::layout::Kind::Seg { first, last }, &p.path, m.r(seg));
            self.card_segment(dc, m.r(seg), self.card_r(), first, last)?;
            if !first {
                self.hairline(dc, m, left + tok::ROW_PAD, right - tok::ROW_PAD, top)?;
            }
            if drag.is_some_and(|d| d.index == i) {
                // Its place in the list, while it is held: a faint tone (an
                // outline in high contrast).
                match self.hc {
                    Some(h) => {
                        let b = dc.CreateSolidColorBrush(&rgba(h.text), None)?;
                        dc.DrawRectangle(&rect(left + 0.5, top + 0.5, right - 0.5, top + ROW_H - 0.5), &b, 1.0, None);
                    }
                    None => self.fill_round(dc, self.wash(0.05), rect(left, top, right, top + ROW_H), 0.0)?,
                }
                continue;
            }
            self.pin_row(dc, hits, i, p, top, m, clip, false, locked)?;
        }
        let end = y + pins.len() as f32 * PITCH;
        // The dragged row over the others, under the mouse.
        if let Some(d) = drag {
            if let Some(p) = pins.get(d.index) {
                let top = (d.y - d.grab).clamp(y - 8.0, end - PITCH + 8.0);
                self.pin_row(dc, hits, d.index, p, top, m, clip, true, false)?;
            }
        }
        Ok(end)
    }

    /// One pin: icon, name, path; × always (a way to remove it in sight),
    /// the icon buttons when active; none when `locked`. A divider: its
    /// line across the row, and ×.
    #[allow(clippy::too_many_arguments)]
    unsafe fn pin_row(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, i: usize, p: &PinEntry, top: f32, m: Mirror, clip: D2D_RECT_F, held: bool, locked: bool) -> Result<()> {
        let (left, right, rtl) = (m.left, m.right, m.on);
        let card = rect(left, top, right, top + ROW_H);
        let active = self.pin_active(i);
        if held {
            // Lifted: it floats on a soft shadow (no outline).
            if self.hc.is_none() {
                let a = if self.light { 0.05 } else { 0.14 };
                for (grow, dy) in [(4.0, 6.0), (2.0, 4.0), (0.5, 2.0)] {
                    self.fill_round(dc, D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a }, rect(card.left - grow, card.top + dy - grow, card.right + grow, card.bottom + dy + grow), self.card_r() + grow)?;
                }
            }
            let bg = match self.hc {
                Some(h) => rgba(h.window),
                None if self.light => D2D1_COLOR_F { r: 0.98, g: 0.98, b: 0.98, a: 1.0 },
                None => D2D1_COLOR_F { r: 0.19, g: 0.19, b: 0.19, a: 1.0 },
            };
            let rad = self.card_r();
            self.fill_round(dc, bg, card, rad)?;
            if let Some(h) = self.hc {
                let b = dc.CreateSolidColorBrush(&rgba(h.highlight), None)?;
                dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(card.left + 1.0, card.top + 1.0, card.right - 1.0, card.bottom - 1.0), radiusX: (rad - 1.0).max(0.0), radiusY: (rad - 1.0).max(0.0) }, &b, 2.0, None);
            }
        } else {
            // (Its part of the card is drawn by the list.)
            if active {
                self.fill_round(dc, self.wash(0.04), m.r(card), 0.0)?;
            }
            if let Some(r) = clip_to(card, clip) {
                hits.push((r, Hit::PinRow(i)));
            }
        }
        let cy = top + ROW_H / 2.0;
        let pad = super::rows::tok::ROW_PAD;
        let divider = p.is_divider();
        if !divider {
            self.draw_icon(dc, &pin_icon(p, self.cfg.dock.icon_overrides.contains_key(&p.path)), m.r(rect(left + pad, cy - 14.0, left + pad + 28.0, cy + 14.0)))?;
        }

        // The buttons, from the far side in: ×, Default (custom icon), Icon…
        let mut x = right - pad;
        // Each with a label, or a glyph and no label.
        let mut buttons: Vec<(D2D_RECT_F, &str, Option<Glyph>, Hit)> = Vec::new();
        if !locked {
            let mut put = |w: f32, label: &'static str, glyph: Option<Glyph>, hit: Hit| {
                buttons.push((rect(x - w, cy - super::rows::tok::CONTROL_H / 2.0, x, cy + super::rows::tok::CONTROL_H / 2.0), label, glyph, hit));
                x -= w + 6.0;
            };
            put(super::rows::tok::CONTROL_H, "", Some(Glyph::Close), Hit::PinRemove(i));
            if active && !divider {
                if self.cfg.dock.icon_overrides.contains_key(&p.path) {
                    put(self.text_w(t("settings.pins.reset"), super::rows::tok::BODY, false) + 24.0, t("settings.pins.reset"), None, Hit::PinReset(i));
                }
                put(self.text_w(t("settings.pins.icon"), super::rows::tok::BODY, false) + 24.0, t("settings.pins.icon"), None, Hit::PinIcon(i));
            }
        }
        // Name and path, up to the buttons.
        let tl = left + pad + 28.0 + 12.0;
        let room = x - 8.0 - tl;
        let tx = if rtl { m.x(tl + room) } else { tl };
        if divider {
            self.hairline(dc, m, left + pad, x - 8.0, cy)?;
        } else if crate::launchpad::is_pin(&p.path) {
            self.text_box(dc, &p.name, tx, self.text_top(cy, super::rows::tok::BODY), super::rows::tok::BODY, false, room, self.ink(), rtl)?;
        } else {
            self.text_box(dc, &p.name, tx, top + 8.0, super::rows::tok::BODY, false, room, self.ink(), rtl)?;
            self.text_box(dc, &p.path, tx, top + 29.0, super::rows::tok::FOOTNOTE, false, room, self.dim(), rtl)?;
        }
        // In reading order for the keyboard.
        for (r, label, glyph, hit) in buttons.into_iter().rev() {
            let r = m.r(r);
            let hot = self.hover == Some(hit);
            match glyph {
                Some(g) => self.glyph_button(dc, r, g, hot)?,
                None => self.button(dc, r, label, hot)?,
            }
            if let Some(r) = clip_to(r, clip) {
                hits.push((r, hit));
            }
        }
        Ok(())
    }

    /// The running apps list, over everything (paint's last layer).
    pub(super) unsafe fn paint_apps_menu(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>) -> Result<()> {
        let (Some(menu), Some(anchor)) = (&self.apps_menu, self.apps_anchor.get()) else { return Ok(()) };
        let labels: Vec<&str> = menu.items.iter().map(|(n, _)| n.as_str()).collect();
        let icons: Vec<String> = menu
            .items
            .iter()
            .enumerate()
            .map(|(k, (n, p))| match menu.rules {
                true => super::input::rule_icon(p).unwrap_or_default(),
                false => pin_icon(&PinEntry { name: n.clone(), path: p.clone(), web: menu.web.get(k).cloned().flatten() }, false),
            })
            .collect();
        let shown = self.paint_list(dc, hits, anchor, &labels, Some(&icons), None, menu.hi, menu.first, &Hit::AppItem, 300.0, crate::i18n::rtl())?;
        self.list_shown.set(shown);
        Ok(())
    }

    /// Mouse down on a pin row: it may be a drag (not in a read-only list).
    pub(super) fn pin_drag_start(&mut self, i: usize, y: f32) {
        if self.pins_locked() {
            return;
        }
        let (top, pitch) = self.pin_list.get();
        self.pin_drag = Some(PinDrag { index: i, start_y: y, grab: y - (top + i as f32 * pitch), y, moving: false });
    }

    /// The mouse moved with a pin held: past a few pixels it follows the
    /// mouse, and the list (and the dock) reorder as it passes other pins.
    pub(super) fn pin_drag_move(&mut self, y: f32) {
        let n = self.pins().len();
        let Some(d) = self.pin_drag.as_mut() else { return };
        if !d.moving && (y - d.start_y).abs() < 4.0 {
            return;
        }
        d.moving = true;
        d.y = y;
        let (top, pitch) = self.pin_list.get();
        let to = model::drop_slot(y - d.grab + ROW_H / 2.0, top, pitch, n);
        let from = d.index;
        // Near the top or bottom edge the page scrolls along.
        let edge = if y < self.content_top() + 20.0 {
            24.0
        } else if y > win_h() - FOOTER - 20.0 {
            -24.0
        } else {
            0.0
        };
        if model::move_item(self.pins_mut(), from, to) {
            if let Some(d) = self.pin_drag.as_mut() {
                d.index = to;
            }
            self.touch_pins("dock.pins");
            self.send_preview();
        } else {
            self.redraw();
        }
        if edge != 0.0 {
            self.wheel(edge);
        }
    }

    /// The mouse let go of a pin (or the capture was lost). A save the
    /// dock made meanwhile is merged now, with the order dragged here on top.
    pub(super) fn pin_drag_end(&mut self) {
        if self.pin_drag.take().is_some_and(|d| d.moving) {
            self.focus = None;
            if !self.pins_locked() {
                self.settle_dividers();
                self.send_preview();
            }
        }
        if self.pins_pending {
            self.pins_saved_elsewhere();
        }
    }

    /// Moves pin `i` by one (Ctrl+Up / Ctrl+Down): the list and the dock
    /// reorder, and the keyboard (and the view) follow it.
    pub(super) fn pin_step(&mut self, i: usize, down: bool) {
        let to = if down { i + 1 } else { i.wrapping_sub(1) };
        if model::move_item(self.pins_mut(), i, to) {
            self.focus = Some(Hit::PinRow(to));
            self.touch_pins("dock.pins");
            self.settle_dividers();
            if let Some(Hit::PinRow(k)) = self.focus {
                self.reveal_pin(k);
            }
            self.send_preview();
        }
    }

    /// Pin row `i` is on screen (drawn inside the page, last paint).
    fn pin_visible(&self, i: usize) -> bool {
        self.hits.iter().any(|(_, h)| *h == Hit::PinRow(i))
    }

    /// Scrolls pin row `i` fully into view (the keyboard moved to it).
    pub(super) fn reveal_pin(&mut self, i: usize) {
        let (top, pitch) = self.pin_list.get();
        let (t, b) = (top + i as f32 * pitch, top + i as f32 * pitch + ROW_H);
        let (lo, hi) = (self.content_top() + 8.0, win_h() - FOOTER - 8.0);
        let d = if b > hi { b - hi } else if t < lo { t - lo } else { 0.0 };
        if d != 0.0 {
            let max = (self.content_h - self.view_h() + 16.0).max(0.0);
            let s = (self.scroll + d).clamp(0.0, max);
            // The list's top moves with the scroll until the next paint.
            self.pin_list.set((top - (s - self.scroll), pitch));
            self.scroll = s;
        }
    }

    /// Pins `entry` at the end of the dock (unless its path is there already).
    fn add_pin(&mut self, entry: PinEntry) {
        let path = entry.path.clone();
        if self.pins().iter().any(|p| p.path.eq_ignore_ascii_case(&path)) {
            self.note = t("settings.pins.already").into();
            self.redraw();
            return;
        }
        self.pins_mut().push(entry);
        self.focus = Some(Hit::PinRow(self.pins().len() - 1));
        self.touch_pins("dock.pins");
        self.touch_pins(&format!("+pin:{path}"));
        self.send_preview();
        // The new row is at the bottom: show it.
        self.scroll = f32::MAX;
        self.wheel(0.0);
    }

    /// Where a list asked for `purpose` now would be shown (Scan: a
    /// reading back for another is not).
    fn scan_want(&self, purpose: Purpose) -> Want {
        Want { purpose, page: self.section, display: model::editing().map(|e| e.key) }
    }

    /// "Add from running apps" or "Add app…" was pressed: the list read a
    /// moment ago, or one read on a thread of its own (the button shows
    /// pressed and the footer says so until it is back:
    /// running_apps_ready).
    pub(super) fn ask_running_apps(&mut self, purpose: Purpose) {
        let now = std::time::Instant::now();
        match SCAN.with(|s| s.borrow_mut().ask(self.scan_want(purpose), now)) {
            Ask::Show(r) => self.show_reading(purpose, r),
            Ask::Wait => self.note = t("settings.pins.loading").into(),
            Ask::Start => {
                self.note = t("settings.pins.loading").into();
                let (hwnd, pins, rules) = (self.hwnd.0 as isize, self.pins().clone(), self.cfg.input.rules.clone());
                let job = (pins.clone(), rules.clone());
                let spawned = std::thread::Builder::new().name("settings-running-apps".into()).spawn(move || {
                    let (pins, rules) = job;
                    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
                    // (As the dock's window tracker reads them.)
                    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                    let r = read_running(hwnd, &pins, &rules);
                    if com {
                        unsafe { CoUninitialize() };
                    }
                    *SCANNED.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                    unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(HWND(hwnd as *mut _)), WM_RUNNING_APPS, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
                    }
                });
                if spawned.is_err() {
                    // No thread: read here, as before.
                    let r = read_running(hwnd, &pins, &rules);
                    if let Some((_, r)) = SCAN.with(|s| s.borrow_mut().done(r, now)) {
                        self.show_reading(purpose, r);
                    }
                }
            }
        }
    }

    /// WM_RUNNING_APPS: a reading is back; its list opens if still wanted
    /// where the window is now (the page and display it was asked on).
    pub(super) fn running_apps_ready(&mut self) {
        let Some(r) = SCANNED.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        let shown = SCAN.with(|s| s.borrow_mut().done(r, std::time::Instant::now()));
        if self.note == t("settings.pins.loading") {
            self.note.clear();
        }
        if let Some((want, r)) = shown {
            if want == self.scan_want(want.purpose) {
                self.show_reading(want.purpose, r);
            } else {
                crate::log!("settings: a list of running apps came back elsewhere; not shown");
            }
        }
        self.redraw();
    }

    /// Opens the list `purpose` asked for.
    fn show_reading(&mut self, purpose: Purpose, r: Reading) {
        match purpose {
            Purpose::Pins => self.show_running(r.pins),
            Purpose::Rules => self.show_rule_apps(r.rules),
        }
    }

    /// Opens the Input page's "Add app…" list (apps given a rule meanwhile
    /// left out).
    fn show_rule_apps(&mut self, list: Vec<(String, String)>) {
        if self.note == t("settings.pins.loading") {
            self.note.clear();
        }
        let items: Vec<(String, String)> = list.into_iter().filter(|(_, k)| !self.cfg.input.rules.iter().any(|r| r.app.eq_ignore_ascii_case(k))).collect();
        if items.is_empty() {
            self.note = t("settings.pins.no_running").into();
            return;
        }
        self.apps_menu = Some(AppsMenu { items, web: Vec::new(), hi: 0, first: 0, rules: true });
    }

    /// Opens the running apps list (those pinned meanwhile left out).
    fn show_running(&mut self, list: Vec<Running>) {
        if self.note == t("settings.pins.loading") {
            self.note.clear();
        }
        let list: Vec<Running> = list.into_iter().filter(|r| !self.pins().iter().any(|p| p.path.eq_ignore_ascii_case(&r.path))).collect();
        if list.is_empty() {
            self.note = t("settings.pins.no_running").into();
            return;
        }
        let web = list.iter().map(|r| r.web.clone()).collect();
        let items = list.into_iter().map(|r| (r.name, r.path)).collect();
        self.apps_menu = Some(AppsMenu { items, web, hi: 0, first: 0, rules: false });
    }

    /// What a pin of `path` is called: its product name (exe), or the
    /// shortcut's name.
    fn pin_name(path: &std::path::Path) -> String {
        let lower = path.to_string_lossy().to_lowercase();
        if lower.ends_with(".exe") {
            let w = crate::apps::AppWindow { hwnd: HWND::default(), exe: Some(lower), aumid: None, pid: 0, started: 0 };
            return crate::apps::display_name(&w);
        }
        path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub(super) fn click_pins(&mut self, h: Hit) {
        let n = self.pins().len();
        if h == Hit::PinsCustomize {
            if model::customize_pins(&mut self.cfg) {
                self.touch("dock.per_display");
                // The keyboard goes on to the list now editable.
                self.focus = Some(if n > 0 { Hit::PinRow(0) } else { Hit::PinAdd });
                self.send_preview();
            }
            return;
        }
        // Another dock's view of the main dock's apps changes nothing.
        if self.pins_locked() {
            return;
        }
        match h {
            Hit::PinRemove(i) if i < n => {
                let p = self.pins_mut().remove(i);
                self.touch_pins("dock.pins");
                self.touch_pins(&format!("-pin:{}", p.path));
                // Its own icon goes with it, unless another dock still
                // has the app (icons are shared by every dock).
                let elsewhere = self.cfg.dock.pins.iter().chain(self.cfg.dock.per_display.values().filter_map(|d| d.pins.as_ref()).flatten()).any(|x| x.path == p.path);
                if !elsewhere {
                    self.cfg.dock.icon_overrides.remove(&p.path);
                    self.touch("dock.icon_overrides");
                    self.touch(&format!("-icon:{}", p.path));
                }
                // The keyboard stays on the list.
                self.focus = (n > 1).then(|| Hit::PinRow(i.min(n - 2)));
                // Dividers it left redundant go too.
                self.settle_dividers();
            }
            Hit::PinReset(i) if i < n => {
                let path = self.pins()[i].path.clone();
                self.cfg.dock.icon_overrides.remove(&path);
                self.touch("dock.icon_overrides");
                self.touch(&format!("-icon:{path}"));
                self.focus = Some(Hit::PinRow(i));
            }
            Hit::PinIcon(i) if i < n => {
                let path = self.pins()[i].path.clone();
                let Some(file) = dialogs::open(self.hwnd, t("settings.pins.icon_dialog"), &[(t("settings.pins.icon_filter"), "*.png;*.ico;*.exe;*.jpg;*.jpeg;*.bmp")]) else { return };
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("png").to_lowercase();
                let ext = if ["png", "ico", "exe", "jpg", "jpeg", "bmp"].contains(&ext.as_str()) { ext } else { "png".into() };
                let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
                let name = format!("{:016x}-{nanos:x}.{ext}", crate::icons::fnv64(path.as_bytes()));
                let dir = crate::config::app_dir().join("icons");
                let _ = std::fs::create_dir_all(&dir);
                if std::fs::copy(&file, dir.join(&name)).is_err() {
                    self.note = t("settings.pins.copy_failed").into();
                    self.redraw();
                    return;
                }
                self.touched.retain(|k| k != &format!("-icon:{path}"));
                self.cfg.dock.icon_overrides.insert(path, format!("icons\\{name}"));
                self.touch("dock.icon_overrides");
            }
            Hit::PinAdd => {
                // The shortcut itself is pinned (as the dock does), never its target.
                let Some(file) = dialogs::open_keep_links(self.hwnd, t("settings.pins.add_dialog"), &[(t("settings.pins.add_filter"), "*.exe;*.lnk")]) else { return };
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                if ext != "exe" && ext != "lnk" {
                    return;
                }
                self.add_pin(PinEntry::new(Self::pin_name(&file), file.to_string_lossy().into_owned()));
                return;
            }
            Hit::PinAddRunning => {
                if self.apps_menu.take().is_some() {
                    self.redraw();
                    return;
                }
                // Pressed again while its list is being read: not opened.
                if scan_waiting(Purpose::Pins) {
                    cancel_scan();
                    if self.note == t("settings.pins.loading") {
                        self.note.clear();
                    }
                } else {
                    self.ask_running_apps(Purpose::Pins);
                }
                self.redraw();
                return;
            }
            Hit::AppItem(k) => {
                let Some(menu) = self.apps_menu.take() else { return };
                let rules = menu.rules;
                let web = menu.web.get(k).cloned().flatten();
                let Some((name, path)) = menu.items.into_iter().nth(k) else { return };
                if rules {
                    self.add_input_rule(name, path, crate::ime::logic::RuleMode::Alpha);
                    return;
                }
                self.focus = Some(Hit::PinAddRunning);
                self.add_pin(PinEntry { name, path, web });
                return;
            }
            _ => return,
        }
        self.send_preview();
    }

    /// Keys while the running apps list is open (like a row's list).
    pub(super) fn apps_menu_key(&mut self, vk: u16, repeat: bool) -> bool {
        const TAB: u16 = 0x09;
        const ENTER: u16 = 0x0D;
        const ESC: u16 = 0x1B;
        const SPACE: u16 = 0x20;
        let Some(menu) = self.apps_menu.as_ref() else { return false };
        let hi = menu.hi;
        match vk {
            ENTER | SPACE if repeat => return true,
            ENTER | SPACE => {
                self.click_pins(Hit::AppItem(hi));
                return true;
            }
            ESC | TAB => {
                let rules = self.apps_menu.take().is_some_and(|m| m.rules);
                self.focus = Some(if rules { Hit::InputAdd } else { Hit::PinAddRunning });
            }
            _ => {
                let labels: Vec<&str> = menu.items.iter().map(|(n, _)| n.as_str()).collect();
                let shown = self.list_shown.get();
                let Some(ni) = super::corners::list_move(vk, hi, &labels, shown) else { return false };
                if let Some(m) = self.apps_menu.as_mut() {
                    m.hi = ni;
                    m.first = Self::keep_in_view(ni, m.first, shown);
                }
            }
        }
        self.redraw();
        true
    }

    /// Keys on a focused pin row (as in a list): Up / Down go to the next
    /// row, Ctrl+Up / Ctrl+Down move the pin itself (on the dock too),
    /// Delete removes it. Each keeps the row in view. Only for a row on
    /// screen (scrolled away: nothing is focused, the keys do nothing to it).
    /// None: not handled here.
    pub(super) fn pin_row_key(&mut self, i: usize, vk: u16, ctrl: bool) -> Option<bool> {
        const UP: u16 = 0x26;
        const DOWN: u16 = 0x28;
        const DELETE: u16 = 0x2E;
        let n = self.pins().len();
        if i >= n || !self.pin_visible(i) {
            return None;
        }
        // Read only: moving through the list, nothing more.
        if self.pins_locked() && (vk == DELETE || ctrl) {
            return Some(true);
        }
        match vk {
            DELETE => {
                self.click_pins(Hit::PinRemove(i));
                if let Some(Hit::PinRow(k)) = self.focus {
                    self.reveal_pin(k);
                }
            }
            UP | DOWN if ctrl => self.pin_step(i, vk == DOWN),
            UP if i > 0 => {
                self.focus = Some(Hit::PinRow(i - 1));
                self.reveal_pin(i - 1);
            }
            DOWN if i + 1 < n => {
                self.focus = Some(Hit::PinRow(i + 1));
                self.reveal_pin(i + 1);
            }
            _ => return None,
        }
        self.redraw();
        Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn settings_messages_are_distinct() {
        // Every message posted to the settings window, by name.
        let ids = [
            ("WM_ICON_READY", super::super::extra::WM_ICON_READY),
            ("WM_SHOW_PAGE_NOW", super::super::WM_SHOW_PAGE_NOW),
            ("WM_UPDATE_DONE", super::super::update_card::WM_UPDATE_DONE),
            ("WM_GPU_LOST", super::super::WM_GPU_LOST),
            ("WM_LEAVE_NOW", super::super::WM_LEAVE_NOW),
            ("WM_LAUNCHERS_FOUND", super::super::WM_LAUNCHERS_FOUND),
            ("WM_ASK_LP_RESET", super::super::WM_ASK_LP_RESET),
            ("WM_CAL_TESTED", super::super::calendar::WM_CAL_TESTED),
            ("WM_PICK_ACCENT", super::super::WM_PICK_ACCENT),
            ("WM_RUNNING_APPS", WM_RUNNING_APPS),
        ];
        for (i, (an, a)) in ids.iter().enumerate() {
            for (bn, b) in &ids[i + 1..] {
                assert_ne!(a, b, "{an} and {bn} share a number");
            }
        }
        // And any defined since (a new file, a new message): every
        // "const WM_...: u32 = ...WM_APP + n" under settings/, its own n.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("settings");
        let mut seen: Vec<(String, u32)> = Vec::new();
        for e in std::fs::read_dir(&dir).expect("settings sources").flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            for line in text.lines() {
                let l = line.trim();
                let Some(at) = l.find("const WM_") else { continue };
                let Some(plus) = l.find("WM_APP + ") else { continue };
                if l.starts_with("//") {
                    continue;
                }
                let name: String = l[at + 6..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                let n: u32 = l[plus + 9..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().expect("a number");
                if let Some((other, _)) = seen.iter().find(|(_, m)| *m == n) {
                    panic!("WM_APP + {n} is both {other} and {name} ({:?})", e.file_name());
                }
                seen.push((name, n));
            }
        }
        assert!(seen.len() >= ids.len(), "all found: {seen:?}");
    }

    fn app(name: &str) -> Running {
        Running { name: name.into(), path: format!(r"C:\Apps\{name}.exe"), web: None }
    }

    fn want(purpose: Purpose, page: usize) -> Want {
        Want { purpose, page, display: None }
    }

    fn reading(name: &str) -> Reading {
        Reading { pins: vec![app(name)], rules: vec![(name.into(), format!("{name}.exe"))] }
    }

    #[test]
    fn running_apps_are_read_once_and_shown_while_wanted() {
        let t0 = Instant::now();
        let mut s = Scan::default();
        let pins = want(Purpose::Pins, 3);
        // First press: read off the UI thread; the button shows pressed.
        assert_eq!(s.ask(pins.clone(), t0), Ask::Start);
        assert!(s.waiting(Purpose::Pins) && !s.waiting(Purpose::Rules));
        // Pressed again while reading (by keyboard repeat, say): no
        // second reading; that one will do.
        assert_eq!(s.ask(pins.clone(), t0), Ask::Wait);
        // Back: shown, once, to who asked.
        assert_eq!(s.done(reading("A"), t0), Some((pins.clone(), reading("A"))));
        assert!(!s.waiting(Purpose::Pins));
        // Asked again a moment later: the list just read, at once.
        assert_eq!(s.ask(pins.clone(), t0 + Duration::from_millis(500)), Ask::Show(reading("A")));
        // Later: read again.
        assert_eq!(s.ask(pins.clone(), t0 + SCAN_FRESH), Ask::Start);
        // Not wanted any more (pressed again to close): kept, not shown.
        s.cancel();
        assert!(!s.waiting(Purpose::Pins));
        assert_eq!(s.done(reading("B"), t0 + SCAN_FRESH), None);
        assert_eq!(s.ask(pins, t0 + SCAN_FRESH + Duration::from_millis(100)), Ask::Show(reading("B")));
    }

    #[test]
    fn a_late_reading_opens_only_where_it_was_asked() {
        let t0 = Instant::now();
        let mut s = Scan::default();
        // A slow reading for the Pins page...
        assert_eq!(s.ask(want(Purpose::Pins, 3), t0), Ask::Start);
        // ...the page left (show_section cancels), and on the Input page
        // "Add app…" pressed: it waits for the same reading.
        s.cancel();
        let rules = want(Purpose::Rules, 7);
        assert_eq!(s.ask(rules.clone(), t0), Ask::Wait);
        assert!(s.waiting(Purpose::Rules) && !s.waiting(Purpose::Pins));
        // Back: for the Input page's list (rules: true), never the pins'.
        let (w, r) = s.done(reading("A"), t0).unwrap();
        assert_eq!(w, rules);
        assert_eq!(w.purpose, Purpose::Rules);
        assert_eq!(r.rules, vec![("A".to_string(), "A.exe".to_string())]);
        // Not cancelled, but the window is elsewhere now (another page,
        // another display's list): App::scan_want differs, not shown.
        assert_eq!(s.ask(want(Purpose::Pins, 3), t0 + SCAN_FRESH), Ask::Start);
        let (w, _) = s.done(reading("B"), t0 + SCAN_FRESH).unwrap();
        assert_ne!(w, want(Purpose::Pins, 4));
        assert_ne!(w, Want { display: Some("GSM5B09-X".into()), ..want(Purpose::Pins, 3) });
        // Another menu opened meanwhile (cancel_scan): nobody waits.
        assert_eq!(s.ask(want(Purpose::Rules, 7), t0 + SCAN_FRESH * 2), Ask::Start);
        s.cancel();
        assert_eq!(s.done(reading("C"), t0 + SCAN_FRESH * 2), None);
    }

    #[test]
    fn the_reading_runs_off_the_ui_thread() {
        // What the click does: the reading on another thread, handed back
        // through SCANNED (the window then gets WM_RUNNING_APPS).
        let ui = std::thread::current().id();
        let reader = std::thread::spawn(move || {
            assert_ne!(std::thread::current().id(), ui);
            let _com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
            // A real reading of this PC's windows (read only).
            read_running(0, &[], &[])
        });
        let both = reader.join().expect("the reading thread");
        // Each app once, by key, in the Input page's list too.
        let mut keys: Vec<String> = both.rules.iter().map(|(_, k)| k.to_lowercase()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), both.rules.len());
        let list = both.pins;
        // Each app once, by name.
        let mut paths: Vec<String> = list.iter().map(|r| r.path.to_lowercase()).collect();
        paths.dedup();
        assert_eq!(paths.len(), list.len());
        assert!(list.windows(2).all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase()));
    }

    #[test]
    fn web_app_pins_draw_the_icon_they_keep() {
        let web = crate::webapps::WebPin { aumid: "Chrome._crx_x".into(), exe: r"C:\B\chrome_proxy.exe".into(), icon: Some(r"C:\App\res.dll,-42".into()), ..Default::default() };
        let p = PinEntry { name: "App".into(), path: crate::webapps::apps_folder_path("Chrome._crx_x"), web: Some(web.clone()) };
        assert_eq!(pin_icon(&p, false), r"C:\App\res.dll,-42", "the window's own icon");
        assert_eq!(pin_icon(&p, true), p.path, "an icon chosen for it: by its path");
        let no_icon = PinEntry { web: Some(crate::webapps::WebPin { icon: None, ..web.clone() }), ..p.clone() };
        assert_eq!(pin_icon(&no_icon, false), r"C:\B\chrome_proxy.exe", "else its browser's");
        // A shortcut draws itself.
        let lnk = PinEntry { path: r"C:\SM\App.lnk".into(), ..p.clone() };
        assert_eq!(pin_icon(&lnk, false), r"C:\SM\App.lnk");
        assert_eq!(pin_icon(&PinEntry::new("X".into(), r"C:\x.exe".into()), false), r"C:\x.exe");
    }

    /// B removed here from A | B | C: the list shown and the file both
    /// keep one divider (the keys the page sends drop the other on disk).
    #[test]
    fn removing_a_pin_here_settles_the_dividers_shown_and_saved() {
        let p = |s: &str| PinEntry::new(s.into(), s.into());
        let d = |n: u32| PinEntry::new(String::new(), format!("nopbar:divider:{n}"));
        let disk = vec![p("A"), d(1), p("B"), d(2), p("C")];
        // What the page does: the pin out, then App::settle_dividers.
        let mut shown = disk.clone();
        shown.remove(2);
        let gone = crate::config::redundant_divider_paths(&shown);
        assert_eq!(gone, [d(2).path]);
        let shown = crate::config::normalize_dividers(&shown);
        assert_eq!(shown, vec![p("A"), d(1), p("C")]);
        // Saved: merged into the file by its keys.
        let mut from = crate::config::Config::default();
        from.dock.pins = shown.clone();
        let mut to = crate::config::Config::default();
        to.dock.pins = disk;
        let keys: Vec<String> = ["dock.pins".to_string(), "-pin:B".to_string()].into_iter().chain(gone.iter().map(|g| format!("-pin:{g}"))).collect();
        model::apply_keys(&from, &mut to, &keys);
        assert_eq!(to.dock.pins, shown);
        // A divider moved to the top here goes on the drop, the same way.
        let moved = vec![d(1), p("A"), p("C")];
        assert_eq!(crate::config::redundant_divider_paths(&moved), [d(1).path]);
    }
}
