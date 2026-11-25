//! Split View from the dock: drag a running app's icon onto another running
//! app's icon, and the two apps' windows go side by side.
//!
//! - While an icon is dragged, holding it over the middle 60 % of another
//!   running app's icon (along the dock, and on the icon across it) for
//!   ARM_MS arms the split: the target icon gets a badge with two halves
//!   and a translucent picture of the halves shows on the target window's
//!   monitor (our own click-through overlay, drawn with Composition; the
//!   screen is never captured). Reordering holds still meanwhile; over the
//!   gaps and the outer 20 % of each side it works as before.
//! - The side follows the half of the target icon the pointer is over:
//!   its physical left half puts the dragged app on the left (also in
//!   right-to-left languages and on side docks: left / right are screen
//!   sides).
//! - Released while armed: the dragged app's most recent window and the
//!   target app's most recent window are restored (minimized / maximized)
//!   and put on the halves of the work area of the target window's monitor
//!   (without the top bar and the taskbar / dock edge), then both come to
//!   the front, the dragged app focused. Shift held at the release gives
//!   the dragged app two thirds (2/3 + 1/3); the picture follows Shift
//!   while hovering. Ultra-wide monitors get the same halves (no thirds).
//! - Cancel: move away from the icon (the hint goes), Esc, or release
//!   elsewhere. The same app (or two icons of one program) never splits.
//! - A window that is not responding or in full screen is not moved (a
//!   notice says why). A window that does not land (it runs as
//!   administrator, refuses the size, closes meanwhile) is told after the
//!   move (snap::split).
//! - The right-click menu of a running app has "Split with" listing the
//!   other running apps (that app left, the chosen one right, 50/50).
//! - The hint's fade (overlay, badge) is the "Split hint" effect
//!   (crate::effects); off, it shows and changes at once.

use super::*;
use crate::i18n::{t as t_, tf};
use windows::UI::Composition::{CompositionBrush, CompositionObject, CompositionSpriteShape, ShapeVisual, Visual};

/// Time over an icon's middle before the split arms.
pub(super) const ARM_MS: u64 = 350;
/// The share of an icon (centered) that arms a split.
pub(super) const CENTER: f32 = 0.6;
/// One-shot: the arm wait is over.
pub(super) const SPLIT_TIMER: usize = 24;
/// A split's helper is done (wParam = the request number): both windows
/// to the front.
pub(super) const WM_SPLIT_DONE: u32 = WM_APP + 85;
/// The hint comes in as things come (7.4): on over FADE_IN_MS, its badge
/// growing from OPEN_SCALE on GLIDE; its halves follow a side / ratio
/// change on GLIDE (at the animation speed; at once with the effect off).
const FADE_MS: i64 = crate::motion::FADE_IN_MS;

/// Where the dragged app goes (physical screen side).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Side {
    #[default]
    Left,
    Right,
}

/// The dragged app's share of the monitor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Ratio {
    /// 50 / 50.
    #[default]
    Half,
    /// 2/3 for the dragged app, 1/3 for the other (Shift).
    TwoThirds,
}

impl Ratio {
    /// The dragged app's share in sixths (shared edges, like snap.rs).
    fn sixths(self) -> i32 {
        match self {
            Ratio::Half => 3,
            Ratio::TwoThirds => 4,
        }
    }
}

/// What a split would do right now, as the hint shows it: the target app
/// and its window (with the process recorded for it in the window list),
/// the monitor, its work area and DPI, the side and the ratio. The hint is
/// redrawn when any of it changes, and a drop splits only if the split it
/// would make is still this one.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Guide {
    pub target: String,
    pub side: Side,
    pub ratio: Ratio,
    pub hwnd: isize,
    pub pid: u32,
    pub started: u64,
    pub mon: isize,
    pub work: RECT,
    pub dpi: u32,
}

impl Guide {
    /// The same windows on the same area (side and ratio aside).
    pub fn same_place(&self, o: &Guide) -> bool {
        self.target == o.target && self.hwnd == o.hwnd && self.pid == o.pid && self.started == o.started && self.mon == o.mon && self.work == o.work && self.dpi == o.dpi
    }
}

// ---- pure rules -------------------------------------------------------------------

/// The pointer `u` is over the middle CENTER share of an icon drawn at
/// `dx`, `w` wide (its outer parts reorder).
pub(super) fn in_center(u: f32, dx: f32, w: f32) -> bool {
    w > 0.0 && (u - (dx + w / 2.0)).abs() < w * CENTER / 2.0
}

/// The side for a pointer at physical x `px` over an icon centered at
/// physical x `cx` (window coordinates): its left half puts the dragged
/// app on the left.
pub(super) fn side_at(px: f32, cx: f32) -> Side {
    if px < cx { Side::Left } else { Side::Right }
}

/// What a hover did to the split hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// Not over a split target: no hint.
    Off,
    /// Over one, armed in this many ms (no hint yet).
    Wait(u64),
    /// Armed: the hint shows (for the target and side now).
    On,
}

/// The hover-arming state of one drag. Pure: the caller passes the time.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Arm {
    target: Option<String>,
    since: Option<Instant>,
    side: Side,
    armed: bool,
}

impl Arm {
    /// The pointer is over split target `over` (app path, side) or none,
    /// at `now`. A new target starts the wait over; a side change does not.
    pub fn update(&mut self, over: Option<(&str, Side)>, now: Instant) -> Step {
        let Some((t, side)) = over else {
            *self = Arm::default();
            return Step::Off;
        };
        if self.target.as_deref() != Some(t) {
            *self = Arm { target: Some(t.to_string()), since: Some(now), side, armed: false };
        }
        self.side = side;
        let waited = self.since.map(|s| now.saturating_duration_since(s).as_millis() as u64).unwrap_or(0);
        if waited >= ARM_MS {
            self.armed = true;
        }
        if self.armed { Step::On } else { Step::Wait(ARM_MS - waited) }
    }

    /// The armed target and side (a release now splits).
    pub fn armed(&self) -> Option<(&str, Side)> {
        if self.armed { self.target.as_deref().map(|t| (t, self.side)) } else { None }
    }
}

