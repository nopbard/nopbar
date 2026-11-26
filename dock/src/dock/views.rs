//! Docks on several displays (Settings > Dock > "On multiple displays").
//!
//! Every dock window is a whole `Dock`: its own visuals, layout, input,
//! menus, previews, stacks, labels, auto-hide, AppBar and UIA tree. The
//! first one, the primary, also owns what exists once: the config and its
//! saves, the window list, the taskbar, shortcuts, the top bar, the tray
//! icon and the settings pipe. In "every display" mode each other display
//! gets a view: a dock that takes its config and window list from the
//! primary (WM_VIEW_SYNC, pulled whenever the primary's change) and hands
//! its own config edits (pins dragged on, off or around) to it. Nothing in
//! the model is kept twice by hand: a view's copy is only ever replaced by
//! the primary's.
//!
//! Icon surfaces live on one shelf for every dock (Composition surfaces are
//! device-wide), so an extra dock only adds its own visuals and brushes.
//!
//! Routing: `with_dock` acts on the dock whose window is handling the
//! current message (`enter`), else on the one the user touched last (menus,
//! previews and stacks opened from it call back), else on the primary.

use super::*;
use std::cell::Cell;
use std::sync::Arc;

use crate::config::DisplayMode;

use super::items::{icons_to_drop, SPARE_ICONS};

/// To a view: the primary's model changed; pull it.
pub(super) const WM_VIEW_SYNC: u32 = WM_APP + 82;
/// To the primary: config edits from a view are queued (it was busy).
pub(super) const WM_VIEW_FORWARD: u32 = WM_APP + 83;
/// To the primary: make the set of views match the displays and the setting.
pub(super) const WM_VIEW_RECONCILE: u32 = WM_APP + 84;
/// Retries of the three above while a dock is busy (a menu is open).
pub(super) const VIEW_SYNC_TIMER: usize = 64;
pub(super) const VIEW_FORWARD_TIMER: usize = 65;
pub(super) const VIEW_RECONCILE_TIMER: usize = 66;
const RETRY_MS: u32 = 60;

struct Entry {
    hwnd: isize,
    /// The monitor it is on (place_window keeps it up to date).
    mon: Cell<isize>,
    primary: bool,
    dock: Rc<RefCell<Dock>>,
    /// Its ticker's "a WM_TICK is on its way" (cleared without the borrow).
    tick_pending: Arc<AtomicBool>,
    /// A WM_VIEW_SYNC is on its way.
    sync_posted: Cell<bool>,
    /// Unique per dock made (a window handle can come back for another).
    gen: u64,
}

thread_local! {
    static VIEWS: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    /// The dock window whose message is being handled (0 = none).
    static CONTEXT: Cell<isize> = const { Cell::new(0) };
    /// Config edits from views waiting for the primary.
    static FORWARDED: RefCell<Vec<worker::ConfigEdit>> = const { RefCell::new(Vec::new()) };
    static RECONCILE_POSTED: Cell<bool> = const { Cell::new(false) };
    static NEXT_GEN: Cell<u64> = const { Cell::new(1) };
    /// Docks whose menu or stack runs a nested loop now (outside their
    /// borrow, so busy() does not see it): (window, generation).
    static MODAL: RefCell<Vec<Token>> = const { RefCell::new(Vec::new()) };
}

/// One dock, for as long as it lives: (window, generation).
pub(super) type Token = (isize, u64);

/// The dock the user touched last (pointer, keys, drops).
static ACTIVE: AtomicIsize = AtomicIsize::new(0);
/// The dock that opened the preview cards / the stack shown now (0 = the
/// primary). Kept apart from the message being handled: every dock's own
/// messages run with itself as the context.
static PREVIEW_OWNER: AtomicIsize = AtomicIsize::new(0);
static STACK_OWNER: AtomicIsize = AtomicIsize::new(0);

// ---- registry ---------------------------------------------------------------

pub(super) fn register(hwnd: HWND, mon: isize, primary: bool, dock: Dock, tick_pending: Arc<AtomicBool>) {
    let e = Entry {
        hwnd: hwnd.0 as isize,
        mon: Cell::new(mon),
        primary,
        dock: Rc::new(RefCell::new(dock)),
        tick_pending,
        sync_posted: Cell::new(false),
        gen: NEXT_GEN.with(|g| g.replace(g.get() + 1)),
    };
    VIEWS.with(|v| v.borrow_mut().push(e));
}

/// The dock `with_dock` would act on now, to come back to it later.
pub(super) fn token() -> Option<Token> {
    let h = target();
    find(h, |e| (e.hwnd, e.gen))
}

/// Which window a result for `token` goes to: that dock while the same one
/// lives, else none (never another dock: a menu picked on a display's dock
/// that went away must not unpin or hide on the main one). Pure, tested.
pub(super) fn token_target(token: Token, live: &[Token]) -> Option<isize> {
    live.contains(&token).then_some(token.0)
}

/// `f` on the dock of `token` (in its context), if it still lives.
pub(super) fn with_token<R>(token: Token, f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    let live: Vec<Token> = VIEWS.with(|v| v.borrow().iter().map(|e| (e.hwnd, e.gen)).collect());
    let h = token_target(token, &live)?;
    let _ctx = enter(h);
    with_dock_at(h, f)
}

