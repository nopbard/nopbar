//! Picture in picture: what is playing, in a small window that stays on
//! top (as on a Mac). The Live Island's card opens it; its button shows
//! while the playing app has a window we can find (`source_for`: the media
//! session's app id matched to a window as audio::session::media_match
//! matches it to a process; for a browser its most recently active window,
//! the one whose title holds the track's first).
//!
//! - The window is a borderless topmost tool window (no taskbar button,
//!   never activated by a click) holding a live DWM thumbnail of the source
//!   window's client area at the source's aspect ratio. Its own content
//!   (GDI, under the thumbnail) is a plain backdrop, with "Minimized" while
//!   the source is minimized (a minimized window has no picture; restoring
//!   it is left to the person).
//! - It lives in a corner of its display's work area: dragged anywhere, it
//!   snaps to the nearest corner on release (a short glide; at once with
//!   animations off). The wheel or a bottom corner resizes it (MIN_W..=MAX_W
//!   DIP wide); it keeps its DIP size across displays (WM_DPICHANGED).
//! - Its controls (play / pause through the island's media path, go to the
//!   window, close) are a second small window owned by it, since the
//!   thumbnail draws over the window's own content: they fade in on hover
//!   (FADE_IN_MS) and out (FADE_OUT_MS), a panel (popup::chrome) of glyph
//!   capsules whose fills are crate::wash's (the pointer's fading, a press
//!   at once and smaller by press_scale; high contrast: the theme's
//!   outlines and inverted glyphs, drawn), the keyboard's focus the shared
//!   ring (gfx::focus_ring).
//! - It closes when the source window is destroyed (WinEvent hooks on the
//!   source's process: destroy, hide, move / size, minimize; no polling) or
//!   when nothing plays for GONE_MS. It never takes the keyboard by itself;
//!   a screen reader can focus it (Tab / arrows, Enter, Esc closes).
//!
//! Idle cost: nothing runs while no PiP is out.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, Interface, Result, BOOL, HSTRING};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionStretch, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::Dwm::{
    DwmRegisterThumbnail, DwmSetWindowAttribute, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties, DWMWA_USE_HOSTBACKDROPBRUSH, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetMonitorInfoW, InvalidateRect, MonitorFromRect, SelectObject, SetBkMode,
    SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HFONT, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use crate::a11y::{self, Act, Node, Role, Tree};
use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::i18n::t;
use crate::theme::metrics::{size, space};
use crate::theme::{Level, Rgba, Role as Kind, State, Theme};
use crate::wash::{Hover, Lit, Shape};

/// Its width when it opens, and the least and most it is resized to (DIP).
const WIDTH: f32 = 360.0;
pub const MIN_W: f32 = 200.0;
pub const MAX_W: f32 = 720.0;
/// From the work area's edges (DIP).
const MARGIN: f32 = size::EDGE_GAP;
/// The bottom corners that resize it (DIP square).
const GRIP: f32 = 16.0;
/// The controls: a row of BTN glyph capsules (their glyphs GLYPH) in a
/// panel BAR_LIFT above the picture's bottom edge; the capsules on the
/// panel's container line (BAR_PAD), BAR_GAP apart.
const BTN: f32 = 32.0;
const GLYPH: f32 = size::ICON_S;
const BAR_PAD: f32 = size::PANEL_PAD;
const BAR_GAP: f32 = space::S2;
const BAR_LIFT: f32 = space::S4;
/// "Minimized": Body (GDI: it draws under the thumbnail).
const NOTE: crate::theme::ramp::TextStyle = crate::theme::ramp::BODY;
/// One wheel notch scales it by this.
const WHEEL_STEP: f32 = 1.1;
const WHEEL_NOTCH: i32 = 120;
/// The glide to a corner (at the effects' speed) and its frame.
const SNAP_MS: i64 = 200;
const FRAME_MS: u32 = 10;
/// The pointer may cross between the picture and its controls this long.
const HIDE_MS: u32 = 150;
/// Nothing plays this long: it closes (a browser drops its session
/// between two videos for a moment).
const GONE_MS: u32 = 3000;
/// The source hidden this long (closed to the tray): it closes.
const HIDDEN_MS: u32 = 1000;
/// Without WinEvent hooks (they failed): the source is looked at this often.
const POLL_MS: u32 = 1000;
const DEFAULT_ASPECT: f32 = 16.0 / 9.0;
/// The card asks often: the answer is kept this long.
const FOUND_FOR: Duration = Duration::from_secs(3);

/// Posted to the PiP by its WinEvent hooks (wparam: the event).
const WM_PIP_SOURCE: u32 = WM_APP + 120;
/// Posted to the PiP: put the windows where the state says (after a
/// display change of DPI, outside that message).
const WM_PIP_SYNC: u32 = WM_APP + 121;
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;
const T_HIDE: usize = 1;
const T_ANIM: usize = 2;
const T_GONE: usize = 3;
const T_HIDDEN: usize = 4;
const T_POLL: usize = 5;
/// On the controls' window: their fade-out is over; hidden.
const T_BAR_GONE: usize = 6;

/// The island card's button that opens it.
pub const G_PIP: Glyph = Glyph::PictureInPicture;
const G_PLAY: Glyph = Glyph::Play;
const G_PAUSE: Glyph = Glyph::Pause;
/// Back to the app's own window.
const G_GO: Glyph = Glyph::BackToApp;
const G_CLOSE: Glyph = Glyph::Close;

// ---- pure: geometry ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    fn left(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::BottomLeft)
    }
    fn top(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::TopRight)
    }
}

fn rw(r: &RECT) -> i32 {
    r.right - r.left
}
fn rh(r: &RECT) -> i32 {
    r.bottom - r.top
}

/// A width (DIP) kept within MIN_W..=MAX_W (nonsense: the default).
pub fn clamp_width(w: f32) -> f32 {
    if w.is_finite() { w.clamp(MIN_W, MAX_W) } else { WIDTH }
}

/// A source's aspect ratio (w / h) worth using; the default otherwise.
fn sane_aspect(a: f32) -> f32 {
    if a.is_finite() && a > 0.0 { a.clamp(0.25, 4.0) } else { DEFAULT_ASPECT }
}

/// Its size in px: `width` DIP (clamped) at `scale` with the source's
/// `aspect`, made smaller (keeping the aspect) to fit `work` less its
/// margins.
pub fn size_px(width: f32, aspect: f32, scale: f32, work: RECT) -> (i32, i32) {
    let a = sane_aspect(aspect);
    let m = 2.0 * MARGIN * scale;
    let room_w = (rw(&work) as f32 - m).max(1.0);
    let room_h = (rh(&work) as f32 - m).max(1.0);
    let mut w = (clamp_width(width) * scale).min(room_w);
    let mut h = w / a;
    if h > room_h {
        h = room_h;
        w = h * a;
    }
    ((w.round() as i32).max(1), (h.round() as i32).max(1))
}