/// An app icon as the split rules see it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Who<'a> {
    pub path: &'a str,
    pub exe: Option<&'a str>,
    /// Its AppUserModelID, when known (a web app runs its browser's exe).
    pub aumid: Option<&'a str>,
    /// An app icon (not the Start button, a separator or the trash).
    pub app: bool,
    /// A pinned folder.
    pub stack: bool,
    /// Its normal windows (the dock's window list).
    pub windows: usize,
}

/// Two icons of one program (the same pin, or a pin and a shortcut to the
/// same exe), by the dock's own grouping rule (apps::AppId::matches): two
/// AUMIDs decide by themselves, and a browser's web app is never its
/// browser (the same exe, chrome.exe) nor another web app.
pub(super) fn same_app(a: &Who, b: &Who) -> bool {
    if a.path.eq_ignore_ascii_case(b.path) {
        return true;
    }
    match (a.aumid, b.aumid) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        (Some(w), None) | (None, Some(w)) if crate::webapps::is_web_app(w) => false,
        _ => matches!((a.exe, b.exe), (Some(x), Some(y)) if x.eq_ignore_ascii_case(y)),
    }
}

/// May `a` (dragged) and `b` (dropped on) be split? Two different running
/// apps with windows.
pub(super) fn can_split(a: &Who, b: &Who) -> bool {
    a.app && b.app && !a.stack && !b.stack && a.windows > 0 && b.windows > 0 && !a.path.is_empty() && !b.path.is_empty() && !same_app(a, b)
}

/// A window that covers its whole monitor without being maximized is in
/// full screen (a game, a video, F11): not split.
pub(super) fn full_screen(win: RECT, mon: RECT, zoomed: bool, minimized: bool) -> bool {
    !zoomed && !minimized && edge::covers(win, mon)
}

/// The halves of work area `work` (physical px): (the dragged app's, the
/// other app's). Neighbours share their edge (no gap, no overlap).
pub(super) fn split_rects(work: RECT, side: Side, ratio: Ratio) -> (RECT, RECT) {
    let wd = work.right - work.left;
    let at = |k: i32| work.left + (wd as i64 * k as i64 / 6) as i32;
    let k = ratio.sixths();
    let cut = match side {
        Side::Left => at(k),
        Side::Right => at(6 - k),
    };
    let left = RECT { left: work.left, top: work.top, right: cut, bottom: work.bottom };
    let right = RECT { left: cut, top: work.top, right: work.right, bottom: work.bottom };
    match side {
        Side::Left => (left, right),
        Side::Right => (right, left),
    }
}

/// The hint's halves inside the overlay (which covers `work`), in its own
/// px: the split rects pulled in by a margin that scales with the
/// monitor's `dpi` (half of it on each side of the cut).
pub(super) fn hint_rects(work: RECT, side: Side, ratio: Ratio, dpi: u32) -> (RECT, RECT) {
    let m = margin_px(dpi);
    let (a, b) = split_rects(work, side, ratio);
    let local = |r: RECT| {
        let mut o = RECT { left: r.left - work.left, top: r.top - work.top, right: r.right - work.left, bottom: r.bottom - work.top };
        let ww = work.right - work.left;
        o.left += if o.left == 0 { m } else { m / 2 };
        o.right -= if o.right == ww { m } else { m - m / 2 };
        o.top += m;
        o.bottom -= m;
        if o.right < o.left {
            o.right = o.left;
        }
        if o.bottom < o.top {
            o.bottom = o.top;
        }
        o
    };
    (local(a), local(b))
}

/// 12 DIP at the monitor's DPI.
fn margin_px(dpi: u32) -> i32 {
    ((12 * dpi.max(96) as i64 + 48) / 96) as i32
}

fn px(v: f32, dpi: u32) -> f32 {
    v * dpi.max(96) as f32 / 96.0
}

/// Shift is held (async: the dock never has the keyboard focus).
fn shift_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0 }
}

pub(super) fn ratio_now() -> Ratio {
    if shift_down() { Ratio::TwoThirds } else { Ratio::Half }
}

/// The hint's animations run (the "Split hint" effect, with animations).
fn animated() -> bool {
    crate::motion::full()
}

// ---- the overlay: the halves on the target's monitor --------------------------------

struct Half {
    shape: ShapeVisual,
    geom: CompositionRoundedRectangleGeometry,
    sprite: CompositionSpriteShape,
    icon: SpriteVisual,
}