/// While alive, dock `token`'s menu or stack is up (a nested loop): the
/// dock is not destroyed under it (reconcile waits and closes it first).
pub(super) struct Modal(Token);

impl Modal {
    pub(super) fn begin(token: Token) -> Self {
        MODAL.with(|m| m.borrow_mut().push(token));
        Modal(token)
    }
}

impl Drop for Modal {
    fn drop(&mut self) {
        MODAL.with(|m| {
            let mut m = m.borrow_mut();
            if let Some(i) = m.iter().rposition(|t| *t == self.0) {
                m.remove(i);
            }
        });
    }
}

/// Dock `hwnd` has a menu or stack up (see Modal).
fn modal_open(hwnd: isize) -> bool {
    MODAL.with(|m| m.borrow().iter().any(|t| t.0 == hwnd))
}

fn take(hwnd: isize) -> Option<Entry> {
    VIEWS.with(|v| {
        let mut v = v.borrow_mut();
        let i = v.iter().position(|e| e.hwnd == hwnd)?;
        Some(v.remove(i))
    })
}

fn find<R>(hwnd: isize, f: impl FnOnce(&Entry) -> R) -> Option<R> {
    VIEWS.with(|v| v.borrow().iter().find(|e| e.hwnd == hwnd).map(f))
}

fn rc(hwnd: isize) -> Option<Rc<RefCell<Dock>>> {
    find(hwnd, |e| e.dock.clone())
}

pub(super) fn primary_hwnd() -> isize {
    DOCK_HWND.load(Ordering::SeqCst)
}

/// One of our dock windows.
pub(super) fn is_dock(h: HWND) -> bool {
    !h.is_invalid() && find(h.0 as isize, |_| ()).is_some()
}

/// The views (docks other than the primary).
pub(super) fn views() -> Vec<isize> {
    VIEWS.with(|v| v.borrow().iter().filter(|e| !e.primary).map(|e| e.hwnd).collect())
}

/// Where the next `with_dock` goes (see the module comment).
pub(super) fn target() -> isize {
    let c = CONTEXT.with(|c| c.get());
    if c != 0 && find(c, |_| ()).is_some() {
        return c;
    }
    let a = ACTIVE.load(Ordering::SeqCst);
    if a != 0 && find(a, |_| ()).is_some() {
        return a;
    }
    primary_hwnd()
}

/// The dock window the preview cards belong to (their buttons and
/// keyboard choices are posted there).
pub(super) fn ui_hwnd() -> HWND {
    HWND(owner(&PREVIEW_OWNER) as *mut _)
}

fn owner(slot: &AtomicIsize) -> isize {
    let o = slot.load(Ordering::SeqCst);
    if o != 0 && find(o, |_| ()).is_some() {
        o
    } else {
        primary_hwnd()
    }
}

/// Dock `hwnd` opens preview cards.
pub(super) fn previews_from(hwnd: HWND) {
    PREVIEW_OWNER.store(hwnd.0 as isize, Ordering::SeqCst);
}

/// Dock `hwnd` opens a stack.
pub(super) fn stack_from(hwnd: HWND) {
    STACK_OWNER.store(hwnd.0 as isize, Ordering::SeqCst);
}

/// Dock `hwnd` opened the stack shown now (or last). By the raw owner id
/// first: a view being destroyed is already out of the registry, and must
/// still close what it opened (not leave it to the primary).
pub(super) fn owns_stack(hwnd: HWND) -> bool {
    owns(&STACK_OWNER, hwnd.0 as isize)
}

fn owns(slot: &AtomicIsize, hwnd: isize) -> bool {
    let o = slot.load(Ordering::SeqCst);
    owns_by(o, hwnd, primary_hwnd(), o != 0 && find(o, |_| ()).is_some())
}

/// Pure: owner slot `o` (0 = the primary), asked by `hwnd`, with the
/// primary `primary` and whether `o` is still registered.
fn owns_by(o: isize, hwnd: isize, primary: isize, o_registered: bool) -> bool {
    if o != 0 && o == hwnd {
        return true;
    }
    // An unset or dead owner falls back to the primary.
    hwnd == primary && (o == 0 || !o_registered)
}

/// While alive, `with_dock` goes to this dock window.
pub(super) struct Context(isize);

pub(super) fn enter(hwnd: isize) -> Context {
    Context(CONTEXT.with(|c| c.replace(hwnd)))
}

impl Drop for Context {
    fn drop(&mut self) {
        CONTEXT.with(|c| c.set(self.0));
    }
}

/// The user acts on this dock (its menus and previews call back to it).
pub(super) fn set_active(hwnd: isize) {
    if find(hwnd, |_| ()).is_some() {
        ACTIVE.store(hwnd, Ordering::SeqCst);
    }
}

/// Dock `hwnd` opened the preview cards shown now (or last); see owns_stack.
pub(super) fn owns_preview(hwnd: HWND) -> bool {
    owns(&PREVIEW_OWNER, hwnd.0 as isize)
}

pub(super) fn with_dock_at<R>(hwnd: isize, f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    let rc = rc(hwnd)?;
    let mut g = rc.try_borrow_mut().ok()?;
    Some(f(&mut g))
}

pub(super) fn with_primary<R>(f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    with_dock_at(primary_hwnd(), f)
}

pub(super) fn primary_busy() -> bool {
    rc(primary_hwnd()).is_some_and(|r| r.try_borrow_mut().is_err())
}

