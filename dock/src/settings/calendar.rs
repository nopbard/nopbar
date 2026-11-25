//! Settings > Clock & calendar: the calendar links (crate::agenda), rows of
//! the Calendar card under its switch: one per link (its name, host and
//! what its last fetch did; Rename and ×), "None" without any, and last
//! "Add calendar…", which opens a field in its place (Enter adds, Esc
//! cancels; the link is checked once added).
//!
//! The links are secrets: the cards show the host only, the field shows
//! the link only while it is being typed, and Test fetches it from this
//! window's own worker thread (the same request the dock makes), sending
//! it to its own host and nowhere else.

use std::sync::Mutex;

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ROUNDED_RECT};
use windows::Win32::UI::Input::Ime::{ImmGetContext, ImmReleaseContext, GCS_COMPSTR, GCS_RESULTSTR};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use super::rows::Mirror;
use super::{clip_to, App, Hit};
use crate::agenda::{self, Why};
use crate::config::CalendarFeed;
use crate::i18n::{t, tf};

/// Posted to the settings window when a Test is done.
pub const WM_CAL_TESTED: u32 = WM_APP + 18;
/// The longest link and name typed or pasted.
const URL_MAX: usize = 2048;
const NAME_MAX: usize = 60;
/// A link's row (two lines: its name, then its host and status).
const ROW_H: f32 = 52.0;

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// What a Test found: the events in the next two days and the calendar's
/// own name, or why it failed.
pub(super) type Tested = std::result::Result<(usize, String), Why>;

/// Tests done on the worker, by source key, waiting for the window.
static DONE: Mutex<Vec<(String, Tested)>> = Mutex::new(Vec::new());
/// The dock's last fetches, read by a worker, waiting for the window.
static READ: Mutex<Option<Vec<agenda::Status>>> = Mutex::new(None);
/// How long the last fetches shown are kept before they are read again.
const STATUS_AGE: u64 = 10;

/// The page's own state (App::cal).
#[derive(Default)]
pub(super) struct Page {
    /// The link being typed into the field.
    pub url: String,
    /// Renaming link i: the name being typed. Each change is in the
    /// config at once (Save, a page change or closing keep it); Esc puts
    /// `rename_was` back.
    pub rename: Option<(usize, String)>,
    pub rename_was: String,
    /// Where the field with the keyboard has its caret (window DIP: x,
    /// top, bottom), for the IME's windows.
    pub caret: std::cell::Cell<Option<(f32, f32, f32)>>,
    /// The IME's composition in the field that has the keyboard.
    pub comp: String,
    /// Tests on their way and done this time (by source key).
    pub testing: Vec<String>,
    pub tested: Vec<(String, Tested)>,
    /// Why the field's link was not added (a text key).
    pub refused: Option<&'static str>,
    /// "Add calendar…" was chosen: the field shows in its place.
    pub adding: bool,
    /// The dock's last fetches (agenda.json), read on a worker when the
    /// page shows (and again after STATUS_AGE seconds or a Test); painting
    /// uses only this copy.
    pub file: std::cell::RefCell<Option<(std::time::Instant, Vec<agenda::Status>)>>,
    /// A read of the file is on its way.
    pub reading: std::cell::Cell<bool>,
}

/// Pure: what UIA reads in the link field: never the link (a secret), a
/// stand-in that says how long it is.
pub(super) fn masked(url: &str) -> String {
    "•".repeat(url.chars().count().min(64))
}

/// Why a fetch failed, in words.
pub(super) fn why_text(w: Why) -> String {
    match w {
        Why::Http(code) => tf("settings.calendar.why.http", &[("code", &code.to_string())]),
        Why::Network => t("settings.calendar.why.network").into(),
        Why::Timeout => t("settings.calendar.why.timeout").into(),
        Why::Format => t("settings.calendar.why.format").into(),
        Why::Size => t("settings.calendar.why.size").into(),
        Why::Link => t("settings.calendar.bad_url").into(),
        Why::Store => t("settings.calendar.why.store").into(),
        Why::Redirect => t("settings.calendar.why.redirect").into(),
    }
}

