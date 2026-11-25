//! One painter for the live Composition surface and acceptance images
//! (docs/design/widgets-2026-11-17/mock4.html, battery).
//!
//! 1 x 1: the lowest connected device (its glyph and name), its level as
//! one big figure, the next device on a line at the foot. Any other size:
//! a list (glyph, name, bar, level; one place wide, no name), as many rows
//! as fit. Connected devices show first; one seen connected in the last
//! 24 hours (model::RECENT) follows with its last level, dimmed (the third
//! grey), its last sighting in a tooltip (runtime). Older ones hide: no
//! empty places, no dashes.
use super::model::{Desktop, Device, Kind, Size};
use crate::{
    companion::chat::paint::{rect, rounded},
    desklayer::card::{self, ink, Line, Mark, PAD},
    gfx::Gfx,
    glyphs::Glyph,
    theme::{Rgba, Theme},
};
use windows::{core::Result, Win32::Graphics::Direct2D::ID2D1DeviceContext};

/// The level as one big figure: 40/700, its "%" 18/600.
const BIG: Line = Line { tracking: -0.04, tabular: true, ..Line::new(40., 700, 40.) };
const UNIT: Line = Line::new(18., 600, 24.);
/// A device's name and its level: 12/600.
const LABEL: Line = Line::new(12., 600, 16.);
const LEVEL: Line = Line { tabular: true, ..Line::new(12., 600, 16.) };
/// A list row (medium, large).
pub const ROW: f32 = 28.;
const LIST_TOP: f32 = 44.;
/// The level column (right aligned).
const LEVEL_W: f32 = 34.;

/// Pure: the devices shown at `now`: those with a level, connected or
/// seen in the last 24 hours; connected ones first, each group lowest
/// level first.
pub fn shown_at<'a>(rows: &[&'a Device], now: i64) -> Vec<&'a Device> {
    let mut v: Vec<&Device> = rows.iter().copied().filter(|d| d.percent.is_some() && d.recent(now)).collect();
    v.sort_by_key(|d| (!d.connected, d.percent, d.id.clone()));
    v
}

/// shown_at() now.
pub fn shown<'a>(rows: &[&'a Device]) -> Vec<&'a Device> {
    shown_at(rows, super::model::clock())
}

/// Pure: a device's ink tier: a low connected one the first grey, a
/// connected one the second, one not connected now the third (dimmed).
pub fn tier(d: &Device) -> u8 {
    match (d.connected, d.percent.unwrap_or(0) < 20) {
        (false, _) => 3,
        (true, true) => 1,
        (true, false) => 2,
    }
}

/// Pure: the device under card point (x, y) DIP (its row, or the big
/// figure's card on 1 x 1), of `rows` as shown at `now`.
pub fn device_at<'a>(desktop: &Desktop, rows: &[&'a Device], now: i64, (w, h): (f32, f32), x: f32, y: f32) -> Option<&'a Device> {
    let live = shown_at(rows, now);
    if x < 0. || y < 0. || x > w || y > h {
        return None;
    }
    if big(desktop.size) {
        let foot = h - 14. - 14.;
        return if y >= foot && y < foot + 14. { live.get(1).copied() } else if y >= 40. { live.first().copied() } else { None };
    }
    if y < LIST_TOP {
        return None;
    }
    let i = ((y - LIST_TOP) / ROW).floor() as usize;
    (i < list_rows(h, desktop.size.capacity())).then(|| live.get(i).copied()).flatten()
}

/// Pure: how long ago `then` was at `now`, as the tooltip says it
/// ("방금", "12분 전", "2시간 전").
pub fn ago(then: i64, now: i64) -> String {
    let s = now.saturating_sub(then).max(0);
    if s < 60 {
        crate::i18n::t("battery.seen_now").to_string()
    } else if s < 3600 {
        crate::i18n::t("battery.seen_minutes").replace("{n}", &(s / 60).to_string())
    } else {
        crate::i18n::t("battery.seen_hours").replace("{n}", &(s / 3600).to_string())
    }
}

