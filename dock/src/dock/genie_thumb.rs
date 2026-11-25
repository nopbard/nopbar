//! Genie minimize without capturing pixels (roadmap item 6).
//!
//! An older capture genie read the window back with
//! Windows.Graphics.Capture; on some drivers heavy screen capture went with
//! GPU hangs (it is gone), so the genie never reads pixels. Instead DWM draws the
//! live window into our overlay as thumbnails (DwmRegisterThumbnail): one
//! registration per horizontal strip (vertical for side docks), each with
//! rcSource = that strip of the window and rcDestination = where the strip
//! is at this frame. A thumbnail is a rectangle, so this is a stepped genie:
//! every strip is narrowed and moved as a whole (no per-pixel warp).
//!
//! Why this and not the alternatives:
//! - A Composition visual of another process's window exists only through
//!   Windows.Graphics.Capture (the path we leave) or undocumented DWM
//!   interfaces (not used).
//! - The OS minimize animation into our GETMINRECT ("Scale") stays the
//!   fallback and its own setting.
//!
//! Sequence (one at a time):
//! 1. click: STRIPS thumbnails registered and laid over the window exactly
//!    (all within START_BUDGET_MS, else the OS animation);
//! 2. the window's DWM transitions off (crash-safe, genie-pending as in
//!    genie_shared.rs), the overlay shown;
//! 3. once that is on screen (DwmFlush x2 on a short-lived thread) the
//!    window is minimized and the strips move: a ticker thread paced by
//!    DwmFlush posts one frame at a time (at most one queued), each frame
//!    updates every thumbnail (DwmUpdateThumbnailProperties);
//! 4. after GENIE_MS: thumbnails unregistered, overlay hidden, transitions
//!    back as they were.
//!
//! Nothing runs while idle: no hover work, no timers, no thread; the
//! overlay window just exists hidden. Thumbnails are held by `Registry`,
//! which unregisters them when dropped, so every exit path (finish, error,
//! watchdog, shutdown, the overlay destroyed) releases them.
//!
//! "None" (no animation) uses the same transitions handling: transitions
//! off, minimize, back on once the window is minimized. Several windows can
//! wait at once ("Minimize all"); genie-pending holds one record each.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmFlush, DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE, DWM_TNP_SOURCECLIENTAREAONLY,
    DWM_TNP_VISIBLE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::genie_shared::{bezier, disable_transitions, frame_bounds, os_minimize, restore_transitions, strip_rect, strip_source, Saved, EASE};
use super::Rect;
use crate::config::Position;

/// Thumbnail registrations (strips). Each costs one DWM call per frame.
pub(super) const STRIPS: usize = 16;
const GENIE_MS: u64 = 300;

/// GENIE_MS at the chosen animation speed (crate::effects).
fn genie_ms() -> u64 {
    crate::effects::ms(GENIE_MS as i64) as u64
}
/// The last part fades out (no icon-sized leftover).
const FADE_MS: u64 = 70;
/// Click -> overlay ready; longer and the OS animation plays instead.
const START_BUDGET_MS: u128 = 120;

const WM_GO: u32 = WM_APP + 1;
const WM_FRAME: u32 = WM_APP + 2;
/// Watchdog (one-shot): overlay up but WM_GO never came / frames stopped.
const SAFETY_TIMER: usize = 1;
/// Frames from a timer if the ticker thread cannot start.
const FRAME_TIMER: usize = 2;
/// "None": waits for the window to be minimized before putting its
/// transitions back.
const POLL_TIMER: usize = 3;
const SHOW_TIMEOUT_MS: u32 = 500;
const PLAY_SLACK_MS: u32 = 500;
const POLL_MS: u32 = 30;
/// "None": transitions back this long after the window is minimized (DWM
/// picks the animation up asynchronously), or after INSTANT_MAX_MS anyway.
const INSTANT_SETTLE_MS: u128 = 150;
const INSTANT_MAX_MS: u128 = 1500;

// ---- geometry (pure, tested) -------------------------------------------------

/// Maps px of the window's visible frame (DWMWA_EXTENDED_FRAME_BOUNDS,
/// relative to its top-left) to the thumbnail's source space: the whole
/// window rect (with the invisible resize borders) at the size DWM reports
/// (DwmQueryThumbnailSourceSize). The scale is 1 for a per-monitor aware
/// dock; it covers a source size reported at another DPI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SourceMap {
    ox: f32,
    oy: f32,
    sx: f32,
    sy: f32,
    /// Source size (the clamp).
    w: i32,
    h: i32,
}

