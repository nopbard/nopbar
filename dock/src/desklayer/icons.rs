//! The desktop's icons as Explorer has them, read (and, for the desk
//! widgets' cells, moved) on a helper thread of their own: desklayer::grid
//! decides, this file talks to Explorer.
//!
//! How: IShellWindows::FindWindowSW(CSIDL_DESKTOP, SWC_DESKTOP) gives the
//! desktop's browser; its active view (IShellBrowser::QueryActiveShellView)
//! is an IFolderView(2): GetSpacing is a grid cell, ItemCount and Items
//! list the icons, GetItemPosition gives each one's place (the icon list's
//! client pixels, mapped to the screen point by point with
//! MapWindowPoints, mirroring included), GetCurrentFolderFlags tells
//! FWF_AUTOARRANGE and FWF_NOICONS, and SelectAndPositionItems with
//! SVSI_POSITIONITEM moves an icon (the way Explorer's own drag does).
//!
//! Icons are the user's: one moves only when nothing is in doubt
//! (run_batch): the flags read, every icon listed and placed, the count
//! matching, a list that is not mirrored, every display (its area, work
//! area and DPI) and every other widget's rectangle read, and nothing
//! changed between the read and the move (all of it read again right
//! before each move). A destination is a cell no icon's whole box touches
//! (icons off the grid included) and no widget keeps. Otherwise the
//! widgets only keep clear of the icons, and the reason is logged once.
//!
//! Never on a widget's thread: a busy or hung Explorer only holds this
//! thread. Each batch of work has a deadline (DEADLINE); past it the call
//! in progress is cancelled (CoCancelCall) and the batch gives up. Nothing
//! is kept between batches (a fresh IShellWindows each time), so after
//! Explorer restarts the next batch simply finds the new desktop.
//!
//! The widgets on the grid (the notes) register their windows;
//! their rectangles are the cells they keep, read when a batch runs (only
//! those shown and not cloaked on another virtual desktop).
//!
//! Tests never get here: under `cfg(test)` no batch ever runs.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};

use super::grid::Desk;

/// What a batch tells the window that asked (wParam).
pub const READ: usize = 0;
pub const PUSHED: usize = 1;

/// Desktop icons under a widget are moved aside (auto-arrange off, and
/// only while nothing is in doubt: run_batch). False ships a build that
/// never moves an icon: the widgets only keep clear of them.
/// (Off in 0.1.50 for the second confirm review's findings, docs/reviews/
/// 2026-11-09-board-look-confirm2.md: an unread widget rect, displays
/// changing between moves, icons off the grid. All three now stop or
/// avoid a move.)
pub const MOVE_ICONS: bool = false;

/// A batch's deadline: past it Explorer is busy, the batch gives up.
const DEADLINE: Duration = Duration::from_secs(3);
/// Icons read at most.
const MAX_ICONS: usize = 4000;

/// The message a batch posts back (registered once).
pub fn wm_grid() -> u32 {
    static MSG: AtomicU32 = AtomicU32::new(0);
    let m = MSG.load(Ordering::Relaxed);
    if m != 0 {
        return m;
    }
    let m = unsafe { windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(w!("NopBar.DeskGrid")) };
    MSG.store(m, Ordering::Relaxed);
    m
}

#[derive(Default)]
struct Queue {
    /// Windows told when the next read is done.
    reads: Vec<isize>,
    /// Widgets whose cells are cleared of icons, and who asked.
    pushes: Vec<(isize, isize)>,
    running: bool,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue { reads: Vec::new(), pushes: Vec::new(), running: false });
static WAKE: Condvar = Condvar::new();
static WIDGETS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
static CARDS: Mutex<Vec<(isize, i32, Option<RECT>)>> = Mutex::new(Vec::new());
static PLACES: Mutex<Vec<(isize, super::place::Place)>> = Mutex::new(Vec::new());
pub fn remember_place(hwnd: HWND, place: super::place::Place) {
    if let Ok(mut places) = PLACES.lock() {
        if let Some((_, p)) = places.iter_mut().find(|(h, _)| *h == hwnd.0 as isize) { *p = place; }
        else { places.push((hwnd.0 as isize, place)); }
    }
}
pub fn saved_place(hwnd: HWND) -> Option<super::place::Place> {
    PLACES.lock().ok()?.iter().find(|(h, _)| *h == hwnd.0 as isize).map(|(_, p)| *p)
}
/// Register the transparent shadow margin in physical pixels.
pub fn register_card(hwnd: HWND, pad: i32) {
    register(hwnd);
    set_pad(hwnd, pad);
}
/// The shadow margin of card window `hwnd` (px), without keeping its cells.
pub fn set_pad(hwnd: HWND, pad: i32) {
    if let Ok(mut cards) = CARDS.lock() {
        if let Some(c) = cards.iter_mut().find(|c| c.0 == hwnd.0 as isize) { c.1 = pad; }
        else { cards.push((hwnd.0 as isize, pad, None)); }
    }
}
pub fn committed(hwnd: HWND) -> Option<RECT> {
    CARDS.lock().ok()?.iter().find(|c| c.0 == hwnd.0 as isize)?.2
}
pub fn commit(hwnd: HWND, rect: RECT) {
    if let Ok(mut cards) = CARDS.lock() {
        if let Some(c) = cards.iter_mut().find(|c| c.0 == hwnd.0 as isize) { c.2 = Some(rect); }
        else { cards.push((hwnd.0 as isize, 0, Some(rect))); }
    }
}
/// Roll back this reservation if a newer placement has not superseded it.
pub fn rollback(hwnd: HWND, rejected: RECT) {
    rollback_to(hwnd, rejected, card(hwnd));
}

fn rollback_to(hwnd: HWND, rejected: RECT, actual: Option<RECT>) {
    if let Ok(mut cards) = CARDS.lock() {
        if let Some(entry) = cards.iter_mut().find(|entry| {
            entry.0 == hwnd.0 as isize && entry.2 == Some(rejected)
        }) {
            entry.2 = actual;
        }
    }
}

#[cfg(test)]
#[test]
fn rejected_placement_restores_actual_reservation_but_never_overwrites_a_newer_one() {
    let hwnd = HWND(0x6b821 as *mut _);
    let actual = RECT { left: 10, top: 10, right: 100, bottom: 100 };
    let rejected = RECT { left: 500, right: 590, ..actual };
    let newer = RECT { top: 500, bottom: 590, ..actual };
    commit(hwnd, rejected);
    rollback_to(hwnd, rejected, Some(actual));
    assert_eq!(committed(hwnd), Some(actual));
    commit(hwnd, newer);
    rollback_to(hwnd, rejected, Some(actual));
    assert_eq!(committed(hwnd), Some(newer));
    rollback_to(hwnd, newer, None);
    assert_eq!(committed(hwnd), None);
    forget(hwnd);
}
/// Reserve the entire reflow while startup placement is excluded. Every
/// reader sees either the old reservation set or the new one.
pub fn realign_batch(desk: &Desk, mons: &[crate::monitors::Monitor], widgets: &[(HWND, RECT, super::widget::Size)]) -> Option<Vec<RECT>> {
    let book = RESERVATIONS.lock().ok()?;
    let old: Vec<_> = widgets.iter().map(|(h, r, _)| committed(*h).unwrap_or(*r)).collect();
    let mut other: Vec<_> = widget_rects(None).into_iter().filter(|r| !old.contains(r)).collect();
    other.extend(book.pending.iter().map(|(_, r)| *r));
    let specs: Vec<_> = widgets.iter().map(|(_, r, size)| (*r, *size)).collect();
    let next = super::widget::realign(desk, mons, &specs, &other)?;
    let mut cards = CARDS.lock().ok()?;
    for ((h, _, _), r) in widgets.iter().zip(&next) {
        if let Some(c) = cards.iter_mut().find(|c| c.0 == h.0 as isize) { c.2 = Some(*r); }
        else { cards.push((h.0 as isize, 0, Some(*r))); }
    }
    Some(next)
}
pub fn card(hwnd: HWND) -> Option<RECT> { rect_of(hwnd) }


