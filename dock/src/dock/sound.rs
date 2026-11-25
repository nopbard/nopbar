//! Per-app sound on the dock: the wheel over a running app's icon turns
//! that app's volume up or down (its audio session, see audio::session),
//! the hover label shows the new level for a moment, and while the pointer
//! is on the dock a small speaker marks the apps that are making sound.
//!
//! The wheel over an icon: plain = that app's volume, Shift+wheel = step
//! through that app's windows (towards you = next, away = previous; each
//! one brought to the front without waiting on the app, see input::focus;
//! windows that are not responding are skipped). The wheel over the
//! preview cards scrolls them (preview.rs).
//!
//! All Core Audio work is on the audio worker; this side only sends
//! requests and applies answers (WM_SOUND). The speaker marks are polled
//! only while the pointer is on the dock (at once, then every
//! PEAK_POLL_MS) and dropped when it leaves: nothing runs otherwise.

use super::*;
use crate::audio::session::{self, Reply, Target, WheelAcc};
use std::time::Duration;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
use windows::Win32::Graphics::Gdi::ScreenToClient;

pub(super) use session::WM_SOUND;
/// One-shot: the volume label's time is up.
pub(super) const VOL_LABEL_TIMER: usize = 16;
/// Repeating while the pointer is on the dock: who is making sound.
pub(super) const PEAK_TIMER: usize = 17;
/// Retry taking answers while the dock is busy (a menu is open).
pub(super) const SOUND_RETRY_TIMER: usize = 18;
/// How long the level stays in the label.
const VOL_LABEL_MS: u32 = 1200;
const PEAK_POLL_MS: u32 = 2000;
/// A speaker stays this long after the last poll that heard the app (one
/// quiet poll does not make it blink).
const PLAYING_HOLD: Duration = Duration::from_millis(2500);
/// A known level is trusted this long for showing a step at once.
const LEVEL_FRESH: Duration = Duration::from_secs(5);
/// Answers to a wheel later than this are not shown (the moment passed).
const ANSWER_LATE: Duration = Duration::from_millis(1500);
/// Speaker size as a share of the icon at its largest.
const SPEAKER: f32 = 0.34;

const G_VOLUME: crate::glyphs::Glyph = crate::glyphs::Glyph::Volume(3);
/// Label cache keys of level labels start with this (never an app name).
pub(super) const VOLUME_KEY: &str = "\u{1}volume ";

#[derive(Default)]
pub(super) struct SoundState {
    wheel: WheelAcc,
    /// The app the wheel turned last, and when.
    wheel_key: Option<String>,
    wheel_at: Option<Instant>,
    /// In the hover label: (app path, percent, muted).
    show: Option<(String, u8, bool)>,
    /// Levels the worker reported, by app path.
    levels: HashMap<String, (u8, bool, Instant)>,
    /// When each app was last heard making sound.
    heard: HashMap<String, Instant>,
    /// The pointer is on the dock (from its enter / leave), apart from
    /// whether we poll: polling also waits while nobody can see the dock.
    hover: bool,
    /// Polling for speakers (peak_polling).
    polling: bool,
    /// The speaker drawing and its px size, shared by every icon (made
    /// again on theme changes and for a new size).
    brush: Option<(i32, CompositionSurfaceBrush)>,
    /// Shift+wheel: the delta not yet a whole notch, for which app.
    cycle_acc: i32,
    cycle_key: Option<String>,
    /// The window Shift+wheel brought forward last, and when (its
    /// activation is asynchronous: the foreground may lag behind).
    cycle_last: Option<(isize, Instant)>,
}

/// Shift+wheel follows its own last pick this long, then the foreground.
const CYCLE_FRESH: Duration = Duration::from_millis(1500);
/// One step per wheel notch.
const NOTCH: i32 = 120;

/// Pure: the next window after `cur` (forward) or before it, of `n`, not
/// `skip`ped (wraps). With no current one, forward starts at the first and
/// backward at the last. None if every window is skipped.
pub(super) fn cycle_pick(n: usize, cur: Option<usize>, forward: bool, skip: &dyn Fn(usize) -> bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let start = match cur {
        Some(c) => c.min(n - 1),
        None if forward => n - 1,
        None => 0,
    };
    (1..=n).map(|k| if forward { (start + k) % n } else { (start + n - k) % n }).find(|&i| !skip(i))
}