pub(super) fn source_map(window: RECT, frame: RECT, src: (i32, i32)) -> Option<SourceMap> {
    let (ww, wh) = (window.right - window.left, window.bottom - window.top);
    if ww <= 0 || wh <= 0 || src.0 <= 0 || src.1 <= 0 {
        return None;
    }
    let sx = src.0 as f32 / ww as f32;
    let sy = src.1 as f32 / wh as f32;
    Some(SourceMap { ox: (frame.left - window.left) as f32 * sx, oy: (frame.top - window.top) as f32 * sy, sx, sy, w: src.0, h: src.1 })
}

impl SourceMap {
    /// Source rect of frame-relative `r`. Edges are rounded (not floored /
    /// ceiled) so neighbouring strips share an edge: no gap, no overlap.
    pub(super) fn rect(&self, r: Rect) -> RECT {
        let x = |v: f32| ((self.ox + v * self.sx).round() as i32).clamp(0, self.w);
        let y = |v: f32| ((self.oy + v * self.sy).round() as i32).clamp(0, self.h);
        RECT { left: x(r.x), top: y(r.y), right: x(r.x + r.w), bottom: y(r.y + r.h) }
    }
}

/// Screen rect `r` in overlay px (overlay at `origin`), rounded per edge,
/// at least 1 px each way, one px longer along the flow so strips moving
/// at slightly different speeds leave no seam.
pub(super) fn dest_rect(pos: Position, r: Rect, origin: (i32, i32)) -> RECT {
    let left = (r.x - origin.0 as f32).round() as i32;
    let top = (r.y - origin.1 as f32).round() as i32;
    let mut right = ((r.x + r.w - origin.0 as f32).round() as i32).max(left + 1);
    let mut bottom = ((r.y + r.h - origin.1 as f32).round() as i32).max(top + 1);
    if pos == Position::Bottom {
        bottom += 1;
    } else {
        right += 1;
    }
    RECT { left, top, right, bottom }
}