/// Pure: what a card says under its name: testing, the Test's answer, the
/// dock's last fetch (its time as `time`), or nothing yet.
pub(super) fn status_line(testing: bool, tested: Option<&Tested>, last: Option<&agenda::Status>, time: &dyn Fn(u64) -> String) -> String {
    if testing {
        return t("settings.calendar.testing").into();
    }
    match (tested, last) {
        (Some(Ok((n, _))), _) => tf("settings.calendar.status.works", &[("n", &n.to_string())]),
        (Some(Err(w)), _) => tf("settings.calendar.status.failed", &[("why", &why_text(*w))]),
        (None, Some(s)) if s.error.is_some() => tf("settings.calendar.status.failed", &[("why", &why_text(s.error.unwrap_or(Why::Network)))]),
        (None, Some(s)) if s.fetched > 0 => tf("settings.calendar.status.updated", &[("time", &time(s.fetched)), ("n", &s.events.to_string())]),
        _ => t("settings.calendar.status.never").into(),
    }
}

/// Pure: adding `url` to `feeds`: the feed to add (named after its host),
/// or why not (a text key).
pub(super) fn new_feed(feeds: &[CalendarFeed], url: &str) -> std::result::Result<CalendarFeed, &'static str> {
    let url = url.trim();
    if agenda::normalize(url).is_none() {
        return Err("settings.calendar.bad_url");
    }
    let key = agenda::source_key(url);
    if feeds.iter().any(|f| agenda::source_key(&f.url) == key) {
        return Err("settings.calendar.already");
    }
    Ok(CalendarFeed { name: agenda::host(url), url: url.to_string() })
}

/// A feed's name as shown (its host when it has none).
fn shown_name(f: &CalendarFeed) -> String {
    if f.name.trim().is_empty() { agenda::host(&f.url) } else { f.name.clone() }
}