/// Pure: a card of `size` shows one level as a big figure (1 x 1); any
/// other size lists the devices.
pub fn big(size: Size) -> bool {
    size.w == 1 && size.h == 1
}

/// Pure: the list rows a card `h` tall holds (`cap` at most).
pub fn list_rows(h: f32, cap: usize) -> usize {
    (((h - LIST_TOP - PAD) / ROW).floor().max(0.) as usize).min(cap)
}

/// Pure: where device `id`'s name shows on the card (x, y, w, h DIP): the
/// rename field covers it. Not shown by name: under the header.
pub fn name_rect(desktop: &Desktop, rows: &[&Device], id: &str, (w, h): (f32, f32)) -> (f32, f32, f32, f32) {
    let live = shown(rows);
    let under_header = (PAD, 40., w - 2. * PAD, 24.);
    if big(desktop.size) {
        return if live.first().is_some_and(|d| d.id == id) { (PAD + 18., 40., w - 2. * PAD - 18., 24.) } else { under_header };
    }
    if desktop.size.w < 2 {
        return under_header;
    }
    let Some(i) = live.iter().take(list_rows(h, desktop.size.capacity())).position(|d| d.id == id) else { return under_header };
    let x = PAD + 20.;
    let bar_x = w - PAD - LEVEL_W - 7. - if w < 200. { 36. } else { 64. };
    (x, LIST_TOP + i as f32 * ROW + 2., (bar_x - 6. - x).max(24.), ROW - 4.)
}

/// A device's glyph, 14 DIP, at (x, y).
unsafe fn device(dc: &ID2D1DeviceContext, gfx: &Gfx, kind: Kind, x: f32, y: f32, c: Rgba) -> Result<()> {
    let size = 14.;
    match kind {
        Kind::Keyboard => card::mark(dc, gfx, Mark::Keyboard, x, y, size, c),
        Kind::Mouse => card::mark(dc, gfx, Mark::Mouse, x, y, size, c),
        k => {
            let g = match k {
                Kind::Headphones => Glyph::Headphones,
                Kind::Gamepad => Glyph::Gamepad,
                _ => Glyph::Bluetooth,
            };
            crate::gfx::draw_glyph(dc, g, x + size / 2., y + size / 2., size, card::rgba(c))
        }
    }
}

/// A level bar 4 DIP high from `x`, `w` wide: its track and fill.
unsafe fn bar(dc: &ID2D1DeviceContext, theme: &Theme, x: f32, cy: f32, w: f32, p: u8, c: Rgba) -> Result<()> {
    let track = if theme.is_light { Rgba { r: 0, g: 0, b: 0, a: 26 } } else { Rgba { r: 255, g: 255, b: 255, a: 36 } };
    rounded(dc, rect(x, cy - 2., w, 4.), 2., card::rgba(track))?;
    let f = w * f32::from(p.min(100)) / 100.;
    if f > 0. {
        rounded(dc, rect(x, cy - 2., f.max(4.), 4.), 2., card::rgba(c))?;
    }
    Ok(())
}

