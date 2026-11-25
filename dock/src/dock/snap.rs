//! Moving windows for the dock: a window preview's "Move to the next
//! display", gathering an app's windows here and Split View.
//!
//! All math is in physical pixels (the process is per-monitor DPI aware
//! v2, so GetWindowRect, DWM frame bounds and monitor work areas agree).
//! Targets are for the window's *visible* edges: the invisible resize
//! borders Windows 10/11 put around most windows (the gap between
//! GetWindowRect and DWMWA_EXTENDED_FRAME_BOUNDS) are added back, so
//! placed windows meet without gaps.

use super::*;

// ---- pure math ------------------------------------------------------------------

fn w(r: RECT) -> i32 {
    r.right - r.left
}
fn h(r: RECT) -> i32 {
    r.bottom - r.top
}

/// The same place on another monitor: the window's center keeps its
/// relative position, its size is kept (shrunk to fit), it stays inside.
pub(super) fn on_other_monitor(visible: RECT, from: RECT, to: RECT) -> RECT {
    let (vw, vh) = (w(visible).min(w(to)), h(visible).min(h(to)));
    let rel = |c: i32, a: i32, len: i32| if len > 0 { (c - a) as f64 / len as f64 } else { 0.5 };
    let fx = rel((visible.left + visible.right) / 2, from.left, w(from)).clamp(0.0, 1.0);
    let fy = rel((visible.top + visible.bottom) / 2, from.top, h(from)).clamp(0.0, 1.0);
    let cx = to.left + (fx * w(to) as f64).round() as i32;
    let cy = to.top + (fy * h(to) as f64).round() as i32;
    let left = (cx - vw / 2).clamp(to.left, to.right - vw);
    let top = (cy - vh / 2).clamp(to.top, to.bottom - vh);
    RECT { left, top, right: left + vw, bottom: top + vh }
}

/// Invisible border widths: GetWindowRect minus the visible frame
/// (left, top, right, bottom). Zero when DWM gave nothing sensible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Margins {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

pub(super) fn margins(win: RECT, frame: Option<RECT>) -> Margins {
    let Some(f) = frame else { return Margins::default() };
    let m = Margins { l: f.left - win.left, t: f.top - win.top, r: win.right - f.right, b: win.bottom - f.bottom };
    // A frame outside the window rect, or absurd borders: ignore it.
    let ok = |v: i32| (0..=64).contains(&v);
    if w(f) <= 0 || h(f) <= 0 || ![m.l, m.t, m.r, m.b].into_iter().all(ok) {
        return Margins::default();
    }
    m
}

/// Borders drawn at another DPI (they scale with the window's DPI).
pub(super) fn scale_margins(m: Margins, from_dpi: u32, to_dpi: u32) -> Margins {
    if from_dpi == 0 || to_dpi == 0 || from_dpi == to_dpi {
        return m;
    }
    let s = |v: i32| ((v as i64 * to_dpi as i64 + from_dpi as i64 / 2) / from_dpi as i64) as i32;
    Margins { l: s(m.l), t: s(m.t), r: s(m.r), b: s(m.b) }
}

/// The window rect that puts the visible edges on `visible`.
pub(super) fn outer(visible: RECT, m: Margins) -> RECT {
    RECT { left: visible.left - m.l, top: visible.top - m.t, right: visible.right + m.r, bottom: visible.bottom + m.b }
}

/// The monitor after `cur` in left-to-right, top-to-bottom order (wraps).
pub(super) fn next_monitor(rects: &[(isize, RECT)], cur: isize) -> Option<isize> {
    if rects.len() < 2 {
        return None;
    }
    let mut v: Vec<(isize, RECT)> = rects.to_vec();
    v.sort_by_key(|(_, r)| (r.left, r.top));
    let i = v.iter().position(|(m, _)| *m == cur)?;
    Some(v[(i + 1) % v.len()].0)
}

// ---- applying it ----------------------------------------------------------------