/// Pure: whole notches in `acc` + `delta` (sign kept) and what is left.
pub(super) fn notches(acc: i32, delta: i32) -> (i32, i32) {
    let total = acc.saturating_add(delta);
    let steps = total / NOTCH;
    (steps, total - steps * NOTCH)
}

thread_local! {
    /// Wheel delta over empty dock space not yet a whole notch.
    static DESK_ACC: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

/// The wheel over empty dock space with dock.wheel_desktops on: one
/// desktop per whole notch (touchpads add up; several notches in one
/// message move as many), up = the previous one, down = the next (no wrap).
fn desktop_wheel(delta: i32) {
    let (steps, rest) = crate::vdesk::notches(DESK_ACC.with(|a| a.get()), delta);
    DESK_ACC.with(|a| a.set(rest));
    if steps != 0 {
        // Several notches at once: as many desktops.
        let s = crate::vdesk::wheel(-steps);
        if s != crate::keys::Sent::Ok {
            crate::log!("desktops: dock wheel: not sent ({s:?})");
        }
    }
}

impl Dock {
    /// Running apps, for the worker to tell whose session is whose.
    fn sound_targets(&self) -> Vec<Target> {
        self.items
            .iter()
            .filter(|it| it.kind == Kind::App && !it.stack && !it.windows.is_empty())
            .map(|it| {
                let mut pids: Vec<u32> = Vec::new();
                for &h in &it.windows {
                    let mut pid = 0u32;
                    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
                    if pid != 0 && !pids.contains(&pid) {
                        pids.push(pid);
                    }
                }
                Target { key: it.path.clone(), pids, exe: it.id.exe.as_ref().map(|e| e.to_lowercase()), aumid: it.id.aumid.clone() }
            })
            .collect()
    }

    /// WM_MOUSEWHEEL at `screen` (px): over a running app, its volume
    /// (`shift`: its windows, one per notch).
    pub(super) fn on_wheel(&mut self, delta: i16, screen: POINT, shift: bool) {
        if self.hidden || self.dragging() || self.ole_drag {
            return;
        }
        let mut p = screen;
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        let (u, _) = self.frame.from_win(p.x as f32 / self.scale, p.y as f32 / self.scale);
        // (A divider is empty space to the wheel.)
        let Some(i) = self.hit(u).filter(|&i| self.items[i].kind != Kind::Divider) else {
            // Empty dock space: virtual desktops, when that is on
            // (dock.wheel_desktops; off by default).
            if self.cfg.dock.wheel_desktops {
                desktop_wheel(delta as i32);
            }
            return;
        };
        let it = &self.items[i];
        if it.kind != Kind::App || it.stack || it.windows.is_empty() {
            return;
        }
        if shift {
            self.cycle_windows(i, delta);
            return;
        }
        let key = it.path.clone();
        if self.sound.wheel_key.as_deref() != Some(key.as_str()) {
            self.sound.wheel.reset();
            self.sound.wheel_key = Some(key.clone());
        }
        let d = self.sound.wheel.feed(delta as i32);
        if d == 0 {
            return;
        }
        self.sound.wheel_at = Some(Instant::now());
        // The cards would cover the label: the wheel is about sound now.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), PREVIEW_TIMER);
        }
        if preview::is_open() {
            preview::hide();
        }
        // A level known from a moment ago: show the step at once (the
        // worker's answer follows and corrects it).
        if let Some(&(p, m, at)) = self.sound.levels.get(&key) {
            if at.elapsed() < LEVEL_FRESH {
                let np = (p as i32 + d).clamp(0, 100) as u8;
                let nm = m && d < 0;
                self.sound.levels.insert(key.clone(), (np, nm, at));
                self.show_volume(&key, np, nm);
            }
        }
        session::step(self.hwnd, &key, d, self.sound_targets());
    }

    /// Shift+wheel over item `i`: its next (towards you) or previous window
    /// to the front, one per notch. Windows that are not responding are
    /// skipped; nothing waits on any app (input::focus).
    fn cycle_windows(&mut self, i: usize, delta: i16) {
        let it = &self.items[i];
        let (key, wins) = (it.path.clone(), it.windows.clone());
        if self.sound.cycle_key.as_deref() != Some(key.as_str()) {
            self.sound.cycle_acc = 0;
            self.sound.cycle_last = None;
            self.sound.cycle_key = Some(key);
        }
        let (steps, rest) = notches(self.sound.cycle_acc, delta as i32);
        self.sound.cycle_acc = rest;
        if steps == 0 {
            return;
        }
        // The cards would cover the windows coming forward.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), PREVIEW_TIMER);
        }
        if preview::is_open() {
            preview::hide();
        }
        // From the window it brought forward a moment ago (the switch may
        // not have happened yet), else from the one in front.
        let last = self.sound.cycle_last.filter(|(_, t)| t.elapsed() < CYCLE_FRESH).map(|(h, _)| h);
        let fg = unsafe { GetForegroundWindow() };
        let mut cur = last.and_then(|h| wins.iter().position(|w| w.0 as isize == h)).or_else(|| wins.iter().position(|&w| w == fg));
        let hung = |k: usize| unsafe { IsHungAppWindow(wins[k]).as_bool() };
        // Towards you (negative delta) = next.
        for _ in 0..steps.unsigned_abs() {
            cur = cycle_pick(wins.len(), cur, steps < 0, &hung);
        }
        let Some(pick) = cur else { return };
        let w = wins[pick];
        self.sound.cycle_last = Some((w.0 as isize, Instant::now()));
        unsafe { input::focus(w) };
    }

    /// The label shows `key`'s level for VOL_LABEL_MS (no first-hover wait:
    /// the user is busy with that icon).
    fn show_volume(&mut self, key: &str, p: u8, muted: bool) {
        self.sound.show = Some((key.to_string(), p, muted));
        if !self.label_armed {
            self.label_armed = true;
            if std::mem::take(&mut self.label_waiting) {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), label::LABEL_TIMER);
                }
            }
        }
        unsafe {
            SetTimer(Some(self.hwnd), VOL_LABEL_TIMER, VOL_LABEL_MS, None);
        }
        self.layout_label();
    }

    /// VOL_LABEL_TIMER: back to the name.
    pub(super) fn volume_label_over(&mut self) {
        if self.sound.show.take().is_some() {
            self.layout_label();
        }
    }

    /// What the label shows instead of item `i`'s name, if anything.
    pub(super) fn volume_label(&self, i: usize) -> Option<(u8, bool)> {
        self.sound.show.as_ref().filter(|(k, _, _)| *k == self.items[i].path).map(|&(_, p, m)| (p, m))
    }

    /// WM_SOUND: the worker's answers.
    pub(super) fn take_sound(&mut self) {
        let now = Instant::now();
        for r in session::take() {
            match r {
                Reply::Level { key, percent, muted } => {
                    // Only fresh levels are ever used: older ones (apps
                    // long closed) go (audit A16).
                    self.sound.levels.retain(|_, (_, _, at)| now.duration_since(*at) < LEVEL_FRESH);
                    self.sound.levels.insert(key.clone(), (percent, muted, now));
                    let recent = self.sound.wheel_at.is_some_and(|t| t.elapsed() < ANSWER_LATE);
                    let under = self.mouse.and_then(|(u, _)| self.hit(u)).is_some_and(|i| self.items[i].path == key);
                    if recent && under && self.sound.wheel_key.as_deref() == Some(key.as_str()) {
                        self.show_volume(&key, percent, muted);
                    }
                }
                Reply::Silent { key } => {
                    // No session: the wheel does nothing for it.
                    self.sound.levels.remove(&key);
                    if self.sound.show.as_ref().is_some_and(|(k, _, _)| *k == key) {
                        self.sound.show = None;
                        self.layout_label();
                    }
                }
                Reply::Playing(keys) => {
                    if self.sound.polling {
                        for k in keys {
                            self.sound.heard.insert(k, now);
                        }
                        self.apply_playing();
                    }
                }
            }
        }
    }

    /// The pointer came onto the dock (true) or left it (false): speakers
    /// are polled only in between.
    pub(super) fn sound_hover(&mut self, on: bool) {
        self.sound.hover = on;
        self.sound_poll();
    }

    /// The session went idle or came back (session.rs): polling follows.
    /// Coming back, the pointer is looked at for real (it may have left
    /// the dock, or still rest on it, while the display was off).
    pub(super) fn sound_resume(&mut self) {
        if self.sound.hover && !pointer_on_window(self.hwnd) {
            self.sound.hover = false;
        }
        self.sound_poll();
    }

    /// Starts or stops the speaker polling as peak_polling says.
    fn sound_poll(&mut self) {
        let on = peak_polling(self.sound.hover, super::session::idle());
        if on == self.sound.polling {
            return;
        }
        self.sound.polling = on;
        unsafe {
            if on {
                SetTimer(Some(self.hwnd), PEAK_TIMER, PEAK_POLL_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), PEAK_TIMER);
            }
        }
        if on {
            self.poll_playing();
        } else {
            self.sound.heard.clear();
            self.sound.wheel_key = None;
            self.sound.wheel.reset();
            self.apply_playing();
        }
    }

    /// PEAK_TIMER (and the pointer's arrival): ask who makes sound.
    pub(super) fn poll_playing(&mut self) {
        if !self.sound.polling {
            return;
        }
        session::poll_playing(self.hwnd, self.sound_targets());
    }

    /// Speakers on the icons of apps heard lately (none unless polling).
    pub(super) fn apply_playing(&mut self) {
        self.sound.heard.retain(|_, t| t.elapsed() < PLAYING_HOLD);
        let Ok(ch) = self.layer.Children() else { return };
        let mut changed = false;
        for i in 0..self.items.len() {
            let it = &self.items[i];
            let want = self.sound.polling && it.kind == Kind::App && !it.windows.is_empty() && self.sound.heard.contains_key(&it.path);
            let have = it.marks.speaker.is_some();
            if want == have {
                continue;
            }
            changed = true;
            if want {
                if let Some(v) = self.speaker_visual() {
                    let _ = ch.InsertAtTop(&v);
                    self.items[i].marks.speaker = Some((v, SPEAKER));
                }
            } else if let Some((v, _)) = self.items[i].marks.speaker.take() {
                let _ = ch.Remove(&v);
            }
        }
        if changed {
            self.layout();
        }
    }

    /// Theme changed: the speakers are drawn again.
    pub(super) fn reset_speakers(&mut self) {
        self.sound.brush = None;
        if let Ok(ch) = self.layer.Children() {
            for it in &mut self.items {
                if let Some((v, _)) = it.marks.speaker.take() {
                    let _ = ch.Remove(&v);
                }
            }
        }
        self.apply_playing();
    }

    /// A speaker in a small disc (the label's colors, so high contrast and
    /// light / dark follow), drawn for the largest icon size.
    fn speaker_visual(&mut self) -> Option<SpriteVisual> {
        let c = self.gfx.compositor.clone();
        let px = (self.look.icon * self.look.max_scale * self.scale * SPEAKER).ceil().max(8.0);
        if self.sound.brush.as_ref().is_none_or(|(n, _)| *n != px as i32) {
            let (bg, fg) = self.theme.label_colors();
            let hc = self.theme.high_contrast;
            let col = |v: Rgba| D2D1_COLOR_F { r: v.r as f32 / 255.0, g: v.g as f32 / 255.0, b: v.b as f32 / 255.0, a: 1.0 };
            let surface = self
                .gfx
                .draw(px as u32, px as u32, |dc, ox, oy| unsafe {
                    let r = px / 2.0;
                    let e = windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE { point: Vector2 { X: ox + r, Y: oy + r }, radiusX: r - 0.5, radiusY: r - 0.5 };
                    let b = dc.CreateSolidColorBrush(&col(bg), None)?;
                    dc.FillEllipse(&e, &b);
                    let edge = if hc { col(fg) } else { D2D1_COLOR_F { a: 0.25, ..col(fg) } };
                    let b = dc.CreateSolidColorBrush(&edge, None)?;
                    dc.DrawEllipse(&e, &b, if hc { 1.5 } else { 1.0 }, None);
                    crate::glyphs::draw(dc, G_VOLUME, ox + r, oy + r, px * 0.56, col(fg))?;
                    Ok(())
                })
                .ok()?;
            let brush = c.CreateSurfaceBrushWithSurface(&surface).ok()?;
            let _ = brush.SetStretch(CompositionStretch::Fill);
            self.sound.brush = Some((px as i32, brush));
        }
        let v = c.CreateSpriteVisual().ok()?;
        let _ = v.SetBrush(&self.sound.brush.as_ref()?.1);
        let _ = v.SetOpacity(0.0);
        Some(v)
    }

    /// The label's content for a level: the speaker glyph (muted: crossed
    /// out), "42%" or "Muted", and a thin level bar under them. Text only
    /// (the pill is the label's visuals); (surface, w, h) in DIPs.
    pub(super) fn volume_surface(&self, p: u8, muted: bool) -> Option<(CompositionDrawingSurface, f32, f32)> {
        let s = self.scale;
        let (_, fg) = self.theme.label_colors();
        use crate::theme::metrics::space;
        let col = |c: crate::theme::Rgba| D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 };
        let ink = col(fg);
        let family = crate::fonts::ui_family();
        let text = session::level_text(p, muted);
        let g = crate::topbar::status::volume_glyph(p, muted);
        // The glyph's box (16, the small icon size), the level in the name
        // label's type (footnote 12, tabular: the pill keeps its width).
        let icon = crate::theme::metrics::size::ICON_S;
        let (gw, gh) = ((icon * s).round(), (icon * s).round());
        let size = crate::theme::ramp::FOOTNOTE.size * s;
        let (tl, tw, th) = self.gfx.text_layout_tabular(&text, &family, size, DWRITE_FONT_WEIGHT_NORMAL).ok()?;
        // As wide as "100%" at least, so the pill does not twitch per step.
        let widest = self.gfx.text_layout_tabular(&session::level_text(100, false), &family, size, DWRITE_FONT_WEIGHT_NORMAL).map(|l| l.1).unwrap_or(tw);
        // The name label's padding, on the 4 DIP grid.
        let (px, py, gap) = (space::S3 * s, space::S2 * s, space::S3 * s);
        let row = gh.max(th);
        // The level bar: a track (TRACK_H, a capsule).
        let bar_h = (crate::theme::metrics::size::TRACK_H * s).round().max(2.0);
        let bar_gap = space::S2 * s;
        let w = (px * 2.0 + gw + gap + tw.max(widest)).ceil();
        let h = (py * 2.0 + row + bar_gap + bar_h).max(crate::gfx::LABEL_MIN_H * s).ceil();
        // The level bar: the track fill, the ink (the faint level when muted).
        let track_c = col(self.theme.fill_track());
        let fill_c = if muted { col(self.theme.text_tertiary()) } else { ink };
        let surface = self
            .gfx
            .draw(w as u32, h as u32, |dc, ox, oy| unsafe {
                use windows::Win32::Graphics::Direct2D::{D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT};
                use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
                let b = dc.CreateSolidColorBrush(&ink, None)?;
                let top = oy + py;
                crate::glyphs::draw(dc, g, ox + px + gw / 2.0, top + row / 2.0, gw, ink)?;
                dc.DrawTextLayout(Vector2 { X: ox + px + gw + gap, Y: (top + (row - th) / 2.0).round() }, &tl, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                let (x0, x1) = (ox + px, ox + w - px);
                let by = top + row + bar_gap;
                let rr = |x: f32| D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x0, top: by, right: x, bottom: by + bar_h }, radiusX: bar_h / 2.0, radiusY: bar_h / 2.0 };
                let track = dc.CreateSolidColorBrush(&track_c, None)?;
                dc.FillRoundedRectangle(&rr(x1), &track);
                if p > 0 {
                    // Muted: the level stays visible, faint.
                    let fill = dc.CreateSolidColorBrush(&fill_c, None)?;
                    dc.FillRoundedRectangle(&rr((x0 + (x1 - x0) * p as f32 / 100.0).max(x0 + bar_h)), &fill);
                }
                Ok(())
            })
            .ok()?;
        Some((surface, w / s, h / s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_wheel_cycles_in_dock_order_and_wraps() {
        let none = |_: usize| false;
        // From the one in front: next, wrapping at the end.
        assert_eq!(cycle_pick(3, Some(0), true, &none), Some(1));
        assert_eq!(cycle_pick(3, Some(2), true, &none), Some(0));
        assert_eq!(cycle_pick(3, Some(0), false, &none), Some(2));
        assert_eq!(cycle_pick(3, Some(1), false, &none), Some(0));
        // None in front: forward starts at the first, backward at the last.
        assert_eq!(cycle_pick(3, None, true, &none), Some(0));
        assert_eq!(cycle_pick(3, None, false, &none), Some(2));
        // A current index past the end is taken as the last.
        assert_eq!(cycle_pick(3, Some(7), true, &none), Some(0));
        // Several notches walk the same order.
        let mut cur = Some(0);
        let mut seen = Vec::new();
        for _ in 0..4 {
            cur = cycle_pick(3, cur, true, &none);
            seen.push(cur.unwrap());
        }
        assert_eq!(seen, [1, 2, 0, 1]);
        assert_eq!(cycle_pick(0, None, true, &none), None);
        assert_eq!(cycle_pick(1, Some(0), true, &none), Some(0), "one window: itself again");
    }

    #[test]
    fn shift_wheel_skips_windows_that_are_not_responding() {
        let hung = |k: usize| k == 1;
        assert_eq!(cycle_pick(3, Some(0), true, &hung), Some(2));
        assert_eq!(cycle_pick(3, Some(2), false, &hung), Some(0));
        // Only the current one answers: it stays.
        let others = |k: usize| k != 2;
        assert_eq!(cycle_pick(3, Some(2), true, &others), Some(2));
        assert_eq!(cycle_pick(3, Some(0), true, &|_| true), None);
    }

    #[test]
    fn wheel_notches_add_up() {
        assert_eq!(notches(0, -120), (-1, 0));
        assert_eq!(notches(0, 240), (2, 0));
        // Fine-grained wheels: steps only per whole notch, the rest kept.
        assert_eq!(notches(0, 40), (0, 40));
        assert_eq!(notches(40, 40), (0, 80));
        assert_eq!(notches(80, 50), (1, 10));
        assert_eq!(notches(-80, -50), (-1, -10));
        assert_eq!(notches(i32::MAX, 1).0, i32::MAX / 120);
    }
}

/// Pure: speakers are polled while the pointer is on the dock and someone
/// can see it (not locked, asleep or with the display off).
pub(super) fn peak_polling(hover: bool, idle: bool) -> bool {
    hover && !idle
}

/// The pointer is inside window `h` right now.
fn pointer_on_window(h: HWND) -> bool {
    let mut pt = POINT::default();
    let mut r = RECT::default();
    unsafe { GetCursorPos(&mut pt).is_ok() && GetWindowRect(h, &mut r).is_ok() && pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom }
}

#[cfg(test)]
mod peak_tests {
    use super::peak_polling;

    #[test]
    fn speakers_come_back_after_the_display_with_the_pointer_still_on_the_dock() {
        // On the dock, then the display goes off: polling stops, the
        // pointer is still there.
        let hover = true;
        assert!(peak_polling(hover, false));
        assert!(!peak_polling(hover, true), "display off / locked: no polling");
        // Display on again, pointer never moved: polling again.
        assert!(peak_polling(hover, false));
        // Left the dock meanwhile (sound_resume looks): stays off.
        assert!(!peak_polling(false, false));
    }
}