/// A line: glyph, bar, level (and the name between, when `name`).
#[allow(clippy::too_many_arguments)]
unsafe fn row(dc: &ID2D1DeviceContext, gfx: &Gfx, theme: &Theme, desktop: &Desktop, d: &Device, y: f32, h: f32, w: f32, name: bool) -> Result<()> {
    let p = d.percent.unwrap_or(0);
    let c = ink(theme, tier(d));
    let cy = y + h / 2.;
    device(dc, gfx, d.kind, PAD, cy - 7., c)?;
    let right = w - PAD;
    let lx = right - LEVEL_W;
    let level = format!("{p}%");
    let lw = card::width(gfx, &level, LEVEL);
    card::line(dc, gfx, &level, right - lw, cy - LEVEL.height / 2., LEVEL, c, LEVEL_W)?;
    let bx = PAD + 14. + 7.;
    if name {
        let bw = if w < 200. { 36. } else { 64. };
        let bar_x = lx - 7. - bw;
        card::line(dc, gfx, desktop.name(d), bx, cy - LABEL.height / 2., LABEL, c, (bar_x - 8. - bx).max(1.))?;
        bar(dc, theme, bar_x, cy, bw, p, c)?;
        // Charging is said only while it is connected (a stale flag otherwise).
        if d.charging && d.connected {
            crate::gfx::draw_glyph(dc, Glyph::Bolt, bar_x - 10., cy, 10., card::rgba(c))?;
        }
    } else {
        bar(dc, theme, bx, cy, (lx - 7. - bx).max(4.), p, c)?;
    }
    Ok(())
}