/// Thumbnail opacity at time `t` (0..1 of GENIE_MS): opaque, then a linear
/// fade over the last FADE_MS.
pub(super) fn opacity(t: f32) -> u8 {
    let start = 1.0 - FADE_MS as f32 / GENIE_MS as f32;
    let t = t.clamp(0.0, 1.0);
    if t <= start {
        255
    } else {
        (255.0 * (1.0 - (t - start) / (1.0 - start))).round().clamp(0.0, 255.0) as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Slice {
    pub src: RECT,
    pub dst: RECT,
}

/// Every strip's source and destination at time `t` (0..1, eased here).
/// `win`: the visible frame, `icon`: the dock icon (screen px).
pub(super) fn slices(pos: Position, t: f32, n: usize, win: Rect, icon: Rect, origin: (i32, i32), map: &SourceMap) -> Vec<Slice> {
    let p = bezier(t, EASE);
    (0..n)
        .map(|i| Slice {
            src: map.rect(strip_source(pos, i, n, win.w, win.h)),
            dst: dest_rect(pos, strip_rect(pos, p, i, n, win, icon), origin),
        })
        .collect()
}

/// Thumbnail handles that must be unregistered: all of them, exactly once,
/// whatever ends the genie (release() or drop).
pub(super) struct Registry<F: FnMut(isize)> {
    ids: Vec<isize>,
    unregister: F,
}

impl<F: FnMut(isize)> Registry<F> {
    pub(super) fn new(unregister: F) -> Self {
        Self { ids: Vec::new(), unregister }
    }

    pub(super) fn add(&mut self, id: isize) {
        self.ids.push(id);
    }

    pub(super) fn ids(&self) -> &[isize] {
        &self.ids
    }

    pub(super) fn release(&mut self) {
        for id in self.ids.drain(..) {
            (self.unregister)(id);
        }
    }
}

impl<F: FnMut(isize)> Drop for Registry<F> {
    fn drop(&mut self) {
        self.release();
    }
}

fn dwm_unregister(id: isize) {
    if let Err(e) = unsafe { DwmUnregisterThumbnail(id) } {
        crate::log!("genie: unregistering a thumbnail failed: {e:?}");
    }
}

type Thumbs = Registry<fn(isize)>;

/// The target's transitions, put back when dropped (any exit path).
struct Restore(Option<Saved>);

impl Drop for Restore {
    fn drop(&mut self) {
        if let Some(s) = self.0.take() {
            restore_transitions(&s);
        }
    }
}

// ---- state -------------------------------------------------------------------------

struct Play {
    target: HWND,
    pos: Position,
    win: Rect,
    icon: Rect,
    origin: (i32, i32),
    map: SourceMap,
    clicked: Instant,
    started: Option<Instant>,
    /// Stops the ticker thread.
    stop: Arc<AtomicBool>,
    /// A WM_FRAME is queued (never more than one).
    pending: Arc<AtomicBool>,
    frames: u32,
    update: Duration,
    // Dropped in this order: thumbnails away, then transitions back.
    thumbs: Thumbs,
    restore: Restore,
}

impl Drop for Play {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Play {
    /// Sets every thumbnail for time `t`. Fails when DWM refuses (window
    /// gone, DWM restarted: the handles are dead).
    fn apply(&mut self, t: f32) -> windows::core::Result<()> {
        let t0 = Instant::now();
        let alpha = opacity(t);
        let all = slices(self.pos, t, self.thumbs.ids().len(), self.win, self.icon, self.origin, &self.map);
        for (id, s) in self.thumbs.ids().iter().zip(all) {
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_OPACITY | DWM_TNP_VISIBLE | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: s.dst,
                rcSource: s.src,
                opacity: alpha,
                fVisible: true.into(),
                fSourceClientAreaOnly: false.into(),
            };
            unsafe { DwmUpdateThumbnailProperties(*id, &props)? };
        }
        self.frames += 1;
        self.update += t0.elapsed();
        Ok(())
    }
}

/// "None": transitions back once the window has been minimized for
/// INSTANT_SETTLE_MS, it is gone, or INSTANT_MAX_MS passed anyway.
pub(super) fn settle_due(iconic_for: Option<Duration>, waited: Duration, gone: bool) -> bool {
    gone || iconic_for.is_some_and(|d| d.as_millis() >= INSTANT_SETTLE_MS) || waited.as_millis() >= INSTANT_MAX_MS
}

/// "None": waiting for the window to be minimized.
struct NoAnimWait {
    target: HWND,
    started: Instant,
    iconic_at: Option<Instant>,
    _restore: Restore,
}

enum Phase {
    Idle,
    /// Overlay shown, waiting for it to reach the screen.
    Showing(Play),
    Playing(Play),
}

impl Phase {
    fn target(&self) -> Option<HWND> {
        match self {
            Phase::Idle => None,
            Phase::Showing(p) | Phase::Playing(p) => Some(p.target),
        }
    }
}

struct State {
    overlay: HWND,
    phase: Phase,
    /// "None" minimizes waiting to put their window's transitions back
    /// (independent of the genie; several at once, e.g. "Minimize all").
    quiet: Vec<NoAnimWait>,
    /// Generation of the current genie; WM_GO / WM_FRAME carry it.
    gen: usize,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

unsafe extern "system" fn overlay_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Placed in physical px by us; never resized for a DPI change.
        WM_DPICHANGED => LRESULT(0),
        WM_GO => {
            if with(|s| s.go(wp.0)).is_none() {
                let _ = PostMessageW(Some(h), msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_FRAME => {
            // Busy (re-entered): try again, or the ticker stays paused on
            // its pending flag.
            if with(|s| s.frame(wp.0)).is_none() {
                let _ = PostMessageW(Some(h), msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == FRAME_TIMER => {
            let _ = with(|s| s.frame(s.gen));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == SAFETY_TIMER => {
            let _ = KillTimer(Some(h), SAFETY_TIMER);
            if with(|s| s.watchdog()).is_none() {
                SetTimer(Some(h), SAFETY_TIMER, 20, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == POLL_TIMER => {
            let _ = with(|s| s.poll());
            LRESULT(0)
        }
        WM_DESTROY => {
            // Thumbnails die with their destination anyway; unregister and
            // put the target back explicitly.
            let _ = with(|s| s.abort());
            LRESULT(0)
        }
        WM_NCDESTROY => {
            STATE.with(|s| {
                if let Ok(mut s) = s.try_borrow_mut() {
                    if s.as_ref().is_some_and(|st| st.overlay == h) {
                        *s = None;
                    }
                }
            });
            DefWindowProcW(h, msg, wp, lp)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

/// Creates the (hidden) overlay once.
fn ensure() -> bool {
    if STATE.with(|s| s.try_borrow().map(|s| s.is_some()).unwrap_or(true)) {
        return true;
    }
    match unsafe { create_overlay() } {
        Ok(overlay) => {
            STATE.with(|s| *s.borrow_mut() = Some(State { overlay, phase: Phase::Idle, quiet: Vec::new(), gen: 0 }));
            true
        }
        Err(e) => {
            crate::log!("genie: thumbnail overlay unavailable: {e:?}");
            false
        }
    }
}

unsafe fn create_overlay() -> windows::core::Result<HWND> {
    let hinst = GetModuleHandleW(None)?;
    let class = w!("NopBarGenieThumb");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(overlay_proc),
        hInstance: hinst.into(),
        lpszClassName: class,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    // Click-through, never active, no taskbar button, above windows. No
    // redirection surface: only the thumbnails are drawn.
    let overlay = CreateWindowExW(
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
    if let Err(e) = SetLayeredWindowAttributes(overlay, COLORREF(0), 255, LWA_ALPHA) {
        // Not kept anywhere yet: destroy it, or every attempt leaks one.
        let _ = DestroyWindow(overlay);
        return Err(e);
    }
    Ok(overlay)
}

/// Frames paced by DWM's composition (DwmFlush), only while a genie plays.
fn ticker(overlay: isize, gen: usize, stop: Arc<AtomicBool>, pending: Arc<AtomicBool>, limit: Duration) {
    let t0 = Instant::now();
    while !stop.load(Ordering::SeqCst) && t0.elapsed() < limit {
        let f0 = Instant::now();
        // DwmFlush returns at once when nothing is composed: never spin.
        if unsafe { DwmFlush() }.is_err() || f0.elapsed() < Duration::from_millis(2) {
            std::thread::sleep(Duration::from_millis(8));
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if !pending.swap(true, Ordering::SeqCst) && unsafe { PostMessageW(Some(HWND(overlay as *mut _)), WM_FRAME, WPARAM(gen), LPARAM(0)) }.is_err() {
            // Overlay gone: the watchdog / shutdown clean up.
            break;
        }
    }
}

impl State {
    unsafe fn start(&mut self, target: HWND, pos: Position, clicked: Instant) -> bool {
        if let Some(t) = self.phase.target() {
            // Another click on the same window: we have it already.
            return t == target;
        }
        if !IsWindowVisible(target).as_bool() || IsIconic(target).as_bool() {
            return false;
        }
        let (Some(fb), Some(ir)) = (frame_bounds(target), super::minrect_for(target)) else { return false };
        let mut wr = RECT::default();
        if GetWindowRect(target, &mut wr).is_err() {
            return false;
        }
        let mut thumbs: Thumbs = Registry::new(dwm_unregister);
        for _ in 0..STRIPS {
            match DwmRegisterThumbnail(self.overlay, target) {
                Ok(id) => thumbs.add(id),
                Err(e) => {
                    crate::log!("genie: cannot register a thumbnail ({e:?}): OS animation");
                    return false;
                }
            }
        }
        let map = match DwmQueryThumbnailSourceSize(thumbs.ids()[0]) {
            Ok(sz) => source_map(wr, fb, (sz.cx, sz.cy)),
            Err(e) => {
                crate::log!("genie: no thumbnail source size ({e:?}): OS animation");
                None
            }
        };
        let Some(map) = map else { return false };
        let win = Rect { x: fb.left as f32, y: fb.top as f32, w: (fb.right - fb.left) as f32, h: (fb.bottom - fb.top) as f32 };
        let icon = Rect { x: ir.left as f32, y: ir.top as f32, w: (ir.right - ir.left) as f32, h: (ir.bottom - ir.top) as f32 };
        // The overlay covers the window and the icon (strips stay between).
        let (ox, oy) = (fb.left.min(ir.left), fb.top.min(ir.top));
        let (ox1, oy1) = (fb.right.max(ir.right), fb.bottom.max(ir.bottom));
        let mut play = Play {
            target,
            pos,
            win,
            icon,
            origin: (ox, oy),
            map,
            clicked,
            started: None,
            stop: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(AtomicBool::new(false)),
            frames: 0,
            update: Duration::ZERO,
            thumbs,
            restore: Restore(None),
        };
        // Exactly over the window before anything shows.
        if let Err(e) = play.apply(0.0) {
            crate::log!("genie: thumbnail setup failed ({e:?}): OS animation");
            return false;
        }
        if clicked.elapsed().as_millis() > START_BUDGET_MS {
            crate::log!("genie: setup took {} ms, over the budget: OS animation", clicked.elapsed().as_millis());
            return false;
        }
        // Recorded on disk first, then changed (see genie_shared::recover_pending).
        let Some(saved) = disable_transitions(target) else { return false };
        play.restore = Restore(saved);
        // Topmost, just under the dock it flies to (the one on the
        // window's display): the strips go in behind its icons.
        let dock = HWND(super::views::for_window(target) as *mut _);
        let after = if dock.is_invalid() { HWND_TOPMOST } else { dock };
        if let Err(e) = SetWindowPos(self.overlay, Some(after), ox, oy, ox1 - ox, oy1 - oy, SWP_NOACTIVATE | SWP_SHOWWINDOW) {
            crate::log!("genie: cannot show the overlay ({e:?}): OS animation");
            let _ = ShowWindow(self.overlay, SW_HIDE);
            return false;
        }
        // The watchdog first: without it nothing would end a lost WM_GO.
        if SetTimer(Some(self.overlay), SAFETY_TIMER, SHOW_TIMEOUT_MS, None) == 0 {
            crate::log!("genie: no watchdog timer: OS animation");
            let _ = ShowWindow(self.overlay, SW_HIDE);
            return false;
        }
        self.gen = self.gen.wrapping_add(1);
        let (gen, overlay) = (self.gen, self.overlay.0 as isize);
        // On screen = DWM composed a frame or two with the overlay.
        let waiter = std::thread::Builder::new().name("genie-show".into()).spawn(move || {
            for _ in 0..2 {
                let _ = DwmFlush();
            }
            // (If this is lost, the watchdog cleans up.)
            let _ = PostMessageW(Some(HWND(overlay as *mut _)), WM_GO, WPARAM(gen), LPARAM(0));
        });
        if waiter.is_err() {
            let _ = PostMessageW(Some(self.overlay), WM_GO, WPARAM(gen), LPARAM(0));
        }
        self.phase = Phase::Showing(play);
        true
    }

    /// The overlay is on screen: minimize the window, start the frames.
    fn go(&mut self, gen: usize) {
        if gen != self.gen {
            return;
        }
        let Phase::Showing(mut play) = std::mem::replace(&mut self.phase, Phase::Idle) else { return };
        let t = play.target;
        let still = unsafe { IsWindow(Some(t)).as_bool() && IsWindowVisible(t).as_bool() && !IsIconic(t).as_bool() };
        if !still {
            crate::log!("genie: the window changed before the genie started; dropped");
            self.finish(play);
            return;
        }
        // Frames and the watchdog first: if either cannot run, clean up and
        // let Windows minimize it (the request is still ours to carry out).
        play.started = Some(Instant::now());
        let (overlay, stop, pending) = (self.overlay.0 as isize, play.stop.clone(), play.pending.clone());
        let limit = Duration::from_millis(genie_ms() + PLAY_SLACK_MS as u64);
        let spawned = std::thread::Builder::new().name("genie-tick".into()).spawn(move || ticker(overlay, gen, stop, pending, limit));
        let ready = unsafe {
            (spawned.is_ok() || SetTimer(Some(self.overlay), FRAME_TIMER, 10, None) != 0)
                && SetTimer(Some(self.overlay), SAFETY_TIMER, genie_ms() as u32 + PLAY_SLACK_MS, None) != 0
        };
        if !ready {
            crate::log!("genie: no frame source / watchdog timer: OS animation");
            self.finish(play);
            os_minimize(t);
            return;
        }
        crate::log!("genie: thumbnails ({} strips) | click -> on screen {} ms", STRIPS, play.clicked.elapsed().as_millis());
        os_minimize(t);
        self.phase = Phase::Playing(play);
    }

    fn frame(&mut self, gen: usize) {
        if gen != self.gen {
            return;
        }
        let Phase::Playing(play) = &mut self.phase else { return };
        play.pending.store(false, Ordering::SeqCst);
        let Some(started) = play.started else { return };
        let t = started.elapsed().as_secs_f32() * 1000.0 / genie_ms() as f32;
        let res = if t >= 1.0 { None } else { Some(play.apply(t)) };
        match res {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                crate::log!("genie: thumbnail update failed ({e:?}); stopping");
                self.end();
            }
            None => {
                let avg = play.update.as_micros() / play.frames.max(1) as u128;
                crate::log!("genie: done, {} frames, {} us per frame in DWM calls", play.frames, avg);
                self.end();
            }
        }
    }

    /// A completion never came (lost message, DWM trouble).
    fn watchdog(&mut self) {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Showing(play) => {
                // Not minimized yet: clean up (transitions back), then let
                // Windows minimize it, so the click is not lost.
                let t = play.target;
                crate::log!("genie: overlay never reached the screen; OS animation");
                self.finish(play);
                if unsafe { IsWindow(Some(t)).as_bool() && IsWindowVisible(t).as_bool() && !IsIconic(t).as_bool() } {
                    os_minimize(t);
                }
            }
            Phase::Playing(play) => {
                // Minimize already asked for: only clean up.
                crate::log!("genie: no progress in time; cleaning up");
                self.finish(play);
            }
            Phase::Idle => {}
        }
    }

    fn end(&mut self) {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Showing(p) | Phase::Playing(p) => self.finish(p),
            Phase::Idle => {}
        }
    }

    /// Overlay hidden, then (dropping `play`) the ticker stopped, the
    /// thumbnails unregistered, the window's transitions put back.
    fn finish(&mut self, play: Play) {
        self.phase = Phase::Idle;
        unsafe {
            let _ = KillTimer(Some(self.overlay), SAFETY_TIMER);
            let _ = KillTimer(Some(self.overlay), FRAME_TIMER);
            let _ = ShowWindow(self.overlay, SW_HIDE);
        }
        drop(play);
    }

    /// "None": transitions off, minimize, back on once minimized. Several
    /// windows may wait at once (each has its own genie-pending record).
    unsafe fn no_anim(&mut self, target: HWND) -> bool {
        if self.quiet.iter().any(|m| m.target == target) || self.phase.target() == Some(target) {
            return true;
        }
        if IsIconic(target).as_bool() {
            return false;
        }
        match disable_transitions(target) {
            None => false,
            Some(None) => {
                // Off already: nothing to put back.
                os_minimize(target);
                true
            }
            Some(saved) => {
                let wait = NoAnimWait { target, started: Instant::now(), iconic_at: None, _restore: Restore(saved) };
                // Without the poll timer nothing would put the transitions
                // back: refuse (dropping `wait` restores them now).
                if self.quiet.is_empty() && SetTimer(Some(self.overlay), POLL_TIMER, POLL_MS, None) == 0 {
                    crate::log!("genie: no timer for a minimize without animation: OS animation");
                    drop(wait);
                    return false;
                }
                os_minimize(target);
                self.quiet.push(wait);
                true
            }
        }
    }

    fn poll(&mut self) {
        self.quiet.retain_mut(|m| {
            let t = m.target;
            let gone = unsafe { !IsWindow(Some(t)).as_bool() };
            if unsafe { IsIconic(t).as_bool() } {
                m.iconic_at.get_or_insert_with(Instant::now);
            }
            // Dropped (transitions back) once due.
            !settle_due(m.iconic_at.map(|at| at.elapsed()), m.started.elapsed(), gone)
        });
        if self.quiet.is_empty() {
            unsafe {
                let _ = KillTimer(Some(self.overlay), POLL_TIMER);
            }
        }
    }

    /// Quitting / overlay destroyed: put everything back now.
    fn abort(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.overlay), POLL_TIMER);
        }
        // Dropped: transitions back.
        self.quiet.clear();
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Showing(p) | Phase::Playing(p) => self.finish(p),
            Phase::Idle => {}
        }
    }
}

// ---- API for the dock ---------------------------------------------------------------

/// Minimize `hwnd` with the thumbnail genie into its icon on a dock at
/// `pos`. True if the genie took it (or already has that window); false:
/// the caller minimizes as usual (OS animation).
pub(super) fn minimize(hwnd: HWND, pos: Position) -> bool {
    let clicked = Instant::now();
    if hwnd.is_invalid() || !ensure() {
        return false;
    }
    with(|s| unsafe { s.start(hwnd, pos, clicked) }).unwrap_or(false)
}

/// Minimize `hwnd` without any animation. False: minimize as usual.
pub(super) fn minimize_instant(hwnd: HWND) -> bool {
    if hwnd.is_invalid() || !ensure() {
        return false;
    }
    with(|s| unsafe { s.no_anim(hwnd) }).unwrap_or(false)
}

/// Our overlay (never a fullscreen app).
pub(super) fn is_overlay(h: HWND) -> bool {
    !h.is_invalid() && with(|s| s.overlay == h).unwrap_or(false)
}

/// Quitting: no window may keep its animations off.
pub(super) fn shutdown() {
    with(|s| s.abort());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const WIN: Rect = Rect { x: 300.0, y: 200.0, w: 900.0, h: 600.0 };

    fn icon_for(pos: Position) -> Rect {
        match pos {
            Position::Bottom => Rect { x: 1230.0, y: 1440.0, w: 100.0, h: 100.0 },
            Position::Left => Rect { x: 10.0, y: 700.0, w: 100.0, h: 100.0 },
            Position::Right => Rect { x: 2450.0, y: 700.0, w: 100.0, h: 100.0 },
        }
    }

    /// A window with 8 px invisible borders left/right/bottom (Windows 11).
    fn window_rect() -> RECT {
        RECT { left: 292, top: 200, right: 1208, bottom: 808 }
    }

    fn frame_rect() -> RECT {
        RECT { left: 300, top: 200, right: 1200, bottom: 800 }
    }

    fn map_1x() -> SourceMap {
        source_map(window_rect(), frame_rect(), (916, 608)).unwrap()
    }

    fn overlay_origin(pos: Position) -> (i32, i32) {
        let ic = icon_for(pos);
        ((WIN.x.min(ic.x)) as i32, (WIN.y.min(ic.y)) as i32)
    }

    #[test]
    fn source_skips_the_invisible_borders() {
        let m = map_1x();
        let all = m.rect(Rect { x: 0.0, y: 0.0, w: 900.0, h: 600.0 });
        assert_eq!(all, RECT { left: 8, top: 0, right: 908, bottom: 600 });
    }

    #[test]
    fn source_scales_with_a_different_dpi() {
        // Source size reported at 96 dpi for a window laid out at 150 %.
        let wr = RECT { left: 0, top: 0, right: 1500, bottom: 900 };
        let fb = RECT { left: 12, top: 0, right: 1488, bottom: 888 };
        let m = source_map(wr, fb, (1000, 600)).unwrap();
        let all = m.rect(Rect { x: 0.0, y: 0.0, w: 1476.0, h: 888.0 });
        assert_eq!(all, RECT { left: 8, top: 0, right: 992, bottom: 592 });
        assert!(source_map(wr, fb, (0, 600)).is_none());
        assert!(source_map(RECT::default(), fb, (10, 10)).is_none());
    }

    #[test]
    fn source_strips_tile_the_frame_exactly() {
        let m = map_1x();
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            let mut rects: Vec<RECT> = (0..STRIPS).map(|i| m.rect(strip_source(pos, i, STRIPS, WIN.w, WIN.h))).collect();
            let area: i32 = rects.iter().map(|r| (r.right - r.left) * (r.bottom - r.top)).sum();
            assert_eq!(area, 900 * 600, "{pos:?}: no gaps, no overlaps");
            // Neighbours share their edge.
            if pos == Position::Bottom {
                rects.sort_by_key(|r| r.top);
                assert!(rects.windows(2).all(|w| w[0].bottom == w[1].top));
            } else {
                rects.sort_by_key(|r| r.left);
                assert!(rects.windows(2).all(|w| w[0].right == w[1].left));
            }
        }
    }

    #[test]
    fn starts_exactly_over_the_window() {
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            let o = overlay_origin(pos);
            for (i, s) in slices(pos, 0.0, STRIPS, WIN, icon_for(pos), o, &map_1x()).iter().enumerate() {
                let src = strip_source(pos, i, STRIPS, WIN.w, WIN.h);
                // Same size as its source (1:1), at its place in the window.
                let (sw, sh) = (s.src.right - s.src.left, s.src.bottom - s.src.top);
                let (dw, dh) = (s.dst.right - s.dst.left, s.dst.bottom - s.dst.top);
                if pos == Position::Bottom {
                    assert!(sw == dw && (dh - sh - 1).abs() <= 1, "{pos:?} {i}: {s:?}");
                } else {
                    assert!(sh == dh && (dw - sw - 1).abs() <= 1, "{pos:?} {i}: {s:?}");
                }
                assert!((s.dst.left + o.0 - (WIN.x + src.x).round() as i32).abs() <= 1, "{pos:?} {i}");
                assert!((s.dst.top + o.1 - (WIN.y + src.y).round() as i32).abs() <= 1, "{pos:?} {i}");
            }
        }
    }

    #[test]
    fn ends_inside_the_icon() {
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            let ic = icon_for(pos);
            let o = overlay_origin(pos);
            for s in slices(pos, 1.0, STRIPS, WIN, ic, o, &map_1x()) {
                let d = s.dst;
                assert!(d.left + o.0 >= ic.x as i32 - 1 && d.right + o.0 <= (ic.x + ic.w) as i32 + 2, "{pos:?}: {d:?}");
                assert!(d.top + o.1 >= ic.y as i32 - 1 && d.bottom + o.1 <= (ic.y + ic.h) as i32 + 2, "{pos:?}: {d:?}");
                assert!(d.right > d.left && d.bottom > d.top, "never empty");
            }
        }
    }

    #[test]
    fn dock_side_pinches_first_and_everything_moves_toward_the_icon() {
        let pos = Position::Bottom;
        let ic = icon_for(pos);
        let o = overlay_origin(pos);
        let mid = slices(pos, 0.35, STRIPS, WIN, ic, o, &map_1x());
        let w = |r: RECT| r.right - r.left;
        assert!(w(mid[STRIPS - 1].dst) < w(mid[0].dst), "dock side narrower");
        // Monotonic over time: each strip's distance to where it ends (in
        // the icon) never grows.
        let end = slices(pos, 1.0, STRIPS, WIN, ic, o, &map_1x());
        for i in 0..STRIPS {
            let centre = ((end[i].dst.left + end[i].dst.right) / 2, (end[i].dst.top + end[i].dst.bottom) / 2);
            let mut last = i32::MAX - 2;
            for k in 0..=20 {
                let s = slices(pos, k as f32 / 20.0, STRIPS, WIN, ic, o, &map_1x())[i];
                let c = ((s.dst.left + s.dst.right) / 2, (s.dst.top + s.dst.bottom) / 2);
                let d = (c.0 - centre.0).abs() + (c.1 - centre.1).abs();
                assert!(d <= last + 2, "strip {i} at {k}: {d} > {last}");
                last = d;
            }
        }
    }

    #[test]
    fn source_rects_stay_fixed_over_time() {
        let pos = Position::Right;
        let a = slices(pos, 0.1, STRIPS, WIN, icon_for(pos), overlay_origin(pos), &map_1x());
        let b = slices(pos, 0.9, STRIPS, WIN, icon_for(pos), overlay_origin(pos), &map_1x());
        assert!(a.iter().zip(&b).all(|(x, y)| x.src == y.src));
    }

    #[test]
    fn dest_rect_rounds_and_overlaps_along_the_flow() {
        let r = Rect { x: 10.4, y: 20.6, w: 0.2, h: 30.0 };
        let b = dest_rect(Position::Bottom, r, (0, 0));
        assert_eq!(b, RECT { left: 10, top: 21, right: 11, bottom: 52 });
        let s = dest_rect(Position::Left, r, (5, 5));
        assert_eq!(s, RECT { left: 5, top: 16, right: 7, bottom: 46 });
    }

    #[test]
    fn eased_and_faded() {
        assert_eq!(opacity(0.0), 255);
        assert_eq!(opacity(0.5), 255);
        assert_eq!(opacity(1.0 - FADE_MS as f32 / GENIE_MS as f32), 255);
        assert_eq!(opacity(1.0), 0);
        assert_eq!(opacity(2.0), 0);
        let mut last = 255;
        for k in 0..=100 {
            let a = opacity(k as f32 / 100.0);
            assert!(a <= last);
            last = a;
        }
        // Slow start: little movement in the first tenth.
        let pos = Position::Bottom;
        let o = overlay_origin(pos);
        let s0 = slices(pos, 0.0, STRIPS, WIN, icon_for(pos), o, &map_1x())[0].dst;
        let s1 = slices(pos, 0.1, STRIPS, WIN, icon_for(pos), o, &map_1x())[0].dst;
        assert!((s1.top - s0.top).abs() < 20, "{s0:?} -> {s1:?}");
    }

    #[test]
    fn no_anim_settles_after_minimize_or_gives_up() {
        let ms = Duration::from_millis;
        assert!(!settle_due(None, ms(100), false), "not minimized yet");
        assert!(!settle_due(Some(ms(50)), ms(100), false), "just minimized");
        assert!(settle_due(Some(ms(INSTANT_SETTLE_MS as u64)), ms(200), false));
        assert!(settle_due(None, ms(10), true), "window gone");
        assert!(settle_due(None, ms(INSTANT_MAX_MS as u64), false), "never minimized: give up");
    }

    #[test]
    fn registry_unregisters_everything_once() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        {
            let log = seen.clone();
            let mut r = Registry::new(move |id| log.borrow_mut().push(id));
            for id in 1..=STRIPS as isize {
                r.add(id);
            }
            r.release();
            assert_eq!(seen.borrow().len(), STRIPS);
            assert!(r.ids().is_empty());
            r.add(99);
            // Dropped here: only what is left goes.
        }
        let mut v = seen.borrow().clone();
        assert_eq!(v.len(), STRIPS + 1);
        v.sort();
        v.dedup();
        assert_eq!(v.len(), STRIPS + 1, "each exactly once");
    }

    #[test]
    fn registry_releases_on_an_early_exit() {
        let count = Rc::new(RefCell::new(0));
        let failing_setup = |n: usize| -> Option<()> {
            let c = count.clone();
            let mut r = Registry::new(move |_| *c.borrow_mut() += 1);
            for id in 0..STRIPS as isize {
                if id as usize == n {
                    return None; // registration failed half way
                }
                r.add(id);
            }
            Some(())
        };
        assert!(failing_setup(5).is_none());
        assert_eq!(*count.borrow(), 5);
    }
}