/// Dock `hwnd` is borrowed now (one of its menus or stacks runs a nested
/// loop, or its own message is being handled further up).
fn busy(hwnd: isize) -> bool {
    rc(hwnd).is_some_and(|r| r.try_borrow_mut().is_err())
}

/// Some view is busy, or has a menu or stack up (see busy, Modal).
pub(super) fn any_view_busy() -> bool {
    views().into_iter().any(|h| busy(h) || modal_open(h))
}

/// `f` on every dock, the primary first. False if one of them was busy
/// (it was skipped; the caller retries).
pub(super) fn for_each(mut f: impl FnMut(&mut Dock)) -> bool {
    let all: Vec<Rc<RefCell<Dock>>> = VIEWS.with(|v| v.borrow().iter().map(|e| e.dock.clone()).collect());
    let mut done = true;
    for rc in all {
        match rc.try_borrow_mut() {
            Ok(mut d) => f(&mut d),
            Err(_) => done = false,
        }
    }
    done
}

/// `f` on every view (not the primary).
pub(super) fn for_views(mut f: impl FnMut(&mut Dock)) -> bool {
    let all: Vec<Rc<RefCell<Dock>>> = VIEWS.with(|v| v.borrow().iter().filter(|e| !e.primary).map(|e| e.dock.clone()).collect());
    let mut done = true;
    for rc in all {
        match rc.try_borrow_mut() {
            Ok(mut d) => f(&mut d),
            Err(_) => done = false,
        }
    }
    done
}

/// Clears a dock's "tick on its way" flag (WM_TICK, before the borrow).
pub(super) fn tick_taken(hwnd: isize) {
    find(hwnd, |e| e.tick_pending.store(false, Ordering::SeqCst));
}

pub(super) fn note_monitor(hwnd: HWND, mon: isize) {
    find(hwnd.0 as isize, |e| e.mon.set(mon));
}

/// The dock on monitor `mon`, if one is there.
fn on_monitor(mon: isize) -> Option<isize> {
    VIEWS.with(|v| {
        let v = v.borrow();
        // The primary first: it may share a monitor with a view for a moment.
        v.iter().filter(|e| e.mon.get() == mon).min_by_key(|e| !e.primary).map(|e| e.hwnd)
    })
}

/// The monitor window `h` is on; a minimized window counts where it would
/// be restored (like the taskbar's "where the window is open").
pub(super) fn window_monitor(h: HWND) -> isize {
    use windows::Win32::Graphics::Gdi::{MonitorFromRect, MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
    unsafe {
        if IsIconic(h).as_bool() {
            let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            if GetWindowPlacement(h, &mut wp).is_ok() {
                return MonitorFromRect(&wp.rcNormalPosition, MONITOR_DEFAULTTONEAREST).0 as isize;
            }
        }
        MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST).0 as isize
    }
}

/// The dock on window `h`'s monitor (the primary when there is none).
pub(super) fn for_window(h: HWND) -> isize {
    on_monitor(window_monitor(h)).unwrap_or_else(primary_hwnd)
}

/// The dock on the monitor of the window in front (keyboard focus mode).
pub(super) fn for_foreground() -> isize {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() || is_dock(fg) {
        return if fg.is_invalid() { primary_hwnd() } else { fg.0 as isize };
    }
    for_window(fg)
}

// ---- the primary's model, pulled by the views -----------------------------

/// What a view takes from the primary.
pub(super) struct ModelSnap {
    pub cfg: Config,
    /// The whole window list (each view filters it for itself).
    pub wins: Vec<apps::AppWindow>,
    pub wins_gen: u64,
    pub active: HWND,
    /// The settings window previews changes on the primary.
    pub previewing: bool,
}

/// What a new view starts from.
pub(super) struct Seed {
    pub gfx: Rc<Gfx>,
    pub theme_raw: Theme,
    pub theme: Theme,
    pub sys: crate::syslook::SysLook,
    pub shelf: Rc<RefCell<IconShelf>>,
    pub snap: ModelSnap,
}

/// The primary's model changed: every view pulls it (coalesced).
pub(super) fn schedule_sync() {
    VIEWS.with(|v| {
        for e in v.borrow().iter().filter(|e| !e.primary && !e.sync_posted.get()) {
            if unsafe { PostMessageW(Some(HWND(e.hwnd as *mut _)), WM_VIEW_SYNC, WPARAM(0), LPARAM(0)) }.is_ok() {
                e.sync_posted.set(true);
            }
        }
    });
}

/// WM_VIEW_SYNC (or its retry) on view `hwnd`.
pub(super) unsafe fn on_sync(hwnd: HWND) {
    let me = hwnd.0 as isize;
    find(me, |e| e.sync_posted.set(false));
    let _ = KillTimer(Some(hwnd), VIEW_SYNC_TIMER);
    let done = with_primary(|p| p.model_snap()).and_then(|s| with_dock_at(me, |d| d.pull(s)));
    if done.is_none() && find(me, |_| ()).is_some() {
        SetTimer(Some(hwnd), VIEW_SYNC_TIMER, RETRY_MS, None);
    }
}