pub unsafe fn content_sized(dc: &ID2D1DeviceContext, gfx: &Gfx, theme: &Theme, desktop: &Desktop, rows: &[&Device], (w, h): (f32, f32)) -> Result<()> {
    card::header(dc, gfx, theme, card::Kind::Battery, crate::i18n::t("battery.title"), "", w)?;
    let live = shown(rows);
    let Some(first) = live.first().copied() else {
        card::line(dc, gfx, crate::i18n::t("battery.none_connected"), PAD, 44., LABEL, ink(theme, 3), w - 2. * PAD)?;
        return Ok(());
    };
    if !big(desktop.size) {
        // One place wide: no room for names (glyph, bar, level).
        let names = desktop.size.w >= 2;
        for (i, d) in live.iter().take(list_rows(h, desktop.size.capacity())).enumerate() {
            row(dc, gfx, theme, desktop, d, LIST_TOP + i as f32 * ROW, ROW, w, names)?;
        }
        return Ok(());
    }
    // The lowest: its glyph and name, then its level, big (dimmed when it
    // is not connected now).
    let p = first.percent.unwrap_or(0);
    let second = ink(theme, if first.connected { 2 } else { 3 });
    let figure = ink(theme, if first.connected { 1 } else { 3 });
    device(dc, gfx, first.kind, PAD, 45., second)?;
    card::line(dc, gfx, desktop.name(first), PAD + 19., 44., LABEL, second, w - 2. * PAD - 19.)?;
    let value = p.to_string();
    let top = h - 44. - BIG.height;
    let vw = card::line(dc, gfx, &value, PAD, top, BIG, figure, w - 2. * PAD)?;
    // "%" on the figure's baseline (CSS: an inline <small>).
    let ux = PAD + vw + 1.;
    card::line(dc, gfx, "%", ux, top + BIG.height - UNIT.height + 1., UNIT, figure, 24.)?;
    if first.charging && first.connected {
        crate::gfx::draw_glyph(dc, Glyph::Bolt, ux + card::width(gfx, "%", UNIT) + 8., top + BIG.height - 9., 12., card::rgba(second))?;
    }
    // The next lowest at the foot (none with one device).
    if let Some(next) = live.get(1) {
        row(dc, gfx, theme, desktop, next, h - 14. - 14., 14., w, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dev(id: &str, p: u8, connected: bool) -> Device {
        Device { id: id.into(), name: id.into(), percent: Some(p), connected, ..Default::default() }
    }
    /// Owner 0.1.91: a gamepad and headphones that were not connected
    /// stayed as dashed rings. Connected devices show first, lowest first;
    /// one never seen connected (or not for a day) never shows.
    #[test]
    fn only_connected_devices_show_lowest_first() {
        let (a, b, c) = (dev("kb", 50, true), dev("mouse", 95, true), dev("pad", 10, false));
        let none = Device { percent: None, ..dev("x", 0, true) };
        let rows = [&b, &c, &a, &none];
        let v = shown(&rows);
        assert_eq!(v.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["kb", "mouse"]);
        assert!(shown(&[&c]).is_empty());
    }

    /// A device seen connected in
    /// the last 24 hours keeps its last level, dimmed, after the connected
    /// ones; at 24 hours it hides; the empty state shows only when nothing
    /// was seen in a day.
    #[test]
    fn recent_devices_stay_dimmed_for_a_day() {
        use super::super::model::RECENT;
        let now = 1_000_000;
        let kb = Device { last_seen: now - 3 * 3600, ..dev("kb", 50, false) };
        let mouse = Device { last_seen: now - RECENT + 1, ..dev("mouse", 95, false) };
        let old = Device { last_seen: now - RECENT, ..dev("old", 5, false) };
        let pad = dev("pad", 80, true);
        let rows = [&kb, &mouse, &old, &pad];
        let v = shown_at(&rows, now);
        assert_eq!(v.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["pad", "kb", "mouse"], "connected first; 24 h is the edge");
        assert_eq!((tier(&pad), tier(&kb), tier(&dev("low", 10, true))), (2, 3, 1), "dimmed: the third grey");
        assert!(shown_at(&[&old], now).is_empty(), "the empty state: nothing in a day");
        assert!(!shown_at(&[&kb], now).is_empty());
    }

    #[test]
    fn the_tooltip_says_how_long_ago() {
        let _lang = crate::i18n::test_lock("ko");
        assert_eq!(ago(100, 130), "방금");
        assert_eq!(ago(0, 12 * 60), "12분 전");
        assert_eq!(ago(0, 2 * 3600 + 59), "2시간 전");
    }

    /// The pointer over a row is that row's device (the tooltip's).
    #[test]
    fn the_device_under_the_pointer() {
        let now = 1_000_000;
        let kb = Device { last_seen: now - 60, ..dev("kb", 50, false) };
        let pad = dev("pad", 80, true);
        let rows = [&kb, &pad];
        let list = Desktop { size: Size::MEDIUM, ..Default::default() };
        let at = |y: f32| device_at(&list, &rows, now, (288., 176.), 100., y).map(|d| d.id.as_str());
        assert_eq!(at(LIST_TOP + 2.), Some("pad"));
        assert_eq!(at(LIST_TOP + ROW + 2.), Some("kb"));
        assert_eq!(at(20.), None, "the header");
        assert_eq!(at(LIST_TOP + 3. * ROW + 2.), None, "no row there");
        let one = Desktop { size: Size::SMALL, ..Default::default() };
        assert_eq!(device_at(&one, &rows, now, (136., 176.), 40., 90.).map(|d| d.id.as_str()), Some("pad"));
    }
    /// The rename field lies on the device's own name, inside the card.
    #[test]
    fn the_rename_field_covers_the_name() {
        let (a, b) = (dev("kb", 50, true), dev("mouse", 95, true));
        let rows = [&a, &b];
        for (size, dims) in [(Size::SMALL, (136., 176.)), (Size::MEDIUM, (288., 176.)), (Size::LARGE, (288., 368.))] {
            let d = Desktop { size, ..Default::default() };
            for id in ["kb", "mouse", "gone"] {
                let (x, y, w, h) = name_rect(&d, &rows, id, dims);
                assert!(x >= PAD && y >= 32. && x + w <= dims.0 - PAD + 0.5 && y + h <= dims.1 - PAD, "{size:?} {id}: {x} {y} {w} {h}");
            }
        }
        let list = Desktop { size: Size::MEDIUM, ..Default::default() };
        assert!(name_rect(&list, &rows, "mouse", (288., 176.)).1 > name_rect(&list, &rows, "kb", (288., 176.)).1, "its own row");
    }

    #[test]
    fn lists_hold_the_rows_that_fit() {
        assert_eq!(list_rows(176., Size::MEDIUM.capacity()), 4);
        assert_eq!(list_rows(368., Size::LARGE.capacity()), 8);
        assert_eq!(list_rows(60., 8), 0);
    }
}