/// The desktop as last read (None: never read yet, or Explorer never
/// answered).
pub fn cached() -> Option<Desk> {
    super::grid_state::cached()
}

/// Window `hwnd` is a widget on the grid (its cells are kept).
pub fn register(hwnd: HWND) {
    if let Ok(mut w) = WIDGETS.lock() {
        let h = hwnd.0 as isize;
        if !w.contains(&h) {
            w.push(h);
        }
    }
}

pub fn forget(hwnd: HWND) {
    if let Ok(mut p) = PLACES.lock() { p.retain(|(h, _)| *h != hwnd.0 as isize); }
    if let Ok(mut c) = CARDS.lock() { c.retain(|c| c.0 != hwnd.0 as isize); }
    if let Ok(mut w) = WIDGETS.lock() {
        w.retain(|h| *h != hwnd.0 as isize);
    }
}

/// Is `h` shown on this virtual desktop? None: not known (its cloaking
/// could not be read).
fn shown_here(h: HWND) -> Option<bool> {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::UI::WindowsAndMessaging::{IsWindow, IsWindowVisible};
    unsafe {
        if !IsWindow(Some(h)).as_bool() || !IsWindowVisible(h).as_bool() {
            return Some(false);
        }
        let mut cloaked = 0u32;
        DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4).ok().map(|_| cloaked == 0)
    }
}

/// Shown, or maybe (its cells are kept in doubt).
pub(crate) fn showing(h: HWND) -> bool {
    shown_here(h) != Some(false)
}

/// The rectangles of the widgets shown now, `but` left out.
pub fn widget_rects(but: Option<HWND>) -> Vec<RECT> {
    let hs: Vec<isize> = WIDGETS.lock().map(|w| w.clone()).unwrap_or_default();
    // One reservation snapshot, so a concurrent batch cannot be observed
    // halfway through while this reader walks the registered windows.
    let saved = CARDS.lock().map(|c| c.clone()).unwrap_or_default();
    hs.into_iter().map(|h| HWND(h as *mut _))
        .filter(|h| Some(*h) != but && (showing(*h) || super::edit::dragging(*h)))
        .filter_map(|h| saved.iter().find(|c| c.0 == h.0 as isize).and_then(|c| c.2).or_else(|| rect_of(h))).collect()
}

/// Asks for the desktop to be read again; `reply` gets wm_grid() (READ)
/// once it is (or once Explorer did not answer: cached() then is what it
/// was).
pub fn read(reply: HWND) {
    enqueue(|q| {
        let h = reply.0 as isize;
        if !q.reads.contains(&h) {
            q.reads.push(h);
        }
    });
}
/// Refresh the shared cache without posting an unused READ response.
pub fn refresh() { read(HWND::default()); }

/// Moves the icons under widget `widget` to the nearest free cells (with
/// auto-arrange off; nothing otherwise), then tells `reply` (PUSHED).
pub fn push(widget: HWND, reply: HWND) {
    enqueue(|q| {
        let p = (widget.0 as isize, reply.0 as isize);
        if !q.pushes.contains(&p) {
            q.pushes.push(p);
        }
    });
}

fn enqueue(f: impl FnOnce(&mut Queue)) {
    if cfg!(test) {
        // Tests never touch the real desktop.
        return;
    }
    let Ok(mut q) = QUEUE.lock() else { return };
    f(&mut q);
    if !q.running {
        q.running = true;
        if std::thread::Builder::new().name("deskgrid".into()).spawn(worker).is_err() {
            q.running = false;
            return;
        }
    }
    WAKE.notify_one();
}

fn post(h: isize, what: usize) {
    if h == 0 { return; }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(HWND(h as *mut _)), wm_grid(), WPARAM(what), LPARAM(0));
    }
}

fn worker() {
    use windows::Win32::System::Com::{CoEnableCallCancellation, CoInitializeEx, COINIT_APARTMENTTHREADED};
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) { unsafe {
            let _ = windows::Win32::System::Com::CoDisableCallCancellation(None);
            windows::Win32::System::Com::CoUninitialize();
        } }
    }
    unsafe {
        if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
            if let Ok(mut q) = QUEUE.lock() { q.running = false; }
            return;
        }
        let _ = CoEnableCallCancellation(None);
    }
    let _apartment = Apartment;
    loop {
        let (reads, pushes) = {
            let Ok(mut q) = QUEUE.lock() else { return };
            while q.reads.is_empty() && q.pushes.is_empty() {
                q = match WAKE.wait_timeout(q, Duration::from_secs(10)) {
                    Ok((mut q, timed)) => {
                        if timed.timed_out() && q.reads.is_empty() && q.pushes.is_empty() {
                            // Same lock as enqueue: a later request starts a new worker.
                            q.running = false;
                            return;
                        }
                        q
                    },
                    Err(_) => return,
                };
            }
            (std::mem::take(&mut q.reads), std::mem::take(&mut q.pushes))
        };
        let moved = guarded(|abort| unsafe { batch(&pushes, abort) });
        if moved.is_none() {
            crate::log!("deskgrid: Explorer did not answer in time; the desktop's icons were not read");
        }
        for (_, reply) in &pushes {
            post(*reply, PUSHED);
        }
        for h in reads {
            post(h, READ);
        }
    }
}

/// Runs `f` with a watchdog: past DEADLINE the COM call in progress on
/// this thread is cancelled (and again, until `f` returns) and `abort` is
/// set, so `f` stops at its next step. None: it was cut short.
fn guarded<R>(f: impl FnOnce(&AtomicBool) -> Option<R>) -> Option<R> {
    use windows::Win32::System::Com::CoCancelCall;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    let tid = unsafe { GetCurrentThreadId() };
    let abort = std::sync::Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let a = abort.clone();
    let dog = std::thread::Builder::new().name("deskgrid-watch".into()).spawn(move || {
        if rx.recv_timeout(DEADLINE).is_ok() {
            return;
        }
        a.store(true, Ordering::SeqCst);
        loop {
            unsafe {
                let _ = CoCancelCall(tid, 0);
            }
            match rx.recv_timeout(Duration::from_millis(250)) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                _ => return,
            }
        }
    });
    let r = f(&abort);
    let _ = tx.send(());
    if let Ok(d) = dog {
        let _ = d.join();
    }
    if abort.load(Ordering::SeqCst) {
        None
    } else {
        r
    }
}

/// What a batch needs from Explorer's desktop view: the real one
/// (Explorer) or a test's fake. Item `i` is the `i`th listed by list();
/// positions are the icon list's client pixels.
pub(crate) trait Shell {
    /// A grid cell (the icons' spacing), px.
    fn spacing(&self) -> Option<(i32, i32)>;
    fn spacing_dpi(&self) -> u32 { 0 }
    /// The folder's FOLDERFLAGS (None: they could not be read).
    fn flags(&self) -> Option<u32>;
    /// The icon list shows.
    fn shown(&self) -> bool;
    /// The icon list is mirrored (WS_EX_LAYOUTRTL).
    fn mirrored(&self) -> bool;
    /// How many items the view says it has.
    fn count(&self) -> Option<usize>;
    /// Every item's position in order (None: that one's could not be read)
    /// and whether the listing ended normally (not an error, not the cap,
    /// not cut short).
    fn list(&mut self, max: usize, abort: &AtomicBool) -> (Vec<Option<POINT>>, bool);
    /// Item `i`'s position now.
    fn position(&self, i: usize) -> Option<POINT>;
    /// Moves item `i` to client point `at`.
    fn place(&self, i: usize, at: POINT) -> bool;
    fn to_screen(&self, p: POINT) -> Option<POINT>;
    fn to_client(&self, p: POINT) -> Option<POINT>;
    /// The displays now (areas, work areas, DPI), primary first. None:
    /// not read surely (the listing or any display's details failed).
    fn monitors(&self) -> Option<Vec<crate::monitors::Monitor>>;
}

