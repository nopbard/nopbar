//! The setup window: Win32 + Direct2D, one fixed-size window (460 x 280
//! DIP) centered on the primary monitor. Our own title strip (the system
//! frame is taken away in WM_NCCALCSIZE; DWM keeps the shadow and, on
//! Windows 11, the rounded corners), per-monitor DPI, light/dark and high
//! contrast from Windows, keyboard (Enter/Space install, Esc close, Tab
//! shows the focus ring), a screen reader name through MSAA annotations
//! (the client area is a push button named for the install action), in
//! the user's language (strings::Lang), mirrored right to left (draw).
//!
//! Drag the icon onto the folder (or click the folder, or Enter): the icon
//! flies in, the installer runs on a worker thread, the bar follows its
//! log; on success the window closes (NopBar is running by then); on
//! failure a message and Retry.

use std::cell::RefCell;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use windows::core::{w, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_IGNORE, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ScreenToClient, UpdateWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CLSID_AccPropServices, IAccPropServices, NotifyWinEvent, PROPID_ACC_DEFAULTACTION, PROPID_ACC_DESCRIPTION, PROPID_ACC_NAME, PROPID_ACC_ROLE,
    ROLE_SYSTEM_PUSHBUTTON,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_RETURN, VK_SPACE, VK_TAB};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Matrix3x2;

use super::draw::{self, Resources, Scene, H, ICON_HOME, INTO_FOLDER, INTO_SCALE, W};
use super::theme::{self, Look, Palette};
use crate::install::{self, Event};
use crate::launch;
use crate::payload;
use crate::strings::{self, Failure, Lang, Strings};

const CLASS: PCWSTR = w!("NopBarSetupWindow");
/// (Win32_UI_Controls, not worth the feature for one constant.)
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER: usize = 1;
/// The worker has news (in the channel).
const WM_APP_EVENT: u32 = WM_APP + 1;
/// The icon flying into the folder (the mockup's .45 s).
const FLY: Duration = Duration::from_millis(450);
/// A dropped-elsewhere icon going home.
const RETURN: Duration = Duration::from_millis(220);
/// The icon coming back after a failure.
const FADE: Duration = Duration::from_millis(250);
/// The full bar stays this long before the window closes.
const HOLD: Duration = Duration::from_millis(300);
/// Exit code when the window is closed without installing (Inno Setup's
/// "cancelled before installing").
const CLOSED: i32 = 2;

// ---------------------------------------------------------------- pure helpers

/// Ease-out (cubic), clamped: the mockup's cubic-bezier(.2, 0, 0, 1) feel.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn lerp(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn progress_of(start: Instant, now: Instant, d: Duration) -> f32 {
    if d.is_zero() {
        return 1.0;
    }
    (now.saturating_duration_since(start).as_secs_f32() / d.as_secs_f32()).clamp(0.0, 1.0)
}

/// Where the bar heads: what the log says, plus a little drift (2 %/s, at
/// most 8 %) so a long step does not look frozen; all of it once done.
pub fn bar_goal(log_fraction: f32, since_change: Duration, done: bool) -> f32 {
    if done {
        return 1.0;
    }
    (log_fraction + (since_change.as_secs_f32() * 0.02).min(0.08)).min(0.98)
}

/// Window size in pixels at `dpi`.
pub fn size_px(dpi: u32) -> (i32, i32) {
    let k = dpi as f32 / 96.0;
    ((W * k).round() as i32, (H * k).round() as i32)
}

/// Centered in `work` (a monitor's work area).
pub fn centered(work: RECT, size: (i32, i32)) -> (i32, i32) {
    (work.left + (work.right - work.left - size.0) / 2, work.top + (work.bottom - work.top - size.1) / 2)
}

/// Client pixels -> DIP.
fn dip(x: i32, y: i32, dpi: u32) -> (f32, f32) {
    let k = 96.0 / dpi.max(1) as f32;
    (x as f32 * k, y as f32 * k)
}