struct Overlay {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    /// [the dragged app's, the other app's].
    halves: [Half; 2],
    /// What shows and the dock that showed it; None: hidden (or fading
    /// out: `hiding`).
    shown: Option<(isize, Guide)>,
    /// Fading out: the crate::motion::after that hides the window once
    /// the fade is over (cancelled if it shows again first).
    hiding: Option<usize>,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
    /// The badge on the target icon: (the dock window, its visuals).
    static BADGE: RefCell<Option<Badge>> = const { RefCell::new(None) };
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Placed in physical px by us; never resized for a DPI change.
        WM_DPICHANGED => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

impl Overlay {
    unsafe fn new(c: &Compositor) -> Result<Self> {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("NopBarSplitHint");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(overlay_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Click-through, never active, no taskbar button.
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED,
            class,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        let made = (|| -> Result<Self> {
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let root = c.CreateContainerVisual()?;
            target.SetRoot(&root)?;
            let half = || -> Result<Half> {
                let geom = c.CreateRoundedRectangleGeometry()?;
                let sprite = c.CreateSpriteShapeWithGeometry(&geom)?;
                let shape = c.CreateShapeVisual()?;
                shape.Shapes()?.Append(&sprite)?;
                root.Children()?.InsertAtTop(&shape)?;
                let icon = c.CreateSpriteVisual()?;
                root.Children()?.InsertAtTop(&icon)?;
                Ok(Half { shape, geom, sprite, icon })
            };
            let halves = [half()?, half()?];
            Ok(Self { hwnd, _target: target, root, halves, shown: None, hiding: None })
        })();
        if made.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        made
    }

    /// Lays the halves out (overlay px) and colors them.
    unsafe fn draw(&self, c: &Compositor, rects: (RECT, RECT), dpi: u32, lit: Color, plain: Color, icons: [Option<CompositionBrush>; 2]) -> Result<()> {
        // Each half stands for a window: a window's corners.
        let rad = px(crate::theme::WINDOW_RADIUS, dpi);
        let stroke = px(2.0, dpi).round().max(1.0);
        let icon = px(64.0, dpi).round();
        for (k, (h, r)) in self.halves.iter().zip([rects.0, rects.1]).enumerate() {
            let (w, ht) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
            h.shape.SetOffset(Vector3 { X: r.left as f32, Y: r.top as f32, Z: 0.0 })?;
            h.shape.SetSize(Vector2 { X: w, Y: ht })?;
            h.geom.SetSize(Vector2 { X: w, Y: ht })?;
            h.geom.SetCornerRadius(Vector2 { X: rad, Y: rad })?;
            let (fill, edge) = if k == 0 { (lit, Color { A: 0xE6, ..lit }) } else { (plain, Color { A: 0xB3, ..plain }) };
            h.sprite.SetFillBrush(&c.CreateColorBrushWithColor(fill)?)?;
            h.sprite.SetStrokeBrush(&c.CreateColorBrushWithColor(edge)?)?;
            h.sprite.SetStrokeThickness(stroke)?;
            let show_icon = icons[k].is_some() && w > icon * 1.5 && ht > icon * 1.5;
            if let (true, Some(b)) = (show_icon, &icons[k]) {
                h.icon.SetBrush(b)?;
            }
            h.icon.SetSize(Vector2 { X: icon, Y: icon })?;
            h.icon.SetOffset(Vector3 { X: r.left as f32 + (w - icon) / 2.0, Y: r.top as f32 + (ht - icon) / 2.0, Z: 0.0 })?;
            h.icon.SetOpacity(if show_icon { 1.0 } else { 0.0 })?;
        }
        Ok(())
    }

    /// Changes slide over (the effect on), or jump (off / a fresh show).
    unsafe fn set_moves(&self, c: &Compositor, on: bool) {
        for h in &self.halves {
            if on {
                let _ = implicit_on(c, &h.shape, &[("Offset", GLIDE)]);
                let _ = implicit_on(c, &h.geom, &[("Size", GLIDE)]);
                let _ = implicit_on(c, &h.icon, &[("Offset", GLIDE)]);
            } else {
                for o in [h.shape.cast::<CompositionObject>(), h.geom.cast::<CompositionObject>(), h.icon.cast::<CompositionObject>()].into_iter().flatten() {
                    let _ = o.SetImplicitAnimations(None);
                }
            }
        }
    }

    /// Fades out (crate::motion's close), then hides; at once without the
    /// effect's motion.
    unsafe fn hide(&mut self) {
        if self.shown.take().is_none() {
            return;
        }
        if let Some(id) = self.hiding.take() {
            crate::motion::cancel(id);
        }
        let c = self.root.Compositor();
        let faded = animated() && c.as_ref().is_ok_and(|c| crate::motion::close(c, &self.root).is_ok());
        if !faded {
            let _ = self.root.StopAnimation(&HSTRING::from("Opacity"));
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            return;
        }
        let hwnd = self.hwnd.0 as isize;
        self.hiding = crate::motion::after(close_wait(), Box::new(move || hidden(hwnd)));
    }

    /// Showing again: a fade-out under way stops (the window stays).
    fn unhide(&mut self) {
        if let Some(id) = self.hiding.take() {
            crate::motion::cancel(id);
        }
    }
}

/// How long a fade-out waits before what it hides goes (ms, at the
/// chosen speed, with a frame to spare).
fn close_wait() -> u32 {
    (crate::effects::ms(crate::motion::close_ms(crate::motion::now())) + 16) as u32
}

/// The overlay's fade-out is over: hidden, unless it showed again.
fn hidden(hwnd: isize) {
    OVERLAY.with(|o| {
        let Ok(mut o) = o.try_borrow_mut() else { return };
        if let Some(o) = o.as_mut().filter(|o| o.hwnd.0 as isize == hwnd) {
            o.hiding = None;
            if o.shown.is_none() {
                unsafe {
                    let _ = ShowWindow(o.hwnd, SW_HIDE);
                }
            }
        }
    });
}

/// Fades `v` in from nothing (the effect on), or shows it at once.
fn fade_in(c: &Compositor, v: &Visual, scale: bool) -> Result<()> {
    let _ = v.StopAnimation(&HSTRING::from("Opacity"));
    let _ = v.StopAnimation(&HSTRING::from("Scale"));
    if !animated() {
        v.SetOpacity(1.0)?;
        v.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })?;
        return Ok(());
    }
    let dur = windows::Foundation::TimeSpan { Duration: crate::effects::ms(FADE_MS) * 10_000 };
    let ((x1, y1), (x2, y2)) = crate::motion::EASE_OUT;
    let ease = c.CreateCubicBezierEasingFunction(Vector2 { X: x1, Y: y1 }, Vector2 { X: x2, Y: y2 })?;
    let a = c.CreateScalarKeyFrameAnimation()?;
    a.InsertKeyFrame(0.0, 0.0)?;
    a.InsertKeyFrameWithEasingFunction(1.0, 1.0, &ease)?;
    a.SetDuration(dur)?;
    v.StartAnimation(&HSTRING::from("Opacity"), &a)?;
    if scale {
        // What comes in grows from OPEN_SCALE on GLIDE (7.4).
        let (grow, from) = (crate::motion::GLIDE_MAX_MS, crate::motion::OPEN_SCALE);
        let lin = c.CreateLinearEasingFunction()?;
        let s = c.CreateVector3KeyFrameAnimation()?;
        for (p, done) in crate::motion::SPRING_GLIDE.keys(grow, crate::motion::SAMPLES) {
            let k = from + (1.0 - from) * done;
            s.InsertKeyFrameWithEasingFunction(p, Vector3 { X: k, Y: k, Z: 1.0 }, &lin)?;
        }
        s.SetDuration(windows::Foundation::TimeSpan { Duration: crate::effects::ms(grow) * 10_000 })?;
        v.StartAnimation(&HSTRING::from("Scale"), &s)?;
    }
    Ok(())
}

// ---- the badge on the target icon ---------------------------------------------------