/// GetWindowRect and the DWM visible frame.
unsafe fn rects(hw: HWND) -> Option<(RECT, Option<RECT>)> {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    let mut win = RECT::default();
    GetWindowRect(hw, &mut win).ok()?;
    let mut f = RECT::default();
    let frame = DwmGetWindowAttribute(hw, DWMWA_EXTENDED_FRAME_BOUNDS, &mut f as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32).ok().map(|_| f);
    Some((win, frame))
}

/// The work area windows may use on monitor `m`: Windows' work area (it
/// already leaves out the dock's reserved edge) below our top bar, or
/// above it while it sits at the bottom.
pub(super) fn work_of(m: &crate::monitors::Monitor) -> RECT {
    let mut r = m.work;
    let bar = crate::topbar::space_for(m.rect);
    r.top = r.top.max(m.rect.top + bar.top);
    r.bottom = r.bottom.min(m.rect.bottom - bar.bottom).max(r.top);
    r
}

pub(super) fn monitor_of(hw: HWND) -> Option<crate::monitors::Monitor> {
    use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
    crate::monitors::info(unsafe { MonitorFromWindow(hw, MONITOR_DEFAULTTONEAREST) }.0 as isize)
}

/// Waits for a window's rect to settle: done once it has been the same
/// for `STABLE_POLLS` polls in a row (after it first changed, if a change
/// is expected). Pure: the caller polls and passes each reading.
pub(super) struct Settle {
    last: Option<RECT>,
    same: u32,
    /// The rect before our request; until it changes the request is not
    /// handled yet (when `want_change`).
    before: RECT,
    want_change: bool,
    changed: bool,
}

const STABLE_POLLS: u32 = 3;
const POLL_MS: u64 = 16;
/// Longest wait for one step (restore, move, maximize).
const STEP_MS: u64 = 600;

impl Settle {
    pub fn new(before: RECT, want_change: bool) -> Self {
        Self { last: None, same: 0, before, want_change, changed: false }
    }
    /// One reading; true when settled.
    pub fn poll(&mut self, r: RECT) -> bool {
        if r != self.before {
            self.changed = true;
        }
        if self.last == Some(r) {
            self.same += 1;
        } else {
            self.same = 0;
            self.last = Some(r);
        }
        (self.changed || !self.want_change) && self.same >= STABLE_POLLS
    }
}