impl App {
    /// The dock's last fetches as known; a stale copy asks a worker to
    /// read the file again (never read here: this runs while painting).
    fn cal_file(&self) -> Vec<agenda::Status> {
        let g = self.cal.file.borrow();
        let stale = g.as_ref().is_none_or(|(at, _)| at.elapsed().as_secs() >= STATUS_AGE);
        if stale && !self.cal.reading.replace(true) {
            let hwnd = self.hwnd.0 as isize;
            let spawned = std::thread::Builder::new().name("calendar-status".into()).spawn(move || {
                *READ.lock().unwrap_or_else(|e| e.into_inner()) = Some(agenda::statuses());
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CAL_TESTED, WPARAM(0), LPARAM(0));
                }
            });
            if spawned.is_err() {
                self.cal.reading.set(false);
            }
        }
        g.as_ref().map(|(_, v)| v.clone()).unwrap_or_default()
    }

    /// The IME's place while a calendar field has the keyboard.
    pub(super) fn cal_caret(&self) -> Option<(f32, f32, f32)> {
        matches!(self.focus, Some(Hit::CalUrl | Hit::CalName)).then(|| self.cal.caret.get()).flatten()
    }

    /// A rename in progress ends, its text kept (it is in the config
    /// already): another control, Save, a page change, closing.
    pub(super) fn cal_end_rename(&mut self) {
        if self.cal.rename.take().is_some() {
            self.cal.comp.clear();
            self.redraw();
        }
    }

    /// The name being typed goes into the config as it is typed.
    fn cal_sync_rename(&mut self) {
        let Some((i, name)) = self.cal.rename.clone() else { return };
        let name = name.trim().to_string();
        if let Some(f) = self.cfg.calendar.feeds.get_mut(i) {
            if !name.is_empty() && name != f.name {
                f.name = name;
                self.touch("calendar.feeds");
            }
        }
    }

    /// The Calendar card under its heading (the page's last row): the
    /// links, "None" without any, then "Add calendar…" or its field.
    /// Returns where the card ends.
    pub(super) unsafe fn paint_calendar(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, mut y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        use super::rows::tok;
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left, right };
        let cr = right - tok::ROW_PAD;
        let tl = left + tok::ROW_PAD;
        let feeds = &self.cfg.calendar.feeds;
        if feeds.is_empty() {
            let r = self.card_row(dc, "calendar.none", y, tok::ROW_MIN, left, right, true, false)?;
            self.text_box(dc, t("settings.list.none"), m.span(tl, cr - tl), self.text_top(r.top + tok::ROW_MIN / 2.0, tok::BODY), tok::BODY, false, cr - tl, self.faint(), rtl)?;
            y = r.bottom;
        }
        let file = self.cal_file();
        let time = |unix: u64| {
            let local = agenda::tz::local_from_utc(unix as i64);
            let (yy, mm, dd) = agenda::tz::civil_from_days(local.div_euclid(86_400));
            let s = local.rem_euclid(86_400);
            let st = windows::Win32::Foundation::SYSTEMTIME { wYear: yy as u16, wMonth: mm as u16, wDay: dd as u16, wHour: (s / 3600) as u16, wMinute: (s / 60 % 60) as u16, ..Default::default() };
            crate::i18n::date::time(&st)
        };
        let rename = t("settings.calendar.rename");
        let rw = self.text_w(rename, tok::BODY, false) + 32.0;
        let ch = tok::CONTROL_H / 2.0;
        for (i, f) in feeds.iter().enumerate() {
            let r = self.card_row(dc, &agenda::source_key(&f.url), y, ROW_H, left, right, i == 0, false)?;
            let top = r.top;
            let cy = top + ROW_H / 2.0;
            // In reading order: Rename, ×.
            let x_close = cr - tok::CONTROL_H;
            let x_rename = x_close - 6.0 - rw;
            let room = x_rename - tok::ROW_PAD - tl;
            let key = agenda::source_key(&f.url);
            match &self.cal.rename {
                Some((ri, name)) if *ri == i => self.cal_field(dc, hits, m.r(rect(tl - 6.0, cy - ch, tl + room, cy + ch)), name, "", Hit::CalName, clip)?,
                _ => {
                    let tx = if rtl { m.x(tl + room) } else { tl };
                    self.text_box(dc, &shown_name(f), tx, top + 8.0, tok::BODY, false, room, self.ink(), rtl)?;
                    let last = file.iter().find(|s| s.key == key);
                    let tested = self.cal.tested.iter().find(|(k, _)| *k == key).map(|(_, r)| r);
                    let line = format!("{}  ·  {}", agenda::host(&f.url), status_line(self.cal.testing.contains(&key), tested, last, &time));
                    self.text_box(dc, &line, tx, top + 28.0, tok::FOOTNOTE, false, room, self.dim(), rtl)?;
                }
            }
            let b = m.r(rect(x_rename, cy - ch, x_rename + rw, cy + ch));
            self.button(dc, b, rename, self.hover == Some(Hit::CalRename(i)))?;
            if let Some(c) = clip_to(b, clip) {
                hits.push((c, Hit::CalRename(i)));
            }
            let close = m.r(rect(x_close, cy - ch, cr, cy + ch));
            self.glyph_button(dc, close, crate::glyphs::Glyph::Close, self.hover == Some(Hit::CalRemove(i)))?;
            if let Some(c) = clip_to(close, clip) {
                hits.push((c, Hit::CalRemove(i)));
            }
            y = r.bottom;
        }
        // The last row: "Add calendar…", or (chosen, or a link being
        // typed) the field and Add, with why a link was refused under them.
        if !(self.cal.adding || !self.cal.url.is_empty() || self.focus == Some(Hit::CalUrl)) {
            let r = self.card_row(dc, "calendar.add", y, tok::ROW_MIN, left, right, false, true)?;
            let cy = r.top + tok::ROW_MIN / 2.0;
            let label = t("settings.calendar.add_feed");
            let w = self.text_w(label, tok::BODY, false) + 32.0;
            let b = m.r(rect(tl, cy - ch, tl + w, cy + ch));
            self.button(dc, b, label, self.hover == Some(Hit::CalOpen))?;
            if let Some(c) = clip_to(b, clip) {
                hits.push((c, Hit::CalOpen));
            }
            return Ok(r.bottom);
        }
        let why = self.cal.refused.map(t);
        let why_h = why.map(|s| self.para_h(s, tok::FOOTNOTE, cr - tl) + 6.0).unwrap_or(0.0);
        let r = self.card_row(dc, "calendar.add", y, tok::ROW_MIN + why_h, left, right, false, true)?;
        let cy = r.top + tok::ROW_MIN / 2.0;
        let add = t("settings.calendar.add");
        let aw = self.text_w(add, tok::BODY, false) + 32.0;
        let field = m.r(rect(tl, cy - ch, cr - aw - 8.0, cy + ch));
        let text = if self.focus == Some(Hit::CalUrl) || !self.cal.url.is_empty() { self.cal.url.as_str() } else { "" };
        self.cal_field(dc, hits, field, text, t("settings.calendar.url_placeholder"), Hit::CalUrl, clip)?;
        self.row_button(dc, hits, m, "calendar.add", cr, cy, add, Hit::CalAdd, clip)?;
        if let Some(s) = why {
            self.para_dir(dc, s, m.span(tl, cr - tl), cy + ch + 4.0, tok::FOOTNOTE, false, cr - tl, self.warn(), rtl)?;
        }
        Ok(r.bottom)
    }

    /// A text field: the text (or the dim `placeholder`), the composition
    /// underlined, the caret while it has the keyboard (a deeper tone then;
    /// an outline only in high contrast).
    #[allow(clippy::too_many_arguments)]
    unsafe fn cal_field(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, r: D2D_RECT_F, text: &str, placeholder: &str, hit: Hit, clip: D2D_RECT_F) -> Result<()> {
        self.note(super::layout::Kind::Field, "", r);
        let focused = self.focus == Some(hit);
        self.fill_round(dc, self.ctl_fill(if focused { crate::theme::State::Hover } else { crate::theme::State::Rest }), r, self.ctl_r())?;
        if let (true, Some(h)) = (focused, self.hc) {
            let b = dc.CreateSolidColorBrush(&super::rgba(h.highlight), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 1.0, r.top + 1.0, r.right - 1.0, r.bottom - 1.0), radiusX: self.ctl_r() - 1.0, radiusY: self.ctl_r() - 1.0 }, &b, 2.0, None);
        }
        let rtl = crate::i18n::rtl();
        let comp = if focused { self.cal.comp.as_str() } else { "" };
        let all = format!("{text}{comp}");
        let room = r.right - r.left - 20.0;
        let cy = (r.top + r.bottom) / 2.0;
        if all.is_empty() {
            self.text_box(dc, placeholder, r.left + 10.0, self.text_top(cy, super::rows::tok::BODY), super::rows::tok::BODY, false, room, self.dim(), rtl)?;
        }
        // A long link shows its end (where the typing is).
        let mut shown = all.as_str();
        while self.text_w(shown, super::rows::tok::BODY, false) > room && !shown.is_empty() {
            let cut = shown.char_indices().nth(1).map(|(i, _)| i).unwrap_or(shown.len());
            shown = &shown[cut..];
        }
        let tw = self.text_w(shown, super::rows::tok::BODY, false);
        if !shown.is_empty() {
            self.text_box(dc, shown, r.left + 10.0, self.text_top(cy, super::rows::tok::BODY), super::rows::tok::BODY, false, room, self.ink(), false)?;
        }
        if !comp.is_empty() {
            let cw = self.text_w(comp, super::rows::tok::BODY, false).min(tw);
            let end = r.left + 10.0 + tw;
            self.fill_round(dc, self.ink(), rect(end - cw, cy + 9.0, end, cy + 10.0), 0.0)?;
        }
        if focused {
            let x = r.left + 10.0 + tw.min(room);
            self.fill_round(dc, self.ink(), rect(x, cy - 9.0, x + 1.0, cy + 9.0), 0.0)?;
            self.cal.caret.set(Some((x, r.top + 6.0, r.bottom - 6.0)));
        }
        if let Some(c) = clip_to(r, clip) {
            hits.push((c, hit));
        }
        Ok(())
    }

    /// Adds the field's link and checks it at once (its card says what the
    /// check found: no Test button).
    fn cal_add(&mut self) {
        match new_feed(&self.cfg.calendar.feeds, &self.cal.url) {
            Ok(f) => {
                let url = f.url.clone();
                self.cfg.calendar.feeds.push(f);
                self.cal.url.clear();
                self.cal.refused = None;
                self.cal.adding = false;
                self.focus = Some(Hit::CalRename(self.cfg.calendar.feeds.len() - 1));
                self.touch("calendar.feeds");
                self.cal_test(url);
                self.send_preview();
            }
            Err(k) => {
                self.cal.refused = Some(k);
                self.redraw();
            }
        }
    }

    /// Fetches `url` on a worker thread (never here) and posts the answer.
    fn cal_test(&mut self, url: String) {
        let key = agenda::source_key(&url);
        if self.cal.testing.contains(&key) {
            return;
        }
        self.cal.testing.push(key.clone());
        self.cal.tested.retain(|(k, _)| *k != key);
        let hwnd = self.hwnd.0 as isize;
        let spawned = std::thread::Builder::new().name("calendar-test".into()).spawn(move || {
            let (from, to) = agenda::days_span(0, agenda::CACHE_DAYS);
            let r: Tested = agenda::fetch_feed(&url, from, to).map(|p| (p.events.len(), p.name.unwrap_or_default()));
            DONE.lock().unwrap_or_else(|e| e.into_inner()).push((key, r));
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CAL_TESTED, WPARAM(0), LPARAM(0));
            }
        });
        if spawned.is_err() {
            self.cal.testing.pop();
        }
    }

    /// WM_CAL_TESTED: the answers in; a link still named after its host
    /// takes the calendar's own name.
    pub(super) fn cal_tested(&mut self) {
        let done: Vec<(String, Tested)> = std::mem::take(&mut *DONE.lock().unwrap_or_else(|e| e.into_inner()));
        if let Some(v) = READ.lock().unwrap_or_else(|e| e.into_inner()).take() {
            *self.cal.file.borrow_mut() = Some((std::time::Instant::now(), v));
            self.cal.reading.set(false);
        }
        let tested_any = !done.is_empty();
        for (key, r) in done {
            self.cal.testing.retain(|k| *k != key);
            if let Ok((_, name)) = &r {
                let feed = self.cfg.calendar.feeds.iter_mut().find(|f| agenda::source_key(&f.url) == key);
                if let Some(f) = feed.filter(|f| !name.trim().is_empty() && (f.name.trim().is_empty() || f.name == agenda::host(&f.url))) {
                    f.name = name.chars().take(NAME_MAX).collect();
                    self.touch("calendar.feeds");
                }
            }
            self.cal.tested.retain(|(k, _)| *k != key);
            self.cal.tested.push((key, r));
        }
        if tested_any {
            // The dock may have fetched meanwhile too.
            *self.cal.file.borrow_mut() = None;
        }
        self.redraw();
    }

    fn cal_commit_rename(&mut self) {
        self.cal_sync_rename();
        self.cal.rename = None;
        self.cal.comp.clear();
    }

    pub(super) fn click_calendar(&mut self, h: Hit) {
        let n = self.cfg.calendar.feeds.len();
        // Clicking elsewhere ends a rename (kept, like Enter).
        if self.cal.rename.is_some() && h != Hit::CalName {
            self.cal_commit_rename();
        }
        match h {
            Hit::CalUrl => {
                self.focus = Some(Hit::CalUrl);
                self.place_ime();
            }
            Hit::CalName => self.focus = Some(Hit::CalName),
            Hit::CalAdd => {
                self.cal_add();
                return;
            }
            Hit::CalOpen => {
                self.cal.adding = true;
                self.focus = Some(Hit::CalUrl);
                self.place_ime();
            }
            Hit::CalRename(i) if i < n => {
                self.cal.rename_was = self.cfg.calendar.feeds[i].name.clone();
                self.cal.rename = Some((i, shown_name(&self.cfg.calendar.feeds[i])));
                self.focus = Some(Hit::CalName);
                self.place_ime();
            }
            Hit::CalRemove(i) if i < n => {
                let f = self.cfg.calendar.feeds.remove(i);
                let key = agenda::source_key(&f.url);
                self.cal.tested.retain(|(k, _)| *k != key);
                // The keyboard stays on the list.
                self.focus = if n > 1 { Some(Hit::CalRemove(i.min(n - 2))) } else { Some(Hit::CalOpen) };
                self.touch("calendar.feeds");
                self.send_preview();
                return;
            }
            _ => {}
        }
        self.redraw();
    }

    /// The text being typed: the link field or a name.
    fn cal_text(&mut self) -> Option<(&mut String, usize)> {
        match self.focus {
            Some(Hit::CalUrl) => Some((&mut self.cal.url, URL_MAX)),
            Some(Hit::CalName) => self.cal.rename.as_mut().map(|(_, s)| (s, NAME_MAX)),
            _ => None,
        }
    }

    /// A typed character for the field with the keyboard (Ctrl+V pastes).
    pub(super) fn cal_char(&mut self, ch: u16) -> bool {
        if !matches!(self.focus, Some(Hit::CalUrl | Hit::CalName)) {
            return false;
        }
        let paste = if ch == 0x16 { unsafe { crate::search::clipboard_text(self.hwnd) } } else { None };
        let mut sur = self.surrogate.take();
        let Some((text, max)) = self.cal_text() else { return false };
        let push = |c: char, text: &mut String| {
            if text.chars().count() < max && !c.is_control() {
                text.push(c);
            }
        };
        match ch {
            0x08 => {
                text.pop();
            }
            0x7F => {
                let keep = text.trim_end().rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
                text.truncate(keep);
            }
            0x16 => {
                // One line: a pasted link with a line break keeps its first line.
                for c in paste.unwrap_or_default().lines().next().unwrap_or("").trim().chars() {
                    push(c, text);
                }
            }
            c if c < 0x20 => return true,
            0xD800..=0xDBFF => sur = Some(ch),
            0xDC00..=0xDFFF => {
                if let Some(Ok(c)) = sur.take().and_then(|hi| char::decode_utf16([hi, ch]).next()) {
                    push(c, text);
                }
            }
            c => {
                if let Some(c) = char::from_u32(c as u32) {
                    push(c, text);
                }
            }
        }
        self.surrogate = sur;
        self.cal.refused = None;
        self.cal_sync_rename();
        self.redraw();
        true
    }

    /// WM_IME_COMPOSITION for the calendar's fields. False: not ours.
    pub(super) fn cal_composition(&mut self, lp: LPARAM) -> bool {
        if !matches!(self.focus, Some(Hit::CalUrl | Hit::CalName)) {
            return false;
        }
        let flags = lp.0 as u32;
        let himc = unsafe { ImmGetContext(self.hwnd) };
        if himc.is_invalid() {
            return false;
        }
        let done = if flags & GCS_RESULTSTR.0 != 0 { self.ime_string(himc, GCS_RESULTSTR) } else { String::new() };
        self.cal.comp = if flags & GCS_COMPSTR.0 != 0 { self.ime_string(himc, GCS_COMPSTR) } else { String::new() };
        unsafe {
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
        if let Some((text, max)) = self.cal_text() {
            for c in done.chars() {
                if text.chars().count() < max {
                    text.push(c);
                }
            }
        }
        self.cal_sync_rename();
        self.place_ime();
        self.redraw();
        true
    }

    /// Keys while a calendar field has the keyboard: Enter adds (or keeps
    /// the name), Esc drops. None: typing (comes as WM_CHAR) or Tab.
    pub(super) fn cal_key(&mut self, vk: u16) -> Option<bool> {
        match vk {
            0x0D => {
                if self.focus == Some(Hit::CalName) {
                    self.cal_commit_rename();
                    self.focus = None;
                } else {
                    self.cal_add();
                    return Some(true);
                }
            }
            0x1B => {
                self.cancel_composition();
                self.cal.comp.clear();
                // The keyboard goes back to what opened the field (so Tab
                // goes on from there, not from the page's top).
                self.focus = match self.cal.rename.take() {
                    // The name it had before; back on its Rename.
                    Some((i, _)) => {
                        let was = std::mem::take(&mut self.cal.rename_was);
                        if let Some(f) = self.cfg.calendar.feeds.get_mut(i) {
                            if f.name != was {
                                f.name = was;
                                self.touch("calendar.feeds");
                            }
                        }
                        Some(Hit::CalRename(i))
                    }
                    // The link dropped; back on "Add calendar…".
                    None => {
                        self.cal.url.clear();
                        self.cal.refused = None;
                        self.cal.adding = false;
                        Some(Hit::CalOpen)
                    }
                };
            }
            _ => return None,
        }
        self.redraw();
        Some(true)
    }

    /// A field's text set by a screen reader.
    pub(super) fn cal_set_value(&mut self, h: Hit, text: String) {
        match h {
            Hit::CalUrl => self.cal.url = text.chars().take(URL_MAX).collect(),
            Hit::CalName => {
                if let Some((_, s)) = self.cal.rename.as_mut() {
                    *s = text.chars().take(NAME_MAX).collect();
                }
                self.cal_sync_rename();
            }
            _ => return,
        }
        self.focus = Some(h);
        self.redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_checked_before_they_are_added() {
        let _l = crate::i18n::test_lock("en");
        let mut feeds = Vec::new();
        let f = new_feed(&feeds, " webcal://p01-calendars.icloud.com/published/2/abc ").unwrap();
        assert_eq!((f.name.as_str(), f.url.as_str()), ("p01-calendars.icloud.com", "webcal://p01-calendars.icloud.com/published/2/abc"), "named after its host");
        feeds.push(f);
        assert_eq!(new_feed(&feeds, "https://p01-calendars.icloud.com/published/2/abc"), Err("settings.calendar.already"), "the same calendar by https");
        assert_eq!(new_feed(&feeds, "http://x.example/a.ics"), Err("settings.calendar.bad_url"));
        assert_eq!(new_feed(&feeds, "calendar"), Err("settings.calendar.bad_url"));
        assert!(new_feed(&feeds, "https://calendar.google.com/calendar/ical/a/private-b/basic.ics").is_ok());
    }

    #[test]
    fn screen_readers_never_get_the_link() {
        let url = "https://calendar.google.com/calendar/ical/me/private-SECRET/basic.ics";
        let m = masked(url);
        assert!(!m.contains("SECRET") && !m.contains("google") && !m.contains("https"));
        assert_eq!(m.chars().count(), url.chars().count().min(64));
        assert_eq!(masked(""), "");
    }

    #[test]
    fn cards_say_what_the_last_fetch_did() {
        let _l = crate::i18n::test_lock("en");
        let time = |_: u64| "9:30 AM".to_string();
        assert_eq!(status_line(true, None, None, &time), t("settings.calendar.testing"));
        assert_eq!(status_line(false, None, None, &time), t("settings.calendar.status.never"));
        let ok: Tested = Ok((3, String::new()));
        assert!(status_line(false, Some(&ok), None, &time).contains('3'));
        let bad: Tested = Err(Why::Http(404));
        assert!(status_line(false, Some(&bad), None, &time).contains("404"));
        let s = agenda::Status { fetched: 5, events: 2, ..Default::default() };
        let line = status_line(false, None, Some(&s), &time);
        assert!(line.contains("9:30 AM") && line.contains('2'), "{line}");
        let failed = agenda::Status { fetched: 5, error: Some(Why::Timeout), ..Default::default() };
        assert!(status_line(false, None, Some(&failed), &time).contains(&why_text(Why::Timeout)));
        // Every reason has words.
        for w in [Why::Http(500), Why::Network, Why::Timeout, Why::Format, Why::Size, Why::Link, Why::Store, Why::Redirect] {
            assert!(!why_text(w).is_empty() && !why_text(w).starts_with("settings."), "{w:?}");
        }
    }
}