struct Badge {
    dock: isize,
    root: ContainerVisual,
    geom: CompositionRoundedRectangleGeometry,
    halves: [SpriteVisual; 2],
    target: String,
    side: Side,
}

/// Takes the badge of dock window `dock` out of the layer it is in: it
/// fades out first (crate::motion's close; at once without the effect's
/// motion).
fn hide_badge(dock: isize) {
    let gone = BADGE.with(|b| {
        let Ok(mut b) = b.try_borrow_mut() else { return None };
        if b.as_ref().is_some_and(|x| x.dock == dock) {
            b.take()
        } else {
            None
        }
    });
    let Some(x) = gone else { return };
    let faded = animated() && x.root.Compositor().is_ok_and(|c| crate::motion::close(&c, &x.root).is_ok());
    let remove = move || {
        if let Ok(ch) = x.root.Parent().and_then(|p| p.Children()) {
            let _ = ch.Remove(&x.root);
        }
    };
    if faded {
        // (A new badge is a new visual: this one only fades and goes.)
        let _ = crate::motion::after(close_wait(), Box::new(remove));
    } else {
        remove();
    }
}

/// Takes down whatever split hint dock window `dock` shows: its overlay,
/// its badge, its arm timer. Needs no Dock (a closing display's dock may
/// be borrowed, or out of the registry already).
pub(super) fn teardown(dock: isize) {
    unsafe {
        let _ = KillTimer(Some(HWND(dock as *mut _)), SPLIT_TIMER);
    }
    OVERLAY.with(|o| {
        if let Ok(mut o) = o.try_borrow_mut() {
            if let Some(o) = o.as_mut().filter(|o| o.shown.as_ref().is_some_and(|(d, _)| *d == dock)) {
                unsafe { o.hide() };
            }
        }
    });
    hide_badge(dock);
}

/// NopBar is quitting: the overlay window goes for good.
pub(super) fn shutdown() {
    OVERLAY.with(|o| {
        if let Ok(mut o) = o.try_borrow_mut() {
            if let Some(o) = o.take() {
                unsafe {
                    let _ = DestroyWindow(o.hwnd);
                }
            }
        }
    });
}

// ---- the dock's side ------------------------------------------------------------------