/// The largest `aspect` rect centered in a `bw` x `bh` box (px).
pub fn fit_rect(bw: i32, bh: i32, aspect: f32) -> RECT {
    let a = sane_aspect(aspect);
    let (bw_f, bh_f) = (bw.max(0) as f32, bh.max(0) as f32);
    let (w, h) = if bh_f > 0.0 && bw_f / bh_f > a { (bh_f * a, bh_f) } else { (bw_f, bw_f / a) };
    let (w, h) = (w.round() as i32, h.round() as i32);
    let (x, y) = ((bw - w) / 2, (bh - h) / 2);
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// The corner of `work` nearest to `r` (by their centers).
pub fn nearest_corner(r: RECT, work: RECT) -> Corner {
    let left = (r.left + r.right) < (work.left + work.right);
    let top = (r.top + r.bottom) < (work.top + work.bottom);
    match (left, top) {
        (true, true) => Corner::TopLeft,
        (false, true) => Corner::TopRight,
        (true, false) => Corner::BottomLeft,
        (false, false) => Corner::BottomRight,
    }
}

/// Where a `w` x `h` window sits in corner `c` of `work`, `m` px in.
pub fn corner_origin(c: Corner, work: RECT, w: i32, h: i32, m: i32) -> POINT {
    POINT { x: if c.left() { work.left + m } else { work.right - m - w }, y: if c.top() { work.top + m } else { work.bottom - m - h } }
}

/// `r` resized to `w` x `h` with its corner `c` where it was.
pub fn anchored(c: Corner, r: RECT, w: i32, h: i32) -> RECT {
    let x = if c.left() { r.left } else { r.right - w };
    let y = if c.top() { r.top } else { r.bottom - h };
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// `r` moved inside `work` (its top left kept in when it is larger).
pub fn clamp_into(r: RECT, work: RECT) -> RECT {
    let (w, h) = (rw(&r), rh(&r));
    let x = r.left.min(work.right - w).max(work.left);
    let y = r.top.min(work.bottom - h).max(work.top);
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// The resize grip under (x, y) of a `w` x `h` window (px, `g` px square):
/// the corner that stays put while it is dragged.
pub fn grip_anchor(x: i32, y: i32, w: i32, h: i32, g: i32) -> Option<Corner> {
    if y < h - g || y >= h {
        return None;
    }
    if x >= 0 && x < g {
        Some(Corner::TopRight)
    } else if x >= w - g && x < w {
        Some(Corner::TopLeft)
    } else {
        None
    }
}

/// Slow at the end.
fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

// ---- pure: the controls -----------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Btn {
    Toggle,
    Go,
    Close,
}

impl Btn {
    fn key(self) -> &'static str {
        match self {
            Btn::Toggle => "toggle",
            Btn::Go => "go",
            Btn::Close => "close",
        }
    }
}

/// The controls in reading (and keyboard) order: play / pause only while
/// the source's app is what plays and lets it be paused.
pub fn buttons(toggle: bool) -> Vec<Btn> {
    let mut v = Vec::with_capacity(3);
    if toggle {
        v.push(Btn::Toggle);
    }
    v.extend([Btn::Go, Btn::Close]);
    v
}

/// The controls' pill for `n` buttons (DIP).
fn bar_size(n: usize) -> (f32, f32) {
    let n = n.max(1) as f32;
    (BAR_PAD * 2.0 + n * BTN + (n - 1.0) * BAR_GAP, BTN + BAR_PAD * 2.0)
}

/// Button `i` of `n` in the pill (DIP; mirrored right to left).
fn btn_rect(i: usize, n: usize, rtl: bool) -> D2D_RECT_F {
    let (w, _) = bar_size(n);
    let l = BAR_PAD + i as f32 * (BTN + BAR_GAP);
    let (l, r) = crate::i18n::mirror::edges(rtl, l, l + BTN, 0.0, w);
    D2D_RECT_F { left: l, top: BAR_PAD, right: r, bottom: BAR_PAD + BTN }
}

/// The pill over the picture `pip` (px): centered, BAR_LIFT above its
/// bottom.
pub fn bar_rect(pip: RECT, scale: f32, n: usize) -> RECT {
    let (w, h) = bar_size(n);
    let (w, h) = ((w * scale).ceil() as i32, (h * scale).ceil() as i32);
    let x = pip.left + (rw(&pip) - w) / 2;
    let y = pip.bottom - (BAR_LIFT * scale).round() as i32 - h;
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// What the controls show of the pointer and the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct BarLook {
    hover: Option<Btn>,
    pressed: Option<Btn>,
    /// The keyboard's focus (only after a key: its ring).
    focus: Option<Btn>,
    playing: bool,
}

impl BarLook {
    /// Pure: `b`'s state (what high contrast draws from and the glyph's
    /// ink): held down, else under the pointer.
    fn state(&self, b: Btn) -> State {
        if self.pressed == Some(b) {
            State::Pressed
        } else if self.hover == Some(b) {
            State::Hover
        } else {
            State::Rest
        }
    }

    /// Pure: the pointer's fill (crate::wash::Hover): the button held
    /// down, else the one under the pointer.
    fn lit(&self) -> Option<(Btn, bool)> {
        self.pressed.map(|b| (b, true)).or(self.hover.map(|b| (b, false)))
    }
}

/// Pure: button `b`'s capsule among `order` (DIP), None when it is not
/// there.
fn btn_shape(order: &[Btn], b: Btn, rtl: bool) -> Option<Shape> {
    let i = order.iter().position(|x| *x == b)?;
    let r = btn_rect(i, order.len(), rtl);
    Some(Shape::capsule(r.left, r.top, r.right - r.left, r.bottom - r.top))
}

/// The controls' picture (DIP): each button's glyph in its state's ink,
/// the keyboard's ring and, in high contrast, the state itself (outline
/// under the pointer, Highlight fill pressed: theme::fill_of / outline_of).
/// The fills outside high contrast are visuals under it (crate::wash).
unsafe fn paint_bar(dc: &ID2D1DeviceContext, theme: &Theme, order: &[Btn], look: BarLook, rtl: bool) -> Result<()> {
    let n = order.len();
    for (i, &b) in order.iter().enumerate() {
        let r = btn_rect(i, n, rtl);
        let rad = (r.bottom - r.top) / 2.0;
        let st = look.state(b);
        if theme.hc.is_some() {
            let fill = theme.fill_of(Kind::Row, st);
            if fill.a > 0 {
                dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &dc.CreateSolidColorBrush(&d2d(fill), None)?);
            }
            if let Some((c, w)) = theme.outline_of(Kind::Row, st) {
                let h = w / 2.0;
                let inner = D2D_RECT_F { left: r.left + h, top: r.top + h, right: r.right - h, bottom: r.bottom - h };
                dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: inner, radiusX: rad - h, radiusY: rad - h }, &dc.CreateSolidColorBrush(&d2d(c), None)?, w, None);
            }
        }
        if look.focus == Some(b) {
            crate::gfx::focus_ring(dc, r, rad, theme, 1.0)?;
        }
        let g = match b {
            Btn::Toggle if look.playing => G_PAUSE,
            Btn::Toggle => G_PLAY,
            Btn::Go => G_GO,
            Btn::Close => G_CLOSE,
        };
        crate::glyphs::draw(dc, g, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, GLYPH, d2d(theme.text_of(Level::Primary, st)))?;
    }
    Ok(())
}

/// The next button from `cur` in `dir` (wraps; none yet: the first or last).
fn step(order: &[Btn], cur: Option<Btn>, dir: i32) -> Option<Btn> {
    if order.is_empty() {
        return None;
    }
    let n = order.len() as i32;
    let i = match cur.and_then(|c| order.iter().position(|b| *b == c)) {
        Some(i) => (i as i32 + dir).rem_euclid(n),
        None => if dir >= 0 { 0 } else { n - 1 },
    };
    Some(order[i as usize])
}

// ---- pure: the source window ------------------------------------------------------

/// A window that may show what plays (z-order: most recently active first).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cand {
    pub exe: Option<String>,
    pub aumid: Option<String>,
    pub title: String,
    pub minimized: bool,
}

/// The window of the media session's app `app` that shows the track
/// `title`: of the app's windows (audio::session::media_match), the one
/// whose title holds the track's, else one on screen, else the first; the
/// most recently active of equals (a browser's front window).
pub fn pick(app: &str, title: &str, wins: &[Cand]) -> Option<usize> {
    let want = title.trim().to_lowercase();
    let mut best: Option<(usize, (bool, bool))> = None;
    for (i, c) in wins.iter().enumerate() {
        if !crate::audio::session::media_match(app, c.exe.as_deref(), c.aumid.as_deref()) {
            continue;
        }
        let titled = want.chars().count() >= 2 && c.title.to_lowercase().contains(&want);
        let rank = (titled, !c.minimized);
        if best.is_none_or(|(_, r)| rank > r) {
            best = Some((i, rank));
        }
    }
    best.map(|b| b.0)
}

// ---- finding the source -----------------------------------------------------------

unsafe fn title_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

/// The window showing what `app` plays now (`title`: the track), looked
/// up fresh.
pub fn find(app: &str, title: &str) -> Option<HWND> {
    if app.trim().is_empty() {
        return None;
    }
    let wins = crate::apps::enumerate(HWND::default());
    let cands: Vec<Cand> = wins
        .iter()
        .map(|w| unsafe { Cand { exe: w.exe.clone(), aumid: w.aumid.clone(), title: title_of(w.hwnd), minimized: IsIconic(w.hwnd).as_bool() } })
        .collect();
    pick(app, title, &cands).map(|i| wins[i].hwnd)
}

thread_local! {
    /// The last answer of source_for: (app, title, when, window).
    static FOUND: RefCell<Option<(String, String, Instant, isize)>> = const { RefCell::new(None) };
}

/// `find`, kept for FOUND_FOR (the card asks on every refresh); a window
/// gone since is looked up again.
pub fn source_for(app: &str, title: &str) -> Option<HWND> {
    let hit = FOUND.with(|f| {
        let f = f.try_borrow().ok()?;
        let (a, tt, at, h) = f.as_ref()?;
        let alive = *h == 0 || unsafe { IsWindow(Some(HWND(*h as *mut _))).as_bool() };
        (a == app && tt == title && at.elapsed() < FOUND_FOR && alive).then_some(*h)
    });
    let h = match hit {
        Some(h) => h,
        None => {
            let h = find(app, title).map(|h| h.0 as isize).unwrap_or(0);
            FOUND.with(|f| {
                if let Ok(mut f) = f.try_borrow_mut() {
                    *f = Some((app.to_string(), title.to_string(), Instant::now(), h));
                }
            });
            h
        }
    };
    (h != 0).then_some(HWND(h as *mut _))
}

/// The source's client aspect ratio (None while it has no client area:
/// minimized).
unsafe fn client_aspect(src: HWND) -> Option<f32> {
    if IsIconic(src).as_bool() {
        return None;
    }
    let mut r = RECT::default();
    GetClientRect(src, &mut r).ok()?;
    (rw(&r) > 0 && rh(&r) > 0).then(|| rw(&r) as f32 / rh(&r) as f32)
}

/// The work area of the display holding most of `r`, and that display's
/// scale.
fn work_of(r: RECT) -> (RECT, f32) {
    unsafe {
        let m = MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut mi);
        let scale = crate::monitors::info(m.0 as isize).map(|i| i.scale()).unwrap_or(1.0);
        (mi.rcWork, scale)
    }
}

// ---- what plays -------------------------------------------------------------------

/// What the island's media listener has now.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Media {
    pub app: String,
    /// The track's title (as the island shows it).
    pub title: String,
    /// Which media session (the listener's count of sessions bound).
    pub session: u32,
    pub playing: bool,
    pub can_toggle: bool,
}