/// The widgets' rectangles as a batch sees them (live, or a test's).
pub(crate) trait Rects {
    /// Widget `h`'s rectangle, if it is surely shown.
    fn of(&self, h: isize) -> Option<RECT>;
    /// Every widget's that is or may be shown. None: one of them could not
    /// be read (its cells unknown: no icon may move).
    fn all(&self) -> Option<Vec<RECT>>;
}

const AUTO: u32 = windows::Win32::UI::Shell::FWF_AUTOARRANGE.0 as u32;
const NOICONS: u32 = windows::Win32::UI::Shell::FWF_NOICONS.0 as u32;

/// Why icons were left where they are although a widget covers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Moving icons is turned off in this build (MOVE_ICONS).
    Off,
    /// The folder's flags could not be read (auto-arrange unknown).
    Flags,
    /// Not every icon and its place could be read.
    Incomplete,
    /// A mirrored (right-to-left) icon list.
    Mirrored,
    /// The displays (areas, work areas, DPI) could not be read surely.
    Displays,
    /// Another widget's rectangle could not be read (its cells unknown).
    Widgets,
    /// Something differed from what the last read and move left: an icon,
    /// the count, a flag, a widget, the list's look or coordinates.
    Changed,
    /// Explorer refused a move.
    MoveFailed,
    /// Explorer put the icon somewhere else than asked.
    Misplaced,
    /// The batch's time ran out (or was cancelled) on the way.
    Cancelled,
}

/// A batch's result.
pub(crate) struct Batch {
    pub desk: Desk,
    pub moved: usize,
    pub refused: Option<Refused>,
}

/// Moves at most this many icons in one batch (each widget's cells are
/// cleared one icon at a time).
const MAX_MOVES: usize = 64;

/// Pure: an icon box's top left on screen from its position mapped to the
/// screen: in a mirrored list the position is the box's right edge there.
pub(crate) fn box_left(mapped: POINT, cell: (i32, i32), mirrored: bool) -> POINT {
    if mirrored {
        POINT { x: mapped.x - cell.0, y: mapped.y }
    } else {
        mapped
    }
}

/// A sure reading of the desktop: every item's place (list pixels) and
/// its box on screen, nothing missing, flags read.
struct Sure {
    pos: Vec<POINT>,
    desk: Desk,
}

/// Reads everything again for a move, from scratch: the flags (read,
/// auto-arrange off, icons shown), the list's look (shown, not mirrored,
/// the same spacing), the count (the items listed at first, no more, no
/// fewer) and every item's place mapped to the screen. None at the first
/// doubt.
fn read_sure(s: &impl Shell, n: usize, cell: (i32, i32)) -> Option<Sure> {
    let flags = s.flags()?;
    if flags & (AUTO | NOICONS) != 0 || !s.shown() || s.mirrored() || s.spacing() != Some(cell) || s.count() != Some(n) {
        return None;
    }
    let mut pos = Vec::with_capacity(n);
    let mut icons = Vec::with_capacity(n);
    for i in 0..n {
        let p = s.position(i)?;
        icons.push(s.to_screen(p)?);
        pos.push(p);
    }
    Some(Sure { pos, desk: Desk { cell, icons, auto_arrange: false, shown: true, rtl: false, movable: true, layout: Vec::new(), spacing_dpi: s.spacing_dpi(), align: flags & windows::Win32::UI::Shell::FWF_SNAPTOGRID.0 as u32 != 0 } })
}

/// Reads the desktop through `s` and, for each widget in `pushes`, moves
/// the icons under it aside, strictly one at a time and only while
/// nothing is in doubt. The first read must be complete and sure (flags
/// read with auto-arrange off and icons shown, every icon listed and
/// placed, the count matching, a plain left-to-right list, the displays
/// and every widget's rectangle read). Then, for each single move: the
/// time checked; everything read again from scratch (the icons, the
/// displays with their work areas and DPI, every widget) and found exactly
/// as the last read and move left it; that one move worked out anew from
/// this reading (to a cell no widget keeps and no icon's box touches); the
/// time checked again; one SelectAndPositionItems; the icon's place read
/// back and found exactly where it was sent. Anything else (a failed call,
/// a different place, a change, something unread, the time running out,
/// MOVE_ICONS off) stops every move left. Never a plan carried on. The
/// widgets keep clear of what was read in any case. None: nothing could
/// be read.
pub(crate) fn run_batch(s: &mut impl Shell, pushes: &[isize], rects: &impl Rects, abort: &AtomicBool, move_icons: bool) -> Option<Batch> {
    let cut = || abort.load(Ordering::SeqCst);
    let cell = s.spacing().filter(|c| c.0 > 0 && c.1 > 0)?;
    let flags = s.flags();
    let mirrored = s.mirrored();
    // The displays and the widgets as surely as the icons (None: not).
    let mons = s.monitors().filter(|m| !m.is_empty());
    let others = rects.all();
    let (pos, listed) = s.list(MAX_ICONS, abort);
    if cut() {
        return None;
    }
    let mut complete = listed && s.count() == Some(pos.len());
    let mut icons = Vec::new();
    for p in &pos {
        match p.and_then(|p| s.to_screen(p)) {
            Some(q) => icons.push(box_left(q, cell, mirrored)),
            None => complete = false,
        }
    }
    let n = pos.len();
    // Unknown flags: as if auto-arrange were on (keep clear, move nothing).
    let auto_arrange = flags.is_none_or(|f| f & AUTO != 0);
    let shown = s.shown() && !flags.is_some_and(|f| f & NOICONS != 0);
    let movable = flags.is_some() && complete && !mirrored;
    let mut desk = Desk { cell, icons, auto_arrange, shown, rtl: mirrored, movable, layout: Vec::new(), spacing_dpi: s.spacing_dpi(), align: flags.is_some_and(|f| f & windows::Win32::UI::Shell::FWF_SNAPTOGRID.0 as u32 != 0) };
    // A widget over icons it may push aside (auto-arrange off, shown); the
    // displays unknown: maybe.
    let seen = others.as_deref().unwrap_or(&[]);
    let covers = |d: &Desk, r: RECT| match &mons {
        Some(m) => !super::grid::plan_pushes(&Desk { auto_arrange: false, ..d.clone() }, m, r, seen).is_empty(),
        None => true,
    };
    let wanted = shown && !auto_arrange && pushes.iter().filter_map(|h| rects.of(*h)).any(|r| covers(&desk, r));
    let wanted_unsure = shown && flags.is_none_or(|f| f & AUTO == 0) && pushes.iter().filter_map(|h| rects.of(*h)).any(|r| covers(&desk, r));
    if !movable {
        let refused = wanted_unsure.then_some(if flags.is_none() {
            Refused::Flags
        } else if mirrored {
            Refused::Mirrored
        } else {
            Refused::Incomplete
        });
        return Some(Batch { desk, moved: 0, refused });
    }
    if !wanted {
        return Some(Batch { desk, moved: 0, refused: None });
    }
    let unsure = |desk: Desk, why: Refused| Some(Batch { desk: Desk { movable: false, ..desk }, moved: 0, refused: Some(why) });
    let Some(mons) = mons else { return unsure(desk, Refused::Displays) };
    if others.is_none() {
        return unsure(desk, Refused::Widgets);
    }
    if !move_icons {
        return Some(Batch { desk, moved: 0, refused: Some(Refused::Off) });
    }
    // What the last read (and then each move) leaves: the places expected.
    let mut expect: Vec<POINT> = pos.iter().map(|p| p.unwrap_or_default()).collect();
    let mut moved = 0;
    let stop = |desk: Desk, moved: usize, why: Refused| Some(Batch { desk: Desk { movable: false, ..desk }, moved, refused: Some(why) });
    for h in pushes {
        let Some(r0) = rects.of(*h) else { continue };
        loop {
            if cut() {
                return stop(desk, moved, Refused::Cancelled);
            }
            if moved >= MAX_MOVES {
                return stop(desk, moved, Refused::Changed);
            }
            // Everything again, from scratch, as expected: the icons, the
            // displays (an area, a work area or a DPI changed: stop), every
            // widget (one unread: stop).
            let Some(now) = read_sure(s, n, cell) else { return stop(desk, moved, Refused::Changed) };
            if s.monitors().as_ref() != Some(&mons) {
                return stop(desk, moved, Refused::Changed);
            }
            let Some(widgets) = rects.all() else { return stop(desk, moved, Refused::Widgets) };
            if cut() {
                return stop(desk, moved, Refused::Cancelled);
            }
            if now.pos != expect || rects.of(*h) != Some(r0) {
                return stop(desk, moved, Refused::Changed);
            }
            desk = now.desk;
            // The one move, worked out from this reading.
            let Some(&(k, to)) = super::grid::plan_pushes(&desk, &mons, r0, &widgets).first() else { break };
            let Some(at) = s.to_client(to) else { return stop(desk, moved, Refused::Changed) };
            if cut() {
                return stop(desk, moved, Refused::Cancelled);
            }
            if !s.place(k, at) {
                return stop(desk, moved, Refused::MoveFailed);
            }
            moved += 1;
            // Exactly where it was sent?
            let back = s.position(k);
            let on_screen = back.and_then(|p| s.to_screen(p));
            if on_screen != Some(to) {
                if let Some(q) = on_screen {
                    desk.icons[k] = q;
                }
                return stop(desk, moved, Refused::Misplaced);
            }
            desk.icons[k] = to;
            expect = now.pos;
            expect[k] = back.unwrap_or(at);
            if cut() {
                return stop(desk, moved, Refused::Cancelled);
            }
        }
    }
    Some(Batch { desk, moved, refused: None })
}