impl Dock {
    fn who(&self, i: usize) -> Who<'_> {
        let it = &self.items[i];
        Who { path: &it.path, exe: it.id.exe.as_deref(), aumid: it.id.aumid.as_deref(), app: it.kind == Kind::App, stack: it.stack, windows: it.windows.len() }
    }

    /// The icon under the pointer (`u`, `v`, canonical) a dragged app could
    /// be split with, and the side: over its middle along the dock and on
    /// it across. None elsewhere (gaps and edges reorder).
    pub(super) fn split_candidate(&self, u: f32, v: f32) -> Option<(usize, Side)> {
        let (path, ..) = self.drag_view()?;
        let a = self.items.iter().position(|it| it.kind == Kind::App && it.path == path)?;
        let t = self.items.iter().enumerate().position(|(k, it)| k != a && it.clickable() && u >= it.dx && u < it.dx + it.w)?;
        let it = &self.items[t];
        if !in_center(u, it.dx, it.w) || !can_split(&self.who(a), &self.who(t)) {
            return None;
        }
        // On the icon across the dock (not above it in the remove zone).
        let rest_top = self.bar_rect.y + PAD_CROSS + self.look.icon - it.w;
        if v < rest_top - 4.0 {
            return None;
        }
        // The physical side: window x of the pointer and of the icon's center.
        let p = self.frame.to_win(Rect { x: u, y: v, w: 0.0, h: 0.0 });
        let c = self.frame.to_win(Rect { x: it.dx + it.w / 2.0, y: rest_top + it.w / 2.0, w: 0.0, h: 0.0 });
        Some((t, side_at(p.x, c.x)))
    }

    /// The pointer moved over `over` while dragging: arms, refreshes or
    /// takes down the hint. True while it is over a split target (the
    /// order then holds still).
    pub(super) fn split_hover(&mut self, over: Option<(usize, Side)>) -> bool {
        let target = over.map(|(t, s)| (self.items[t].path.clone(), s));
        let Some(step) = self.drag.as_mut().map(|d| d.split.update(target.as_ref().map(|(p, s)| (p.as_str(), *s)), Instant::now())) else {
            return false;
        };
        unsafe {
            let _ = KillTimer(Some(self.hwnd), SPLIT_TIMER);
        }
        match step {
            Step::Off => self.split_hide(),
            Step::Wait(ms) => {
                self.split_hide();
                unsafe {
                    SetTimer(Some(self.hwnd), SPLIT_TIMER, ms.clamp(10, ARM_MS) as u32, None);
                }
            }
            Step::On => self.split_show(),
        }
        over.is_some()
    }

    /// SPLIT_TIMER fired: the pointer may have rested on the icon long
    /// enough (it does not have to move for the hint).
    pub(super) fn split_wait_over(&mut self) {
        if !self.dragging() || drag::escape_down() {
            self.split_hide();
            return;
        }
        if let Some((u, v)) = self.mouse {
            let over = self.split_candidate(u, v);
            self.split_hover(over);
        }
    }

    /// While dragging (the Esc poll): the picture follows Shift.
    pub(super) fn split_poll(&mut self) {
        if self.drag.as_ref().is_some_and(|d| d.split.armed().is_some()) {
            self.split_show();
        }
    }

    /// The process the dock's window list recorded for window `h`: (pid,
    /// its creation time). None if the list does not have it.
    pub(super) fn win_owner(&self, h: HWND) -> Option<(u32, u64)> {
        self.last_wins.iter().find(|w| w.hwnd == h).map(|w| (w.pid, w.started)).filter(|(pid, _)| *pid != 0)
    }

    /// What splitting with item `t` on `side` with `ratio` would do now.
    pub(super) fn split_guide(&self, t: usize, side: Side, ratio: Ratio) -> Option<Guide> {
        let h = self.recent_window(t)?;
        let (pid, started) = self.win_owner(h)?;
        let mon = snap::monitor_of(h)?;
        let work = snap::work_of(&mon);
        if work.right <= work.left || work.bottom <= work.top {
            return None;
        }
        Some(Guide { target: self.items[t].path.clone(), side, ratio, hwnd: h.0 as isize, pid, started, mon: mon.handle, work, dpi: mon.dpi })
    }

    /// Shows (or updates) the hint for the armed target.
    fn split_show(&mut self) {
        let Some((target, side)) = self.drag.as_ref().and_then(|d| d.split.armed().map(|(t, s)| (t.to_string(), s))) else {
            self.split_hide();
            return;
        };
        let (Some((path, ..)), Some(t)) = (self.drag_view(), self.find_app_index_pub(&target)) else {
            self.split_hide();
            return;
        };
        let Some(a) = self.find_app_index_pub(path) else { return };
        // The target window, its monitor and work area are looked at every
        // time (another of its windows may come forward, a display may
        // change); only an unchanged guide is skipped.
        let Some(g) = self.split_guide(t, side, ratio_now()) else {
            self.split_hide();
            return;
        };
        let me = self.hwnd.0 as isize;
        let same = OVERLAY.with(|o| o.borrow().as_ref().and_then(|o| o.shown.clone()).is_some_and(|(d, s)| d == me && s == g));
        if !same {
            if let Err(e) = unsafe { self.show_overlay(a, t, g) } {
                crate::log!("split: hint unavailable: {e:?}");
            }
        }
        self.show_badge(&target, side);
    }

    /// The guide the hint shows for this dock (what a drop now would do).
    pub(super) fn split_shown(&self) -> Option<Guide> {
        let me = self.hwnd.0 as isize;
        OVERLAY.with(|o| o.borrow().as_ref().and_then(|o| o.shown.clone()).filter(|(d, _)| *d == me).map(|(_, g)| g))
    }

    unsafe fn show_overlay(&mut self, a: usize, t: usize, g: Guide) -> Result<()> {
        let c = self.gfx.compositor.clone();
        // (Made once; again if its window went away.)
        let fresh_needed = OVERLAY.with(|o| o.borrow().as_ref().is_none_or(|o| !IsWindow(Some(o.hwnd)).as_bool()));
        if fresh_needed {
            let made = Overlay::new(&c)?;
            OVERLAY.with(|o| *o.borrow_mut() = Some(made));
        }
        let (lit, plain) = self.split_colors();
        let icons = [self.items[a].large_brush(), self.items[t].large_brush()];
        let rects = hint_rects(g.work, g.side, g.ratio, g.dpi);
        let dock = views::ui_hwnd();
        let me = self.hwnd.0 as isize;
        OVERLAY.with(|o| -> Result<()> {
            let mut o = o.borrow_mut();
            let Some(o) = o.as_mut() else { return Ok(()) };
            // A fresh show (another dock, window or monitor) appears in
            // place; side / ratio changes after that slide (the effect on).
            // (Fading out meanwhile: it stays up and fades in again.)
            o.unhide();
            let fresh = o.shown.as_ref().is_none_or(|(d, s)| *d != me || !s.same_place(&g));
            o.set_moves(&c, !fresh && animated());
            o.draw(&c, rects, g.dpi, lit, plain, icons)?;
            if fresh {
                // Just under the dock: the dock and its labels stay on top.
                let after = if dock.is_invalid() { HWND_TOPMOST } else { dock };
                let w = g.work;
                let _ = SetWindowPos(o.hwnd, Some(after), w.left, w.top, w.right - w.left, w.bottom - w.top, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                fade_in(&c, &o.root.cast::<Visual>()?, false)?;
            }
            o.set_moves(&c, animated());
            o.shown = Some((me, g));
            Ok(())
        })
    }

    /// The dragged app's half (the accent) and the other's (neutral);
    /// high contrast uses its highlight.
    fn split_colors(&self) -> (Color, Color) {
        match self.theme.focus_outline() {
            Some(h) => (color(0x66, h.r, h.g, h.b), color(0x40, 0xFF, 0xFF, 0xFF)),
            None => {
                let a = self.theme.accent();
                (color(0x59, a.r, a.g, a.b), color(0x40, 0x80, 0x80, 0x80))
            }
        }
    }

    fn show_badge(&mut self, target: &str, side: Side) {
        let me = self.hwnd.0 as isize;
        let current = BADGE.with(|b| b.borrow().as_ref().map(|b| (b.dock, b.target.clone(), b.side)));
        if current.as_ref().is_some_and(|(d, p, s)| *d == me && p == target && *s == side) {
            return;
        }
        let fresh = !current.as_ref().is_some_and(|(d, p, _)| *d == me && p == target);
        if fresh {
            hide_badge(self.hwnd.0 as isize);
            if let Err(e) = self.make_badge(target, side) {
                crate::log!("split: no badge: {e:?}");
                return;
            }
        } else {
            BADGE.with(|b| {
                if let Some(b) = b.borrow_mut().as_mut() {
                    b.side = side;
                }
            });
        }
        self.place_split_badge();
        if fresh {
            let root = BADGE.with(|b| b.borrow().as_ref().map(|b| b.root.clone()));
            if let Some(Ok(v)) = root.map(|r| r.cast::<Visual>()) {
                let _ = fade_in(&self.gfx.compositor, &v, true);
            }
        }
    }

    fn make_badge(&mut self, target: &str, side: Side) -> Result<()> {
        let c = &self.gfx.compositor;
        let root = c.CreateContainerVisual()?;
        let geom = c.CreateRoundedRectangleGeometry()?;
        root.SetClip(&c.CreateGeometricClipWithGeometry(&geom)?)?;
        let back = c.CreateSpriteVisual()?;
        back.SetBrush(&c.CreateColorBrushWithColor(color(0xE6, 0x20, 0x20, 0x24))?)?;
        back.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        root.Children()?.InsertAtTop(&back)?;
        let (lit, _) = self.split_colors();
        let lit = Color { A: 0xFF, ..lit };
        let a = c.CreateSpriteVisual()?;
        a.SetBrush(&c.CreateColorBrushWithColor(lit)?)?;
        let b = c.CreateSpriteVisual()?;
        b.SetBrush(&c.CreateColorBrushWithColor(color(0xD9, 0xF2, 0xF2, 0xF2))?)?;
        root.Children()?.InsertAtTop(&a)?;
        root.Children()?.InsertAtTop(&b)?;
        // Above every icon.
        self.layer.Children()?.InsertAtTop(&root)?;
        BADGE.with(|s| *s.borrow_mut() = Some(Badge { dock: self.hwnd.0 as isize, root, geom, halves: [a, b], target: target.to_string(), side }));
        Ok(())
    }

    /// Keeps the badge on the target icon's top corner (after every
    /// layout: the icon magnifies and slides).
    pub(super) fn place_split_badge(&self) {
        let me = self.hwnd.0 as isize;
        BADGE.with(|b| {
            let b = b.borrow();
            let Some(b) = b.as_ref().filter(|b| b.dock == me) else { return };
            let it = self.items.iter().find(|it| it.kind == Kind::App && it.path == b.target);
            let Some((Ok(o), Ok(sz))) = it.map(|it| (it.plate.Offset(), it.plate.Size())) else {
                let _ = b.root.SetOpacity(0.0);
                return;
            };
            let bw = (sz.X.min(sz.Y) * 0.46).max(16.0);
            let bh = (bw * 0.72).round();
            let at = Vector3 { X: o.X + sz.X - bw * 0.8, Y: o.Y - bh * 0.3, Z: 0.0 };
            let _ = b.root.SetOffset(at);
            let _ = b.root.SetSize(Vector2 { X: bw, Y: bh });
            let _ = b.root.SetCenterPoint(Vector3 { X: bw / 2.0, Y: bh / 2.0, Z: 0.0 });
            let _ = b.geom.SetSize(Vector2 { X: bw, Y: bh });
            let r = (bh * 0.25).max(3.0);
            let _ = b.geom.SetCornerRadius(Vector2 { X: r, Y: r });
            // Two halves with a gap; the dragged app's (lit) on its side.
            let pad = (bh * 0.16).max(2.0);
            let gap = (bw * 0.06).max(1.5);
            let hw = (bw - pad * 2.0 - gap) / 2.0;
            let hh = bh - pad * 2.0;
            let (lx, rx) = (pad, pad + hw + gap);
            let (ax, bx) = match b.side {
                Side::Left => (lx, rx),
                Side::Right => (rx, lx),
            };
            for (v, x) in [(&b.halves[0], ax), (&b.halves[1], bx)] {
                let _ = v.SetOffset(Vector3 { X: x, Y: pad, Z: 0.0 });
                let _ = v.SetSize(Vector2 { X: hw, Y: hh });
            }
        });
    }

    /// Takes the hint down (overlay and badge).
    pub(super) fn split_hide(&mut self) {
        teardown(self.hwnd.0 as isize);
    }

    /// The end of a drag: the hint goes, the arm timer stops.
    pub(super) fn split_end(&mut self) {
        teardown(self.hwnd.0 as isize);
    }

    fn find_app_index_pub(&self, path: &str) -> Option<usize> {
        self.items.iter().position(|it| it.kind == Kind::App && it.path == path)
    }

    /// Splits app `a` (on `side`, with `ratio`) and app `b` (the other
    /// half) on the monitor of `b`'s most recent window. Checked here:
    /// both still running, not the same app, neither window hung or in
    /// full screen (a notice next to `a`'s icon says why nothing moved).
    /// `guided`: the hint shown when it was dropped; the split goes ahead
    /// only if it would still do exactly that.
    pub(super) fn do_split(&mut self, a: &str, b: &str, side: Side, ratio: Ratio, guided: Option<&Guide>) {
        let (Some(ai), Some(bi)) = (self.find_app_index_pub(a), self.find_app_index_pub(b)) else { return };
        if !can_split(&self.who(ai), &self.who(bi)) {
            return;
        }
        let at = Some((self.menu_anchor(ai), self.menu_align()));
        let (Some(ha), Some(hb)) = (self.recent_window(ai), self.recent_window(bi)) else {
            crate::log!("split: {a:?} or {b:?} has no window now");
            return;
        };
        if ha == hb {
            return;
        }
        let name = |i: usize| self.items[i].name.clone();
        unsafe {
            for (h, i) in [(ha, ai), (hb, bi)] {
                if IsHungAppWindow(h).as_bool() {
                    crate::log!("split: window {:#x} is not responding; nothing moved", h.0 as isize);
                    crate::launch::tell(tf("dock.split.hung", &[("name", &name(i))]), t_("dock.split.hung.why").into(), Vec::new(), at);
                    return;
                }
                let mut r = RECT::default();
                let mon = snap::monitor_of(h);
                if GetWindowRect(h, &mut r).is_ok() && mon.is_some_and(|m| full_screen(r, m.rect, IsZoomed(h).as_bool(), IsIconic(h).as_bool())) {
                    crate::log!("split: window {:#x} is in full screen; nothing moved", h.0 as isize);
                    crate::launch::tell(tf("dock.split.fullscreen", &[("name", &name(i))]), t_("dock.split.fullscreen.why").into(), Vec::new(), at);
                    return;
                }
            }
        }
        // Who each window belonged to when the window list was taken: the
        // split acts only on those processes' windows.
        let (Some((pid_a, st_a)), Some(g)) = (self.win_owner(ha), self.split_guide(bi, side, ratio)) else {
            crate::log!("split: a window is not in the window list (yet); nothing moved");
            return;
        };
        if let Some(want) = guided {
            if !want.same_place(&g) {
                crate::log!("split: the target changed since the hint was drawn ({want:?} -> {g:?}); nothing moved");
                return;
            }
        }
        let (ra, rb) = split_rects(g.work, side, ratio);
        crate::log!("split: {a:?} {side:?} {ratio:?} with {b:?} on {:?}", g.work);
        let pa = snap::SplitPart { hwnd: ha.0 as isize, pid: pid_a, started: st_a, to: ra, name: name(ai) };
        let pb = snap::SplitPart { hwnd: g.hwnd, pid: g.pid, started: g.started, to: rb, name: name(bi) };
        snap::split(pa, pb, g.dpi, self.hwnd, WM_SPLIT_DONE, at);
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn r(l: i32, t: i32, rr: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: rr, bottom: b }
    }

    #[test]
    fn arming_waits_350_ms_over_one_icon() {
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        let mut a = Arm::default();
        assert_eq!(a.update(Some(("B", Side::Left)), t0), Step::Wait(ARM_MS));
        assert_eq!(a.update(Some(("B", Side::Left)), ms(200)), Step::Wait(150));
        assert!(a.armed().is_none(), "not before the delay");
        // A side change keeps the wait going.
        assert_eq!(a.update(Some(("B", Side::Right)), ms(300)), Step::Wait(50));
        assert_eq!(a.update(Some(("B", Side::Right)), ms(350)), Step::On);
        assert_eq!(a.armed(), Some(("B", Side::Right)));
        // Armed: the side follows the pointer.
        assert_eq!(a.update(Some(("B", Side::Left)), ms(400)), Step::On);
        assert_eq!(a.armed(), Some(("B", Side::Left)));
    }

    #[test]
    fn another_icon_starts_over_and_moving_away_cancels() {
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        let mut a = Arm::default();
        a.update(Some(("B", Side::Left)), t0);
        assert_eq!(a.update(Some(("B", Side::Left)), ms(400)), Step::On);
        // On to another app: a new wait, nothing armed meanwhile.
        assert_eq!(a.update(Some(("C", Side::Left)), ms(410)), Step::Wait(ARM_MS));
        assert!(a.armed().is_none());
        // Away (a gap, an edge, off the dock): cancelled.
        assert_eq!(a.update(None, ms(900)), Step::Off);
        assert!(a.armed().is_none());
        // Back on the same icon: the wait starts again.
        assert_eq!(a.update(Some(("C", Side::Left)), ms(1000)), Step::Wait(ARM_MS));
        // A clock read before the start is not a negative wait.
        let mut b = Arm::default();
        b.update(Some(("B", Side::Left)), ms(500));
        assert_eq!(b.update(Some(("B", Side::Left)), ms(100)), Step::Wait(ARM_MS));
    }

    #[test]
    fn only_the_middle_sixty_percent_arms() {
        // A 50 DIP icon at 100: its middle 60 % is 110..140.
        assert!(in_center(125.0, 100.0, 50.0));
        assert!(in_center(110.5, 100.0, 50.0));
        assert!(in_center(139.5, 100.0, 50.0));
        assert!(!in_center(109.0, 100.0, 50.0), "left fifth reorders");
        assert!(!in_center(141.0, 100.0, 50.0), "right fifth reorders");
        assert!(!in_center(125.0, 100.0, 0.0), "a vanished icon");
    }

    #[test]
    fn side_by_pointer_half() {
        assert_eq!(side_at(10.0, 25.0), Side::Left);
        assert_eq!(side_at(30.0, 25.0), Side::Right);
        assert_eq!(side_at(25.0, 25.0), Side::Right);
        // Side docks: the physical x across the icon decides (left dock:
        // the desktop side, larger canonical v, is physical right).
        use super::super::edge::Frame;
        let f = Frame { pos: Position::Left, depth: 135.0 };
        let p = f.to_win(Rect { x: 300.0, y: 100.0, w: 0.0, h: 0.0 });
        let c = f.to_win(Rect { x: 300.0, y: 90.0, w: 0.0, h: 0.0 });
        assert_eq!(side_at(p.x, c.x), Side::Left, "towards the screen edge is left on a left dock");
        let f = Frame { pos: Position::Right, depth: 135.0 };
        let p = f.to_win(Rect { x: 300.0, y: 100.0, w: 0.0, h: 0.0 });
        let c = f.to_win(Rect { x: 300.0, y: 90.0, w: 0.0, h: 0.0 });
        assert_eq!(side_at(p.x, c.x), Side::Right, "towards the screen edge is right on a right dock");
    }

    fn who<'a>(path: &'a str, exe: Option<&'a str>, windows: usize) -> Who<'a> {
        Who { path, exe, aumid: None, app: true, stack: false, windows }
    }

    #[test]
    fn a_web_app_is_not_its_browser() {
        let chrome = r"c:\program files\google\chrome\application\chrome.exe";
        let browser = who(r"C:\SM\Google Chrome.lnk", Some(chrome), 1);
        let crd = Who { aumid: Some("Chrome._crx_cmkncekebbplodngbpllndjkfo"), ..who(r"C:\SM\Chrome 앱\CRD.lnk", Some(chrome), 1) };
        let music = Who { aumid: Some("Chrome._crx_cinhimbnkkghhklpknlkffjgod"), ..who(r"shell:AppsFolder\Chrome._crx_cinhimbnkkghhklpknlkffjgod", Some(chrome), 1) };
        // Dragged onto Chrome (or Chrome onto it): two apps, split.
        assert!(can_split(&crd, &browser) && can_split(&browser, &crd));
        // Two web apps of one browser: two apps.
        assert!(can_split(&crd, &music));
        // The same web app (another icon of it, any case): one app.
        let again = Who { aumid: Some("chrome._crx_CMKNCEKEBBPLODNGBPLLNDJKFO"), ..who(r"C:\Desktop\CRD.lnk", Some(chrome), 1) };
        assert!(!can_split(&crd, &again));
        // A browser profile's window (an AUMID, not a web app's) and a
        // plain one: the same program, as the dock groups them.
        let profile = Who { aumid: Some("Chrome.UserData.Profile1"), ..who(r"C:\SM\Profile.lnk", Some(chrome), 1) };
        assert!(!can_split(&profile, &browser));
    }

    #[test]
    fn eligibility() {
        let a = who("C:\\a.lnk", Some("C:\\a.exe"), 1);
        let b = who("C:\\b.lnk", Some("C:\\b.exe"), 2);
        assert!(can_split(&a, &b));
        // Same app: by path (any case) or by program.
        assert!(!can_split(&a, &who("c:\\A.LNK", None, 1)));
        assert!(!can_split(&a, &who("C:\\other.lnk", Some("c:\\A.EXE"), 1)));
        // Both must be running.
        assert!(!can_split(&a, &who("C:\\b.lnk", Some("C:\\b.exe"), 0)));
        assert!(!can_split(&who("C:\\a.lnk", None, 0), &b));
        // Stacks, the trash, the Start button: never.
        assert!(!can_split(&a, &Who { stack: true, ..b }));
        assert!(!can_split(&a, &Who { app: false, ..b }));
        assert!(!can_split(&a, &Who { path: "", ..b }));
        // Two apps without a known program are told apart by path.
        assert!(can_split(&who("x", None, 1), &who("y", None, 1)));
    }

    #[test]
    fn full_screen_is_not_maximized() {
        let mon = r(0, 0, 1920, 1080);
        assert!(full_screen(r(0, 0, 1920, 1080), mon, false, false));
        assert!(full_screen(r(-8, -8, 1928, 1088), mon, false, false));
        assert!(!full_screen(r(-8, -8, 1928, 1088), mon, true, false), "maximized: restored, then split");
        assert!(!full_screen(r(0, 0, 1920, 1080), mon, false, true), "minimized");
        assert!(!full_screen(r(0, 0, 960, 1080), mon, false, false));
    }

    #[test]
    fn halves_share_their_edge_on_the_work_area() {
        // Top bar 32 px, dock reserved 80 px at the bottom.
        let work = r(0, 32, 1920, 1000);
        assert_eq!(split_rects(work, Side::Left, Ratio::Half), (r(0, 32, 960, 1000), r(960, 32, 1920, 1000)));
        assert_eq!(split_rects(work, Side::Right, Ratio::Half), (r(960, 32, 1920, 1000), r(0, 32, 960, 1000)));
        // Odd width on a monitor left of the main one: no gap, no overlap.
        let work = r(-1001, 0, 0, 777);
        let (a, b) = split_rects(work, Side::Left, Ratio::Half);
        assert_eq!((a.left, a.right, b.left, b.right), (-1001, -501, -501, 0));
        assert_eq!((a.right - a.left) + (b.right - b.left), 1001);
    }

    #[test]
    fn shift_gives_the_dragged_app_two_thirds() {
        let work = r(0, 0, 1800, 1000);
        let (a, b) = split_rects(work, Side::Left, Ratio::TwoThirds);
        assert_eq!((a, b), (r(0, 0, 1200, 1000), r(1200, 0, 1800, 1000)));
        let (a, b) = split_rects(work, Side::Right, Ratio::TwoThirds);
        assert_eq!((a, b), (r(600, 0, 1800, 1000), r(0, 0, 600, 1000)));
        // Ultra-wide (3440 x 1440, aspect 2.4): still halves.
        let uw = r(0, 0, 3440, 1400);
        let (a, b) = split_rects(uw, Side::Left, Ratio::Half);
        assert_eq!((a.right - a.left, b.right - b.left), (1720, 1720));
    }

    #[test]
    fn hint_margins_follow_dpi() {
        let work = r(100, 40, 2020, 1040);
        let (a, b) = hint_rects(work, Side::Left, Ratio::Half, 96);
        // Local to the overlay, 12 px outside, 6 px each side of the cut.
        assert_eq!(a, r(12, 12, 954, 988));
        assert_eq!(b, r(966, 12, 1908, 988));
        let (a, b) = hint_rects(work, Side::Left, Ratio::Half, 144);
        assert_eq!(a, r(18, 18, 951, 982), "150 %");
        assert_eq!(b, r(969, 18, 1902, 982));
        // The dragged app on the right with two thirds.
        let (a, b) = hint_rects(work, Side::Right, Ratio::TwoThirds, 96);
        assert_eq!((a.left, a.right, b.left, b.right), (646, 1908, 12, 634));
        // A tiny area never gets inside-out rects.
        let (a, _) = hint_rects(r(0, 0, 10, 10), Side::Left, Ratio::Half, 192);
        assert!(a.right >= a.left && a.bottom >= a.top);
    }

    #[test]
    fn handles_are_trusted_only_for_the_recorded_process() {
        use crate::apps::same_owner;
        assert!(same_owner(7, 1000, 7, Some(1000)));
        // Reused by another process (or the same id given to a new one).
        assert!(!same_owner(7, 1000, 9, Some(1000)));
        assert!(!same_owner(7, 1000, 7, Some(2000)), "same id, newer process");
        assert!(!same_owner(7, 1000, 7, None), "start time unreadable now");
        assert!(!same_owner(0, 0, 0, None), "nothing recorded");
        // The list could not read the start time: the id alone.
        assert!(same_owner(7, 0, 7, None));
    }

    #[test]
    fn newest_split_request_wins() {
        assert!(snap::split_current(3, 3));
        assert!(!snap::split_current(2, 3), "an older helper stops");
        assert!(snap::split_current(0, 3), "not a split helper (menu snap)");
    }

    fn guide() -> Guide {
        Guide { target: "B".into(), side: Side::Left, ratio: Ratio::Half, hwnd: 0xB, pid: 7, started: 99, mon: 1, work: r(0, 0, 1920, 1040), dpi: 96 }
    }

    #[test]
    fn hint_follows_window_monitor_and_work_area() {
        let g = guide();
        // Side / ratio changes keep the place (the halves slide).
        assert!(g.same_place(&Guide { side: Side::Right, ratio: Ratio::TwoThirds, ..g.clone() }));
        assert_ne!(g, Guide { side: Side::Right, ..g.clone() }, "redrawn");
        // Another window, process, monitor, work area or DPI: a new place
        // (and a drop guided by the old one does nothing).
        for other in [
            Guide { hwnd: 0xC, ..g.clone() },
            Guide { pid: 8, ..g.clone() },
            Guide { started: 100, ..g.clone() },
            Guide { mon: 2, ..g.clone() },
            Guide { work: r(0, 0, 1920, 1000), ..g.clone() },
            Guide { dpi: 144, ..g.clone() },
            Guide { target: "C".into(), ..g.clone() },
        ] {
            assert!(!g.same_place(&other), "{other:?}");
        }
    }

    #[test]
    fn split_activation_ignores_the_other_half_in_front() {
        assert_eq!(snap::split_front(0xB, 0xA, 0xB), 0xA);
        assert_eq!(snap::split_front(0x77, 0xA, 0xB), 0x77);
    }
}