/// Polls `hw` until `done` and its rect is stable, at most STEP_MS.
/// False if it timed out, the window went away or stopped responding.
/// (GetWindowRect / IsIconic / IsZoomed read state; they never wait on
/// the app.)
unsafe fn wait_for(hw: HWND, want_change: bool, done: impl Fn() -> bool) -> bool {
    let mut win = RECT::default();
    let _ = GetWindowRect(hw, &mut win);
    let mut s = Settle::new(win, want_change);
    let start = Instant::now();
    while start.elapsed().as_millis() < STEP_MS as u128 {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        if !ours(hw) {
            return false;
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(hw, &mut r);
        if s.poll(r) && done() {
            return true;
        }
    }
    false
}

/// Asks the window (asynchronously) to move so its visible edges sit on
/// `visible` (on a monitor with `to_dpi`), waits for it, and corrects
/// once more: a move to another DPI makes the app resize itself
/// (WM_DPICHANGED) and its invisible borders change with it.
unsafe fn place(hw: HWND, visible: RECT, from_dpi: u32, to_dpi: u32) -> bool {
    let Some((win, frame)) = rects(hw) else { return false };
    let m = scale_margins(margins(win, frame), from_dpi, to_dpi);
    let o = outer(visible, m);
    // ASYNCWINDOWPOS: the request is posted to the app's thread; we never
    // wait inside SetWindowPos for an app that may stop answering.
    let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
    if !ours(hw) {
        return false;
    }
    let _ = SetWindowPos(hw, None, o.left, o.top, w(o), h(o), flags);
    if !wait_for(hw, win != o, || true) {
        return false;
    }
    if let Some((win2, frame2)) = rects(hw) {
        let o2 = outer(visible, margins(win2, frame2));
        if win2 != o2 {
            if !ours(hw) {
                return false;
            }
            let _ = SetWindowPos(hw, None, o2.left, o2.top, w(o2), h(o2), flags);
            return wait_for(hw, true, || true);
        }
    }
    true
}

/// Restores a minimized / maximized window (asynchronously) and waits.
unsafe fn restore(hw: HWND) -> bool {
    for _ in 0..2 {
        if !(IsIconic(hw).as_bool() || IsZoomed(hw).as_bool()) {
            return true;
        }
        if !ours(hw) {
            return false;
        }
        let _ = ShowWindowAsync(hw, SW_RESTORE);
        let _ = wait_for(hw, true, || !IsIconic(hw).as_bool() && !IsZoomed(hw).as_bool());
    }
    !(IsIconic(hw).as_bool() || IsZoomed(hw).as_bool())
}

/// The move itself, on a helper thread: every call here either only
/// reads state or is asynchronous, and each wait is bounded. The window
/// keeps its place relative to the screen, maximized stays maximized.
unsafe fn run(hw: HWND) -> bool {
    if !ours(hw) {
        return false;
    }
    let was_zoomed = IsZoomed(hw).as_bool();
    // A maximized or minimized window ignores its rect: restore first.
    if !restore(hw) {
        crate::log!("snap: window {:#x} did not restore in time; left as is", hw.0 as isize);
        return false;
    }
    let Some(mon) = monitor_of(hw) else { return false };
    let Some((win, frame)) = rects(hw) else { return false };
    let visible = frame.filter(|_| margins(win, frame) != Margins::default()).unwrap_or(win);
    let list: Vec<crate::monitors::Monitor> = crate::monitors::all();
    let rects: Vec<(isize, RECT)> = list.iter().map(|m| (m.handle, m.rect)).collect();
    let Some(next) = next_monitor(&rects, mon.handle).and_then(|h| list.iter().find(|m| m.handle == h)) else { return false };
    let to = on_other_monitor(visible, work_of(&mon), work_of(next));
    crate::log!("snap: window {:#x} to the next monitor {:?}", hw.0 as isize, next.rect);
    let ok = place(hw, to, mon.dpi, next.dpi);
    if ok && was_zoomed && ours(hw) {
        let _ = ShowWindowAsync(hw, SW_MAXIMIZE);
        return wait_for(hw, false, || IsZoomed(hw).as_bool());
    }
    ok
}

/// What the user last asked to bring to the front (UI thread). A helper
/// that finishes later activates its window only if it is still this
/// (audit fix review: a slow restore must not take the front back).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Intent {
    pub hwnd: isize,
    pub pid: u32,
    /// The foreground window when it was asked for.
    pub fg: isize,
}

static INTENT: std::sync::Mutex<Option<Intent>> = std::sync::Mutex::new(None);
/// Windows with a restore helper running (one per window).
static RESTORING: std::sync::Mutex<Vec<isize>> = std::sync::Mutex::new(Vec::new());

pub(super) fn pid_of(hw: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hw, Some(&mut pid)) };
    pid
}