/// `p` from window `from`'s client pixels to `to`'s (None: the screen),
/// mirroring included (one point at a time: two would be read as a
/// rectangle). None when it fails.
pub(crate) fn map_point(from: Option<HWND>, to: Option<HWND>, p: POINT) -> Option<POINT> {
    use windows::Win32::Foundation::{GetLastError, SetLastError, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::Graphics::Gdi::MapWindowPoints;
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;
    unsafe {
        for h in [from, to].into_iter().flatten() {
            if !IsWindow(Some(h)).as_bool() {
                return None;
            }
        }
        let mut a = [p];
        SetLastError(WIN32_ERROR(0));
        let r = MapWindowPoints(from, to, &mut a);
        (r != 0 || GetLastError() == ERROR_SUCCESS).then_some(a[0])
    }
}

/// Explorer's desktop view.
struct Explorer {
    fv: windows::Win32::UI::Shell::IFolderView,
    list: HWND,
    pidls: Vec<*mut windows::Win32::UI::Shell::Common::ITEMIDLIST>,
}

impl Drop for Explorer {
    fn drop(&mut self) {
        for p in self.pidls.drain(..) {
            unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p as *const _)) };
        }
    }
}

unsafe fn explorer() -> Option<Explorer> {
    use windows::Win32::System::Com::{CoCreateInstance, IServiceProvider, CLSCTX_LOCAL_SERVER};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::{IShellBrowser, IShellWindows, ShellWindows, CSIDL_DESKTOP, SID_STopLevelBrowser, SWC_DESKTOP, SWFO_NEEDDISPATCH};
    use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;
    use windows::core::Interface;
    let sw: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()?;
    let loc = VARIANT::from(CSIDL_DESKTOP as i32);
    let root = VARIANT::default();
    let mut hwnd = 0i32;
    let disp = sw.FindWindowSW(&loc, &root, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH).ok()?;
    let sp: IServiceProvider = disp.cast().ok()?;
    let sb: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser).ok()?;
    let view = sb.QueryActiveShellView().ok()?;
    let fv: windows::Win32::UI::Shell::IFolderView = view.cast().ok()?;
    let defview = view.GetWindow().ok()?;
    // The icons' own window (their positions are its client pixels).
    let list = FindWindowExW(Some(defview), None, w!("SysListView32"), windows::core::PCWSTR::null()).ok()?;
    Some(Explorer { fv, list, pidls: Vec::new() })
}

impl Shell for Explorer {
    fn spacing_dpi(&self) -> u32 { unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(self.list) }.max(96) }
    // One SysListView32 / IFolderView owns every desktop icon. GetSpacing is
    // the view's pixel cell, and GetItemPosition returns points in that same
    // view, mapped to screen without a per-monitor DPI multiplication.
    // https://learn.microsoft.com/windows/win32/api/shobjidl_core/nf-shobjidl_core-ifolderview-getspacing
    fn spacing(&self) -> Option<(i32, i32)> {
        let mut p = POINT::default();
        unsafe { self.fv.GetSpacing(&mut p) }.ok()?;
        Some((p.x, p.y))
    }

    fn flags(&self) -> Option<u32> {
        use windows::core::Interface;
        let f2: windows::Win32::UI::Shell::IFolderView2 = self.fv.cast().ok()?;
        unsafe { f2.GetCurrentFolderFlags() }.ok()
    }

    fn shown(&self) -> bool {
        unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(self.list) }.as_bool()
    }

    fn mirrored(&self) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongW, GWL_EXSTYLE, WS_EX_LAYOUTRTL};
        unsafe { GetWindowLongW(self.list, GWL_EXSTYLE) as u32 & WS_EX_LAYOUTRTL.0 != 0 }
    }

    fn count(&self) -> Option<usize> {
        unsafe { self.fv.ItemCount(windows::Win32::UI::Shell::SVGIO_ALLVIEW) }.ok().and_then(|n| usize::try_from(n).ok())
    }

    fn list(&mut self, max: usize, abort: &AtomicBool) -> (Vec<Option<POINT>>, bool) {
        use windows::Win32::Foundation::{S_FALSE, S_OK};
        use windows::Win32::UI::Shell::{IEnumIDList, SVGIO_ALLVIEW};
        let Ok(en) = (unsafe { self.fv.Items::<IEnumIDList>(SVGIO_ALLVIEW) }) else { return (Vec::new(), false) };
        let mut out = Vec::new();
        loop {
            if abort.load(Ordering::SeqCst) || self.pidls.len() >= max {
                return (out, false);
            }
            let mut one = [std::ptr::null_mut(); 1];
            let mut got = 0u32;
            let hr = unsafe { en.Next(&mut one, Some(&mut got)) };
            if hr == S_OK && got == 1 && !one[0].is_null() {
                self.pidls.push(one[0]);
                out.push(unsafe { self.fv.GetItemPosition(one[0]) }.ok());
            } else {
                // S_FALSE with nothing: the end; anything else: an error.
                return (out, hr == S_FALSE && got == 0);
            }
        }
    }

    fn position(&self, i: usize) -> Option<POINT> {
        unsafe { self.fv.GetItemPosition(*self.pidls.get(i)?) }.ok()
    }

    fn place(&self, i: usize, at: POINT) -> bool {
        use windows::Win32::UI::Shell::SVSI_POSITIONITEM;
        let Some(p) = self.pidls.get(i) else { return false };
        let one = [*p as *const windows::Win32::UI::Shell::Common::ITEMIDLIST];
        unsafe { self.fv.SelectAndPositionItems(1, one.as_ptr(), Some(&at), SVSI_POSITIONITEM.0 as u32) }.is_ok()
    }

    fn to_screen(&self, p: POINT) -> Option<POINT> {
        map_point(Some(self.list), None, p)
    }

    fn to_client(&self, p: POINT) -> Option<POINT> {
        map_point(None, Some(self.list), p)
    }

    fn monitors(&self) -> Option<Vec<crate::monitors::Monitor>> {
        crate::monitors::all_sure()
    }
}