/// The window is mirrored (Arabic, Persian, Hebrew): hit tests take window
/// points and this; a dragged icon's position goes through draw::logical.
fn rtl() -> bool {
    Lang::current().rtl()
}

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as u16 as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32)
}

// ---------------------------------------------------------------- state

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Ready,
    /// Pressed on the icon; a drag once the pointer moves.
    Pressed { at: (f32, f32), grab: (f32, f32) },
    Dragging { grab: (f32, f32) },
    Returning { from: (f32, f32), t0: Instant },
    Flying { from: (f32, f32), t0: Instant },
    Busy,
    Done { t0: Instant },
    Failed { failure: Failure, t0: Instant },
}

struct WindowTarget {
    rt: ID2D1HwndRenderTarget,
    icon: ID2D1Bitmap,
}

struct App {
    hwnd: HWND,
    dpi: u32,
    lang: Lang,
    s: &'static Strings,
    look: Look,
    pal: Palette,
    res: Resources,
    rt: Option<WindowTarget>,
    phase: Phase,
    /// The icon's center at rest or while dragged.
    icon: (f32, f32),
    over_folder: bool,
    close_hover: bool,
    close_down: bool,
    retry_hover: bool,
    tracking: bool,
    /// Show the focus ring (after Tab, as :focus-visible).
    kb_focus: bool,
    /// The log's fraction, when it last moved, and the bar as drawn.
    target: f32,
    target_at: Instant,
    shown: f32,
    /// The worker's verdict, applied once the icon has landed.
    result: Option<std::result::Result<(), Failure>>,
    events: Option<Receiver<Event>>,
    last_tick: Instant,
    timer: bool,
    exit_code: i32,
    acc: Option<IAccPropServices>,
    inner: &'static [u8],
    version: &'static str,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app unless it is missing or busy (a re-entrant
/// message): those fall back to DefWindowProc.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

impl App {
    fn ready_like(&self) -> bool {
        matches!(self.phase, Phase::Ready | Phase::Failed { .. })
    }

    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Flying { .. } | Phase::Busy | Phase::Done { .. })
    }

    fn failed(&self) -> Option<Failure> {
        match self.phase {
            Phase::Failed { failure, .. } => Some(failure),
            _ => None,
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn start_timer(&mut self) {
        if !self.timer {
            self.last_tick = Instant::now();
            unsafe { SetTimer(Some(self.hwnd), TIMER, 15, None) };
            self.timer = true;
        }
    }

    fn duration(&self, d: Duration) -> Duration {
        if self.look.animations {
            d
        } else {
            Duration::ZERO
        }
    }

    /// The client area's accessible name (a push button).
    fn announce(&self, name: &str) {
        if let Some(acc) = &self.acc {
            unsafe {
                let _ = acc.SetHwndPropStr(self.hwnd, OBJID_CLIENT.0 as u32, CHILDID_SELF, PROPID_ACC_NAME, &HSTRING::from(name));
                NotifyWinEvent(EVENT_OBJECT_NAMECHANGE, self.hwnd, OBJID_CLIENT.0, CHILDID_SELF as i32);
            }
        }
    }

    fn start_install(&mut self, from: (f32, f32)) {
        if !self.ready_like() && !matches!(self.phase, Phase::Pressed { .. } | Phase::Dragging { .. }) {
            return;
        }
        let now = Instant::now();
        self.phase = Phase::Flying { from, t0: now };
        self.over_folder = false;
        self.close_hover = false;
        self.retry_hover = false;
        self.target = 0.0;
        self.target_at = now;
        self.shown = 0.0;
        self.result = None;
        let (tx, rx) = channel();
        self.events = Some(rx);
        let hwnd = self.hwnd.0 as isize;
        let (inner, version) = (self.inner, self.version);
        std::thread::spawn(move || {
            let wake = move || unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_EVENT, WPARAM(0), LPARAM(0));
            };
            install::install(
                inner,
                version,
                &std::env::temp_dir(),
                |e| {
                    let _ = tx.send(e);
                    wake();
                },
                || {
                    let _ = launch::ensure_running(Duration::from_secs(4));
                },
            );
        });
        self.announce(self.s.installing);
        self.start_timer();
        self.invalidate();
    }

    fn drain(&mut self) {
        let now = Instant::now();
        if let Some(rx) = &self.events {
            while let Ok(e) = rx.try_recv() {
                match e {
                    Event::Progress(f) if f > self.target => {
                        self.target = f;
                        self.target_at = now;
                    }
                    Event::Progress(_) => {}
                    Event::Finished(r) => self.result = Some(r),
                }
            }
        }
        self.land(now);
        self.start_timer();
    }

    /// Applies the worker's verdict once the icon is in the folder.
    fn land(&mut self, now: Instant) {
        if self.phase != Phase::Busy {
            return;
        }
        match self.result.take() {
            Some(Ok(())) => {
                self.exit_code = 0;
                self.phase = Phase::Done { t0: now };
            }
            Some(Err(failure)) => {
                self.exit_code = match failure {
                    Failure::Code(c) => c,
                    _ => 1,
                };
                self.phase = Phase::Failed { failure, t0: now };
                self.icon = ICON_HOME;
                self.events = None;
                let msg = strings::failure(self.lang, failure);
                self.announce(&format!("{msg} {}", self.s.retry));
            }
            None => {}
        }
    }

    /// One frame of animation. Returns (close the window, keep ticking).
    fn tick(&mut self, now: Instant) -> (bool, bool) {
        let dt = now.saturating_duration_since(self.last_tick).min(Duration::from_millis(100)).as_secs_f32();
        self.last_tick = now;
        let mut close = false;
        match self.phase {
            Phase::Returning { t0, .. } if progress_of(t0, now, self.duration(RETURN)) >= 1.0 => {
                self.phase = Phase::Ready;
                self.icon = ICON_HOME;
            }
            Phase::Flying { t0, .. } if progress_of(t0, now, self.duration(FLY)) >= 1.0 => {
                self.phase = Phase::Busy;
                self.land(now);
            }
            _ => {}
        }
        let done = matches!(self.phase, Phase::Done { .. });
        if self.busy() {
            let goal = bar_goal(self.target, now.saturating_duration_since(self.target_at), done);
            if self.look.animations {
                let k = 1.0 - (-dt * if done { 12.0 } else { 5.0 }).exp();
                self.shown += (goal - self.shown) * k;
            } else {
                self.shown = goal;
            }
            if done && 1.0 - self.shown < 0.003 {
                self.shown = 1.0;
            }
        }
        if let Phase::Done { t0 } = self.phase {
            close = self.shown >= 1.0 && now.saturating_duration_since(t0) >= self.duration(HOLD);
        }
        let keep = match self.phase {
            Phase::Returning { .. } | Phase::Flying { .. } | Phase::Busy | Phase::Done { .. } => true,
            Phase::Failed { t0, .. } => progress_of(t0, now, self.duration(FADE)) < 1.0,
            _ => false,
        };
        (close, keep)
    }

    fn scene(&self, now: Instant) -> Scene<'static> {
        let mut s = Scene::ready();
        s.icon = self.icon;
        s.focus = self.kb_focus && !self.busy();
        s.close_hover = self.close_hover;
        s.close_enabled = !self.busy();
        s.retry_hover = self.retry_hover;
        match self.phase {
            Phase::Ready | Phase::Pressed { .. } => {}
            Phase::Dragging { .. } => s.folder_scale = if self.over_folder { 1.04 } else { 1.0 },
            Phase::Returning { from, t0 } => s.icon = lerp(from, ICON_HOME, ease(progress_of(t0, now, self.duration(RETURN)))),
            Phase::Flying { from, t0 } => {
                let t = progress_of(t0, now, self.duration(FLY));
                let e = ease(t);
                s.icon = lerp(from, INTO_FOLDER, e);
                s.icon_scale = 1.0 + (INTO_SCALE - 1.0) * e;
                s.icon_alpha = 1.0 - e;
                s.folder_scale = 1.0 + 0.04 * (t * std::f32::consts::PI).sin();
                s.hint = false;
                s.progress = Some(self.shown);
            }
            Phase::Busy | Phase::Done { .. } => {
                s.icon_alpha = 0.0;
                s.hint = false;
                s.progress = Some(self.shown);
            }
            Phase::Failed { failure, t0 } => {
                s.icon = ICON_HOME;
                s.icon_alpha = ease(progress_of(t0, now, self.duration(FADE)));
                s.hint = false;
                s.message = Some(strings::failure(self.lang, failure));
            }
        }
        s
    }

    fn render(&mut self) {
        if self.rt.is_none() {
            self.rt = self.make_target().ok();
        }
        let Some(WindowTarget { rt, icon }) = &self.rt else { return };
        let scene = self.scene(Instant::now());
        unsafe {
            rt.BeginDraw();
            let _ = draw::draw(rt, icon, &self.res, &self.pal, self.s, &scene, Matrix3x2::identity());
            if rt.EndDraw(None, None).is_err() {
                // D2DERR_RECREATE_TARGET (device lost) and the like.
                self.rt = None;
                self.invalidate();
            }
        }
    }

    fn make_target(&self) -> Result<WindowTarget> {
        unsafe {
            let mut rc = RECT::default();
            GetClientRect(self.hwnd, &mut rc)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: self.dpi as f32,
                dpiY: self.dpi as f32,
                ..Default::default()
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U { width: (rc.right - rc.left).max(1) as u32, height: (rc.bottom - rc.top).max(1) as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = self.res.factory.CreateHwndRenderTarget(&props, &hwnd_props)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let icon = draw::load_icon(&rt, &wic)?;
            Ok(WindowTarget { rt, icon })
        }
    }

    fn retheme(&mut self) {
        self.look = theme::current();
        self.pal = theme::palette(self.look.dark, self.look.high_contrast);
        dwm_look(self.hwnd, &self.look);
        self.invalidate();
    }
}