/// A view's own config edit goes to the primary (which saves it and syncs
/// every view back). Right away when it can, else queued.
pub(super) fn forward(edit: worker::ConfigEdit) {
    let e = edit.clone();
    if with_primary(move |p| p.update_config(move |c| e(c))).is_some() {
        return;
    }
    FORWARDED.with(|q| q.borrow_mut().push(edit));
    let p = HWND(primary_hwnd() as *mut _);
    unsafe {
        let _ = PostMessageW(Some(p), WM_VIEW_FORWARD, WPARAM(0), LPARAM(0));
    }
}

/// WM_VIEW_FORWARD (or its retry) on the primary.
pub(super) unsafe fn on_forward(hwnd: HWND) {
    let _ = KillTimer(Some(hwnd), VIEW_FORWARD_TIMER);
    let edits: Vec<worker::ConfigEdit> = FORWARDED.with(|q| std::mem::take(&mut *q.borrow_mut()));
    if edits.is_empty() {
        return;
    }
    let kept = edits.clone();
    let done = with_primary(move |p| {
        for e in edits {
            p.update_config(move |c| e(c));
        }
    });
    if done.is_none() {
        FORWARDED.with(|q| {
            let mut q = q.borrow_mut();
            let newer = std::mem::take(&mut *q);
            *q = kept;
            q.extend(newer);
        });
        SetTimer(Some(hwnd), VIEW_FORWARD_TIMER, RETRY_MS, None);
    }
}

// ---- which views exist ------------------------------------------------------

/// What reconcile changes: views to close (indexes into the current list)
/// and monitors that get a new one.
#[derive(Debug, PartialEq, Eq, Default)]
pub(super) struct Plan {
    pub remove: Vec<usize>,
    pub add: Vec<isize>,
}

/// Views wanted for `mode` on `monitors` (handle, is the main display),
/// with the primary dock on `primary_on`, given views now on `views_on`.
/// Only "every display" has views: one per monitor other than the
/// primary's. A view whose monitor went away, became the primary's or
/// has a twin goes.
pub(super) fn plan_views(mode: DisplayMode, monitors: &[(isize, bool)], primary_on: isize, views_on: &[isize]) -> Plan {
    let want: Vec<isize> = if mode == DisplayMode::Every { monitors.iter().map(|m| m.0).filter(|&m| m != primary_on).collect() } else { Vec::new() };
    let mut plan = Plan::default();
    let mut kept: Vec<isize> = Vec::new();
    for (i, m) in views_on.iter().enumerate() {
        if want.contains(m) && !kept.contains(m) {
            kept.push(*m);
        } else {
            plan.remove.push(i);
        }
    }
    plan.add = want.into_iter().filter(|m| !kept.contains(m)).collect();
    plan
}

/// Where the primary dock belongs: the main display unless it follows the
/// pointer (then where it is, if that monitor is still there).
pub(super) fn primary_home(mode: DisplayMode, monitors: &[(isize, bool)], now_on: isize) -> Option<isize> {
    let main = monitors.iter().find(|m| m.1).or(monitors.first()).map(|m| m.0)?;
    if mode == DisplayMode::Follow && monitors.iter().any(|m| m.0 == now_on) {
        return Some(now_on);
    }
    Some(main)
}

/// Make the views match the displays and the setting (coalesced, run on
/// the primary's window outside every borrow).
pub(super) fn post_reconcile() {
    if RECONCILE_POSTED.with(|p| p.replace(true)) {
        return;
    }
    let p = HWND(primary_hwnd() as *mut _);
    if unsafe { PostMessageW(Some(p), WM_VIEW_RECONCILE, WPARAM(0), LPARAM(0)) }.is_err() {
        RECONCILE_POSTED.with(|p| p.set(false));
    }
}