/// The registered widgets, live.
struct Live;

impl Rects for Live {
    fn of(&self, h: isize) -> Option<RECT> {
        // Icons move aside only for a widget surely shown.
        let h = HWND(h as *mut _);
        if shown_here(h) == Some(true) {
            rect_of(h)
        } else {
            None
        }
    }

    fn all(&self) -> Option<Vec<RECT>> {
        // Hidden (or gone) for sure: no cells; shown or maybe: its
        // rectangle, which must be read.
        let hs: Vec<isize> = WIDGETS.lock().ok()?.clone();
        let mut out = Vec::with_capacity(hs.len());
        for h in hs.into_iter().map(|h| HWND(h as *mut _)) {
            if shown_here(h) != Some(false) {
                out.push(rect_of(h)?);
            }
        }
        Some(out)
    }
}

/// One batch on the real desktop; the cache updated. None: cut short or
/// Explorer not there.
unsafe fn batch(pushes: &[(isize, isize)], abort: &AtomicBool) -> Option<()> {
    let mut ex = explorer()?;
    let widgets: Vec<isize> = pushes.iter().map(|(w, _)| *w).collect();
    let b = run_batch(&mut ex, &widgets, &Live, abort, MOVE_ICONS)?;
    drop(ex);
    tell_once(b.refused);
    if b.moved > 0 {
        crate::log!("deskgrid: {} desktop icons moved out from under a widget", b.moved);
    }
    if abort.load(Ordering::SeqCst) {
        return None;
    }
    super::grid_state::publish(b.desk);
    Some(())
}

/// Logs why icons stayed (or the moves stopped), once per reason.
fn tell_once(r: Option<Refused>) {
    static TOLD: Mutex<Vec<Refused>> = Mutex::new(Vec::new());
    let (Some(r), Ok(mut told)) = (r, TOLD.lock()) else { return };
    if !told.contains(&r) {
        told.push(r);
        crate::log!("deskgrid: desktop icons left where they are ({r:?}); the widget only keeps clear of them");
    }
}

fn rect_of(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(h, &mut r) }.ok()?;
    let pad = CARDS.lock().ok().and_then(|c| c.iter().find(|c| c.0 == h.0 as isize).map(|c| c.1)).unwrap_or(0);
    Some(super::widget::card_rect(r, pad))
}

/// A watch of the desktop folders (watch()): its registration and the
/// folder ids it was made with, released together (unwatch()).
#[derive(Default, Debug, PartialEq)]
pub struct Watch {
    id: u32,
    /// The folder ids (ITEMIDLIST pointers), freed once deregistered.
    pidls: Vec<isize>,
}

impl Watch {
    /// Pure: deregistered (if it was registered), then each folder id
    /// freed once.
    fn release(self, deregister: impl FnOnce(u32), mut free: impl FnMut(isize)) {
        if self.id != 0 {
            deregister(self.id);
        }
        for p in self.pidls {
            free(p);
        }
    }
}

fn free_pidl(p: isize) {
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p as *const _)) };
}

/// Watches the desktop folders (the user's and the public one) for icons
/// coming and going: `msg` is posted to `hwnd` (new delivery: on_notice
/// releases it). A watch that could not be made holds nothing.
pub fn watch(hwnd: HWND, msg: u32) -> Watch {
    use windows::core::BOOL;
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_PublicDesktop, SHChangeNotifyEntry, SHChangeNotifyRegister, SHGetKnownFolderIDList, SHCNE_CREATE, SHCNE_DELETE, SHCNE_MKDIR,
        SHCNE_RENAMEFOLDER, SHCNE_RENAMEITEM, SHCNE_RMDIR, SHCNRF_InterruptLevel, SHCNRF_NewDelivery, SHCNRF_ShellLevel,
    };
    if cfg!(test) {
        return Watch::default();
    }
    unsafe {
        let pidls: Vec<_> = [FOLDERID_Desktop, FOLDERID_PublicDesktop].iter().filter_map(|id| SHGetKnownFolderIDList(id, 0, None).ok()).collect();
        let entries: Vec<SHChangeNotifyEntry> = pidls.iter().map(|p| SHChangeNotifyEntry { pidl: *p, fRecursive: BOOL(0) }).collect();
        let events = SHCNE_CREATE.0 | SHCNE_DELETE.0 | SHCNE_MKDIR.0 | SHCNE_RMDIR.0 | SHCNE_RENAMEITEM.0 | SHCNE_RENAMEFOLDER.0;
        let id = if entries.is_empty() {
            0
        } else {
            SHChangeNotifyRegister(hwnd, SHCNRF_ShellLevel | SHCNRF_InterruptLevel | SHCNRF_NewDelivery, events as i32, msg, entries.len() as i32, entries.as_ptr())
        };
        let w = Watch { id, pidls: pidls.into_iter().map(|p| p as isize).collect() };
        if id == 0 {
            // Nothing registered: nothing kept either.
            w.release(|_| {}, free_pidl);
            return Watch::default();
        }
        w
    }
}

/// Ends a watch: deregistered, its folder ids freed.
pub fn unwatch(w: Watch) {
    w.release(
        |id| unsafe {
            let _ = windows::Win32::UI::Shell::SHChangeNotifyDeregister(id);
        },
        free_pidl,
    );
}