/// Dark title-bar hint (shadow and border tone), round corners, and no
/// outline (DWM's border would take the accent color) unless high
/// contrast wants it.
fn dwm_look(hwnd: HWND, look: &Look) {
    unsafe {
        let dark = BOOL::from(look.dark);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const _, 4);
        // DWMWA_COLOR_NONE / DWMWA_COLOR_DEFAULT.
        let border: u32 = if look.high_contrast.is_some() { 0xFFFF_FFFF } else { 0xFFFF_FFFE };
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);
    }
}

// ---------------------------------------------------------------- window procedure

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // The whole window is client area: no system title bar or frame.
            // Both forms: the first one, at creation, comes with wParam
            // FALSE; answering only TRUE left Windows' own caption drawn
            // over ours (0.1.41).
            WM_NCCALCSIZE => return LRESULT(0),
            // Nothing to repaint in a frame that is not there.
            WM_NCACTIVATE => return DefWindowProcW(hwnd, msg, wp, LPARAM(-1)),
            WM_NCHITTEST => {
                let (x, y) = lparam_xy(lp);
                let mut pt = POINT { x, y };
                let _ = ScreenToClient(hwnd, &mut pt);
                let p = dip(pt.x, pt.y, GetDpiForWindow(hwnd));
                return LRESULT(if draw::hit_caption(p, rtl()) { HTCAPTION } else { HTCLIENT } as isize);
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                with(|a| a.render());
                let _ = EndPaint(hwnd, &ps);
                return LRESULT(0);
            }
            WM_SIZE => {
                let (w, h) = lparam_xy(lp);
                with(|a| {
                    if let Some(WindowTarget { rt, .. }) = &a.rt {
                        let _ = rt.Resize(&D2D_SIZE_U { width: w.max(1) as u32, height: h.max(1) as u32 });
                    }
                    a.invalidate();
                });
                return LRESULT(0);
            }
            WM_DPICHANGED => {
                let dpi = ((wp.0 >> 16) & 0xFFFF) as u32;
                let r = *(lp.0 as *const RECT);
                with(|a| {
                    a.dpi = dpi;
                    a.rt = None;
                    a.invalidate();
                });
                let (w, h) = size_px(dpi);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
                return LRESULT(0);
            }
            WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED => {
                with(|a| a.retheme());
            }
            WM_TIMER if wp.0 == TIMER => {
                let r = with(|a| {
                    let (close, keep) = a.tick(Instant::now());
                    if !keep {
                        let _ = KillTimer(Some(hwnd), TIMER);
                        a.timer = false;
                    }
                    a.invalidate();
                    close
                });
                if r == Some(true) {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            }
            WM_APP_EVENT => {
                with(|a| a.drain());
                return LRESULT(0);
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
                let hand = with(|a| {
                    let (p, rtl) = (dip(pt.x, pt.y, a.dpi), rtl());
                    a.ready_like() && (draw::hit_folder(p, rtl) || (a.failed().is_some() && draw::hit_retry(p, rtl)))
                });
                if hand == Some(true) {
                    if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                        SetCursor(Some(c));
                        return LRESULT(1);
                    }
                }
            }
            WM_LBUTTONDOWN => {
                let (x, y) = lparam_xy(lp);
                let capture = with(|a| {
                    let (p, rtl) = (dip(x, y, a.dpi), rtl());
                    if draw::hit_close(p, rtl) && !a.busy() {
                        a.close_down = true;
                        return true;
                    }
                    if !a.ready_like() {
                        return false;
                    }
                    let icon = if a.failed().is_some() { ICON_HOME } else { a.icon };
                    if draw::hit_icon(p, icon, rtl) {
                        a.icon = icon;
                        // The grab offset in layout coordinates, as the icon moves.
                        let l = draw::logical(p, rtl);
                        a.phase = Phase::Pressed { at: p, grab: (l.0 - icon.0, l.1 - icon.1) };
                        return true;
                    }
                    if draw::hit_folder(p, rtl) || (a.failed().is_some() && draw::hit_retry(p, rtl)) {
                        a.start_install(ICON_HOME);
                    }
                    false
                });
                if capture == Some(true) {
                    SetCapture(hwnd);
                }
                return LRESULT(0);
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lp);
                with(|a| {
                    let (p, rtl) = (dip(x, y, a.dpi), rtl());
                    a.close_hover = draw::hit_close(p, rtl);
                    a.retry_hover = a.failed().is_some() && draw::hit_retry(p, rtl);
                    match a.phase {
                        Phase::Pressed { at, grab } if (p.0 - at.0).hypot(p.1 - at.1) > 3.0 => a.phase = Phase::Dragging { grab },
                        _ => {}
                    }
                    if let Phase::Dragging { grab } = a.phase {
                        let l = draw::logical(p, rtl);
                        a.icon = ((l.0 - grab.0).clamp(0.0, W), (l.1 - grab.1).clamp(draw::STRIP, H));
                        a.over_folder = draw::hit_drop(a.icon);
                    }
                    if !a.tracking {
                        let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                        a.tracking = TrackMouseEvent(&mut t).is_ok();
                    }
                    a.invalidate();
                });
                return LRESULT(0);
            }
            WM_MOUSELEAVE => {
                with(|a| {
                    a.tracking = false;
                    a.close_hover = false;
                    a.retry_hover = false;
                    a.invalidate();
                });
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lp);
                let close = with(|a| {
                    let p = dip(x, y, a.dpi);
                    let mut close = false;
                    if a.close_down {
                        a.close_down = false;
                        close = draw::hit_close(p, rtl());
                    }
                    match a.phase {
                        Phase::Pressed { .. } => a.phase = Phase::Ready,
                        Phase::Dragging { .. } if a.over_folder => a.start_install(a.icon),
                        Phase::Dragging { .. } => {
                            a.phase = Phase::Returning { from: a.icon, t0: Instant::now() };
                            a.over_folder = false;
                            a.start_timer();
                        }
                        _ => {}
                    }
                    a.invalidate();
                    close
                });
                let _ = ReleaseCapture();
                if close == Some(true) {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                return LRESULT(0);
            }
            WM_CAPTURECHANGED => {
                with(|a| {
                    a.close_down = false;
                    match a.phase {
                        Phase::Pressed { .. } => a.phase = Phase::Ready,
                        Phase::Dragging { .. } => {
                            a.phase = Phase::Returning { from: a.icon, t0: Instant::now() };
                            a.over_folder = false;
                            a.start_timer();
                        }
                        _ => {}
                    }
                    a.invalidate();
                });
                return LRESULT(0);
            }
            WM_KEYDOWN => {
                let key = wp.0 as u16;
                let close = with(|a| {
                    if key == VK_RETURN.0 || key == VK_SPACE.0 {
                        if a.ready_like() {
                            a.start_install(ICON_HOME);
                        }
                    } else if key == VK_TAB.0 {
                        a.kb_focus = true;
                        a.invalidate();
                    } else if key == VK_ESCAPE.0 {
                        return true;
                    }
                    false
                });
                if close == Some(true) {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                return LRESULT(0);
            }
            WM_CLOSE => {
                // The installer cannot be cancelled (/NOCANCEL): the window
                // stays until it is done.
                if with(|a| a.busy()) == Some(true) {
                    return LRESULT(0);
                }
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            WM_DESTROY => {
                let code = with(|a| {
                    if let Some(acc) = &a.acc {
                        let _ = acc.ClearHwndProps(hwnd, OBJID_CLIENT.0 as u32, CHILDID_SELF, &[PROPID_ACC_NAME, PROPID_ACC_ROLE, PROPID_ACC_DESCRIPTION, PROPID_ACC_DEFAULTACTION]);
                    }
                    a.exit_code
                })
                .unwrap_or(CLOSED);
                PostQuitMessage(code);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

// ---------------------------------------------------------------- run

/// Shows the window and runs until it closes. Returns the exit code: 0
/// installed, the installer's code (or 1) after a failure, 2 closed
/// without installing. If the window cannot be made (no Direct2D), the
/// installer runs with its own wizard instead.
pub fn run(inner: &'static [u8], version: &'static str) -> i32 {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // One window: a second start brings the first one forward.
        let _mutex = CreateMutexW(None, true, w!("Local\\NopBar.SetupWindow"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(h) = FindWindowW(CLASS, None) {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
            }
            return 0;
        }
        match open(inner, version) {
            Ok(()) => {}
            Err(_) => return payload::passthrough(inner, version, std::ffi::OsStr::new(""), None, &std::env::temp_dir()),
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        msg.wParam.0 as i32
    }
}

/// A captioned window (DWM shadow, rounded corners, Alt+Space menu)
/// without a sizing frame or maximize, whose whole area is client area
/// (WM_NCCALCSIZE): our own title strip is the only one. The frame change
/// is applied once more after creation so no system caption is left.
unsafe fn create_frameless(hinst: windows::Win32::Foundation::HINSTANCE, title: &str, x: i32, y: i32, size: (i32, i32)) -> Result<HWND> {
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        CLASS,
        &HSTRING::from(title),
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
        x,
        y,
        size.0,
        size.1,
        None,
        None,
        Some(hinst),
        None,
    )?;
    let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    Ok(hwnd)
}

fn open(inner: &'static [u8], version: &'static str) -> Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let lang = Lang::current();
        let s = strings::table(lang);
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let res = Resources::new(factory, lang)?;
        // The exe's icon (setup.rc: ICON 1).
        let icon = LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).ok();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hIcon: icon.unwrap_or_default(),
            hIconSm: icon.unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Centered on the primary monitor's work area, at its DPI.
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let size = size_px(dx);
        let (x, y) = centered(mi.rcWork, size);
        let hwnd = create_frameless(hinst.into(), s.window_title, x, y, size)?;
        let look = theme::current();
        dwm_look(hwnd, &look);
        let acc: Option<IAccPropServices> = CoCreateInstance(&CLSID_AccPropServices, None, CLSCTX_INPROC_SERVER).ok();
        if let Some(acc) = &acc {
            let (obj, me) = (OBJID_CLIENT.0 as u32, CHILDID_SELF);
            let _ = acc.SetHwndProp(hwnd, obj, me, PROPID_ACC_ROLE, &VARIANT::from(ROLE_SYSTEM_PUSHBUTTON as i32));
            let _ = acc.SetHwndPropStr(hwnd, obj, me, PROPID_ACC_NAME, &HSTRING::from(s.action));
            let _ = acc.SetHwndPropStr(hwnd, obj, me, PROPID_ACC_DESCRIPTION, &HSTRING::from(s.hint));
            let _ = acc.SetHwndPropStr(hwnd, obj, me, PROPID_ACC_DEFAULTACTION, &HSTRING::from(s.action_verb));
        }
        let now = Instant::now();
        let app = App {
            hwnd,
            dpi: GetDpiForWindow(hwnd).max(96),
            lang,
            s,
            pal: theme::palette(look.dark, look.high_contrast),
            look,
            res,
            rt: None,
            phase: Phase::Ready,
            icon: ICON_HOME,
            over_folder: false,
            close_hover: false,
            close_down: false,
            retry_hover: false,
            tracking: false,
            kb_focus: false,
            target: 0.0,
            target_at: now,
            shown: 0.0,
            result: None,
            events: None,
            last_tick: now,
            timer: false,
            exit_code: CLOSED,
            acc,
            inner,
            version,
        };
        APP.with(|a| *a.borrow_mut() = Some(app));
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = UpdateWindow(hwnd);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0.1.41 shipped with Windows' own title bar drawn over ours: the
    /// window must be all client area. A real (hidden) window of the same
    /// class and style: its client rect is its window rect.
    #[test]
    fn the_window_has_no_system_title_bar() {
        unsafe {
            let hinst = GetModuleHandleW(None).unwrap();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst.into(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let hwnd = create_frameless(hinst.into(), "NopBar", 100, 100, (460, 310)).unwrap();
            let (mut w, mut c) = (RECT::default(), RECT::default());
            GetWindowRect(hwnd, &mut w).unwrap();
            GetClientRect(hwnd, &mut c).unwrap();
            let _ = DestroyWindow(hwnd);
            assert_eq!((c.right - c.left, c.bottom - c.top), (w.right - w.left, w.bottom - w.top), "client {c:?} vs window {w:?}");
            assert_eq!((c.right, c.bottom), (460, 310));
        }
    }

    #[test]
    fn easing_runs_from_0_to_1() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert_eq!(ease(2.0), 1.0);
        assert!(ease(0.5) > 0.5, "fast start, soft landing");
        let mut last = 0.0;
        for i in 0..=100 {
            let e = ease(i as f32 / 100.0);
            assert!(e >= last);
            last = e;
        }
        assert_eq!(lerp((0.0, 10.0), (10.0, 0.0), 0.5), (5.0, 5.0));
    }

    #[test]
    fn the_bar_follows_the_log_and_drifts_a_little() {
        assert_eq!(bar_goal(0.3, Duration::ZERO, false), 0.3);
        assert!((bar_goal(0.3, Duration::from_secs(2), false) - 0.34).abs() < 1e-6);
        assert!((bar_goal(0.3, Duration::from_secs(60), false) - 0.38).abs() < 1e-6, "never more than 8 % ahead");
        assert!(bar_goal(0.97, Duration::from_secs(60), false) < 1.0, "full only when done");
        assert_eq!(bar_goal(0.1, Duration::ZERO, true), 1.0);
    }

    #[test]
    fn the_window_is_460_by_280_dip_and_centered() {
        assert_eq!(size_px(96), (460, 280));
        assert_eq!(size_px(144), (690, 420));
        assert_eq!(size_px(192), (920, 560));
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1032 };
        assert_eq!(centered(work, (460, 280)), (730, 376));
        let second = RECT { left: -1280, top: 0, right: 0, bottom: 1024 };
        assert_eq!(centered(second, (460, 280)), (-870, 372));
        assert_eq!(dip(150, 75, 144), (100.0, 50.0));
        assert_eq!(lparam_xy(LPARAM(((-5i32 as u16 as isize) << 16) | 7)), (7, -5));
    }
}