/// WM_VIEW_RECONCILE (or its retry). Surviving views are re-placed (a
/// display or work-area change usually brought us here).
pub(super) unsafe fn reconcile(hwnd: HWND) {
    RECONCILE_POSTED.with(|p| p.set(false));
    let _ = KillTimer(Some(hwnd), VIEW_RECONCILE_TIMER);
    if SHUTTING_DOWN.load(Ordering::SeqCst) {
        return;
    }
    let Some((mode, now_on)) = with_primary(|p| (p.cfg.dock.display_mode(), p.mon)) else {
        SetTimer(Some(hwnd), VIEW_RECONCILE_TIMER, RETRY_MS, None);
        return;
    };
    let monitors: Vec<(isize, bool)> = crate::monitors::all().iter().map(|m| (m.handle, m.primary)).collect();
    if monitors.is_empty() {
        return;
    }
    // Who each display is (its own dock settings go by it): read only
    // with a dock on every display.
    let displays = if mode == DisplayMode::Every { crate::displays::current() } else { Vec::new() };
    let key_of = |mon: isize| crate::displays::key_of(&displays, mon);
    // Another display became the main one: Dock 1's settings move to it
    // and the old main keeps its dock as a display of its own (config
    // hand_over_main). Saved like any edit; the views follow its sync.
    if let Some(main) = displays.iter().find(|d| d.primary) {
        let key = main.key.clone();
        let changed = with_primary(|p| p.cfg.dock.main_display != key).unwrap_or(false);
        if changed {
            let old = with_primary(|p| p.cfg.dock.main_display.clone()).unwrap_or_default();
            let old_name = displays.iter().find(|d| d.key == old).map(|d| d.name.clone()).unwrap_or_default();
            crate::log!("dock: main display is {key} now (was {})", if old.is_empty() { "unknown" } else { &old });
            with_primary(move |p| p.update_config(move |c| {
                c.dock.hand_over_main(&key, &old_name);
            }));
        }
    }
    // The primary first: views go everywhere else.
    let home = primary_home(mode, &monitors, now_on).unwrap_or(now_on);
    if home != now_on {
        with_primary(|p| p.go_to_monitor(home));
    }
    let primary_on = find(primary_hwnd(), |e| e.mon.get()).unwrap_or(home);
    let current: Vec<(isize, isize)> = VIEWS.with(|v| v.borrow().iter().filter(|e| !e.primary).map(|e| (e.hwnd, e.mon.get())).collect());
    let on: Vec<isize> = current.iter().map(|c| c.1).collect();
    let plan = plan_views(mode, &monitors, primary_on, &on);
    if plan.remove.iter().any(|&i| busy(current[i].0) || modal_open(current[i].0)) {
        // A view that goes has a menu or stack up (a nested loop further
        // up this stack) or is borrowed: it is closed first, and the view
        // goes once the loop has ended. Destroyed under it, its shutdown
        // was skipped and the pick went to another dock.
        crate::popup::cancel();
        stack::cancel();
        SetTimer(Some(hwnd), VIEW_RECONCILE_TIMER, RETRY_MS, None);
        return;
    }
    for &i in &plan.remove {
        destroy_view(current[i].0);
    }
    if !plan.add.is_empty() {
        let Some(seed) = with_primary(|p| p.seed()) else {
            SetTimer(Some(hwnd), VIEW_RECONCILE_TIMER, RETRY_MS, None);
            return;
        };
        for &m in &plan.add {
            create_view(m, &seed, key_of(m));
        }
        // A new dock on a display with a fullscreen app steps aside.
        schedule_fullscreen_check();
    }
    // Survivors: their monitor may have moved or changed DPI.
    let survivors: Vec<isize> = current.iter().enumerate().filter(|(i, _)| !plan.remove.contains(i)).map(|(_, c)| c.0).collect();
    for h in survivors {
        let key = find(h, |e| e.mon.get()).and_then(key_of);
        with_dock_at(h, |d| d.set_display(key));
        // The newest model first (the change that brought us here may not
        // have reached the view yet), then its AppBar and place for it.
        if let Some(snap) = with_primary(|p| p.model_snap()) {
            with_dock_at(h, |d| d.pull(snap));
        }
        with_dock_at(h, |d| {
            d.apply_taskbar_mode();
            d.replace_on_monitor();
        });
    }
    if !plan.remove.is_empty() || !plan.add.is_empty() {
        crate::log!("dock: {} dock(s) on {} display(s) ({:?})", views().len() + 1, monitors.len(), mode);
    }
}

/// Every view goes (quitting, a new rendering device).
pub(super) unsafe fn destroy_all() {
    for h in views() {
        destroy_view(h);
    }
}