/// A notice from watch(): released (new delivery keeps it in shared
/// memory until then).
pub fn on_notice(wp: WPARAM, lp: LPARAM) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::UI::Shell::{SHChangeNotification_Lock, SHChangeNotification_Unlock};
    unsafe {
        let mut pidls: *mut *mut windows::Win32::UI::Shell::Common::ITEMIDLIST = std::ptr::null_mut();
        let mut event = 0i32;
        let lock = SHChangeNotification_Lock(HANDLE(wp.0 as *mut _), lp.0 as u32, Some(&mut pidls), Some(&mut event));
        if !lock.is_invalid() {
            let _ = SHChangeNotification_Unlock(lock);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitors::Monitor;
    use std::cell::{Cell, RefCell};

    fn pt(x: i32, y: i32) -> POINT {
        POINT { x, y }
    }

    /// Where a fake call is.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Call {
        Flags,
        Position(usize),
        /// Right after a move was done.
        Placed(usize),
    }

    /// A fake desktop view: icons at client points (the list's corner at
    /// the screen's), with a hook to make trouble at any call.
    struct Fake {
        at: RefCell<Vec<Option<POINT>>>,
        flags: Cell<Option<u32>>,
        complete: bool,
        count_off: Cell<usize>,
        mirrored: bool,
        place_ok: Cell<bool>,
        /// Explorer puts a moved icon here instead (once).
        lands: Cell<Option<POINT>>,
        placed: RefCell<Vec<(usize, POINT)>>,
        flag_reads: Cell<usize>,
        /// The displays as read now (None: not surely).
        mons: RefCell<Option<Vec<Monitor>>>,
        abort: std::sync::Arc<AtomicBool>,
        hook: Option<Box<dyn Fn(&Fake, Call)>>,
    }

    impl Fake {
        fn new(at: Vec<POINT>) -> Fake {
            Fake {
                at: RefCell::new(at.into_iter().map(Some).collect()),
                flags: Cell::new(Some(0)),
                complete: true,
                count_off: Cell::new(0),
                mirrored: false,
                place_ok: Cell::new(true),
                lands: Cell::new(None),
                placed: RefCell::new(Vec::new()),
                flag_reads: Cell::new(0),
                mons: RefCell::new(Some(mons())),
                abort: std::sync::Arc::new(AtomicBool::new(false)),
                hook: None,
            }
        }

        fn poke(&self, c: Call) {
            if let Some(h) = &self.hook {
                h(self, c);
            }
        }

        fn moves(&self) -> usize {
            self.placed.borrow().len()
        }

        /// No two icons on one place.
        fn apart(&self) -> bool {
            let at = self.at.borrow();
            at.iter().enumerate().all(|(i, a)| at.iter().skip(i + 1).all(|b| a != b))
        }

        /// No two icons' boxes (76 x 96) overlap at all.
        fn boxes_apart(&self) -> bool {
            let at: Vec<POINT> = self.at.borrow().iter().flatten().copied().collect();
            let hit = |a: &POINT, b: &POINT| a.x < b.x + 76 && b.x < a.x + 76 && a.y < b.y + 96 && b.y < a.y + 96;
            at.iter().enumerate().all(|(i, a)| at.iter().skip(i + 1).all(|b| !hit(a, b)))
        }
    }

    impl Shell for Fake {
        fn spacing(&self) -> Option<(i32, i32)> {
            Some((76, 96))
        }
        fn flags(&self) -> Option<u32> {
            self.flag_reads.set(self.flag_reads.get() + 1);
            self.poke(Call::Flags);
            self.flags.get()
        }
        fn shown(&self) -> bool {
            true
        }
        fn mirrored(&self) -> bool {
            self.mirrored
        }
        fn count(&self) -> Option<usize> {
            Some(self.at.borrow().len() + self.count_off.get())
        }
        fn list(&mut self, _max: usize, _abort: &AtomicBool) -> (Vec<Option<POINT>>, bool) {
            (self.at.borrow().clone(), self.complete)
        }
        fn position(&self, i: usize) -> Option<POINT> {
            self.poke(Call::Position(i));
            self.at.borrow()[i]
        }
        fn place(&self, i: usize, at: POINT) -> bool {
            self.placed.borrow_mut().push((i, at));
            if !self.place_ok.get() {
                return false;
            }
            self.at.borrow_mut()[i] = Some(self.lands.take().unwrap_or(at));
            self.poke(Call::Placed(i));
            true
        }
        fn to_screen(&self, p: POINT) -> Option<POINT> {
            Some(if self.mirrored { pt(1920 - p.x, p.y) } else { p })
        }
        fn to_client(&self, p: POINT) -> Option<POINT> {
            Some(if self.mirrored { pt(1920 - p.x, p.y) } else { p })
        }
        fn monitors(&self) -> Option<Vec<Monitor>> {
            self.mons.borrow().clone()
        }
    }

    /// The widgets, shared so a hook can change them mid-batch: each one's
    /// rectangle, None for one shown whose rectangle could not be read.
    struct Fixed(std::rc::Rc<RefCell<Vec<(isize, Option<RECT>)>>>);

    impl Rects for Fixed {
        fn of(&self, h: isize) -> Option<RECT> {
            self.0.borrow().iter().find(|(x, _)| *x == h).and_then(|(_, r)| *r)
        }
        fn all(&self) -> Option<Vec<RECT>> {
            self.0.borrow().iter().map(|(_, r)| *r).collect()
        }
    }

    fn mons() -> Vec<Monitor> {
        let rect = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        vec![Monitor { handle: 1, rect, work: RECT { bottom: 1032, ..rect }, primary: true, dpi: 96 }]
    }

    /// Cells start at (2, 2), 25 columns (the last is 24). Two icons at the
    /// top right under the widget, one at the top left.
    fn setup() -> (Fake, Fixed) {
        let icons = vec![pt(2, 2), pt(2 + 24 * 76, 2), pt(2 + 23 * 76, 2 + 96)];
        let widget = RECT { left: 2 + 21 * 76 + 4, top: 2 + 4, right: 2 + 25 * 76 - 4, bottom: 2 + 4 * 96 - 4 };
        (Fake::new(icons), Fixed(std::rc::Rc::new(RefCell::new(vec![(7, Some(widget))]))))
    }

    /// The first move's and the second's destinations in setup().
    const FIRST: POINT = POINT { x: 2 + 20 * 76, y: 2 };
    const SECOND: POINT = POINT { x: 2 + 20 * 76, y: 2 + 96 };

    fn run(f: &mut Fake, w: &Fixed) -> Batch {
        // The fake's own flag, so a hook can set it mid-call.
        let abort = f.abort.clone();
        run_batch(f, &[7], w, &abort, true).unwrap()
    }

    /// Moving turned off (MOVE_ICONS, as shipped in 0.1.50): a sure read
    /// over covered icons still moves none, and says why.
    #[test]
    fn with_moving_off_no_icon_ever_moves() {
        let (mut f, w) = setup();
        let abort = f.abort.clone();
        let b = run_batch(&mut f, &[7], &w, &abort, false).unwrap();
        assert_eq!((b.moved, b.refused), (0, Some(Refused::Off)));
        assert_eq!(f.moves(), 0);
        assert!(f.placed.borrow().is_empty());
    }

    #[test]
    fn sure_reads_move_only_the_icons_under_the_widget_one_at_a_time() {
        let (mut f, w) = setup();
        let b = run(&mut f, &w);
        assert_eq!(b.refused, None);
        assert_eq!(b.moved, 2);
        assert_eq!(*f.placed.borrow(), vec![(1, FIRST), (2, SECOND)]);
        assert!(f.apart());
        assert_eq!(b.desk.icons[1], FIRST);
        assert_eq!(b.desk.icons[2], SECOND);
        assert_eq!(b.desk.icons[0], pt(2, 2), "the top-left icon stays");
        // Everything was read again before each move: flags read once at
        // first, then once per move and once to find nothing left.
        assert_eq!(f.flag_reads.get(), 1 + 3);
    }

    #[test]
    fn unread_flags_move_nothing_and_keep_clear_as_if_auto_arranged() {
        let (mut f, w) = setup();
        f.flags.set(None);
        let b = run(&mut f, &w);
        assert_eq!(f.moves(), 0);
        assert_eq!(b.refused, Some(Refused::Flags));
        assert!(b.desk.auto_arrange && !b.desk.movable);
    }

    #[test]
    fn an_incomplete_listing_moves_nothing() {
        // The listing ended with an error (or at the cap).
        let (mut f, w) = setup();
        f.complete = false;
        let b = run(&mut f, &w);
        assert_eq!(f.moves(), 0);
        assert_eq!(b.refused, Some(Refused::Incomplete));
        // One icon's place could not be read.
        let (mut f, w) = setup();
        f.at.borrow_mut()[0] = None;
        assert_eq!(run(&mut f, &w).refused, Some(Refused::Incomplete));
        assert_eq!(f.moves(), 0);
        // The view counts more items than were listed.
        let (mut f, w) = setup();
        f.count_off.set(1);
        assert_eq!(run(&mut f, &w).refused, Some(Refused::Incomplete));
        assert_eq!(f.moves(), 0);
    }

    #[test]
    fn the_second_destination_taken_after_the_first_move_stops_everything() {
        // The user drops the top-left icon on the second move's
        // destination right after the first move.
        let (mut f, w) = setup();
        f.hook = Some(Box::new(|f: &Fake, c| {
            if c == Call::Placed(1) {
                f.at.borrow_mut()[0] = Some(SECOND);
            }
        }));
        let b = run(&mut f, &w);
        assert_eq!(b.refused, Some(Refused::Changed));
        assert_eq!(f.moves(), 1, "no move after the change");
        assert!(f.apart(), "never two icons on one place");
        assert!(!b.desk.movable);
    }

    #[test]
    fn any_other_change_between_moves_stops_everything() {
        // An icon under the widget moved by the user.
        let (mut f, w) = setup();
        f.hook = Some(Box::new(|f: &Fake, c| {
            if c == Call::Placed(1) {
                f.at.borrow_mut()[2].as_mut().unwrap().x += 300;
            }
        }));
        assert_eq!(run(&mut f, &w).refused, Some(Refused::Changed));
        assert_eq!(f.moves(), 1);
        // Auto-arrange turned on.
        let (mut f, w) = setup();
        f.hook = Some(Box::new(|f: &Fake, c| {
            if c == Call::Placed(1) {
                f.flags.set(Some(AUTO));
            }
        }));
        assert_eq!(run(&mut f, &w).refused, Some(Refused::Changed));
        assert_eq!(f.moves(), 1);
        // A new icon on the desktop (the count grows).
        let (mut f, w) = setup();
        f.hook = Some(Box::new(|f: &Fake, c| {
            if c == Call::Placed(1) {
                f.count_off.set(1);
            }
        }));
        assert_eq!(run(&mut f, &w).refused, Some(Refused::Changed));
        assert_eq!(f.moves(), 1);
    }

    #[test]
    fn a_move_explorer_refuses_stops_everything() {
        let (mut f, w) = setup();
        f.place_ok.set(false);
        let b = run(&mut f, &w);
        assert_eq!(b.refused, Some(Refused::MoveFailed));
        assert_eq!(f.moves(), 1, "the one call tried, nothing after");
        assert_eq!(b.moved, 0);
    }

    #[test]
    fn a_move_explorer_puts_elsewhere_stops_everything() {
        // Explorer puts the first icon on the second's destination.
        let (mut f, w) = setup();
        f.lands.set(Some(SECOND));
        let b = run(&mut f, &w);
        assert_eq!(b.refused, Some(Refused::Misplaced));
        assert_eq!(f.moves(), 1, "no second move onto it");
        assert!(f.apart());
        assert_eq!(b.desk.icons[1], SECOND, "the cache has where it really is");
    }

    #[test]
    fn time_running_out_during_a_check_moves_nothing_after() {
        // The batch's time runs out inside the check before the first move
        // (the flags' second read), the call itself answering normally.
        let (mut f, w) = setup();
        f.hook = Some(Box::new(|f: &Fake, c| {
            if c == Call::Flags && f.flag_reads.get() == 2 {
                f.abort.store(true, Ordering::SeqCst);
            }
        }));
        let b = run(&mut f, &w);
        assert_eq!(f.moves(), 0);
        assert_eq!(b.refused, Some(Refused::Cancelled));
        // Inside the last position read before the second move.
        let (mut f, w) = setup();
        f.hook = Some(Box::new(|f: &Fake, c| {
            if c == Call::Position(2) && f.moves() == 1 {
                f.abort.store(true, Ordering::SeqCst);
            }
        }));
        let b = run(&mut f, &w);
        assert_eq!(f.moves(), 1);
        assert_eq!(b.refused, Some(Refused::Cancelled));
    }

    /// confirm2 finding 1: another widget shown whose rectangle could not
    /// be read (its cells unknown) stops the whole batch before any move.
    #[test]
    fn another_widgets_unread_rect_moves_nothing() {
        // The review's case: widget 7 read, widget 8 shown but unread.
        let (mut f, w) = setup();
        w.0.borrow_mut().push((8, None));
        let abort = f.abort.clone();
        let b = run_batch(&mut f, &[7, 8], &w, &abort, true).unwrap();
        assert_eq!((f.moves(), b.moved, b.refused), (0, 0, Some(Refused::Widgets)));
        assert!(!b.desk.movable);
        // Still kept clear of: the icons as read.
        assert_eq!(b.desk.icons.len(), 3);
        // Only the other widget asked for nothing: the same.
        let (mut f, w) = setup();
        w.0.borrow_mut().push((8, None));
        assert_eq!(run(&mut f, &w).refused, Some(Refused::Widgets));
        assert_eq!(f.moves(), 0);
    }

    #[test]
    fn a_widget_turning_unreadable_between_moves_stops_everything() {
        let (mut f, w) = setup();
        let shared = w.0.clone();
        f.hook = Some(Box::new(move |_: &Fake, c| {
            if c == Call::Placed(1) {
                shared.borrow_mut().push((8, None));
            }
        }));
        let b = run(&mut f, &w);
        assert_eq!(b.refused, Some(Refused::Widgets));
        assert_eq!(f.moves(), 1, "no move after it");
        // Another widget moved over the second destination between moves:
        // the next move is worked out around it (never into its cells).
        let (mut f, w) = setup();
        let shared = w.0.clone();
        let over = RECT { left: SECOND.x + 4, top: SECOND.y + 4, right: SECOND.x + 2 * 76 - 4, bottom: SECOND.y + 96 - 4 };
        f.hook = Some(Box::new(move |_: &Fake, c| {
            if c == Call::Placed(1) {
                shared.borrow_mut().push((9, Some(over)));
            }
        }));
        let b = run(&mut f, &w);
        assert_eq!(b.refused, None);
        let to = f.placed.borrow()[1].1;
        assert!(to != SECOND && (to.x + 76 <= over.left || to.x >= over.right || to.y + 96 <= over.top || to.y >= over.bottom), "{to:?}");
    }

    /// confirm2 finding 2: the displays read again before each move.
    #[test]
    fn displays_not_read_surely_move_nothing() {
        let (mut f, w) = setup();
        *f.mons.borrow_mut() = None;
        let b = run(&mut f, &w);
        assert_eq!((f.moves(), b.refused), (0, Some(Refused::Displays)));
        assert!(!b.desk.movable);
        let (mut f, w) = setup();
        *f.mons.borrow_mut() = Some(Vec::new());
        assert_eq!(run(&mut f, &w).refused, Some(Refused::Displays));
        assert_eq!(f.moves(), 0);
    }

    #[test]
    fn a_display_change_between_moves_stops_everything() {
        type Change = fn(&mut Vec<Monitor>);
        let changes: [(&str, Change); 5] = [
            // The review's case: a taskbar or appbar takes the right edge.
            ("work area", |m| m[0].work.right = 1500),
            ("dpi", |m| m[0].dpi = 144),
            ("area", |m| m[0].rect.right = 2560),
            ("a display added", |m| {
                let rect = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
                m.push(Monitor { handle: 2, rect, work: rect, primary: false, dpi: 96 });
            }),
            ("another display", |m| m[0].handle = 5),
        ];
        for (what, change) in changes {
            let (mut f, w) = setup();
            f.hook = Some(Box::new(move |f: &Fake, c| {
                if c == Call::Placed(1) {
                    if let Some(m) = f.mons.borrow_mut().as_mut() {
                        change(m);
                    }
                }
            }));
            let b = run(&mut f, &w);
            assert_eq!(b.refused, Some(Refused::Changed), "{what}");
            assert_eq!(f.moves(), 1, "{what}: no move after it");
        }
        // The displays unreadable between moves: the same.
        let (mut f, w) = setup();
        f.hook = Some(Box::new(|f: &Fake, c| {
            if c == Call::Placed(1) {
                *f.mons.borrow_mut() = None;
            }
        }));
        assert_eq!(run(&mut f, &w).refused, Some(Refused::Changed));
        assert_eq!(f.moves(), 1);
    }

    /// confirm2 finding 3: an icon off the grid ("Align icons to grid"
    /// off) reaching into a free-looking cell keeps it.
    #[test]
    fn an_icon_off_the_grid_is_never_overlapped() {
        // The review's case: an icon at x 1480 (column 19 by its centre,
        // its box reaching 34 px into column 20, the first destination).
        let (mut f, w) = setup();
        f.at.borrow_mut().push(Some(pt(1480, 2)));
        let b = run(&mut f, &w);
        assert_eq!(b.refused, None);
        assert_eq!(b.moved, 2);
        assert!(f.placed.borrow().iter().all(|(_, to)| *to != FIRST), "{:?}", f.placed.borrow());
        assert!(f.boxes_apart(), "no two boxes overlap: {:?}", f.at.borrow());
        assert_eq!(f.at.borrow()[3], Some(pt(1480, 2)), "it stays");
        // Every icon off the grid by a few px: still no overlap.
        let (mut f, w) = setup();
        for p in f.at.borrow_mut().iter_mut().flatten() {
            p.x += 17;
            p.y += 9;
        }
        let b = run(&mut f, &w);
        assert!(f.boxes_apart(), "{:?} {:?}", b.refused, f.at.borrow());
    }

    #[test]
    fn a_mirrored_list_reads_right_to_left_and_moves_nothing() {
        let (mut f, w) = setup();
        f.mirrored = true;
        // Client points as a mirrored list has them: the same boxes on screen.
        let mirrored: Vec<Option<POINT>> = f.at.borrow().iter().map(|p| p.map(|p| pt(1920 - (p.x + 76), p.y))).collect();
        *f.at.borrow_mut() = mirrored;
        let b = run(&mut f, &w);
        assert_eq!(f.moves(), 0);
        assert_eq!(b.refused, Some(Refused::Mirrored));
        assert!(b.desk.rtl);
        assert_eq!(b.desk.icons[0], pt(2, 2), "the box's left edge, as left to right");
        assert_eq!(b.desk.icons[1], pt(2 + 24 * 76, 2));
    }

    #[test]
    fn mirrored_windows_map_their_points_flipped_and_back() {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_EX_LAYOUTRTL, WS_EX_TOOLWINDOW, WS_POPUP};
        unsafe {
            // Hidden windows of our own (never the desktop's).
            let make = |ex| CreateWindowExW(WS_EX_TOOLWINDOW | ex, w!("STATIC"), None, WS_POPUP, 100, 200, 300, 100, None, None, None, None).unwrap();
            let plain = make(Default::default());
            let rtl = make(WS_EX_LAYOUTRTL);
            assert_eq!(map_point(Some(plain), None, pt(10, 5)), Some(pt(110, 205)));
            let a = map_point(Some(rtl), None, pt(10, 5)).unwrap();
            let b = map_point(Some(rtl), None, pt(50, 5)).unwrap();
            assert!(a.x > b.x, "x runs right to left: {a:?} {b:?}");
            assert!((a.x - (100 + 300 - 10)).abs() <= 1, "{a:?}");
            assert_eq!(map_point(None, Some(rtl), a), Some(pt(10, 5)), "and back");
            // A 76 px box at client x 10: its left edge on screen.
            assert_eq!(box_left(a, (76, 96), true).x, a.x - 76);
            assert_eq!(box_left(pt(110, 205), (76, 96), false), pt(110, 205));
            let _ = DestroyWindow(plain);
            let _ = DestroyWindow(rtl);
            assert_eq!(map_point(Some(plain), None, pt(1, 1)), None, "a window gone: no answer");
        }
    }

    #[test]
    fn a_watch_frees_its_folder_ids_once_deregistered() {
        let calls = RefCell::new(Vec::new());
        Watch { id: 9, pidls: vec![11, 12] }.release(|id| calls.borrow_mut().push(format!("dereg {id}")), |p| calls.borrow_mut().push(format!("free {p}")));
        assert_eq!(*calls.borrow(), vec!["dereg 9", "free 11", "free 12"]);
        // Never registered: nothing to deregister, nothing to free.
        let calls = RefCell::new(Vec::new());
        Watch::default().release(|id| calls.borrow_mut().push(format!("dereg {id}")), |p| calls.borrow_mut().push(format!("free {p}")));
        assert!(calls.borrow().is_empty());
    }
}