/// The session a PiP's play / pause belongs to after `m` came: the one it
/// had while `m` is that session (or nothing plays); a new session of the
/// same app only when its track is found in the PiP's own window
/// (`shows_source`, asked only then: a window lookup); else still the old
/// one, so the button goes rather than pausing another window's player.
pub fn tie(tied: Option<u32>, app: &str, m: Option<&Media>, shows_source: impl FnOnce(&Media) -> bool) -> Option<u32> {
    match m {
        None => tied,
        Some(m) if Some(m.session) == tied => tied,
        Some(m) if m.app == app && shows_source(m) => Some(m.session),
        Some(_) => tied,
    }
}

/// Play / pause shows: what plays is the PiP's own session and it may be
/// paused.
pub fn can_toggle(tied: Option<u32>, m: Option<&Media>) -> bool {
    m.is_some_and(|m| Some(m.session) == tied && m.can_toggle)
}

/// What the T_GONE timer finds: nothing has played since `since` (None:
/// something plays now).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gone {
    /// Something plays again: stays.
    Stay,
    /// Not long enough yet (a timer of an earlier absence): look again then.
    Wait(Duration),
    Close,
}

pub fn gone_check(since: Option<Instant>, now: Instant, grace: Duration) -> Gone {
    let Some(s) = since else { return Gone::Stay };
    let past = now.saturating_duration_since(s);
    if past >= grace { Gone::Close } else { Gone::Wait(grace - past) }
}

/// The source window's state: alive, or gone (destroyed, or hidden for
/// `grace`: closed to the tray). Returns it and since when it is hidden
/// (None: on screen), for the hooks and the polling alike.
pub fn alive(exists: bool, visible: bool, hidden_since: Option<Instant>, now: Instant, grace: Duration) -> (bool, Option<Instant>) {
    if !exists {
        return (false, None);
    }
    if visible {
        return (true, None);
    }
    let since = hidden_since.unwrap_or(now);
    (now.saturating_duration_since(since) < grace, Some(since))
}

/// What plays changed (the island's main bar; None: nothing plays). The
/// PiP closes after GONE_MS of nothing (counted from the first None;
/// repeats keep that deadline); another session playing only takes its
/// play / pause away.
pub fn media(m: Option<Media>) {
    // Does a new session of the PiP's app play in its window? Only then is
    // a window looked up (rare), and outside the borrow.
    let mut asked = None;
    let Some((session, app, src)) = with_pip(|p| (p.session, p.app.clone(), p.src)) else { return };
    tie(session, &app, m.as_ref(), |n| {
        asked = Some(find(&n.app, &n.title) == Some(src));
        false
    });
    let Some((hwnd, arm, changed)) = with_pip(|p| {
        let before = (p.media.clone(), p.session);
        p.session = tie(p.session, &p.app, m.as_ref(), |_| asked.unwrap_or(false));
        p.media = m.clone();
        let arm = match (&m, p.gone_since) {
            (None, None) => {
                p.gone_since = Some(Instant::now());
                Some(true)
            }
            (None, Some(_)) => None,
            (Some(_), _) => {
                p.gone_since = None;
                Some(false)
            }
        };
        (p.hwnd, arm, before != (p.media.clone(), p.session))
    }) else {
        return;
    };
    unsafe {
        match arm {
            Some(true) => {
                SetTimer(Some(hwnd), T_GONE, GONE_MS, None);
            }
            Some(false) => {
                let _ = KillTimer(Some(hwnd), T_GONE);
            }
            None => {}
        }
    }
    if changed {
        with_pip(|p| p.forget_gone());
        sync();
    }
}

/// T_GONE: closes only if nothing plays still and since GONE_MS (a timer
/// already queued when media came back, or left from an earlier absence,
/// closes nothing).
fn gone_timer(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), T_GONE);
    }
    let check = with_pip(|p| gone_check(p.gone_since.filter(|_| p.media.is_none()), Instant::now(), Duration::from_millis(GONE_MS as u64)));
    match check {
        Some(Gone::Close) => close(),
        Some(Gone::Wait(d)) => unsafe {
            SetTimer(Some(hwnd), T_GONE, (d.as_millis() as u32).max(1), None);
        },
        _ => {}
    }
}

/// The look changed (the theme, high contrast).
pub fn relook(theme: &Theme) {
    let Some(hwnd) = with_pip(|p| {
        p.theme = theme.clone();
        p.bar.built = None;
        p.bar.wash.clear();
        p.hwnd
    }) else {
        return;
    };
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    sync();
}

// ---- the window -------------------------------------------------------------------

/// What `open` needs.
pub struct Request {
    pub gfx: Rc<Gfx>,
    pub theme: Theme,
    pub source: HWND,
    pub app: String,
    /// The display it opens on (HMONITOR).
    pub monitor: isize,
    pub media: Option<Media>,
}

#[derive(Clone, Copy, Debug)]
enum Grab {
    Move,
    /// Resized from a bottom corner; this corner stays.
    Resize(Corner),
}

struct Drag {
    grab: Grab,
    /// The cursor when it started (screen px) and the window then.
    at: POINT,
    from: RECT,
}

struct Anim {
    from: POINT,
    to: POINT,
    start: Instant,
    ms: i64,
}

/// The controls' window.
struct Bar {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    outer: ContainerVisual,
    content: SpriteVisual,
    /// Under the picture: the pointer's fill (crate::wash).
    under: ContainerVisual,
    wash: Hover<Btn>,
    /// The chrome is built for (buttons, scale bits).
    built: Option<(usize, u32)>,
    /// Wanted on screen (hover, the keyboard).
    shown: bool,
    /// Faded in (its window up); false while it fades out and after.
    lit: bool,
    tracking: bool,
}

struct Pip {
    hwnd: HWND,
    bar: Bar,
    src: HWND,
    app: String,
    media: Option<Media>,
    /// The media session its play / pause belongs to (`tie`).
    session: Option<u32>,
    /// Nothing has played since (T_GONE's deadline counts from it).
    gone_since: Option<Instant>,
    /// The source has been hidden since (closed after HIDDEN_MS).
    hidden_since: Option<Instant>,
    thumb: isize,
    hooks: Vec<HWINEVENTHOOK>,
    /// The source's client w / h.
    aspect: f32,
    /// Its width (DIP) and its display's scale.
    width: f32,
    scale: f32,
    /// On screen (px).
    rect: RECT,
    minimized: bool,
    /// The thumbnail's properties as last set: (destination, visible).
    thumb_set: Option<(RECT, bool)>,
    gfx: Rc<Gfx>,
    theme: Theme,
    font: HFONT,
    font_scale: f32,
    rtl: bool,
    drag: Option<Drag>,
    anim: Option<Anim>,
    hover: Option<Btn>,
    pressed: Option<Btn>,
    focus: Option<Btn>,
    /// A key moved the focus (its ring shows).
    keyboard: bool,
    tracking: bool,
    wheel_acc: i32,
}

thread_local! {
    static PIP: RefCell<Option<Pip>> = const { RefCell::new(None) };
    /// Closed, fading out (close): painted, never driven.
    static CLOSING: RefCell<Vec<Pip>> = const { RefCell::new(Vec::new()) };
}

/// The source window and the PiP, for the WinEvent hooks (no borrow).
static SRC: AtomicIsize = AtomicIsize::new(0);
static PIP_HWND: AtomicIsize = AtomicIsize::new(0);
/// A move / size of the source is posted and not yet handled.
static MOVED: AtomicBool = AtomicBool::new(false);

fn with_pip<R>(f: impl FnOnce(&mut Pip) -> R) -> Option<R> {
    PIP.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

unsafe fn register_classes() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    for (name, proc_) in [(w!("NopBarPip"), pip_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT), (w!("NopBarPipBar"), bar_proc)] {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: hinst.into(),
            lpszClassName: name,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        // Fails harmlessly once registered.
        RegisterClassExW(&wc);
    }
}

/// Shows `req.source` in a PiP (closing one still out). False if it could
/// not.
pub fn open(req: Request) -> bool {
    close();
    let src = req.source;
    unsafe {
        if !IsWindow(Some(src)).as_bool() {
            return false;
        }
        register_classes();
        let mon = crate::monitors::info(req.monitor).or_else(crate::monitors::primary);
        let (work, scale) = match mon {
            Some(m) => (m.work, m.scale()),
            None => work_of(RECT::default()),
        };
        let aspect = client_aspect(src).unwrap_or(DEFAULT_ASPECT);
        let (w, h) = size_px(WIDTH, aspect, scale, work);
        let o = corner_origin(Corner::BottomRight, work, w, h, (MARGIN * scale).round() as i32);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("NopBarPip"),
            &HSTRING::from(t("pip.name")),
            WS_POPUP,
            o.x,
            o.y,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                crate::log!("pip: cannot create the window: {e}");
                return false;
            }
        };
        let round = DWMWCP_ROUND;
        // Windows 10 has no such attribute: square corners there.
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const _, 4);
        let thumb = match DwmRegisterThumbnail(hwnd, src) {
            Ok(t) => t,
            Err(e) => {
                crate::log!("pip: no thumbnail of the window: {e}");
                let _ = DestroyWindow(hwnd);
                return false;
            }
        };
        let bar = match Bar::new(&req.gfx, hwnd) {
            Ok(b) => b,
            Err(e) => {
                crate::log!("pip: cannot create its controls: {e}");
                let _ = DwmUnregisterThumbnail(thumb);
                let _ = DestroyWindow(hwnd);
                return false;
            }
        };
        let mut pid = 0u32;
        GetWindowThreadProcessId(src, Some(&mut pid));
        let hooks = hook(pid);
        // The window's own DPI (where it really opened).
        let dpi = GetDpiForWindow(hwnd);
        let scale = if dpi > 0 { dpi as f32 / 96.0 } else { scale };
        let theme = req.theme.clone();
        let font = note_font(&theme, scale);
        let pip = Pip {
            hwnd,
            bar,
            src,
            app: req.app,
            session: req.media.as_ref().map(|m| m.session),
            media: req.media,
            gone_since: None,
            hidden_since: None,
            thumb,
            hooks,
            aspect,
            width: WIDTH,
            scale,
            rect: RECT { left: o.x, top: o.y, right: o.x + w, bottom: o.y + h },
            minimized: IsIconic(src).as_bool(),
            thumb_set: None,
            gfx: req.gfx,
            theme,
            font,
            font_scale: scale,
            rtl: crate::i18n::rtl(),
            drag: None,
            anim: None,
            hover: None,
            pressed: None,
            focus: None,
            keyboard: false,
            tracking: false,
            wheel_acc: 0,
        };
        let poll = pip.hooks.is_empty();
        PIP.with(|c| *c.borrow_mut() = Some(pip));
        SRC.store(src.0 as isize, Ordering::SeqCst);
        PIP_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        MOVED.store(false, Ordering::SeqCst);
        if poll {
            SetTimer(Some(hwnd), T_POLL, POLL_MS, None);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    sync();
    true
}