unsafe fn create_view(mon: isize, seed: &Seed, display: Option<String>) -> Option<isize> {
    let before = crate::log::debug_enabled().then(crate::res::usage);
    let hinst = GetModuleHandleW(None).ok()?;
    let hwnd = match CreateWindowExW(
        WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
        w!("NopBarWindow"),
        w!("Dock"),
        WS_POPUP,
        0,
        0,
        100,
        100,
        None,
        None,
        Some(hinst.into()),
        None,
    ) {
        Ok(h) => h,
        Err(e) => {
            crate::log!("dock: cannot create the dock for another display: {e}");
            return None;
        }
    };
    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
    let ticker = Ticker::new(hwnd);
    let pending = ticker.pending.clone();
    let dock = {
        // Messages sent while it is built (placement) are its own.
        let _ctx = enter(hwnd.0 as isize);
        Dock::new_view(hwnd, ticker, seed, mon, display)
    };
    match dock {
        Ok(d) => register(hwnd, mon, false, d, pending),
        Err(e) => {
            crate::log!("dock: cannot build the dock for another display: {e}");
            let _ = DestroyWindow(hwnd);
            return None;
        }
    }
    {
        let _ctx = enter(hwnd.0 as isize);
        // Behind a fullscreen app already in front (a display added, or
        // the views built again after a GPU reset, often mid-game), before
        // it is ever shown.
        // Placed and laid out first (view_started), then put in its place
        // and shown with its shadow: nothing of it shows before.
        let shown = with_dock_at(hwnd.0 as isize, |d| {
            d.view_started();
            d.reveal();
        });
        if shown.is_none() {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    drag::register(hwnd);
    if let Some(b) = before {
        let a = crate::res::usage();
        crate::debug_log!(
            "dock: view for monitor {mon:#x}: +{} KB private, +{} KB working set, +{} handles, +{} USER objects (shared icons: {})",
            a.private_kb as i64 - b.private_kb as i64,
            a.working_kb as i64 - b.working_kb as i64,
            a.handles as i64 - b.handles as i64,
            a.user as i64 - b.user as i64,
            seed.shelf.borrow().len()
        );
    }
    Some(hwnd.0 as isize)
}

unsafe fn destroy_view(hwnd: isize) {
    // Its Split View hint goes even if the dock is borrowed now.
    super::split::teardown(hwnd);
    let Some(e) = take(hwnd) else { return };
    if let Ok(mut d) = e.dock.try_borrow_mut() {
        let _ctx = enter(hwnd);
        d.view_shutdown();
    }
    let h = HWND(hwnd as *mut _);
    let _ = windows::Win32::System::Ole::RevokeDragDrop(h);
    crate::a11y::forget(h);
    forget_minrects(hwnd);
    let _ = DestroyWindow(h);
    for slot in [&ACTIVE, &PREVIEW_OWNER, &STACK_OWNER] {
        if slot.load(Ordering::SeqCst) == hwnd {
            slot.store(0, Ordering::SeqCst);
        }
    }
    crate::log!("dock: closed the dock on monitor {:#x}", e.mon.get());
}

/// Messages a view leaves to the primary (broadcasts every top-level
/// window gets, session and quit). Some(answer) when handled here.
pub(super) unsafe fn view_message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let tb = WM_TASKBAR_CREATED.load(Ordering::SeqCst);
    if tb != 0 && msg == tb {
        // The primary re-registers every dock's AppBar.
        return Some(LRESULT(0));
    }
    match msg {
        WM_VIEW_SYNC => {
            on_sync(hwnd);
            Some(LRESULT(0))
        }
        WM_TIMER if wp.0 == VIEW_SYNC_TIMER => {
            on_sync(hwnd);
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED_MSG => Some(DefWindowProcW(hwnd, msg, wp, lp)),
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            // The primary looks at every display (and reconciles).
            if !DISPLAY_PENDING.swap(true, Ordering::SeqCst) {
                SetTimer(Some(HWND(primary_hwnd() as *mut _)), DISPLAY_TIMER, DISPLAY_DEBOUNCE_MS, None);
            }
            Some(DefWindowProcW(hwnd, msg, wp, lp))
        }
        WM_QUERYENDSESSION | WM_POWERBROADCAST => Some(LRESULT(1)),
        WM_ENDSESSION | WM_DESTROY | WM_APPLY_TASKBAR | WM_QUIT_REQUEST | WM_DEV_HANG | WM_DEV_PANIC => Some(LRESULT(0)),
        _ => None,
    }
}

// ---- minimize targets ---------------------------------------------------------

/// Where one dock draws its running apps' icons (screen px).
#[derive(Clone, Debug, Default)]
pub(super) struct MinEntry {
    pub dock: isize,
    pub mon: isize,
    pub primary: bool,
    pub rects: Vec<(Vec<HWND>, RECT)>,
}

/// The icon a window minimizes to: on the dock of the window's own
/// monitor if that dock shows it, else the primary's, else any dock's.
/// `wins`: the window and its root window.
pub(super) fn pick_minrect(entries: &[MinEntry], wins: &[HWND], mon: isize) -> Option<RECT> {
    let has = |e: &MinEntry| e.rects.iter().find(|(w, _)| wins.iter().any(|h| !h.is_invalid() && w.contains(h))).map(|(_, r)| *r);
    entries
        .iter()
        .filter(|e| e.mon == mon)
        .find_map(has)
        .or_else(|| entries.iter().filter(|e| e.primary).find_map(has))
        .or_else(|| entries.iter().find_map(has))
}

fn forget_minrects(hwnd: isize) {
    MINRECTS.with(|m| m.borrow_mut().retain(|e| e.dock != hwnd));
}

// ---- running apps per display -------------------------------------------------

/// The windows a dock on `mon` shows: all of them, or with "show only apps
/// on this display" just those on its monitor.
pub(super) fn windows_for<T: Clone>(wins: &[T], only_on: Option<isize>, monitor_of: impl Fn(&T) -> isize) -> Vec<T> {
    match only_on {
        None => wins.to_vec(),
        Some(mon) => wins.iter().filter(|w| monitor_of(w) == mon).cloned().collect(),
    }
}

// ---- shared icon surfaces -----------------------------------------------------

/// One icon's pictures (sharp.rs), one set per dress and resting size the
/// docks showing it need (system apps and stacks keep their shape while
/// other apps sharing the picture get a backplate: other sizes).
#[derive(Clone)]
pub(super) struct Pic {
    pub info: icons::IconInfo,
    pub sizes: Vec<PicSize>,
    /// The bundled app icon uses the brand silhouette and its own margins.
    pub bundled: bool,
    /// One of NopBar's drawn tiles (icons::Source::Drawn): the look it was
    /// drawn for (a dock with another look asks for its own). None: any
    /// other source.
    pub drawn: Option<icons::Appearance>,
}

/// An icon drawn at each step from rest to the peak (sharp::step_sides)
/// for one dress and resting slot.
#[derive(Clone)]
pub(super) struct PicSize {
    pub dress: sharp::Dress,
    /// The resting slot (device px) it was drawn for.
    pub slot: u32,
    /// (side px, picture), ascending, the peak one last (always there).
    pub steps: Vec<(u32, CompositionDrawingSurface)>,
    /// A smaller step could not be drawn (the next larger one shows for
    /// it meanwhile; drawn again later).
    pub failed: bool,
}

impl Pic {
    /// The pictures for this dress and resting slot.
    pub fn get(&self, dress: sharp::Dress, slot: u32) -> Option<&PicSize> {
        self.sizes.iter().find(|p| p.dress == dress && p.slot == slot)
    }

    /// Whatever is closest, to show until the right ones are drawn.
    pub fn closest(&self, dress: sharp::Dress) -> Option<&PicSize> {
        self.sizes.iter().find(|p| p.dress == dress).or(self.sizes.first())
    }
}

pub(super) struct ShelfEntry {
    pub pic: Pic,
    /// Changes when a new picture replaces it (views re-dress their icons).
    pub ver: u64,
    /// Last put on an item (for pruning).
    pub used: u64,
}

/// Icon pictures by (icon key, peak px), for every dock, plus the squircle
/// masks by size. Each dock says which sizes and keys it uses, so one
/// dock's pruning never drops another's icons.
#[derive(Default)]
pub(super) struct IconShelf {
    map: HashMap<(String, i32), ShelfEntry>,
    clock: u64,
    masks: HashMap<i32, CompositionDrawingSurface>,
    /// Each dock's icon sizes: peak (the key) and resting slot (masks).
    sizes: HashMap<isize, (i32, i32)>,
    live: HashMap<isize, HashSet<(String, i32)>>,
}

impl IconShelf {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// The pictures for `key`, marked as used now, and their version.
    pub fn take(&mut self, key: &(String, i32)) -> Option<(Pic, u64)> {
        self.clock += 1;
        let now = self.clock;
        self.map.get_mut(key).map(|e| {
            e.used = now;
            (e.pic.clone(), e.ver)
        })
    }

    /// The version of `key` without touching it (0 = none).
    pub fn ver(&self, key: &(String, i32)) -> u64 {
        self.map.get(key).map(|e| e.ver).unwrap_or(0)
    }

    /// New pictures for `key`; returns their version.
    pub fn put(&mut self, key: (String, i32), pic: Pic) -> u64 {
        self.clock += 1;
        let ver = self.clock;
        self.map.insert(key, ShelfEntry { pic, ver, used: ver });
        ver
    }

    /// The dresses and resting slots `key` was drawn for that some dock
    /// still uses: drawn again with it (a dock never drops another's).
    pub fn drawn_for(&self, key: &(String, i32)) -> Vec<(sharp::Dress, u32)> {
        let Some(e) = self.map.get(key) else { return Vec::new() };
        let slots: HashSet<u32> = self.sizes.values().filter(|s| s.0 == key.1).map(|s| s.1.max(0) as u32).collect();
        e.pic.sizes.iter().filter(|p| slots.contains(&p.slot)).map(|p| (p.dress, p.slot)).collect()
    }

    /// The resting slots of the docks drawing icons at peak `px`.
    pub fn slots_at(&self, px: i32) -> Vec<u32> {
        self.sizes.values().filter(|s| s.0 == px).map(|s| s.1.max(1) as u32).collect()
    }

    /// Dock `dock` draws icons at peak `px` and resting slot `slot`: sizes
    /// no dock uses any more go.
    pub fn use_size(&mut self, dock: isize, px: i32, slot: i32) {
        self.sizes.insert(dock, (px, slot));
        let peaks: HashSet<i32> = self.sizes.values().map(|s| s.0).collect();
        // Backplate masks: every step of every dock's sizes.
        let masks: HashSet<i32> = self
            .sizes
            .values()
            .flat_map(|s| sharp::step_slots(s.1.max(1) as u32, s.0.max(1) as u32))
            .map(|v| v as i32)
            .collect();
        self.map.retain(|(_, s), _| peaks.contains(s));
        self.masks.retain(|s, _| masks.contains(s));
    }

    /// Dock `dock` shows these icons now: keeps every dock's icons plus a
    /// few recently gone ones (items::icons_to_drop).
    pub fn prune(&mut self, dock: isize, live: HashSet<(String, i32)>) {
        self.live.insert(dock, live);
        let all: HashSet<(String, i32)> = self.live.values().flatten().cloned().collect();
        let cached: Vec<((String, i32), u64)> = self.map.iter().map(|(k, v)| (k.clone(), v.used)).collect();
        for k in icons_to_drop(&cached, &all, SPARE_ICONS) {
            self.map.remove(&k);
        }
    }

    /// Dock `dock` is gone.
    pub fn forget(&mut self, dock: isize) {
        self.live.remove(&dock);
        self.sizes.remove(&dock);
    }

    pub fn mask(&self, px: i32) -> Option<CompositionDrawingSurface> {
        self.masks.get(&px).cloned()
    }

    pub fn put_mask(&mut self, px: i32, s: CompositionDrawingSurface) {
        self.masks.insert(px, s);
    }

    /// The rendering device was replaced: every picture is gone.
    pub fn clear(&mut self) {
        self.map.clear();
        self.masks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_menu_pick_goes_only_to_the_dock_it_was_opened_on() {
        let (primary, view) = ((100, 1), (200, 2));
        // Menu open on the second display's dock; that display goes.
        let after = [primary];
        assert_eq!(token_target(view, &after), None, "dropped, never the primary's settings");
        assert_eq!(token_target(view, &[primary, view]), Some(200));
        // Its window handle came back for a newer dock: still not it.
        assert_eq!(token_target(view, &[primary, (200, 3)]), None);
        assert_eq!(token_target(primary, &after), Some(100));
    }

    #[test]
    fn a_closing_dock_still_owns_what_it_opened() {
        // The view (200) is out of the registry while it shuts down.
        assert!(owns_by(200, 200, 100, false), "its own stack, not the primary's");
        assert!(!owns_by(300, 200, 100, true), "another dock's");
        // Unset or dead owners fall back to the primary.
        assert!(owns_by(0, 100, 100, false));
        assert!(owns_by(300, 100, 100, false));
        assert!(!owns_by(300, 100, 100, true));
        assert!(!owns_by(0, 200, 100, false));
    }

    const A: isize = 0xA;
    const B: isize = 0xB;
    const C: isize = 0xC;

    fn mons(list: &[(isize, bool)]) -> Vec<(isize, bool)> {
        list.to_vec()
    }

    #[test]
    fn only_every_display_has_views() {
        let m = mons(&[(A, true), (B, false), (C, false)]);
        assert_eq!(plan_views(DisplayMode::Follow, &m, A, &[]), Plan::default());
        assert_eq!(plan_views(DisplayMode::Main, &m, A, &[]), Plan::default());
        assert_eq!(plan_views(DisplayMode::Every, &m, A, &[]), Plan { remove: vec![], add: vec![B, C] });
        // Switched back to one dock: every view goes.
        assert_eq!(plan_views(DisplayMode::Main, &m, A, &[B, C]), Plan { remove: vec![0, 1], add: vec![] });
    }

    #[test]
    fn displays_coming_and_going() {
        // A display was added.
        let m = mons(&[(A, true), (B, false), (C, false)]);
        assert_eq!(plan_views(DisplayMode::Every, &m, A, &[B]), Plan { remove: vec![], add: vec![C] });
        // One went away: its view goes, the other stays.
        let m = mons(&[(A, true), (C, false)]);
        assert_eq!(plan_views(DisplayMode::Every, &m, A, &[B, C]), Plan { remove: vec![0], add: vec![] });
        // Nothing changed: nothing to do.
        assert_eq!(plan_views(DisplayMode::Every, &m, A, &[C]), Plan::default());
        // A single display: no views at all.
        assert_eq!(plan_views(DisplayMode::Every, &mons(&[(A, true)]), A, &[]), Plan::default());
    }

    #[test]
    fn main_display_changes_hands() {
        // B became the main display: the primary dock moves there, the view
        // on B goes and A gets one.
        let m = mons(&[(B, true), (A, false)]);
        let home = primary_home(DisplayMode::Every, &m, A).unwrap();
        assert_eq!(home, B);
        assert_eq!(plan_views(DisplayMode::Every, &m, home, &[B]), Plan { remove: vec![0], add: vec![A] });
        // Main only: also to the new main display; following stays put.
        assert_eq!(primary_home(DisplayMode::Main, &m, A), Some(B));
        assert_eq!(primary_home(DisplayMode::Follow, &m, A), Some(A));
        // Following, but its monitor is gone: the main display.
        assert_eq!(primary_home(DisplayMode::Follow, &mons(&[(B, true)]), A), Some(B));
        assert_eq!(primary_home(DisplayMode::Every, &[], A), None);
    }

    #[test]
    fn twin_views_are_closed() {
        let m = mons(&[(A, true), (B, false)]);
        assert_eq!(plan_views(DisplayMode::Every, &m, A, &[B, B]), Plan { remove: vec![1], add: vec![] });
        // A view on the primary's own monitor goes too.
        assert_eq!(plan_views(DisplayMode::Every, &m, A, &[A, B]), Plan { remove: vec![0], add: vec![] });
    }

    fn h(n: isize) -> HWND {
        HWND(n as *mut _)
    }

    fn r(x: i32) -> RECT {
        RECT { left: x, top: 0, right: x + 10, bottom: 10 }
    }

    #[test]
    fn minimize_goes_to_the_dock_on_the_windows_monitor() {
        let w = h(100);
        let entries = vec![
            MinEntry { dock: 1, mon: A, primary: true, rects: vec![(vec![w], r(0))] },
            MinEntry { dock: 2, mon: B, primary: false, rects: vec![(vec![w], r(1000))] },
        ];
        assert_eq!(pick_minrect(&entries, &[w], B), Some(r(1000)));
        assert_eq!(pick_minrect(&entries, &[w], A), Some(r(0)));
        // A monitor without a dock: the primary's.
        assert_eq!(pick_minrect(&entries, &[w], C), Some(r(0)));
        // Its own monitor's dock does not show it ("only apps on this
        // display" moved it): another dock that does.
        let entries = vec![
            MinEntry { dock: 1, mon: A, primary: true, rects: vec![] },
            MinEntry { dock: 2, mon: B, primary: false, rects: vec![(vec![w], r(1000))] },
        ];
        assert_eq!(pick_minrect(&entries, &[w], A), Some(r(1000)));
        // Found by its root window; unknown windows have no target.
        let child = h(101);
        assert_eq!(pick_minrect(&entries, &[child, w], A), Some(r(1000)));
        assert_eq!(pick_minrect(&entries, &[child], A), None);
        assert_eq!(pick_minrect(&[], &[w], A), None);
    }

    #[test]
    fn running_apps_per_display() {
        // (window, monitor)
        let wins = vec![(1, A), (2, B), (3, A), (4, C)];
        let on = |w: &(i32, isize)| w.1;
        assert_eq!(windows_for(&wins, None, on), wins);
        assert_eq!(windows_for(&wins, Some(A), on), vec![(1, A), (3, A)]);
        assert_eq!(windows_for(&wins, Some(B), on), vec![(2, B)]);
        assert!(windows_for(&wins, Some(0xD), on).is_empty());
    }
}