#[cfg(test)] mod review_tests {
 use super::*;
 #[test] fn review_07_concurrent_startup_reserves_before_window_creation(){
  let book=std::sync::Arc::new(Mutex::new(Reservations::default()));
  let start=std::sync::Arc::new(std::sync::Barrier::new(3));
  let mut threads=vec![];
  for _ in 0..2 {let book=book.clone();let start=start.clone();threads.push(std::thread::spawn(move||{
   start.wait();let mut book=book.lock().unwrap();
   book.choose(vec![],|occupied|{
    let g=super::super::grid::Grid {ox:0,oy:0,cw:76,ch:96,cols:8,rows:4, ..Default::default() };
    super::super::widget::nearest_free(&g,RECT{left:8,top:8,right:144,bottom:184},(2,2),8,occupied)
   }).unwrap()
  }));}
  start.wait();let a=threads.remove(0).join().unwrap();let b=threads.remove(0).join().unwrap();
  assert_ne!(a.0,b.0);assert!(!super::super::widget::intersects(a.1,b.1));
  let mut book=book.lock().unwrap();assert_eq!(book.pending.len(),2);book.release(a.0);assert_eq!(book.pending.len(),1);
 }
}

/// Pending rectangles bridge selection and native window creation across threads.
#[derive(Default)]
struct Reservations {next:u64,pending:Vec<(u64,RECT)>}
impl Reservations {
 fn choose(&mut self,mut occupied:Vec<RECT>,choose:impl FnOnce(&[RECT])->Option<RECT>)->Option<(u64,RECT)> {
  occupied.extend(self.pending.iter().map(|(_,r)|*r));
  let rect=choose(&occupied)?;self.next+=1;let key=self.next;
  self.pending.push((key,rect));Some((key,rect))
 }
 fn release(&mut self,key:u64){self.pending.retain(|(id,_)|*id!=key);}
}
static RESERVATIONS:Mutex<Reservations>=Mutex::new(Reservations{next:0,pending:Vec::new()});
pub struct Reservation {key:u64,pub rect:RECT}
impl Drop for Reservation {fn drop(&mut self){RESERVATIONS.lock().unwrap_or_else(|e|e.into_inner()).release(self.key);}}
/// Select and reserve under one lock; retain the guard until commit/window placement.
pub fn reserve(but:Option<HWND>,choose:impl FnOnce(&[RECT])->Option<RECT>)->Option<Reservation>{
 let mut book=RESERVATIONS.lock().unwrap_or_else(|e|e.into_inner());
 let (key,rect)=book.choose(widget_rects(but),choose)?;Some(Reservation{key,rect})
}