/// Closes the PiP (never from inside its borrow). Its controls go at
/// once; the picture fades out (crate::motion's close: click-through for
/// every app meanwhile), then the window goes.
pub fn close() {
    let p = PIP.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.take()));
    let Some(p) = p else { return };
    SRC.store(0, Ordering::SeqCst);
    PIP_HWND.store(0, Ordering::SeqCst);
    unsafe {
        for h in &p.hooks {
            let _ = UnhookWinEvent(*h);
        }
        let cap = GetCapture();
        if cap == p.hwnd || cap == p.bar.hwnd {
            let _ = ReleaseCapture();
        }
        let _ = KillTimer(Some(p.hwnd), T_ANIM);
        let _ = DestroyWindow(p.bar.hwnd);
    }
    let (hwnd, thumb) = (p.hwnd, p.thumb);
    // As every surface closes (motion::close: CLOSE_MS on the ease-in at
    // the chosen speed; 60 ms straight with reduced motion).
    let ms = close_fade();
    // A window drawn by GDI fades as a layered window; it keeps painting
    // from CLOSING meanwhile (pip_proc).
    if !(ms > 0 && unsafe { crate::motion::click_through(hwnd) }) {
        unsafe { gone(p) };
        return;
    }
    CLOSING.with(|c| c.borrow_mut().push(p));
    crate::motion::steps(
        ms,
        Box::new(move |l| unsafe {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (255.0 * l) as u8, LWA_ALPHA);
            let props = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_OPACITY, opacity: (255.0 * l) as u8, ..Default::default() };
            let _ = DwmUpdateThumbnailProperties(thumb, &props);
        }),
        Box::new(move || {
            let p = CLOSING.with(|c| {
                let mut c = c.borrow_mut();
                c.iter().position(|p| p.hwnd == hwnd).map(|k| c.remove(k))
            });
            if let Some(p) = p {
                unsafe { gone(p) };
            }
        }),
    );
}

/// How long closing fades (ms of real time): motion::close's plan.
fn close_fade() -> i64 {
    let mode = crate::motion::now();
    crate::motion::fade_plan(mode, crate::motion::close_ms(mode), crate::motion::EASE_IN, crate::effects::current().speed).0
}

/// A closed PiP's last things go: its thumbnail, its window, its font.
unsafe fn gone(p: Pip) {
    let _ = DwmUnregisterThumbnail(p.thumb);
    let _ = DestroyWindow(p.hwnd);
    if !p.font.is_invalid() {
        let _ = DeleteObject(p.font.into());
    }
}

/// The WinEvent hooks on the source's process (empty if none could be set).
unsafe fn hook(pid: u32) -> Vec<HWINEVENTHOOK> {
    if pid == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (a, b) in [
        (EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY),
        (EVENT_OBJECT_HIDE, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
    ] {
        let h = SetWinEventHook(a, b, None, Some(win_event), pid, 0, WINEVENT_OUTOFCONTEXT);
        if !h.0.is_null() {
            out.push(h);
        }
    }
    if out.len() < 4 {
        // Partly set: look at the source on a timer instead.
        for h in out.drain(..) {
            let _ = UnhookWinEvent(h);
        }
    }
    out
}

/// The source window changed: told to the PiP (posted, never handled in
/// here).
unsafe extern "system" fn win_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, obj: i32, child: i32, _: u32, _: u32) {
    if obj != OBJID_WINDOW.0 || child != 0 {
        return;
    }
    let src = SRC.load(Ordering::SeqCst);
    let pip = PIP_HWND.load(Ordering::SeqCst);
    if src == 0 || pip == 0 || hwnd.0 as isize != src {
        return;
    }
    // A drag of the source sends a stream of these: one at a time.
    if event == EVENT_OBJECT_LOCATIONCHANGE && MOVED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = PostMessageW(Some(HWND(pip as *mut _)), WM_PIP_SOURCE, WPARAM(event as usize), LPARAM(0));
}

/// The note's font at `scale` (GDI: it draws under the thumbnail).
fn note_font(theme: &Theme, scale: f32) -> HFONT {
    unsafe {
        CreateFontW(
            -((NOTE.size * scale).round() as i32),
            0,
            0,
            0,
            NOTE.weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            &HSTRING::from(theme.font.family.as_str()),
        )
    }
}

fn colorref(c: Rgba) -> COLORREF {
    COLORREF(c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16)
}

/// `fg` over `bg` by its alpha.
fn over(fg: Rgba, bg: Rgba) -> Rgba {
    let a = fg.a as u32;
    let mix = |f: u8, b: u8| ((f as u32 * a + b as u32 * (255 - a)) / 255) as u8;
    Rgba { r: mix(fg.r, bg.r), g: mix(fg.g, bg.g), b: mix(fg.b, bg.b), a: 255 }
}

fn d2d(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

impl Bar {
    unsafe fn new(gfx: &Gfx, owner: HWND) -> Result<Self> {
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("NopBarPipBar"),
            &HSTRING::from(t("pip.name")),
            WS_POPUP,
            0,
            0,
            1,
            1,
            // Owned: always above the picture.
            Some(owner),
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        let built = (|| -> Result<Self> {
            let enable = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &enable as *const _ as *const _, 4);
            let c = &gfx.compositor;
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let outer = c.CreateContainerVisual()?;
            target.SetRoot(&outer)?;
            // Faded in when first shown.
            outer.SetOpacity(0.0)?;
            let content = c.CreateSpriteVisual()?;
            let under = c.CreateContainerVisual()?;
            let wash = Hover::new(c, &under, true)?;
            Ok(Bar { hwnd, _target: target, outer, content, under, wash, built: None, shown: false, lit: false, tracking: false })
        })();
        if built.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        built
    }
}

impl Pip {
    fn buttons(&self) -> Vec<Btn> {
        buttons(can_toggle(self.session, self.media.as_ref()))
    }

    fn playing(&self) -> bool {
        self.media.as_ref().is_some_and(|m| Some(m.session) == self.session && m.playing)
    }

    fn bar_rect(&self) -> RECT {
        bar_rect(self.rect, self.scale, self.buttons().len())
    }

    /// Hover, press and focus on buttons no longer there go.
    fn forget_gone(&mut self) {
        let order = self.buttons();
        for b in [&mut self.hover, &mut self.pressed, &mut self.focus] {
            if b.is_some_and(|x| !order.contains(&x)) {
                *b = None;
            }
        }
    }