thread_local! {
    /// On a helper thread: the process the window belonged to when the
    /// user asked (0: not checked). Every step re-checks it, so a handle
    /// reused by another app's window meanwhile is never touched.
    static EXPECT_PID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Pure: may a helper still act on a window that is `alive`, `hung` or
/// not, now of process `pid_now`, asked for as process `expect` (0: any)?
pub(super) fn still_ours(alive: bool, hung: bool, pid_now: u32, expect: u32) -> bool {
    alive && !hung && pid_now != 0 && (expect == 0 || pid_now == expect)
}

/// Still the window the request was for, alive and answering.
unsafe fn ours(hw: HWND) -> bool {
    let alive = IsWindow(Some(hw)).as_bool();
    let current = split_current(SPLIT_MINE.with(|m| m.get()), SPLIT_GEN.load(Ordering::SeqCst));
    current && still_ours(alive, alive && IsHungAppWindow(hw).as_bool(), if alive { pid_of(hw) } else { 0 }, EXPECT_PID.with(|p| p.get()))
}

/// Records `hw` as the window the user wants in front now.
fn set_intent(hw: HWND) {
    set_intent_pid(hw, pid_of(hw));
}

fn set_intent_pid(hw: HWND, pid: u32) {
    let fg = unsafe { GetForegroundWindow() };
    if let Ok(mut i) = INTENT.lock() {
        *i = Some(Intent { hwnd: hw.0 as isize, pid, fg: fg.0 as isize });
    }
}

/// A helper finished with `done`: activate it? Only if it is still the
/// latest request, still the same process's window, and the user has not
/// switched to something else meanwhile (`own_fg`: one of our own windows
/// is in front, e.g. the menu that asked).
pub(super) fn still_wanted(latest: Option<Intent>, done: isize, pid_now: u32, fg_now: isize, own_fg: bool) -> bool {
    let Some(i) = latest else { return false };
    i.hwnd == done && i.pid == pid_now && pid_now != 0 && (fg_now == i.fg || fg_now == done || own_fg)
}

/// WM_SNAP_DONE: a helper finished with window `hw`.
pub(super) unsafe fn finish(hw: HWND) {
    let latest = INTENT.lock().ok().and_then(|i| *i);
    let fg = GetForegroundWindow();
    let own = pid_of(fg) == std::process::id();
    if still_wanted(latest, hw.0 as isize, pid_of(hw), fg.0 as isize, own) {
        activate(hw);
    }
}

/// Moves window `hw` of process `pid` (as the caller knew it: a preview
/// card) to the next display off the UI thread; when it is done the dock
/// window gets `done_msg` (wParam = the window) to bring it to the front.
/// Nothing is done to a window of another process that took over the
/// handle meanwhile. The dock never waits on the app: if the app stops
/// answering halfway, only the helper's bounded waits run out.
pub(super) fn to_next_monitor(hw: HWND, pid: u32, dock: HWND, done_msg: u32) {
    if pid == 0 || pid_of(hw) != pid {
        return;
    }
    set_intent_pid(hw, pid);
    let (h, d) = (hw.0 as isize, dock.0 as isize);
    let spawned = std::thread::Builder::new().name("snap".into()).spawn(move || unsafe {
        EXPECT_PID.with(|p| p.set(pid));
        let hw = HWND(h as *mut _);
        if !run(hw) {
            crate::log!("snap: window {h:#x} to the next monitor not completed (timed out, closed or not responding)");
        }
        let _ = PostMessageW(Some(HWND(d as *mut _)), done_msg, WPARAM(h as usize), LPARAM(0));
    });
    if let Err(e) = spawned {
        crate::log!("snap: no helper thread ({e})");
    }
}

/// One window of a split: the window, its process and that process's
/// creation time as the dock's window list recorded them (never read
/// afresh from the handle), where its visible edges go, and its app's name
/// for a notice.
#[derive(Clone, Debug)]
pub(super) struct SplitPart {
    pub hwnd: isize,
    pub pid: u32,
    pub started: u64,
    pub to: RECT,
    pub name: String,
}

impl SplitPart {
    /// The handle still belongs to the recorded process (same id, same
    /// creation time).
    pub fn same_owner(&self) -> bool {
        let pid_now = pid_of(HWND(self.hwnd as *mut _));
        crate::apps::same_owner(self.pid, self.started, pid_now, crate::apps::process_started(pid_now))
    }
}

/// The newest split request; a helper whose number is older stops before
/// its next step (newest wins).
static SPLIT_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// One split helper at a time: a new one waits for the old one (which
/// stops at its next step) before it touches any window.
static SPLIT_RUN: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// The windows of each finished request, for its WM_SPLIT_DONE: (request,
/// a, b). Only the newest request's windows are fronted.
static SPLIT_DONE: std::sync::Mutex<Option<(u64, SplitPart, SplitPart)>> = std::sync::Mutex::new(None);

thread_local! {
    /// On a split helper: its request number (0: not a split helper).
    /// ours() fails once a newer split was asked for.
    static SPLIT_MINE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Pure: may a split helper for request `mine` (0: not one) still act,
/// with `newest` the latest request?
pub(super) fn split_current(mine: u64, newest: u64) -> bool {
    mine == 0 || mine == newest
}

/// Split View (split.rs): puts `b` (the app dropped on) and then `a` (the
/// dragged app) on their halves of a monitor with `to_dpi`, on a helper
/// thread with the same bounded, posted steps as a snap (restore first,
/// frame compensation, the process re-checked before every step). Requests
/// run one at a time and the newest wins (an older helper stops before its
/// next step). When it is done the dock window gets `done_msg` (wParam =
/// the request) to bring both to the front, `a` focused. A window that did
/// not land (it stopped answering, closed, refused the size, or runs
/// elevated so UIPI drops our requests) is told next to the icon (`at`).
pub(super) fn split(a: SplitPart, b: SplitPart, to_dpi: u32, dock: HWND, done_msg: u32, at: Option<(POINT, crate::popup::Align)>) {
    if !a.same_owner() || !b.same_owner() {
        crate::log!("split: a window changed hands since the window list; nothing moved");
        return;
    }
    let gen = SPLIT_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    // The dragged app is what the user wants in front.
    set_intent_pid(HWND(a.hwnd as *mut _), a.pid);
    // Notices from the helper are shown by this (the UI) thread.
    crate::launch::prepare();
    let d = dock.0 as isize;
    let spawned = std::thread::Builder::new().name("split".into()).spawn(move || unsafe {
        let _one_at_a_time = SPLIT_RUN.lock().unwrap_or_else(|e| e.into_inner());
        SPLIT_MINE.with(|m| m.set(gen));
        let one = |p: &SplitPart| -> bool {
            EXPECT_PID.with(|e| e.set(p.pid));
            let hw = HWND(p.hwnd as *mut _);
            // The recorded owner (also its creation time) before each step.
            if !p.same_owner() || !ours(hw) || !restore(hw) || !p.same_owner() {
                return false;
            }
            let from = monitor_of(hw).map(|m| m.dpi).unwrap_or(to_dpi);
            place(hw, p.to, from, to_dpi)
        };
        let ok_b = one(&b);
        let ok_a = one(&a);
        let current = split_current(gen, SPLIT_GEN.load(Ordering::SeqCst));
        crate::log!("split #{gen}: {:#x} -> {:?} {ok_a}, {:#x} -> {:?} {ok_b}{}", a.hwnd, a.to, b.hwnd, b.to, if current { "" } else { " (superseded)" });
        if !current {
            // A newer split took over: it reports and fronts its own.
            return;
        }
        let fails: Vec<(String, u32)> = [(&a, ok_a), (&b, ok_b)].iter().filter(|(_, ok)| !ok).map(|(p, _)| (p.name.clone(), p.pid)).collect();
        if let Ok(mut s) = SPLIT_DONE.lock() {
            *s = Some((gen, a, b));
        }
        let _ = PostMessageW(Some(HWND(d as *mut _)), done_msg, WPARAM(gen as usize), LPARAM(0));
        for (name, pid) in fails {
            let why = if crate::apps::is_locked(pid) { "dock.split.elevated.why" } else { "dock.split.failed.why" };
            crate::launch::tell_later(crate::i18n::tf("dock.split.failed", &[("name", &name)]), crate::i18n::t(why).into(), Vec::new(), at);
        }
    });
    if let Err(e) = spawned {
        crate::log!("split: no helper thread ({e})");
    }
}

/// Pure: the front window counts as unchanged for the split's activation
/// when it is the other half (`b`) its restore brought forward.
pub(super) fn split_front(fg_now: isize, a: isize, b: isize) -> isize {
    if fg_now == b { a } else { fg_now }
}

/// Split request `gen` is done: both windows to the front, `a` (the
/// dragged app) last so it has the focus. Only the newest request, only
/// windows still owned by the processes recorded for it, and only if `a`
/// is still what the user asked for last (see finish).
pub(super) unsafe fn finish_split(gen: u64) {
    let Some((g, a, b)) = SPLIT_DONE.lock().ok().and_then(|s| s.clone()) else { return };
    if g != gen || !split_current(gen, SPLIT_GEN.load(Ordering::SeqCst)) || !a.same_owner() {
        return;
    }
    let (ha, hb) = (HWND(a.hwnd as *mut _), HWND(b.hwnd as *mut _));
    let latest = INTENT.lock().ok().and_then(|i| *i);
    let fg = GetForegroundWindow();
    let own = pid_of(fg) == std::process::id();
    let front = split_front(fg.0 as isize, a.hwnd, b.hwnd);
    if !still_wanted(latest, a.hwnd, a.pid, front, own) {
        return;
    }
    if b.same_owner() {
        activate(hb);
    }
    activate(ha);
}

/// Brings a window to the front without any call that waits on it: no
/// ShowWindow / BringWindowToTop, and no AttachThreadInput (sharing an
/// input queue with another app makes even "async" window calls wait on
/// it). If the foreground lock refuses, the window stays behind (Windows
/// flashes its taskbar button instead) rather than risking the dock.
pub(super) unsafe fn activate(hw: HWND) {
    if !IsWindow(Some(hw)).as_bool() || IsHungAppWindow(hw).as_bool() || IsIconic(hw).as_bool() {
        return;
    }
    if SetForegroundWindow(hw).as_bool() && GetForegroundWindow() == hw {
        return;
    }
    // Raised with a posted request (another input queue: SetWindowPos posts).
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
    let _ = SetWindowPos(hw, Some(HWND_TOP), 0, 0, 0, 0, flags);
    let _ = SetForegroundWindow(hw);
}
/// Where a window at `r` on work area `from` lands on work area `to`: the
/// same offset from the top-left corner, kept inside `to`.
pub(super) fn gather_spot(r: RECT, from: RECT, to: RECT) -> (i32, i32) {
    let (w, ht) = (r.right - r.left, r.bottom - r.top);
    let x = (to.left + (r.left - from.left)).clamp(to.left, (to.right - w).max(to.left));
    let y = (to.top + (r.top - from.top)).clamp(to.top, (to.bottom - ht).max(to.top));
    (x, y)
}

/// "Gather here": moves `wins` to the same spot on work area `to`, on a
/// helper thread with posted requests and bounded waits (audit A02: the UI
/// thread used to restore and move each window synchronously).
pub(super) fn gather(wins: Vec<HWND>, to: RECT) {
    // Each window's process as it is now: re-checked before every step.
    let raw: Vec<(isize, u32)> = wins.iter().map(|h| (h.0 as isize, pid_of(*h))).collect();
    let spawned = std::thread::Builder::new().name("gather".into()).spawn(move || {
        for (h, pid) in raw {
            if pid == 0 {
                continue;
            }
            EXPECT_PID.with(|p| p.set(pid));
            unsafe { gather_one(HWND(h as *mut _), to) };
        }
    });
    if let Err(e) = spawned {
        crate::log!("gather: no helper thread ({e})");
    }
}

unsafe fn gather_one(h: HWND, to: RECT) {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    if !ours(h) {
        return;
    }
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() || mi.rcWork == to {
        return;
    }
    let zoomed = IsZoomed(h).as_bool();
    if (zoomed || IsIconic(h).as_bool()) && !restore(h) {
        crate::log!("gather: window {:#x} did not restore in time; left as is", h.0 as isize);
        return;
    }
    let mut r = RECT::default();
    if GetWindowRect(h, &mut r).is_err() {
        return;
    }
    let (x, y) = gather_spot(r, mi.rcWork, to);
    let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
    if !ours(h) {
        return;
    }
    let _ = SetWindowPos(h, None, x, y, 0, 0, flags);
    if zoomed {
        // Maximized again on its new monitor once the move is through.
        let _ = wait_for(h, true, || true);
        if ours(h) {
            let _ = ShowWindowAsync(h, SW_MAXIMIZE);
        }
    }
}

/// Restores `hw` if minimized and brings it to the front, without the UI
/// thread ever waiting on the app (audit A02): the restore is posted
/// (ShowWindowAsync) and a helper waits a bounded time for it, then the
/// dock window gets `done_msg` (wParam = the window) and activates it if
/// it is still what the user wants (see finish). A window that is not
/// minimized is activated at once.
pub(super) unsafe fn bring(hw: HWND, dock: HWND, done_msg: u32) {
    if !IsWindow(Some(hw)).as_bool() {
        return;
    }
    // Every request is the newest intent: older helpers' results lose.
    set_intent(hw);
    if !IsIconic(hw).as_bool() {
        activate(hw);
        return;
    }
    if IsHungAppWindow(hw).as_bool() {
        crate::log!("focus: window {:#x} is not responding; not restored", hw.0 as isize);
        return;
    }
    let h = hw.0 as isize;
    // A helper already waits for this window: it reports it (and the
    // intent above makes it count).
    match RESTORING.lock() {
        Ok(r) if r.contains(&h) => return,
        Ok(mut r) => r.push(h),
        Err(_) => return,
    }
    // SW_RESTORE also activates the window when the app handles it; the
    // activation after the wait covers apps the foreground lock holds back.
    let _ = ShowWindowAsync(hw, SW_RESTORE);
    let d = dock.0 as isize;
    let spawned = std::thread::Builder::new().name("restore".into()).spawn(move || unsafe {
        let hw = HWND(h as *mut _);
        let start = Instant::now();
        let mut restored = false;
        while start.elapsed().as_millis() < STEP_MS as u128 {
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
            if !IsWindow(Some(hw)).as_bool() || IsHungAppWindow(hw).as_bool() {
                break;
            }
            if !IsIconic(hw).as_bool() {
                restored = true;
                break;
            }
        }
        if let Ok(mut r) = RESTORING.lock() {
            r.retain(|&x| x != h);
        }
        if restored {
            let _ = PostMessageW(Some(HWND(d as *mut _)), done_msg, WPARAM(h as usize), LPARAM(0));
        } else {
            crate::log!("focus: window {h:#x} did not restore in time");
        }
    });
    if let Err(e) = spawned {
        if let Ok(mut r) = RESTORING.lock() {
            r.retain(|&x| x != h);
        }
        crate::log!("focus: no helper thread ({e})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, rr: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: rr, bottom: b }
    }

    #[test]
    fn late_helpers_activate_only_the_latest_request() {
        let a = Intent { hwnd: 0xA, pid: 1, fg: 0x99 };
        // Still wanted: same window and process, front unchanged or itself.
        assert!(still_wanted(Some(a), 0xA, 1, 0x99, false));
        assert!(still_wanted(Some(a), 0xA, 1, 0xA, false));
        assert!(still_wanted(Some(a), 0xA, 1, 0x55, true), "our own menu in front");
        // The user asked for B since: A's late result loses.
        let b = Intent { hwnd: 0xB, pid: 2, fg: 0xA };
        assert!(!still_wanted(Some(b), 0xA, 1, 0xA, false));
        // The user switched elsewhere meanwhile.
        assert!(!still_wanted(Some(a), 0xA, 1, 0x77, false));
        // The handle now belongs to another process (reused).
        assert!(!still_wanted(Some(a), 0xA, 3, 0x99, false));
        assert!(!still_wanted(None, 0xA, 1, 0x99, false));
    }

    #[test]
    fn helpers_touch_only_the_window_they_were_asked_for() {
        assert!(still_ours(true, false, 7, 7));
        assert!(still_ours(true, false, 7, 0), "no process recorded: any");
        // The handle now belongs to another app's window.
        assert!(!still_ours(true, false, 9, 7));
        assert!(!still_ours(false, false, 0, 7), "closed");
        assert!(!still_ours(true, true, 7, 7), "not responding");
        assert!(!still_ours(true, false, 0, 0));
    }

    #[test]
    fn gather_keeps_the_offset_and_stays_inside() {
        let from = r(0, 0, 1920, 1040);
        let to = r(1920, 0, 3840, 1040);
        // Same offset on the other monitor.
        assert_eq!(gather_spot(r(100, 50, 900, 650), from, to), (2020, 50));
        // Would stick out: pulled back inside.
        assert_eq!(gather_spot(r(1500, 900, 2300, 1300), from, to), (3040, 640));
        // Larger than the target: pinned to its top-left.
        assert_eq!(gather_spot(r(0, 0, 5000, 3000), from, to), (1920, 0));
    }

    #[test]
    fn frame_compensation() {
        // Windows 11: 7 px invisible borders left/right/bottom, none on top.
        let win = r(93, 100, 607, 507);
        let frame = r(100, 100, 600, 500);
        let m = margins(win, Some(frame));
        assert_eq!(m, Margins { l: 7, t: 0, r: 7, b: 7 });
        let o = outer(r(0, 0, 960, 1040), m);
        assert_eq!(o, r(-7, 0, 967, 1047));
        // No DWM frame (or a nonsense one): no compensation.
        assert_eq!(margins(win, None), Margins::default());
        assert_eq!(margins(win, Some(r(0, 0, 2000, 2000))), Margins::default());
        assert_eq!(margins(win, Some(r(100, 100, 100, 500))), Margins::default());
    }

    #[test]
    fn borders_follow_dpi() {
        let m = Margins { l: 7, t: 0, r: 7, b: 7 };
        assert_eq!(scale_margins(m, 96, 144), Margins { l: 11, t: 0, r: 11, b: 11 }, "150 %: 10.5 rounds up");
        assert_eq!(scale_margins(m, 96, 192), Margins { l: 14, t: 0, r: 14, b: 14 });
        assert_eq!(scale_margins(Margins { l: 11, t: 0, r: 11, b: 11 }, 144, 96), Margins { l: 7, t: 0, r: 7, b: 7 });
        assert_eq!(scale_margins(m, 96, 96), m);
        assert_eq!(scale_margins(m, 0, 144), m);
    }

    #[test]
    fn other_monitor_keeps_relative_place() {
        let from = r(0, 0, 1920, 1040);
        let to = r(1920, 0, 1920 + 2560, 1400);
        // Centered window stays centered.
        let v = r(460, 220, 1460, 820);
        let o = on_other_monitor(v, from, to);
        assert_eq!((w(o), h(o)), (1000, 600));
        assert_eq!(((o.left + o.right) / 2, (o.top + o.bottom) / 2), (1920 + 1280, 700));
        // Window at the right edge stays inside.
        let o = on_other_monitor(r(1500, 0, 1920, 400), from, to);
        assert!(o.right <= to.right && o.left >= to.left);
        // Too big for a small monitor: shrunk to its work area.
        let small = r(-1280, 0, 0, 1000);
        assert_eq!(on_other_monitor(from, from, small), small);
    }

    #[test]
    fn next_monitor_goes_left_to_right_and_wraps() {
        let a = (1, r(0, 0, 1920, 1080));
        let b = (2, r(-1280, 0, 0, 1024));
        let c = (3, r(1920, -200, 4480, 1240));
        let all = [a, b, c];
        assert_eq!(next_monitor(&all, 2), Some(1));
        assert_eq!(next_monitor(&all, 1), Some(3));
        assert_eq!(next_monitor(&all, 3), Some(2));
        assert_eq!(next_monitor(&[a], 1), None);
        assert_eq!(next_monitor(&[a, b], 9), None);
    }

    #[test]
    fn settle_waits_for_the_change_then_for_quiet() {
        let a = r(0, 0, 100, 100);
        let b = r(0, 0, 960, 1040);
        let mut s = Settle::new(a, true);
        // The request is not handled yet: an unchanged rect never settles.
        for _ in 0..10 {
            assert!(!s.poll(a));
        }
        // It moves, then resizes again (DPI change), then holds still.
        assert!(!s.poll(b));
        assert!(!s.poll(r(0, 0, 970, 1050)));
        assert!(!s.poll(b));
        assert!(!s.poll(b));
        assert!(!s.poll(b));
        assert!(s.poll(b), "stable for three polls");
        // Nothing to wait for: settles once still.
        let mut s = Settle::new(a, false);
        assert!(!s.poll(a) && !s.poll(a) && !s.poll(a));
        assert!(s.poll(a));
    }

}