    /// The thumbnail fills the window at the source's aspect (hidden while
    /// the source is minimized: the note shows).
    fn fit_thumb(&mut self) {
        let dest = fit_rect(rw(&self.rect), rh(&self.rect), self.aspect);
        let visible = !self.minimized;
        if self.thumb_set == Some((dest, visible)) {
            return;
        }
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_SOURCECLIENTAREAONLY | DWM_TNP_OPACITY,
            rcDestination: dest,
            rcSource: RECT::default(),
            opacity: 255,
            fVisible: BOOL(visible as i32),
            fSourceClientAreaOnly: BOOL(1),
        };
        if unsafe { DwmUpdateThumbnailProperties(self.thumb, &props) }.is_ok() {
            self.thumb_set = Some((dest, visible));
        }
    }

    /// The window's size for its width now, its corner `c` kept, inside
    /// its work area.
    fn resize_at(&mut self, c: Corner) {
        let (work, _) = work_of(self.rect);
        let (w, h) = size_px(self.width, self.aspect, self.scale, work);
        self.rect = clamp_into(anchored(c, self.rect, w, h), work);
    }

    /// Where the nearest corner is (and its size there).
    fn snap_target(&self) -> RECT {
        let (work, mscale) = work_of(self.rect);
        let (w, h) = size_px(self.width, self.aspect, self.scale, work);
        let o = corner_origin(nearest_corner(self.rect, work), work, w, h, (MARGIN * mscale).round() as i32);
        RECT { left: o.x, top: o.y, right: o.x + w, bottom: o.y + h }
    }

    /// The window as dragged to `cur` (screen px).
    fn dragged(&mut self, cur: POINT) -> Option<RECT> {
        let d = self.drag.as_ref()?;
        let (dx, dy) = (cur.x - d.at.x, cur.y - d.at.y);
        let from = d.from;
        Some(match d.grab {
            Grab::Move => RECT { left: from.left + dx, top: from.top + dy, right: from.right + dx, bottom: from.bottom + dy },
            Grab::Resize(c) => {
                let w = if c.left() { rw(&from) + dx } else { rw(&from) - dx };
                self.width = clamp_width(w as f32 / self.scale);
                let (work, _) = work_of(from);
                let (w, h) = size_px(self.width, self.aspect, self.scale, work);
                clamp_into(anchored(c, from, w, h), work)
            }
        })
    }

    /// The source changed: gone (destroyed, or hidden for HIDDEN_MS),
    /// minimized or restored (`min`: as its event says; else asked),
    /// resized. False: it is gone. The hooks' events and the polling both
    /// come here.
    fn check_source(&mut self, min: Option<bool>) -> bool {
        unsafe {
            let exists = IsWindow(Some(self.src)).as_bool();
            let visible = exists && IsWindowVisible(self.src).as_bool();
            let (live, since) = alive(exists, visible, self.hidden_since, Instant::now(), Duration::from_millis(HIDDEN_MS as u64));
            let newly_hidden = since.is_some() && self.hidden_since.is_none();
            self.hidden_since = since;
            if !live {
                return false;
            }
            if newly_hidden && !self.hooks.is_empty() {
                // Looked at again when the grace is over (polling looks
                // anyway).
                SetTimer(Some(self.hwnd), T_HIDDEN, HIDDEN_MS + 50, None);
            }
            let min = min.unwrap_or_else(|| IsIconic(self.src).as_bool());
            if min != self.minimized {
                self.minimized = min;
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
            if let Some(a) = client_aspect(self.src) {
                if (a - self.aspect).abs() / self.aspect > 0.01 {
                    self.aspect = a;
                    if self.drag.is_none() {
                        let (work, _) = work_of(self.rect);
                        self.resize_at(nearest_corner(self.rect, work));
                    }
                }
            }
        }
        true
    }

    // ---- the controls

    /// The chrome for the buttons now at the scale now (made again when
    /// either changed), then the buttons drawn.
    fn build_bar(&mut self) {
        let n = self.buttons().len();
        let key = (n, self.scale.to_bits());
        if self.bar.built != Some(key) {
            let s = self.scale;
            let (w, h) = bar_size(n);
            let r = (|| -> Result<()> {
                for v in [self.bar.content.cast::<windows::UI::Composition::Visual>()?, self.bar.under.cast()?] {
                    if let Ok(p) = v.Parent() {
                        let _ = p.Children().and_then(|c| c.Remove(&v));
                    }
                }
                self.bar.outer.Children()?.RemoveAll()?;
                self.bar.outer.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
                let (root, border) = crate::popup::chrome(&self.gfx, &self.theme, &self.bar.outer, w, h, s, self.theme.radii().panel)?;
                self.bar.content.SetSize(Vector2 { X: w, Y: h })?;
                root.Children()?.InsertBelow(&self.bar.content, &border)?;
                root.Children()?.InsertBelow(&self.bar.under, &self.bar.content)?;
                Ok(())
            })();
            match r {
                Ok(()) => self.bar.built = Some(key),
                Err(e) => crate::log!("pip: controls' chrome failed: {e}"),
            }
        }
        self.draw_bar();
    }

    /// What the controls show now.
    fn bar_look(&self) -> BarLook {
        BarLook { hover: self.hover, pressed: self.pressed, focus: self.focus.filter(|_| self.keyboard), playing: self.playing() }
    }

    /// The controls' picture, and the pointer's fill under it.
    fn draw_bar(&mut self) {
        let s = self.scale;
        let order = self.buttons();
        let (w, h) = bar_size(order.len());
        let look = self.bar_look();
        let (theme, rtl) = (&self.theme, self.rtl);
        let surface = self.gfx.draw((w * s).ceil() as u32, (h * s).ceil() as u32, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
            let r = paint_bar(dc, theme, &order, look, rtl);
            dc.SetTransform(&Matrix3x2::identity());
            r
        });
        match surface {
            Ok(surface) => {
                if let Ok(brush) = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) {
                    let _ = brush.SetStretch(CompositionStretch::Fill);
                    let _ = self.bar.content.SetBrush(&brush);
                }
            }
            Err(e) => crate::log!("pip: controls' draw failed: {e}"),
        }
        // (The fill's layer is in DIP: the bar's outer visual is scaled.)
        let want = look.lit().and_then(|(t, down)| btn_shape(&order, t, rtl).map(|shape| Lit { t, down, shape }));
        let c = self.gfx.compositor.clone();
        self.bar.wash.show(&c, &self.theme, want);
    }

    /// The button under (x, y) of the controls' window (client px).
    fn bar_hit(&self, x: i32, y: i32) -> Option<Btn> {
        let (x, y) = (x as f32 / self.scale, y as f32 / self.scale);
        let order = self.buttons();
        let n = order.len();
        order.into_iter().enumerate().find(|(i, _)| {
            let r = btn_rect(*i, n, self.rtl);
            x >= r.left && x < r.right && y >= r.top && y < r.bottom
        })
        .map(|(_, b)| b)
    }

    // ---- the picture's own content

    fn backdrop(&self) -> Rgba {
        match self.theme.hc {
            Some(h) => h.window,
            // The menus' surface, opaque (no tinted greys of its own).
            None => Rgba { a: 255, ..self.theme.popup_tint() },
        }
    }

    unsafe fn paint(&mut self) {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(self.hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut client);
        let bg = self.backdrop();
        let brush = CreateSolidBrush(colorref(bg));
        FillRect(dc, &client, brush);
        let _ = DeleteObject(brush.into());
        if self.minimized {
            if self.font_scale != self.scale || self.font.is_invalid() {
                if !self.font.is_invalid() {
                    let _ = DeleteObject(self.font.into());
                }
                self.font = note_font(&self.theme, self.scale);
                self.font_scale = self.scale;
            }
            // (GDI draws it opaque: the level laid over the backdrop.)
            let ink = over(self.theme.text_of(Level::Secondary, State::Rest), bg);
            let old = SelectObject(dc, self.font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, colorref(ink));
            let mut text: Vec<u16> = t("pip.minimized").encode_utf16().collect();
            let mut r = client;
            DrawTextW(dc, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
            SelectObject(dc, old);
        }
        let _ = EndPaint(self.hwnd, &ps);
    }

    // ---- screen readers

    /// The picture's tree (its buttons, even while the controls are
    /// hidden) and the controls' own (what is under the pointer there).
    fn publish(&self) {
        if a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.tree());
        }
        if a11y::wanted(self.bar.hwnd) {
            a11y::publish(self.bar.hwnd, self.bar_tree());
        }
    }

    fn bar_tree(&self) -> Tree {
        let mut tree = self.tree();
        tree.nodes.retain(|n| n.role == Role::Button);
        tree.role = Role::ToolBar;
        // The keyboard is the picture's (one focused element, there).
        tree.focus = None;
        tree.active = false;
        tree
    }

    fn tree(&self) -> Tree {
        let order = self.buttons();
        let n = order.len();
        let br = self.bar_rect();
        let mut nodes = Vec::new();
        if self.minimized {
            nodes.push(Node::new("note", Role::Text, t("pip.minimized"), self.rect));
        }
        for (i, b) in order.into_iter().enumerate() {
            let r = btn_rect(i, n, self.rtl);
            let name = match b {
                Btn::Toggle if self.playing() => t("island.pause"),
                Btn::Toggle => t("island.play"),
                Btn::Go => t("pip.go"),
                Btn::Close => t("pip.close"),
            };
            nodes.push(Node::new(b.key(), Role::Button, name, a11y::px_rect((br.left, br.top), self.scale, r.left, r.top, r.right - r.left, r.bottom - r.top)).invoke());
        }
        let active = unsafe { GetFocus() } == self.hwnd;
        Tree { role: Role::Group, name: t("pip.name").into(), nodes, focus: self.focus.filter(|_| active).map(|b| b.key().to_string()), focusable: true, active, focus_in: None }
    }
}

/// Puts the windows where the state says. Moving a window sends it
/// messages (WM_DPICHANGED among them), so no borrow is held while it
/// moves.
fn sync() {
    let Some((hwnd, r)) = with_pip(|p| (p.hwnd, p.rect)) else { return };
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), r.left, r.top, rw(&r), rh(&r), SWP_NOACTIVATE);
    }
    let bar = with_pip(|p| {
        p.fit_thumb();
        if p.bar.shown {
            p.build_bar();
        }
        p.publish();
        // The controls fade in (FADE_IN_MS) and out (FADE_OUT_MS): their
        // window goes when the fade it really got is over (T_BAR_GONE).
        let fade = (p.bar.shown != p.bar.lit).then(|| {
            p.bar.lit = p.bar.shown;
            let (ms, curve) = bar_fade(p.bar.shown);
            let c = p.gfx.compositor.clone();
            crate::motion::fade_to(&c, &p.bar.outer, if p.bar.shown { 1.0 } else { 0.0 }, ms, curve).unwrap_or(0)
        });
        (p.bar.hwnd, p.bar_rect(), p.bar.shown, fade)
    });
    let Some((bh, br, shown, fade)) = bar else { return };
    unsafe {
        let show = if shown { SWP_SHOWWINDOW } else { SWP_NOREDRAW };
        let _ = SetWindowPos(bh, None, br.left, br.top, rw(&br), rh(&br), SWP_NOACTIVATE | SWP_NOZORDER | show);
        if shown {
            let _ = KillTimer(Some(bh), T_BAR_GONE);
        } else if let Some(ms) = fade {
            SetTimer(Some(bh), T_BAR_GONE, ms.max(1) as u32, None);
        }
    }
}

/// Pure: the controls' fade (ms at speed 1, curve): in over FADE_IN_MS on
/// the ease-out, out over FADE_OUT_MS on the ease-in (motion::fade_to
/// plays it: 60 ms straight with reduced motion).
fn bar_fade(on: bool) -> (i64, crate::motion::Curve) {
    if on {
        (crate::motion::FADE_IN_MS, crate::motion::EASE_OUT)
    } else {
        (crate::motion::FADE_OUT_MS, crate::motion::EASE_IN)
    }
}

/// Glides (or jumps, with animations off or `animate` false) to the
/// nearest corner.
fn snap(animate: bool) {
    let glide = with_pip(|p| {
        let to = p.snap_target();
        let from = POINT { x: p.rect.left, y: p.rect.top };
        // Its size there at once; only its place glides.
        p.rect = RECT { left: from.x, top: from.y, right: from.x + rw(&to), bottom: from.y + rh(&to) };
        let far = (to.left - from.x).abs() + (to.top - from.y).abs() > 1;
        if animate && far && crate::motion::full() {
            p.anim = Some(Anim { from, to: POINT { x: to.left, y: to.top }, start: Instant::now(), ms: crate::effects::ms(SNAP_MS) });
            Some(p.hwnd)
        } else {
            p.anim = None;
            p.rect = to;
            None
        }
    });
    if let Some(Some(h)) = glide {
        unsafe {
            SetTimer(Some(h), T_ANIM, FRAME_MS, None);
        }
    }
    sync();
}

/// A glide's frame.
fn glide() {
    let done = with_pip(|p| {
        let Some(a) = &p.anim else { return true };
        let k = ease_out(a.start.elapsed().as_millis() as f32 / a.ms.max(1) as f32);
        let x = a.from.x + ((a.to.x - a.from.x) as f32 * k).round() as i32;
        let y = a.from.y + ((a.to.y - a.from.y) as f32 * k).round() as i32;
        let (w, h) = (rw(&p.rect), rh(&p.rect));
        p.rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
        if k >= 1.0 {
            p.anim = None;
        }
        p.anim.is_none()
    });
    if done != Some(false) {
        if let Some(h) = with_pip(|p| p.hwnd) {
            unsafe {
                let _ = KillTimer(Some(h), T_ANIM);
            }
        }
    }
    sync();
}

/// A drag starts from `from` (the picture or its controls) at `cur`.
fn start_drag(from: HWND, cur: POINT, grab: Grab) {
    let ok = with_pip(|p| {
        p.anim = None;
        p.drag = Some(Drag { grab, at: cur, from: p.rect });
        let _ = unsafe { KillTimer(Some(p.hwnd), T_ANIM) };
    });
    if ok.is_some() {
        unsafe {
            SetCapture(from);
        }
    }
}

/// The drag follows the cursor.
fn drag_to(cur: POINT) -> bool {
    let moved = with_pip(|p| {
        let r = p.dragged(cur)?;
        p.rect = r;
        Some(())
    });
    if moved.flatten().is_some() {
        sync();
        return true;
    }
    false
}

/// The drag ends: to the nearest corner.
fn end_drag() -> bool {
    let was = with_pip(|p| p.drag.take().is_some()).unwrap_or(false);
    if was {
        snap(true);
    }
    was
}

/// The controls show (or go) on hover.
fn show_bar(on: bool) {
    let changed = with_pip(|p| {
        if p.bar.shown == on {
            return false;
        }
        p.bar.shown = on;
        if !on {
            p.hover = None;
            p.pressed = None;
        }
        true
    });
    if changed == Some(true) {
        sync();
    }
}

/// The pointer left one of the two windows: the controls go soon unless
/// it is over the picture again (or a drag or the keyboard holds them).
fn left_soon() {
    if let Some(h) = with_pip(|p| p.hwnd) {
        unsafe {
            SetTimer(Some(h), T_HIDE, HIDE_MS, None);
        }
    }
}

fn hide_if_away() {
    let keep = with_pip(|p| {
        let mut pt = POINT::default();
        let inside = unsafe { GetCursorPos(&mut pt) }.is_ok() && pt.x >= p.rect.left && pt.x < p.rect.right && pt.y >= p.rect.top && pt.y < p.rect.bottom;
        let focused = p.keyboard && unsafe { GetFocus() } == p.hwnd;
        inside || p.drag.is_some() || p.pressed.is_some() || focused
    });
    if keep == Some(false) {
        show_bar(false);
    }
}

/// Runs a button (no borrow held: it may close the PiP).
fn act(b: Btn) {
    match b {
        Btn::Toggle => {
            if let Some(Some(s)) = with_pip(|p| p.session) {
                crate::topbar::island::media_toggle(s);
            }
        }
        Btn::Go => {
            let src = with_pip(|p| p.src);
            close();
            if let Some(s) = src {
                crate::dock::bring_to_front(s);
            }
        }
        Btn::Close => close(),
    }
}

/// The wheel resizes it about its nearest corner.
fn wheel(delta: i32) {
    let changed = with_pip(|p| {
        if (p.wheel_acc > 0) != (delta > 0) {
            p.wheel_acc = 0;
        }
        p.wheel_acc += delta;
        let notches = p.wheel_acc / WHEEL_NOTCH;
        p.wheel_acc -= notches * WHEEL_NOTCH;
        if notches == 0 || p.drag.is_some() {
            return false;
        }
        let w = clamp_width(p.width * WHEEL_STEP.powi(notches));
        if w == p.width {
            return false;
        }
        p.width = w;
        p.anim = None;
        let (work, _) = work_of(p.rect);
        p.resize_at(nearest_corner(p.rect, work));
        true
    });
    if changed == Some(true) {
        sync();
    }
}

/// A key on the picture (it has the keyboard only when a screen reader
/// gave it): Tab / arrows between the buttons, Enter / Space, Esc closes.
fn key(vk: u16) {
    if vk == VK_ESCAPE.0 {
        close();
        return;
    }
    let todo = with_pip(|p| {
        let order = p.buttons();
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let dir = match vk {
            v if v == VK_TAB.0 => Some(if shift { -1 } else { 1 }),
            v if v == VK_DOWN.0 => Some(1),
            v if v == VK_UP.0 => Some(-1),
            v if v == VK_RIGHT.0 => Some(crate::i18n::mirror::arrow(p.rtl, true)),
            v if v == VK_LEFT.0 => Some(crate::i18n::mirror::arrow(p.rtl, false)),
            _ => None,
        };
        if let Some(d) = dir {
            // The ring shows once the keyboard moves the focus (not with
            // Ctrl, Alt or the Windows key: crate::input).
            p.keyboard |= crate::input::navigates(vk);
            p.focus = step(&order, p.focus, d);
            p.bar.shown = true;
            return (None, true);
        }
        if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
            return (p.focus.filter(|f| order.contains(f)), false);
        }
        (None, false)
    });
    match todo {
        Some((Some(b), _)) => act(b),
        Some((None, true)) => sync(),
        _ => {}
    }
}

/// What a screen reader asked of either window's buttons: press one, or
/// move the keyboard there (the picture takes it: Esc and the keys work).
fn a11y_actions(hwnd: HWND) {
    for (k, a) in a11y::take(hwnd) {
        // (invoke this, take the keyboard)
        let todo = with_pip(|p| {
            let b = p.buttons().into_iter().find(|b| b.key() == k);
            match a {
                Act::Invoke => (b, None),
                Act::Focus => {
                    // The focus moves; the ring as it was, or with Windows'
                    // keyboard cues always on (crate::input).
                    p.focus = b;
                    p.keyboard = crate::input::reader_focus(p.keyboard);
                    p.bar.shown = true;
                    (None, Some(p.hwnd))
                }
                _ => {
                    p.publish();
                    (None, None)
                }
            }
        });
        match todo {
            Some((Some(b), _)) => act(b),
            Some((None, Some(pip))) => unsafe {
                let _ = SetForegroundWindow(pip);
                let _ = SetFocus(Some(pip));
                sync();
            },
            _ => {}
        }
    }
}

fn client_pt(lp: LPARAM) -> POINT {
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
}

fn cursor() -> POINT {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    pt
}

fn track_leave(hwnd: HWND) {
    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
    unsafe {
        let _ = TrackMouseEvent(&mut tme);
    }
}

unsafe extern "system" fn pip_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Fading out after it closed: it still paints (its picture fades as it
    // is), clicks go through, nothing else reaches it.
    let closing = CLOSING.with(|c| c.try_borrow().map(|c| c.iter().any(|p| p.hwnd == hwnd)).unwrap_or(false));
    if closing {
        return match msg {
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_PAINT => {
                let painted = CLOSING.with(|c| c.try_borrow_mut().ok().and_then(|mut c| c.iter_mut().find(|p| p.hwnd == hwnd).map(|p| p.paint())));
                if painted.is_none() {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(hwnd, &mut ps);
                    let _ = EndPaint(hwnd, &ps);
                }
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        };
    }
    match msg {
        WM_PAINT => {
            if with_pip(|p| p.paint()).is_none() {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let mut pt = cursor();
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            let grip = with_pip(|p| grip_anchor(pt.x, pt.y, rw(&p.rect), rh(&p.rect), (GRIP * p.scale).round() as i32)).flatten();
            let id = match grip {
                Some(Corner::TopRight) => IDC_SIZENESW,
                Some(_) => IDC_SIZENWSE,
                None => IDC_ARROW,
            };
            SetCursor(LoadCursorW(None, id).ok());
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            if drag_to(cursor()) {
                return LRESULT(0);
            }
            let start = with_pip(|p| {
                if !p.tracking {
                    track_leave(hwnd);
                    p.tracking = true;
                }
                !p.bar.shown
            });
            if start == Some(true) {
                show_bar(true);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE_MSG => {
            with_pip(|p| p.tracking = false);
            left_soon();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let pt = client_pt(lp);
            let grab = with_pip(|p| match grip_anchor(pt.x, pt.y, rw(&p.rect), rh(&p.rect), (GRIP * p.scale).round() as i32) {
                Some(c) => Grab::Resize(c),
                None => Grab::Move,
            });
            if let Some(g) = grab {
                start_drag(hwnd, cursor(), g);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if end_drag() && GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // The mouse was taken away mid-drag: it ends where it is.
            if HWND(lp.0 as *mut _) != hwnd {
                end_drag();
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            wheel(((wp.0 >> 16) & 0xFFFF) as i16 as i32);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            key(wp.0 as u16);
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            with_pip(|p| {
                p.keyboard = false;
                p.focus = None;
                if p.bar.shown {
                    p.draw_bar();
                }
                p.publish();
            });
            left_soon();
            LRESULT(0)
        }
        WM_TIMER => {
            match wp.0 {
                T_HIDE => {
                    let _ = KillTimer(Some(hwnd), T_HIDE);
                    hide_if_away();
                }
                T_ANIM => glide(),
                T_GONE => gone_timer(hwnd),
                T_HIDDEN | T_POLL => {
                    if wp.0 == T_HIDDEN {
                        let _ = KillTimer(Some(hwnd), T_HIDDEN);
                    }
                    if with_pip(|p| p.check_source(None)) == Some(false) {
                        close();
                    } else {
                        sync();
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_PIP_SOURCE => {
            let ev = wp.0 as u32;
            if ev == EVENT_OBJECT_LOCATIONCHANGE {
                MOVED.store(false, Ordering::SeqCst);
            }
            match ev {
                EVENT_OBJECT_DESTROY => close(),
                _ => {
                    // (Minimizing starts before the window says it is.)
                    let min = match ev {
                        EVENT_SYSTEM_MINIMIZESTART => Some(true),
                        EVENT_SYSTEM_MINIMIZEEND => Some(false),
                        _ => None,
                    };
                    if with_pip(|p| p.check_source(min)) == Some(false) {
                        close();
                    } else {
                        sync();
                    }
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // Across displays: the same DIP size at the new scale, where
            // Windows suggests.
            let dpi = (wp.0 & 0xFFFF) as u32;
            let suggested = *(lp.0 as *const RECT);
            let cur = cursor();
            with_pip(|p| {
                p.scale = dpi.max(96) as f32 / 96.0;
                let (work, _) = work_of(suggested);
                let (w, h) = size_px(p.width, p.aspect, p.scale, work);
                p.rect = RECT { left: suggested.left, top: suggested.top, right: suggested.left + w, bottom: suggested.top + h };
                if let Some(d) = p.drag.as_mut() {
                    d.at = cur;
                    d.from = p.rect;
                }
                p.bar.built = None;
                let _ = InvalidateRect(Some(hwnd), None, false);
            });
            // Moved later, not inside the move that brought this (a window
            // on a display boundary could bounce between two scales).
            let _ = PostMessageW(Some(hwnd), WM_PIP_SYNC, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        WM_PIP_SYNC => {
            sync();
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            snap(false);
            LRESULT(0)
        }
        WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETWORKAREA.0 => {
            snap(false);
            LRESULT(0)
        }
        WM_GETOBJECT => {
            let answer = a11y::on_getobject(hwnd, wp, lp, || {
                with_pip(|p| p.publish());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == a11y::wm_action() => {
            a11y_actions(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        WM_DESTROY => {
            a11y::forget(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            if drag_to(cursor()) {
                return LRESULT(0);
            }
            let pt = client_pt(lp);
            let away = with_pip(|p| {
                if !p.bar.tracking {
                    track_leave(hwnd);
                    p.bar.tracking = true;
                }
                let h = p.bar_hit(pt.x, pt.y);
                if h != p.hover {
                    p.hover = h;
                    p.keyboard = false;
                    p.draw_bar();
                }
                !p.bar.shown
            });
            // Back on them while they fade out: they come back.
            if away == Some(true) {
                show_bar(true);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_BAR_GONE => {
            let _ = KillTimer(Some(hwnd), T_BAR_GONE);
            if with_pip(|p| p.bar.shown) != Some(true) {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE_MSG => {
            with_pip(|p| {
                p.bar.tracking = false;
                if p.hover.take().is_some() {
                    p.draw_bar();
                }
            });
            left_soon();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let pt = client_pt(lp);
            let hit = with_pip(|p| {
                let h = p.bar_hit(pt.x, pt.y);
                p.pressed = h;
                p.keyboard = false;
                if h.is_some() {
                    p.draw_bar();
                }
                h
            });
            match hit {
                // Between the buttons: the picture is dragged.
                Some(None) => start_drag(hwnd, cursor(), Grab::Move),
                Some(Some(_)) => {
                    SetCapture(hwnd);
                }
                None => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if end_drag() {
                if GetCapture() == hwnd {
                    let _ = ReleaseCapture();
                }
                return LRESULT(0);
            }
            let pt = client_pt(lp);
            let todo = with_pip(|p| {
                let pressed = p.pressed.take();
                p.draw_bar();
                pressed.filter(|b| p.bar_hit(pt.x, pt.y) == Some(*b))
            })
            .flatten();
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
            if let Some(b) = todo {
                act(b);
            } else {
                hide_if_away();
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if HWND(lp.0 as *mut _) != hwnd {
                end_drag();
                with_pip(|p| {
                    if p.pressed.take().is_some() {
                        p.draw_bar();
                    }
                });
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            wheel(((wp.0 >> 16) & 0xFFFF) as i16 as i32);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            key(wp.0 as u16);
            LRESULT(0)
        }
        // Its place and size follow the picture's scale (sync).
        WM_DPICHANGED => LRESULT(0),
        WM_GETOBJECT => {
            let answer = a11y::on_getobject(hwnd, wp, lp, || {
                with_pip(|p| p.publish());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == a11y::wm_action() => {
            a11y_actions(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            a11y::forget(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    /// The ring on the controls (crate::input): only the keys that move
    /// the focus show it, a press of the pointer hides it, a screen
    /// reader's focus by the rule.
    #[test]
    fn the_ring_follows_the_keys_that_move_the_focus() {
        let src = include_str!("pip.rs").replace("\r\n", "\n");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(code.contains("p.keyboard |= crate::input::navigates(vk);"));
        assert!(code.contains("p.keyboard = crate::input::reader_focus(p.keyboard);"));
        assert_eq!(code.matches("p.keyboard = true;").count(), 0, "nothing else shows it");
        assert!(code.contains("p.pressed = h;\n                p.keyboard = false;"), "a press hides it");
    }

    use super::*;

    const WORK: RECT = RECT { left: 0, top: 32, right: 1920, bottom: 1040 };

    #[test]
    fn the_picture_keeps_the_sources_aspect() {
        // 4:3 at 360 DIP, 1x and 1.5x.
        assert_eq!(size_px(360.0, 4.0 / 3.0, 1.0, WORK), (360, 270));
        assert_eq!(size_px(360.0, 4.0 / 3.0, 1.5, WORK), (540, 405));
        let (w, h) = size_px(360.0, 16.0 / 9.0, 1.0, WORK);
        assert!(w == 360 && (202..=203).contains(&h));
        // A tall source on a short screen: made smaller, still its shape.
        let short = RECT { left: 0, top: 0, right: 1920, bottom: 400 };
        let (w, h) = size_px(720.0, 9.0 / 16.0, 1.0, short);
        assert_eq!(h, 400 - (2.0 * MARGIN) as i32, "EDGE_GAP from each edge");
        assert!(((w as f32 / h as f32) - 9.0 / 16.0).abs() < 0.01);
        // Nonsense aspect: the default.
        assert_eq!(size_px(360.0, 0.0, 1.0, WORK), size_px(360.0, DEFAULT_ASPECT, 1.0, WORK));
        assert_eq!(size_px(360.0, f32::NAN, 1.0, WORK), size_px(360.0, DEFAULT_ASPECT, 1.0, WORK));
        // The thumbnail centered in a box of another shape.
        assert_eq!(fit_rect(400, 300, 4.0 / 3.0), RECT { left: 0, top: 0, right: 400, bottom: 300 });
        assert_eq!(fit_rect(400, 400, 2.0), RECT { left: 0, top: 100, right: 400, bottom: 300 }, "letterboxed");
        assert_eq!(fit_rect(400, 100, 2.0), RECT { left: 100, top: 0, right: 300, bottom: 100 }, "pillarboxed");
        assert_eq!(fit_rect(0, 0, 2.0), RECT::default());
    }

    #[test]
    fn its_width_stays_in_bounds() {
        assert_eq!(clamp_width(100.0), MIN_W);
        assert_eq!(clamp_width(5000.0), MAX_W);
        assert_eq!(clamp_width(360.0), 360.0);
        assert_eq!(clamp_width(f32::NAN), WIDTH);
        assert_eq!(size_px(10.0, 1.0, 1.0, WORK), (200, 200));
        assert_eq!(size_px(9999.0, 2.0, 1.0, WORK), (720, 360));
    }

    #[test]
    fn it_snaps_to_the_nearest_corner() {
        let at = |x: i32, y: i32| RECT { left: x, top: y, right: x + 360, bottom: y + 203 };
        assert_eq!(nearest_corner(at(10, 40), WORK), Corner::TopLeft);
        assert_eq!(nearest_corner(at(1500, 40), WORK), Corner::TopRight);
        assert_eq!(nearest_corner(at(10, 800), WORK), Corner::BottomLeft);
        assert_eq!(nearest_corner(at(1500, 800), WORK), Corner::BottomRight);
        // By its center, not its corner: mostly right of the middle is right.
        assert_eq!(nearest_corner(at(900, 800), WORK), Corner::BottomRight);
        assert_eq!(nearest_corner(at(700, 800), WORK), Corner::BottomLeft);
        // The places, MARGIN in.
        assert_eq!(corner_origin(Corner::BottomRight, WORK, 360, 203, 16), POINT { x: 1920 - 16 - 360, y: 1040 - 16 - 203 });
        assert_eq!(corner_origin(Corner::TopLeft, WORK, 360, 203, 16), POINT { x: 16, y: 48 });
        // Resized about a corner, then kept on the work area.
        let r = at(1544, 821);
        assert_eq!(anchored(Corner::BottomRight, r, 200, 113), RECT { left: 1704, top: 911, right: 1904, bottom: 1024 });
        assert_eq!(anchored(Corner::TopLeft, r, 200, 113), RECT { left: 1544, top: 821, right: 1744, bottom: 934 });
        assert_eq!(clamp_into(anchored(Corner::TopLeft, r, 720, 405), WORK), RECT { left: 1200, top: 635, right: 1920, bottom: 1040 });
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5);
    }

    #[test]
    fn its_bottom_corners_resize_it() {
        // 200 x 100, an 18 px grip: the corner that stays put.
        assert_eq!(grip_anchor(5, 95, 200, 100, 18), Some(Corner::TopRight));
        assert_eq!(grip_anchor(195, 95, 200, 100, 18), Some(Corner::TopLeft));
        assert_eq!(grip_anchor(100, 95, 200, 100, 18), None);
        assert_eq!(grip_anchor(5, 10, 200, 100, 18), None, "the top corners move it");
        assert_eq!(grip_anchor(195, 100, 200, 100, 18), None, "outside");
    }

    #[test]
    fn the_controls_sit_over_the_picture() {
        assert_eq!(buttons(true), [Btn::Toggle, Btn::Go, Btn::Close]);
        assert_eq!(buttons(false), [Btn::Go, Btn::Close]);
        let pip = RECT { left: 1000, top: 500, right: 1360, bottom: 703 };
        let b = bar_rect(pip, 1.0, 3);
        assert_eq!((b.left + b.right) / 2, 1180);
        assert_eq!(b.bottom, 703 - BAR_LIFT as i32);
        assert!(b.top > pip.top && rw(&b) < rw(&pip));
        // Even at the least width the controls miss the resize grips.
        let small = RECT { left: 0, top: 0, right: MIN_W as i32, bottom: 113 };
        let b = bar_rect(small, 1.0, 3);
        assert!(b.left >= GRIP as i32 && b.right <= small.right - GRIP as i32);
        // Mirrored right to left.
        let (l, r) = (btn_rect(0, 3, false), btn_rect(0, 3, true));
        assert_eq!(l.left, BAR_PAD);
        assert_eq!(r.right, bar_size(3).0 - BAR_PAD);
        // The keyboard's order wraps.
        let order = buttons(true);
        assert_eq!(step(&order, None, 1), Some(Btn::Toggle));
        assert_eq!(step(&order, None, -1), Some(Btn::Close));
        assert_eq!(step(&order, Some(Btn::Close), 1), Some(Btn::Toggle));
        assert_eq!(step(&buttons(false), Some(Btn::Toggle), 1), Some(Btn::Go), "gone: from the start");
    }

    #[test]
    fn play_pause_stays_with_the_session_it_opened_for() {
        let m = |session: u32, app: &str| Media { app: app.into(), title: "t".into(), session, playing: true, can_toggle: true };
        let never = |_: &Media| -> bool { panic!("no window lookup for this") };
        // Its own session: kept, and play / pause shows.
        assert_eq!(tie(Some(3), "Chrome", Some(&m(3, "Chrome")), never), Some(3));
        assert!(can_toggle(Some(3), Some(&m(3, "Chrome"))));
        // Chrome window B plays now (its track is not in the PiP's window):
        // still tied to A, and the button goes (B is never paused by it).
        assert_eq!(tie(Some(3), "Chrome", Some(&m(4, "Chrome")), |_| false), Some(3));
        assert!(!can_toggle(Some(3), Some(&m(4, "Chrome"))));
        // Back in the PiP's window (a new session count): tied again.
        assert_eq!(tie(Some(3), "Chrome", Some(&m(5, "Chrome")), |_| true), Some(5));
        // Another app: no lookup, the tie stays; nothing plays: stays too.
        assert_eq!(tie(Some(3), "Chrome", Some(&m(6, "Spotify.exe")), never), Some(3));
        assert_eq!(tie(Some(3), "Chrome", None, never), Some(3));
        assert!(!can_toggle(Some(3), None));
        // Its own session but the app will not pause: no button.
        assert!(!can_toggle(Some(3), Some(&Media { can_toggle: false, ..m(3, "Chrome") })));
        assert!(!can_toggle(None, Some(&m(3, "Chrome"))));
    }

    #[test]
    fn it_goes_when_nothing_plays_for_a_while() {
        let t0 = Instant::now();
        let grace = Duration::from_millis(GONE_MS as u64);
        // Something plays again: a timer already queued closes nothing.
        assert_eq!(gone_check(None, t0 + grace * 2, grace), Gone::Stay);
        // The deadline counts from the first None, not from the timer.
        assert_eq!(gone_check(Some(t0), t0 + grace, grace), Gone::Close);
        assert_eq!(gone_check(Some(t0), t0 + Duration::from_millis(1000), grace), Gone::Wait(Duration::from_millis(GONE_MS as u64 - 1000)));
        // A timer of an earlier absence finds a newer one not due yet.
        assert_eq!(gone_check(Some(t0 + grace), t0 + grace, grace), Gone::Wait(grace));
    }

    #[test]
    fn a_hidden_source_gets_a_moment_then_it_goes() {
        let t0 = Instant::now();
        let grace = Duration::from_millis(HIDDEN_MS as u64);
        assert_eq!(alive(false, false, None, t0, grace), (false, None), "destroyed");
        assert_eq!(alive(true, true, Some(t0), t0 + grace, grace), (true, None), "shown again");
        assert_eq!(alive(true, false, None, t0, grace), (true, Some(t0)), "hidden: the grace starts");
        assert_eq!(alive(true, false, Some(t0), t0 + Duration::from_millis(500), grace), (true, Some(t0)), "kept, from the first look");
        assert_eq!(alive(true, false, Some(t0), t0 + grace, grace), (false, Some(t0)), "hidden too long");
    }

    #[test]
    fn the_playing_apps_window_is_picked() {
        let chrome = Some(r"c:\program files\google\chrome\application\chrome.exe".to_string());
        let edge = Some(r"c:\program files (x86)\microsoft\edge\application\msedge.exe".to_string());
        let win = |exe: &Option<String>, title: &str, minimized: bool| Cand { exe: exe.clone(), aumid: None, title: title.into(), minimized };
        let wins = vec![
            win(&edge, "Mail - Outlook - Microsoft Edge", false),
            win(&chrome, "Docs - Google Chrome", false),
            win(&chrome, "Lofi beats - YouTube - Google Chrome", false),
            win(&chrome, "Other - Google Chrome", false),
        ];
        // The browser's window whose title holds the track.
        assert_eq!(pick("Chrome", "Lofi Beats", &wins), Some(2));
        // No title to go by: its most recently active window.
        assert_eq!(pick("Chrome", "", &wins), Some(1));
        assert_eq!(pick("Chrome", "Not open anywhere", &wins), Some(1));
        assert_eq!(pick("MSEdge", "Lofi beats", &wins), Some(0), "only the app's own windows");
        // On screen before minimized; the title still wins.
        let min = vec![win(&chrome, "Docs - Google Chrome", true), win(&chrome, "News - Google Chrome", false)];
        assert_eq!(pick("Chrome", "", &min), Some(1));
        assert_eq!(pick("Chrome", "Docs", &min), Some(0));
        // Packaged apps by their AUMID only.
        let pkg = "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify";
        let spotify = Cand { exe: Some(r"c:\program files\windowsapps\spotify\spotify.exe".into()), aumid: Some(pkg.to_lowercase()), title: "Spotify Premium".into(), minimized: false };
        assert_eq!(pick(pkg, "Song", &[win(&chrome, "Song - Google Chrome", false), spotify]), Some(1));
        // Nothing of the app's, or no app.
        assert_eq!(pick("Spotify.exe", "x", &wins), None);
        assert_eq!(pick("", "x", &wins), None);
    }
}

/// The controls drawn for the design audit (tests only).
#[cfg(test)]
#[path = "pip_harmony.rs"]
mod harmony;
