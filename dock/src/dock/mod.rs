//! The dock window: items, layout, magnification, input and animation.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::f32::consts::PI;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::thread::Thread;
use std::time::Instant;

use windows::core::{w, Interface, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmFlush, DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, SetWindowRgn, MONITORINFO,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionBrush, CompositionDrawingSurface, CompositionRoundedRectangleGeometry,
    CompositionStretch, CompositionSurfaceBrush, Compositor, ContainerVisual, SpriteVisual,
};
use windows_numerics::{Vector2, Vector3};

use crate::apps::{self, AppId};
use crate::config::{Config, Indicator, PinEntry, Position};
use crate::gfx::Gfx;
use crate::icons;
use crate::taskbar;
use crate::theme::{Rgba, Theme};
use crate::worker::{self, Event, Workers};

mod access;
mod badges;
mod digits;
mod dividers;
mod drag;
mod edge;
mod full_quit;
mod fx;
mod genie_shared;
mod genie_thumb;
mod gpu;
mod hung;
mod input;
pub(crate) use input::covers_as_fullscreen;
mod items;
mod label;
mod layout;
mod menu;
mod ontop;
mod preview;
mod ring;
mod sensors;
mod session;
mod shadow;
mod sharp;
mod shell;
mod snap;
mod sound;
mod split;
mod stack;
mod trayicon;
pub(crate) use trayicon::companion_news_anchor as companion_tray_news_anchor;
mod views;

use edge::Frame;
/// Exit code the dock asked for (GPU recovery gave up), for main().
pub use gpu::requested_exit_code;
/// Text to the clipboard (also used by the settings window).
pub(crate) use menu::copy_text;
use shell::*;
/// Removes a tray icon a dead dock left (guardian, uninstall).
pub use trayicon::{is_own as is_own_tray_icon, remove_stale as remove_stale_tray_icon, started as process_started,
    bar_click as own_tray_click, Gone as TrayIconGone,
};
#[cfg(test)]
pub use trayicon::tray_png as own_tray_png;
use views::{IconShelf, ModelSnap, Seed};

// ---- Look & feel (DIPs) ----------------------------------------------------
// Harmony v2.1 (1.3, 3.3): the same padding on every side of the bar and
// the same space between icons (8, the panels' "edge to card 8, card to
// card 8"); a separator's slot is 8 too, so the icons either side of one
// are 24 apart.
const SEP_W: f32 = crate::theme::metrics::size::DOCK_GAP; // separator slot width
const PAD_AXIS: f32 = crate::theme::metrics::space::S3; // bar padding along the dock
const PAD_CROSS: f32 = crate::theme::metrics::space::S3; // bar padding across (icon to bar edge)
const EDGE_GAP: f32 = crate::theme::metrics::size::EDGE_GAP; // gap between bar and screen edge
const HEADROOM: f32 = 44.0; // space above magnified icons for labels (bottom dock)
/// Side docks put labels beside the icons, so they need room for a name.
const SIDE_HEADROOM: f32 = 240.0;
/// Auto-hide: how long the dock stays after the mouse leaves, and the px
/// strip at the screen edge that brings it back.
/// Auto-hide's wait when the config cannot be read (the setting is
/// dock.autohide_delay_ms: DockCfg::autohide_delay).
const AUTOHIDE_DELAY_MS: u32 = crate::config::AUTOHIDE_DELAY_DEFAULT;
const AUTOHIDE_STRIP_PX: i32 = 2;
/// Auto-hide slide: the bar's place changes, so GLIDE (7.4), sampled over
/// its keyframe cap.
const SLIDE_MS: i64 = crate::motion::GLIDE_MAX_MS;
/// One hop (polish plan §3: a single, short bounce).
const BOUNCE_SECS: f32 = 0.45;
/// The running dot (theme metrics), and the space between the two dots of
/// an app with several windows.
const DOT: f32 = crate::theme::metrics::DOT;
const DOT_GAP: f32 = crate::theme::metrics::space::S1;

const WM_TICK: u32 = WM_APP + 1;
/// Posted once after the dock is visible: hiding the taskbar and registering
/// the AppBar take ~0.5 s of synchronous explorer round trips, so they run
/// after the first frame instead of before it.
pub const WM_APPLY_TASKBAR: u32 = WM_APP + 5;
/// Test-only signals, honored only when NOPDOCK_DEV=1 is set: freeze the UI
/// thread for 40 s / panic, so the guardian's hang and crash handling can be
/// exercised on our own process.
const WM_DEV_HANG: u32 = WM_APP + 90;
const WM_DEV_PANIC: u32 = WM_APP + 91;
/// One-shot debounce for window-list refreshes (no periodic timers).
const REFRESH_TIMER: usize = 1;
const REFRESH_DEBOUNCE_MS: u32 = 80;
/// Window-region edges are snapped to this many px so the region is not
/// rebuilt on every animation frame.
const REGION_SNAP: i32 = 16;
/// WM_MOUSELEAVE (lives in Win32_UI_Controls, which this crate does not enable).
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;
/// WM_DWMCOLORIZATIONCOLORCHANGED (accent color changed).
const WM_DWMCOLORIZATIONCOLORCHANGED_MSG: u32 = 0x0320;
/// One-shot re-hide checks after explorer restarts (explorer shows its
/// taskbar again ~100 ms after we hide it; see docs/spikes/S5-reshow.md).
const REHIDE_TIMER: usize = 2;
/// Retry draining worker events if the dock was busy (modal menu).
const EVENTS_RETRY_TIMER: usize = 3;
/// After explorer exits: look for the new explorer every 100 ms, at most
/// EXPLORER_WAIT_TRIES times (bounded; only runs during a restart).
const EXPLORER_WAIT_TIMER: usize = 4;
const EXPLORER_WAIT_TRIES: u32 = 100;
/// Gaps between the checks: fires at 0.5, 1.5, 3, 5 and 10 s.
const REHIDE_STEPS_MS: [u32; 5] = [500, 1000, 1500, 2000, 5000];
/// One-shot: hide the auto-hide dock after the mouse left.
const AUTOHIDE_TIMER: usize = 5;
/// Poll the strip only while auto-hide is active, including under another window.
const EDGE_CURSOR_TIMER: usize = 69;
const EDGE_CURSOR_MS: u32 = 60;
/// While the magnification is held for something the dock opened (6.2
/// "open"): has it closed? Runs only while held.
const HOLD_TIMER: usize = 68;
const HOLD_CHECK_MS: u32 = 100;
/// After asking for taskbar auto-hide: is its strip free yet (taskbar.rs)?
/// HIDE_CHECKS tries HIDE_CHECK_MS apart, then auto-hide is asked again,
/// and after as many more the taskbar is kept (sharing the screen).
const HIDE_CHECK_TIMER: usize = 61;
const HIDE_CHECK_MS: u32 = 300;
const HIDE_CHECKS: u32 = 6;
/// One-shot, coalesced: the work area changed (SPI_SETWORKAREA).
const WORKAREA_TIMER: usize = 62;
const WORKAREA_DEBOUNCE_MS: u32 = 150;
/// Re-checks after the taskbar was given back (explorer reserves its strip
/// again, shows its windows, a moment later): fires at 0.3, 1 and 2.5 s.
const COEXIST_TIMER: usize = 63;
const COEXIST_STEPS_MS: [u32; 3] = [300, 700, 1500];
/// After an explorer restart: does the shell keep windows out of what our
/// AppBars reserve? If not (it forgot them: a re-registration was missed or
/// came before the new shell took AppBars), register them again. Gaps
/// between the checks: fires at 1.5, 4 and 10 s.
const RESERVE_CHECK_TIMER: usize = 67;
const RESERVE_CHECK_STEPS_MS: [u32; 3] = [1500, 2500, 6000];
/// One-shot: the hide slide has finished; shrink the region to the strip.
const SLIDE_DONE_TIMER: usize = 6;
/// One-shot: the pointer rested on a running app long enough for previews.
const PREVIEW_TIMER: usize = 10;
const PREVIEW_DELAY_MS: u32 = 400;
/// One-shot, coalesced: re-check for a fullscreen foreground window.
const FULLSCREEN_TIMER: usize = 7;
const FULLSCREEN_DEBOUNCE_MS: u32 = 120;
/// One-shot, coalesced: re-read the system look after settings changes.
const SYSLOOK_TIMER: usize = 8;
const SYSLOOK_DEBOUNCE_MS: u32 = 150;
/// One-shot, coalesced: monitors / DPI changed.
const DISPLAY_TIMER: usize = 9;
const DISPLAY_DEBOUNCE_MS: u32 = 50;

static REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
static WM_TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static WM_SHELLHOOK: AtomicU32 = AtomicU32::new(0);
static DOCK_HWND: AtomicIsize = AtomicIsize::new(0);
/// Mirrors cfg.hide_taskbar for hook callbacks, which can run while the dock
/// is borrowed (e.g. during a SendMessage inside SHAppBarMessage).
static HIDE_TASKBAR: AtomicBool = AtomicBool::new(false);
static TRAY_SHOW_HOOK: AtomicIsize = AtomicIsize::new(0);
static REHIDE_STEP: AtomicU32 = AtomicU32::new(0);
static RESERVE_CHECK_STEP: AtomicU32 = AtomicU32::new(0);
/// The explorer (pid) whose start the restart path last ran for (or that
/// was running when the dock started).
static HANDLED_EXPLORER: AtomicU32 = AtomicU32::new(0);
static HIDE_CHECK_STEP: AtomicU32 = AtomicU32::new(0);
static COEXIST_STEP: AtomicU32 = AtomicU32::new(0);
static WORKAREA_PENDING: AtomicBool = AtomicBool::new(false);
/// Set from WM_QUERYENDSESSION until the session really ends or the end is
/// cancelled: the taskbar has been given back and must not be hidden again.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
/// A WM_MOUSELEAVE arrived while the dock was borrowed (e.g. inside the
/// context menu's modal loop); applied once the borrow is released.
static PENDING_LEAVE: AtomicBool = AtomicBool::new(false);
static DEV_SIGNALS: AtomicBool = AtomicBool::new(false);
static EXPLORER_WAIT_LEFT: AtomicU32 = AtomicU32::new(0);
static FULLSCREEN_PENDING: AtomicBool = AtomicBool::new(false);
static SYSLOOK_PENDING: AtomicBool = AtomicBool::new(false);
static TRASH_PENDING: AtomicBool = AtomicBool::new(false);
/// SHChangeNotifyRegister id (0 = none), released at exit.
static TRASH_WATCH: AtomicU32 = AtomicU32::new(0);
static DISPLAY_PENDING: AtomicBool = AtomicBool::new(false);
/// EVENT_OBJECT_LOCATIONCHANGE hook on the foreground window's thread (moved
/// on every foreground change), so F11 / exclusive fullscreen in the same
/// window is noticed without a global location hook.
static FG_LOCATION_HOOK: AtomicIsize = AtomicIsize::new(0);
/// The thread FG_LOCATION_HOOK listens to (0 = none).
static FG_LOCATION_TID: AtomicU32 = AtomicU32::new(0);

thread_local! {
    /// Where each dock draws each app's icon (screen px), published by
    /// every layout. The shell hook's GETMINRECT answer reads this, never a
    /// Dock (it may be borrowed while Windows asks, e.g. during a restore
    /// we started). A dock that is hidden or in fullscreen publishes none.
    static MINRECTS: RefCell<Vec<views::MinEntry>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Start,
    App,
    Sep,
    /// A divider the user put among the pinned apps (a pin entry of its
    /// own: config::PinEntry::divider). Drawn like the separator.
    Divider,
    /// The Recycle Bin, always last.
    Trash,
}

impl Kind {
    /// Drawn as a line in a separator's slot: no icon, never magnified.
    fn is_line(self) -> bool {
        matches!(self, Kind::Sep | Kind::Divider)
    }
    /// Can be dragged along the dock (an app, or a divider).
    fn movable(self) -> bool {
        matches!(self, Kind::App | Kind::Divider)
    }
    /// Its slot along the dock at rest (DIP): a separator's for a line.
    fn base_w(self, icon: f32) -> f32 {
        if self.is_line() { SEP_W } else { icon }
    }
    /// Drawn this wide (DIP) at magnification `mag` and presence `pres`
    /// (lines keep their slot: they never magnify).
    fn drawn_w(self, icon: f32, mag: f32, pres: f32) -> f32 {
        self.base_w(icon) * if self.is_line() { 1.0 } else { mag } * pres
    }
}

/// The Recycle Bin's shell name (also its icon key: the icon thread loads
/// it fresh, so it shows empty / full).
pub(super) const TRASH: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";
/// Shell change notice (SHChangeNotifyRegister): the trash may have changed.
const WM_TRASH_CHANGED: u32 = WM_APP + 70;
/// The settings window asked the dock to quit (and give the taskbar back).
pub(crate) const WM_QUIT_REQUEST: u32 = WM_APP + 71;
/// One-shot, coalesced: refresh the trash icon.
const TRASH_TIMER: usize = 11;
/// The update planner asks (SendMessage from its thread) whether nothing
/// is open on the dock's side; the answer is 1 when quiet.
const WM_UPDATE_QUIET: u32 = WM_APP + 110;

/// No panel, menu, drag or modal loop of ours is going (for a quiet
/// automatic update; crate::autoupdate). Asked from another thread; false
/// if the dock does not answer within 2 s.
pub fn ui_quiet() -> bool {
    let dock = DOCK_HWND.load(Ordering::SeqCst);
    if dock == 0 {
        return false;
    }
    let mut r = 0usize;
    let answered = unsafe { SendMessageTimeoutW(HWND(dock as *mut _), WM_UPDATE_QUIET, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, 2_000, Some(&mut r)) }.0 != 0;
    answered && r == 1
}

/// On the UI thread: nothing of ours is open or held.
unsafe fn ui_is_quiet() -> bool {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    let mut gui = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    // GUI_INMENUMODE | GUI_INMOVESIZE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE
    let modal = GetGUIThreadInfo(GetCurrentThreadId(), &mut gui).is_ok() && gui.flags.0 & (0x4 | 0x2 | 0x10 | 0x8) != 0;
    let captured = !windows::Win32::UI::Input::KeyboardAndMouse::GetCapture().is_invalid();
    !(modal
        || captured
        || crate::launchpad::is_open()
        || crate::search::is_open()
        || crate::topbar::control::is_open()
        || crate::topbar::stats::is_open()
        || crate::shot::busy())
}
/// The top bar has tray choices to save (TRAY_SAVES).
const WM_TRAY_SAVE: u32 = WM_APP + 74;
/// A snap finished on its helper thread (wParam = the window to front).
pub(super) const WM_SNAP_DONE: u32 = WM_APP + 75;
/// Retry for WM_TRAY_SAVE while the dock is busy (a modal menu).
const TRAY_SAVE_TIMER: usize = 41;
/// Tray icons the user dragged across the chevron: (stable id, hidden).
static TRAY_SAVES: std::sync::Mutex<Vec<(String, bool)>> = std::sync::Mutex::new(Vec::new());

/// The top bar's tray choice, saved to the config by the dock (it owns the
/// config). Posted: the bar calls this from inside its own borrow, and
/// saving re-applies the config to every bar.
pub fn save_tray_choice(id: String, hidden: bool) {
    if let Ok(mut q) = TRAY_SAVES.lock() {
        // The newest choice for an icon replaces an older pending one.
        q.retain(|(i, _)| *i != id);
        q.push((id, hidden));
    }
    let dock = DOCK_HWND.load(Ordering::SeqCst);
    let posted = dock != 0 && unsafe { PostMessageW(Some(HWND(dock as *mut _)), WM_TRAY_SAVE, WPARAM(0), LPARAM(0)) }.is_ok();
    if !posted {
        // Stays queued: the next choice (or retry) saves it too.
        crate::log!("tray: could not ask the dock to save a tray choice; kept pending");
    }
}

/// The tray order the user dragged into, to save (newest wins).
static TRAY_ORDER_SAVE: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

/// The top bar's new tray order (stable ids only), saved like a choice.
pub fn save_tray_order(order: Vec<String>) {
    if let Ok(mut o) = TRAY_ORDER_SAVE.lock() {
        *o = Some(order);
    }
    let dock = DOCK_HWND.load(Ordering::SeqCst);
    let posted = dock != 0 && unsafe { PostMessageW(Some(HWND(dock as *mut _)), WM_TRAY_SAVE, WPARAM(0), LPARAM(0)) }.is_ok();
    if !posted {
        crate::log!("tray: could not ask the dock to save the tray order; kept pending");
    }
}

/// Failed saves in a row (sets the retry delay).
static TRAY_SAVE_FAILS: AtomicU32 = AtomicU32::new(0);

/// What flush_tray_saves wants next.
#[derive(PartialEq, Eq, Debug)]
enum Flush {
    Done,
    /// Try again in this many ms (dock busy, or the file did not land).
    Retry(u32),
    /// The config is read-only: kept pending, no timer (nothing will
    /// change until a later choice or restart).
    Held,
}

/// Puts choices that did not reach the file back in the queue, unless a
/// newer choice for the same icon came in meanwhile.
fn requeue_tray_saves(failed: Vec<(String, bool)>) {
    if let Ok(mut q) = TRAY_SAVES.lock() {
        let newer = std::mem::take(&mut *q);
        *q = merge_requeue(failed, newer);
    }
}

/// Failed choices first, then the ones queued since; a failed choice for
/// an icon that has a newer queued choice is dropped (the newer one wins).
fn merge_requeue(failed: Vec<(String, bool)>, newer: Vec<(String, bool)>) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = failed.into_iter().filter(|(id, _)| !newer.iter().any(|(n, _)| n == id)).collect();
    out.extend(newer);
    out
}

/// Moves each queued id to tray_hidden or tray_visible (and out of the
/// other), then saves. Choices that are not saved stay queued.
fn flush_tray_saves() -> Flush {
    if views::primary_busy() {
        return Flush::Retry(200);
    }
    let saves: Vec<(String, bool)> = TRAY_SAVES.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
    let order: Option<Vec<String>> = TRAY_ORDER_SAVE.lock().map(|mut o| o.take()).unwrap_or_default();
    if saves.is_empty() && order.is_none() {
        return Flush::Done;
    }
    // Not saved: back in place unless a newer order came in meanwhile.
    let requeue_order = |o: Option<Vec<String>>| {
        if let (Some(o), Ok(mut q)) = (o, TRAY_ORDER_SAVE.lock()) {
            if q.is_none() {
                *q = Some(o);
            }
        }
    };
    let apply = saves.clone();
    let apply_order = order.clone();
    let result = views::with_primary(|d| {
        d.try_update_config(move |c| {
            if let Some(o) = &apply_order {
                c.topbar.tray_order = o.clone();
            }
            for (id, hidden) in &apply {
                c.topbar.tray_hidden.retain(|x| x != id);
                c.topbar.tray_visible.retain(|x| x != id);
                if *hidden {
                    c.topbar.tray_hidden.push(id.clone());
                } else {
                    c.topbar.tray_visible.push(id.clone());
                }
            }
        })
    });
    match result {
        Some(Ok(())) => {
            TRAY_SAVE_FAILS.store(0, Ordering::SeqCst);
            Flush::Done
        }
        Some(Err(e)) => {
            let n = saves.len();
            requeue_tray_saves(saves);
            requeue_order(order);
            if e == "read-only" {
                crate::log!("tray: {n} tray choice(s) not saved, the settings file is read-only; kept pending");
                return Flush::Held;
            }
            let fails = TRAY_SAVE_FAILS.fetch_add(1, Ordering::SeqCst) + 1;
            let delay = (1000u32 << (fails - 1).min(6)).min(60_000);
            crate::log!("tray: saving {n} tray choice(s) failed ({e}); retry {fails} in {delay} ms");
            Flush::Retry(delay)
        }
        None => {
            // Borrowed after all: nothing was applied.
            requeue_tray_saves(saves);
            requeue_order(order);
            Flush::Retry(200)
        }
    }
}

struct Item {
    kind: Kind,
    name: String,
    path: String,
    pinned: bool,
    id: AppId,
    /// A browser's web app: its details as this item last followed them
    /// (webapps::read; items.rs follow_web_apps). None: none yet, or not one.
    web: Option<crate::webapps::WebApp>,
    windows: Vec<HWND>,
    /// Its windows sent to the tray (crate::traywin: minimized and
    /// tucked): not among `windows`, but the app still counts as running,
    /// with a mark on its icon.
    tray: Vec<HWND>,
    icon: SpriteVisual,
    /// Squircle backplate under the icon (only visible in backplate mode).
    plate: SpriteVisual,
    /// Independent hover/foreground tile, also for unmasked app icons.
    tile: SpriteVisual,
    tile_geom: CompositionRoundedRectangleGeometry,
    tile_fill: Option<Rgba>,
    /// Icon sits inset on the backplate.
    plated: bool,
    /// Its brushes at each step from rest to the peak (sharp.rs); None
    /// until it has a picture.
    pics: Option<items::Pics>,
    /// The steps shown (icon, backplate); usize::MAX: none yet.
    shown: (usize, usize),
    dot: SpriteVisual,
    /// The shelf version of the picture it shows (0 = none from the shelf).
    icon_ver: u64,
    /// Rounded clip of the indicator (resized for the active bar).
    dot_geom: Option<CompositionRoundedRectangleGeometry>,
    /// One of its windows is in front: long accent indicator.
    active: bool,
    /// One of its windows asked for attention (flashing on the taskbar).
    attention: bool,
    /// ... this one (the request ends when it closes).
    attention_hwnd: HWND,
    /// One of its windows has not responded for hung::DEBOUNCE.
    hung: bool,
    mag: f32,
    mag_t: f32,
    /// Magnification spring velocity (scale units per second).
    mag_v: f32,
    /// Taskbar progress / badge shown on the icon.
    marks: badges::Marks,
    /// A pinned folder (stack): click opens its contents.
    stack: bool,
    pres: f32,
    pres_t: f32,
    /// Presence's speed (per second): it glides (layout::glide_step).
    pres_v: f32,
    /// The running dot is shown (faded in and out by show()).
    dot_on: bool,
    bounce: Option<Instant>,
    x: f32,
    w: f32,
    /// Index in the dock at the last layout (a change means it moved).
    slot: usize,
    /// Was drawn under the pointer (dragged) at the last layout.
    was_dragged: bool,
    /// Where it was actually drawn along the dock (slot + slide, or under
    /// the pointer): hit testing and labels use this.
    dx: f32,
    /// Drawn this far from its slot (along the dock), easing to 0: icons
    /// slide to new places instead of jumping.
    slide: f32,
    slide_v: f32,
    /// Open space before it (0..1 of a slot) and its target: where files
    /// dragged from outside would be pinned.
    gap: f32,
    gap_t: f32,
    gap_v: f32,
}

impl Item {
    fn base_w(&self, icon: f32) -> f32 {
        self.kind.base_w(icon)
    }
    /// Under the pointer for clicks, menus and drags (a divider too: it is
    /// moved and removed like a pin).
    fn clickable(&self) -> bool {
        self.kind != Kind::Sep && self.pres_t > 0.0
    }
    fn is_line(&self) -> bool {
        self.kind.is_line()
    }
    fn movable(&self) -> bool {
        self.kind.movable()
    }
    /// One of the pinned list's entries (an app or a divider).
    fn pin_slot(&self) -> bool {
        self.pinned && self.movable()
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// Size and magnification values, from config (clamped to sane ranges).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Look {
    /// Resting icon size (DIP).
    pub icon: f32,
    /// Magnification peak.
    pub max_scale: f32,
    /// Magnification falloff radius (DIP).
    pub range: f32,
    /// Space between icons (DIP).
    pub gap: f32,
    /// How running apps are marked.
    pub indicator: crate::config::Indicator,
    /// Dress icons with squircle masks / backplates.
    pub squircle: bool,
    /// The bar's corner radius (DIP): worked out from the icons (1.3), so
    /// the corners stay concentric at every icon size.
    pub radius: f32,
}

impl Look {
    /// `magnify`: the magnification effect is on (crate::effects; off
    /// with Windows' animation effects off under Auto, and in Low).
    pub fn from_cfg(d: &crate::config::DockCfg, magnify: bool) -> Self {
        let icon = d.icon_size.clamp(24.0, 128.0);
        Self {
            icon,
            // Never past 128 DIP (polish plan 2.1).
            max_scale: if magnify { d.max_scale.clamp(1.0, 3.0).min(128.0 / icon).max(1.0) } else { 1.0 },
            range: icon * crate::config::DOCK_RANGE,
            gap: crate::config::DOCK_GAP,
            indicator: d.indicator,
            squircle: d.squircle,
            radius: 8.0,
        }
    }
}

/// A settings-window preview: what is saved (`base`, kept up to date with
/// the file and the dock's own changes) plus the fields the window edited.
struct PreviewState {
    owner: u64,
    base: Config,
    edits: Config,
    keys: Vec<String>,
}

impl PreviewState {
    fn shown(&self) -> Config {
        let mut c = self.base.clone();
        crate::settings::apply_keys(&self.edits, &mut c, &self.keys);
        c
    }
}

struct Dock {
    /// The first dock: owns the config, the window list, the taskbar and
    /// everything else there is once (see views.rs). Views pull from it.
    primary: bool,
    /// The window list's generation (the primary counts; a view has the
    /// one it pulled last).
    wins_gen: u64,
    /// The top-level window in front, as last marked.
    active_top: HWND,
    /// A view: the primary shows a settings preview (pins are then saved
    /// as changes, not as a whole list).
    view_previewing: bool,
    /// A view: the key of its display (crate::displays), whose own dock
    /// settings it shows (config dock.per_display). None: the primary, or
    /// a display Windows could not name.
    display: Option<String>,
    /// The effects this dock's visuals were set up for.
    fx_seen: crate::effects::Resolved,
    /// While the settings app previews changes.
    preview: Option<PreviewState>,
    /// The dock's own config edits not yet reported saved, by request
    /// number (items.rs update_config).
    pending_edits: std::collections::VecDeque<(u64, crate::worker::ConfigEdit)>,
    edit_gen: u64,
    /// Pins built with a path other than their entry's (items.rs
    /// build_items: a web app's shortcut another app's now): (entry, entry
    /// as built), saved with the next window list (sync_windows).
    pin_fixes: Vec<(crate::config::PinEntry, crate::config::PinEntry)>,
    hwnd: HWND,
    gfx: Rc<Gfx>,
    cfg: Config,
    /// The theme as loaded, and as resolved for the current system look.
    theme_raw: Theme,
    theme: Theme,
    sys: crate::syslook::SysLook,
    look: Look,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    /// Holds everything; its Offset slides the dock away for auto-hide
    /// (animated by the compositor).
    slide: ContainerVisual,
    layer: ContainerVisual,
    bar: ContainerVisual,
    bar_fill: Vec<SpriteVisual>,
    /// Blurred-wallpaper layer for the mica material (monitor-sized, under
    /// the bar's clip).
    mica: SpriteVisual,
    bar_geom: CompositionRoundedRectangleGeometry,
    border_geom: CompositionRoundedRectangleGeometry,
    border_shape: windows::UI::Composition::CompositionSpriteShape,
    /// The 1 DIP top highlight just inside the border (theme dock.highlight).
    highlight_geom: CompositionRoundedRectangleGeometry,
    highlight_shape: windows::UI::Composition::CompositionSpriteShape,
    /// The rim's visual: its opacity is the rim's strength (it comes up
    /// with the dock back from auto-hide, crate::motion::light).
    highlight_v: windows::UI::Composition::ShapeVisual,
    /// Over the icons: the keyboard's focus ring and high contrast's
    /// pressed outline (ring.rs).
    rings: ring::Rings,
    /// The drop shadow (its own click-through window, shadow.rs) and what
    /// it is drawn for now (no layers: none).
    shadow: shadow::BarShadow,
    shadow_spec: shadow::Spec,
    label: ContainerVisual,
    label_parts: label::LabelParts,
    /// The magnification is held where it was: something the dock opened
    /// is up (6.2 "open"; layout.rs holds_open).
    held: bool,
    /// The drop glow is lit (drag.rs place_drop_glow).
    glow_on: bool,
    /// The 300 ms label wait is over for this visit of the pointer.
    label_armed: bool,
    /// LABEL_TIMER is running.
    label_waiting: bool,
    labels: HashMap<String, (CompositionSurfaceBrush, f32, f32)>,
    items: Vec<Item>,
    scale: f32,
    origin: POINT,
    monitor: RECT,
    /// Window length along the dock (DIP).
    along: f32,
    /// Edge and window depth; maps canonical layout onto the window.
    frame: Frame,
    /// The monitor the dock is on (HMONITOR as isize).
    mon: isize,
    /// Bar rect in the canonical frame (see edge.rs).
    bar_rect: Rect,
    /// Auto-hide: the dock is slid away and only the edge strip is live.
    hidden: bool,
    /// Item under the pointer last time (for preview timing), and its app:
    /// the index can go stale while the preview timer runs.
    hover_item: Option<usize>,
    hover_app: Option<String>,
    /// The region currently covers the expanded (hover) area.
    region_expanded: bool,
    /// A fullscreen app is in front on our monitor: the dock is hidden.
    fullscreen: bool,
    fullscreen_window: HWND,
    /// Auto-hide state saved while Start shows over a fullscreen app.
    start_hidden: Option<bool>,
    /// Auto-hide: the slide-out finished and the region is the edge strip.
    strip_ready: bool,
    mouse: Option<(f32, f32)>,
    /// TME_LEAVE is armed (we will get WM_MOUSELEAVE).
    tracking: bool,
    /// Current window region in px (x0, y0, x1, y1); clicks outside it go
    /// to whatever is below the dock.
    region: (i32, i32, i32, i32),
    pressed: Option<(usize, u32)>,
    /// The icon let go of, and when: its pressed dim comes back to full
    /// (layout::press_dim).
    press_back: Option<(usize, Instant)>,
    /// Where the button went down (canonical), to tell a drag from a click.
    press_at: Option<(f32, f32)>,
    /// An icon being dragged.
    drag: Option<drag::Drag>,
    /// The app of the pressed icon (the index may go stale).
    press_path: Option<String>,
    /// Files are being dragged over the dock (no previews then).
    ole_drag: bool,
    file_hover: Option<String>,
    /// The last pin removed by dragging it off (Ctrl+Z on the dock).
    undo: Option<drag::Undo>,
    /// Ctrl+Z is registered for that.
    /// Removed icons still playing their poof (started, icon, backplate).
    poofs: Vec<(Instant, SpriteVisual, SpriteVisual)>,
    /// The trash changed while its icon request was out.
    trash_dirty: bool,
    last_tick: Instant,
    ticker: Ticker,
    appbar: bool,
    /// The area place_window last placed the dock in (px): a work-area
    /// change re-places only when this would move.
    placed_area: RECT,
    workers: Workers,
    events: worker::EventRx,
    /// Newest window-list request applied (older snapshots are dropped).
    last_seq: u64,
    last_wins: Vec<apps::AppWindow>,
    /// Icon surfaces by (icon key, px size) and squircle masks, shared by
    /// every dock (views.rs); reused when an app returns.
    shelf: Rc<RefCell<IconShelf>>,
    /// Icons requested from the worker and not back yet.
    icon_wait: HashSet<(String, i32)>,
    /// Icons whose resting picture could not be drawn: how many times
    /// (retried on the next dress, up to items::REST_TRIES).
    rest_fails: HashMap<String, u8>,
    first_snapshot: bool,
    icons_logged: bool,
    /// Taskbar progress / badges by app key, as last taken from the UIA thread.
    marks: HashMap<String, badges::AppMark>,
    /// Not-responding windows (see hung.rs) and whether HUNG_TIMER runs.
    hang: hung::HangWatch,
    hang_timer: bool,
    /// Stacks: last listing request, the one waited for (seq, folder,
    /// when) and a listing ready to show.
    stack_seq: u64,
    stack_req: Option<(u64, String, Instant)>,
    stack_plan: Option<stack::StackPlan>,
    /// Per-app volume (wheel) and the speaker marks.
    sound: sound::SoundState,
    /// Behind the stack that files are being dragged over (they would go
    /// into its folder), and which stack that is.
    drop_glow: SpriteVisual,
    drop_glow_geom: CompositionRoundedRectangleGeometry,
    drop_stack: Option<String>,
    /// Keyboard mode (see access.rs): the focused icon's key, and the
    /// window that had the focus before.
    kb: Option<String>,
    /// Keyboard mode shows its focus ring: entered by the keys (the
    /// dock's focus shortcut, Esc back from an icon's menu), or by a
    /// screen reader with the ring already shown or Windows' keyboard cues
    /// always on (crate::input::reader_focus).
    kb_ring: bool,
    kb_prev: HWND,
    /// The stack opened last (its panel may still be up).
    last_stack: String,
}

fn since_start() -> u128 {
    crate::START.get().map(|t| t.elapsed().as_millis()).unwrap_or(0)
}

fn color(a: u8, r: u8, g: u8, b: u8) -> Color {
    Color { A: a, R: r, G: g, B: b }
}

fn rgba(c: Rgba) -> Color {
    color(c.a, c.r, c.g, c.b)
}

/// Size (DIP) of the running indicator for a style, in the canonical frame
/// (along x across); `upright` gives its on-screen size on a side dock.
/// `two`: the app has several windows (dots: two of them side by side).
pub(super) fn indicator_size(style: Indicator, active: bool, two: bool) -> Vector2 {
    match style {
        Indicator::Dot if two => Vector2 { X: DOT * 2.0 + DOT_GAP, Y: DOT },
        Indicator::Dot => Vector2 { X: DOT, Y: DOT },
        // Running: short; in front: long (polish plan 2.1).
        Indicator::Bar => Vector2 { X: if active { 18.0 } else { 7.0 }, Y: 3.0 },
        Indicator::Glow => Vector2 { X: 16.0, Y: 16.0 },
        Indicator::None => Vector2 { X: 0.0, Y: 0.0 },
    }
}

fn upright(size: Vector2, vertical: bool) -> Vector2 {
    if vertical { Vector2 { X: size.Y, Y: size.X } } else { size }
}

/// (Re)styles an indicator sprite: shape, size and brush for `style`.
/// Dots are two round children of the sprite (the second shows for an app
/// with several windows, `show_two`); the other styles draw the sprite.
pub(super) fn style_indicator(
    c: &Compositor,
    v: &SpriteVisual,
    style: Indicator,
    fill: Color,
    vertical: bool,
    active: bool,
    two: bool,
) -> Result<Option<CompositionRoundedRectangleGeometry>> {
    let size = upright(indicator_size(style, active, two), vertical);
    v.SetSize(size)?;
    v.Children()?.RemoveAll()?;
    match style {
        Indicator::Dot => {
            let _ = v.SetClip(None);
            v.SetBrush(None::<&CompositionBrush>)?;
            let brush = c.CreateColorBrushWithColor(fill)?;
            for k in 0..2 {
                let pip = c.CreateSpriteVisual()?;
                pip.SetSize(Vector2 { X: DOT, Y: DOT })?;
                let step = k as f32 * (DOT + DOT_GAP);
                pip.SetOffset(if vertical { Vector3 { X: 0.0, Y: step, Z: 0.0 } } else { Vector3 { X: step, Y: 0.0, Z: 0.0 } })?;
                pip.SetBrush(&brush)?;
                let geom = c.CreateEllipseGeometry()?;
                geom.SetCenter(Vector2 { X: DOT / 2.0, Y: DOT / 2.0 })?;
                geom.SetRadius(Vector2 { X: DOT / 2.0, Y: DOT / 2.0 })?;
                pip.SetClip(&c.CreateGeometricClipWithGeometry(&geom)?)?;
                if k == 1 {
                    pip.SetOpacity(if two { 1.0 } else { 0.0 })?;
                }
                v.Children()?.InsertAtTop(&pip)?;
            }
        }
        Indicator::None => {
            v.SetBrush(&c.CreateColorBrushWithColor(fill)?)?;
            let geom = c.CreateEllipseGeometry()?;
            geom.SetCenter(Vector2 { X: size.X / 2.0, Y: size.Y / 2.0 })?;
            geom.SetRadius(Vector2 { X: size.X / 2.0, Y: size.Y / 2.0 })?;
            v.SetClip(&c.CreateGeometricClipWithGeometry(&geom)?)?;
        }
        Indicator::Bar => {
            v.SetBrush(&c.CreateColorBrushWithColor(fill)?)?;
            let geom = c.CreateRoundedRectangleGeometry()?;
            geom.SetSize(size)?;
            let r = size.X.min(size.Y) / 2.0;
            geom.SetCornerRadius(Vector2 { X: r, Y: r })?;
            v.SetClip(&c.CreateGeometricClipWithGeometry(&geom)?)?;
            return Ok(Some(geom));
        }
        Indicator::Glow => {
            // Soft spot: the theme color in the middle fading to nothing.
            let brush = c.CreateRadialGradientBrush()?;
            let stops = brush.ColorStops()?;
            stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(0.0, fill)?)?;
            let mid = Color { A: (fill.A as u16 * 3 / 5) as u8, ..fill };
            stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(0.35, mid)?)?;
            stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(1.0, Color { A: 0, ..fill })?)?;
            v.SetBrush(&brush)?;
            let _ = v.SetClip(None);
        }
    }
    Ok(None)
}

/// The bar's rim (theme::rim, design system v2.1, 2.2): white along its
/// inner edge, bright at the top, through 38 % of its height, almost gone
/// at the bottom (all clear on Flat and on glass A: no line); no stroke in
/// high contrast. Used when a dock is built
/// and whenever the theme is applied again.
pub(super) fn paint_highlight(c: &Compositor, shape: &windows::UI::Composition::CompositionSpriteShape, theme: &Theme, m: crate::theme::Material) {
    let lit = theme.rim_of(m).and_then(|stops| {
        let b = c.CreateLinearGradientBrush().ok()?;
        b.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 }).ok()?;
        b.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 }).ok()?;
        let list = b.ColorStops().ok()?;
        for (at, col) in crate::theme::RIM_STOPS.iter().zip(stops) {
            list.Append(&c.CreateColorGradientStopWithOffsetAndColor(*at, rgba(col)).ok()?).ok()?;
        }
        Some(b)
    });
    let _ = match &lit {
        Some(b) => shape.SetStrokeBrush(b),
        None => shape.SetStrokeBrush(None::<&CompositionBrush>),
    };
}

/// Colors an indicator: its dots (style_indicator), or the sprite itself.
pub(super) fn paint_indicator(v: &SpriteVisual, b: &windows::UI::Composition::CompositionColorBrush) {
    let pips: Vec<SpriteVisual> = v
        .Children()
        .and_then(|ch| ch.First())
        .map(|it| {
            let mut out = Vec::new();
            while it.HasCurrent().unwrap_or(false) {
                if let Ok(p) = it.Current().and_then(|x| x.cast::<SpriteVisual>()) {
                    out.push(p);
                }
                if !it.MoveNext().unwrap_or(false) {
                    break;
                }
            }
            out
        })
        .unwrap_or_default();
    if pips.is_empty() {
        let _ = v.SetBrush(b);
    } else {
        for p in pips {
            let _ = p.SetBrush(b);
        }
    }
}

/// An app's dots: one, or two for several windows (other styles: as is).
pub(super) fn show_two(v: &SpriteVisual, style: Indicator, active: bool, two: bool, vertical: bool) {
    if style != Indicator::Dot {
        return;
    }
    let _ = v.SetSize(upright(indicator_size(style, active, two), vertical));
    if let Ok(Ok(second)) = v.Children().and_then(|ch| ch.First()).map(|it| {
        let _ = it.MoveNext();
        it.Current().and_then(|x| x.cast::<SpriteVisual>())
    }) {
        let _ = second.SetOpacity(if two { 1.0 } else { 0.0 });
    }
}


/// implicit() for other windows on this thread (the top bar's tray slide).
/// Empty (changes apply at once) when Windows' animations are off.
pub fn implicit_visual(c: &Compositor, v: &impl windows::core::Interface, props: &[(&str, i64)]) -> Result<()> {
    implicit(c, v, props)
}

/// Shows the right-click menu outside the dock's borrow (the popup pumps
/// messages, the dock keeps working), then runs the pick inside it.
fn show_menu(item: Option<usize>, shift: bool, anchor: Option<POINT>) -> crate::popup::Outcome {
    let Some(owner) = views::token() else { return crate::popup::Outcome::Cancelled };
    let Some(plan) = views::with_token(owner, |d| d.plan_menu(item, shift, anchor)).flatten() else { return crate::popup::Outcome::Cancelled };
    // The dock stays while its menu is up (views::reconcile waits).
    let modal = views::Modal::begin(owner);
    let live_quit = menu::LiveQuit::begin(plan.full_quit, plan.actions);
    let out = crate::popup::track(crate::popup::Request {
        gfx: &plan.gfx,
        theme: &plan.theme,
        items: plan.rows,
        at: plan.at,
        align: plan.align,
        scale: plan.scale,
        titles: Default::default(),
        on_slide: None,
        drag_ids: 0..0,
        on_space: None,
        on_pick: None,
        // Never taller than the room off the dock (it would slide back
        // over a magnified icon): compact rows or a scroll instead.
        hangs: true,
    });
    let action = if let crate::popup::Outcome::Picked(id) = out { live_quit.action(id) } else { None };
    drop(live_quit);
    drop(modal);
    // The menu is gone: the magnification it held lets go (6.2 "open").
    views::with_token(owner, |d| d.release_open());
    if let Some(a) = action {
        // Only on the dock it was opened on (gone: dropped).
        views::with_token(owner, |d| unsafe { d.run_action(a) });
    }
    out
}

/// An icon's menu asked for from the keyboard (or a screen reader): it
/// opens at the icon, not at the pointer, and cancelled with Esc the focus
/// comes back to that icon.
fn show_menu_from_keyboard(item: usize) {
    let Some(owner) = views::token() else { return };
    let Some((key, kb, at)) = views::with_token(owner, |d| (d.key_of(item), d.kb.is_some(), d.menu_anchor(item))) else { return };
    let out = show_menu(Some(item), false, Some(at));
    if kb && out == crate::popup::Outcome::Cancelled {
        views::with_token(owner, |d| unsafe { d.enter_keyboard(key.as_deref(), true) });
    }
}

/// Hear about icon changes (the Recycle Bin's empty/full icon among them).
/// Event-driven: the shell posts WM_TRASH_CHANGED.
unsafe fn watch_trash(hwnd: HWND) {
    // Never twice (a cancelled logoff registers again, audit A10).
    if TRASH_WATCH.load(Ordering::SeqCst) != 0 {
        return;
    }
    use windows::Win32::UI::Shell::{
        SHCNRF_InterruptLevel, SHCNRF_NewDelivery, SHCNRF_ShellLevel, SHChangeNotifyEntry,
        SHChangeNotifyRegister, SHCNE_UPDATEIMAGE,
    };
    let entry = SHChangeNotifyEntry { pidl: std::ptr::null_mut(), fRecursive: BOOL(1),
    };
    let id = SHChangeNotifyRegister(
        hwnd,
        SHCNRF_ShellLevel | SHCNRF_InterruptLevel | SHCNRF_NewDelivery,
        SHCNE_UPDATEIMAGE.0 as i32,
        WM_TRASH_CHANGED,
        1,
        &entry,
    );
    if id == 0 {
        crate::log!("trash: cannot watch icon changes");
    }
    TRASH_WATCH.store(id, Ordering::SeqCst);
}

/// A fullscreen app covers the monitor with rect `mon` (hot corners, the
/// sensors: they only keep the rect).
pub fn fullscreen_on(mon: RECT) -> bool {
    let dock = HWND(DOCK_HWND.load(Ordering::SeqCst) as *mut _);
    unsafe { input::fullscreen_window(dock, None, mon) }
}

/// SetWindowPos's insert-after for a bar of ours (`me`): topmost, or with
/// a fullscreen app in front (`covered`) right behind it (the top bars).
/// Window `me` (topmost) lies under a normal window (input::sunk).
pub unsafe fn sunk(me: HWND) -> bool {
    input::sunk(me, true)
}

pub unsafe fn z_after(covered: bool, me: HWND) -> HWND {
    input::z_after(covered, me)
}

/// Nobody can see the dock or the bars: the session is locked, asleep or
/// disconnected, or its display is off (session.rs). Background ticking
/// (the bar clock's seconds) rests meanwhile.
pub fn session_idle() -> bool {
    session::idle()
}

/// A fullscreen app covers monitor `hmon` (last seen at `mon`; the top bars).
pub fn fullscreen_on_monitor(hmon: isize, mon: RECT) -> bool {
    let dock = HWND(DOCK_HWND.load(Ordering::SeqCst) as *mut _);
    unsafe { input::fullscreen_window(dock, Some(hmon), mon) }
}

/// A shell fullscreen notice reached another of our windows.
pub fn fullscreen_notice() {
    schedule_fullscreen_check();
}

/// Restores (if minimized) and fronts another app's window without the UI
/// thread waiting on that app (the top bar's window list uses it).
pub fn bring_to_front(h: HWND) {
    unsafe { input::focus(h) };
}

/// The dock's graphics and theme (for Launchpad); None while it is busy.
pub fn look() -> Option<(Rc<Gfx>, Theme)> {
    with_dock(|d| (d.gfx.clone(), d.theme.clone()))
}

/// Labels do not interrupt the companion; modal UI yields, full covers hide.
pub fn companion_mode() -> crate::companion::visibility::Mode {
    crate::companion::visibility::mode(
        crate::launchpad::is_open() || crate::companion::ui_covered(), session::idle(), crate::popup::is_tracking(),
        preview::is_open(), stack::is_open(), crate::search::is_open(),
        crate::companion::ui_paused() || crate::companion::convenience::menu_open(),
    )
}

pub(crate) fn companion_label_window() -> Option<HWND> {
    let mut window=None;
    views::for_each(|d| { if d.label_parts.shown { window=Some(d.hwnd); } });
    window
}

thread_local! { static COMPANION_TOGGLE: std::cell::Cell<(bool, u8)> = const { std::cell::Cell::new((false, 0)) }; }
pub(crate) fn cancel_companion_toggle() {
    COMPANION_TOGGLE.with(|p| p.set((false, 0)));
    unsafe { let _ = KillTimer(Some(HWND(DOCK_HWND.load(Ordering::SeqCst) as *mut _)), crate::companion::TOGGLE_TIMER); }
}
unsafe fn retry_companion_toggle(hwnd: HWND) {
    let _ = KillTimer(Some(hwnd), crate::companion::TOGGLE_TIMER);
    let (pending, attempts) = COMPANION_TOGGLE.with(|p| p.get());
    if !pending { return; }
    if with_dock(|d| {
        if d.cfg.companion.enabled {
            let visible = !d.cfg.companion.visible;
            d.update_config(move |c| {c.companion.set_visible(visible);});
        }
    }).is_some() {
        COMPANION_TOGGLE.with(|p| p.set((false, 0)));
    } else if attempts < 40 {
        COMPANION_TOGGLE.with(|p| p.set((true, attempts + 1)));
        SetTimer(Some(hwnd), crate::companion::TOGGLE_TIMER, 50, None);
    } else {
        COMPANION_TOGGLE.with(|p| p.set((false, 0)));
        crate::log!("companion: toggle still blocked after 2s");
    }
}

impl Dock {
    fn companion_ground(&self) {
        crate::companion::publish_ground(self.companion_ground_value(),self.primary);
    }
    fn companion_ground_value(&self)->crate::companion::Ground {
        let work=crate::monitors::info(self.mon).map(|m|m.work).unwrap_or(self.monitor);
        // An auto-hide dock only visits: the companion keeps the screen
        // floor and the dock rises in front of it (no hop onto the dock).
        let floor=if !self.cfg.dock.autohide && !self.hidden && self.frame.pos==Position::Bottom {
            self.origin.y as f32+self.frame.to_win(self.bar_rect).y*self.scale
        }else{work.bottom as f32};
        crate::companion::Ground{monitor:self.monitor,floor,dpi:self.scale,covered:self.fullscreen}
    }
}

/// A fresh snapshot, without publishing back into the companion borrow.
pub fn companion_grounds()->Option<Vec<(crate::companion::Ground,bool)>> {
    let mut out=Vec::new();
    views::for_each(|d|out.push((d.companion_ground_value(),d.primary))).then_some(out)
}

/// Visible dock chrome in screen pixels, for non-overlapping companion menus.
pub(crate) fn companion_obstacles()->Option<Vec<RECT>> {
    let mut out=Vec::new();
    views::for_each(|d| {if !d.hidden&&!d.fullscreen {
        let r=d.frame.to_win(d.bar_rect);
        out.push(RECT{left:(d.origin.x as f32+r.x*d.scale).floor() as i32,top:(d.origin.y as f32+r.y*d.scale).floor() as i32,
            right:(d.origin.x as f32+(r.x+r.w)*d.scale).ceil() as i32,bottom:(d.origin.y as f32+(r.y+r.h)*d.scale).ceil() as i32});
    }}).then_some(out)
}

/// The pinned paths (Launchpad's "Keep in Dock" / "Remove from Dock").
pub fn pinned_paths() -> Vec<String> {
    with_dock(|d| d.cfg.dock.pins.iter().map(|p| p.path.clone()).collect()).unwrap_or_default()
}

/// Persist a drag through the normal config saver, after the companion borrow.
pub fn save_companion_position(home:Option<crate::companion::grab::Home>,visible:bool)->bool{with_dock(|d|d.update_config(move|c|{c.companion.home=home;c.companion.set_visible(visible);})).is_some()}
pub fn save_companion_greet(date:Option<u32>)->bool{with_dock(|d|d.update_config(move|c|c.companion.last_greet=date)).is_some()}
pub fn companion_theme()->Option<crate::theme::Theme>{with_dock(|d|d.theme.non_shell().clone())}
pub fn companion_action(action:crate::companion::convenience::Action)->bool {with_dock(|d|d.update_config(move|c|{
 crate::companion::convenience::apply(&mut c.companion,action,crate::companion::convenience::now());

})).is_some()}
pub fn set_companion_size(size:crate::companion::model::Size)->bool{with_dock(|d|d.update_config(move|c|c.companion.size=size)).is_some()}

/// Saves Launchpad's arrangement (it was arranged; called outside its
/// borrow). Through the dock's config saver like any change of its own.
pub fn set_launchpad_layout(layout: Vec<Vec<crate::config::LpItem>>) {
    let saved = with_dock(move |d| d.update_config(move |c| c.launchpad.layout = layout.clone()));
    if saved.is_none() {
        crate::log!("launchpad: the dock was busy; the arrangement was not saved");
    }
}

/// Launchpad's whole section (Home, the Library's choices, hidden apps).
pub fn launchpad_cfg() -> crate::config::LaunchpadCfg {
    with_dock(|d| d.cfg.launchpad.clone()).unwrap_or_default()
}

/// Saves what Launchpad itself changes in its section (Home, the schema,
/// seen apps, categories, hidden apps, the clean-up banner); the Settings
/// window's switch (`show_library`) stays as it is in the config.
pub fn set_launchpad_state(state: crate::config::LaunchpadCfg) {
    let saved = with_dock(move |d| {
        d.update_config(move |c| {
            let show = c.launchpad.show_library;
            c.launchpad = crate::config::LaunchpadCfg { show_library: show, ..state.clone() };
        })
    });
    if saved.is_none() {
        crate::log!("launchpad: the dock was busy; its Home and Library were not saved");
    }
}

/// Pins (at the end) or unpins an app from outside the dock (Launchpad).
pub fn set_pinned(name: &str, path: &str, on: bool) {
    let (name, path) = (name.to_string(), path.to_string());
    with_dock(move |d| {
        d.update_config(move |c| {
            if on {
                if !c.dock.pins.iter().any(|p| p.path.eq_ignore_ascii_case(&path)) {
                    c.dock.pins.push(PinEntry::new(name.clone(), path.clone()));
                }
            } else {
                c.dock.pins.retain(|p| !p.path.eq_ignore_ascii_case(&path));
            }
        })
    });
}

/// Animations run at all (crate::effects: Windows' "Animation effects"
/// under Auto, else the chosen preset). Off: every implicit animation below
/// is attached empty (changes apply at once) and the dock skips bounces.
pub fn motion() -> bool {
    crate::effects::motion()
}

/// Full motion (crate::motion::now, the one gate: 7.2 rule 9): things
/// slide, spring and resize. Reduced: nothing moves or changes size by
/// itself (12.2), fades are REDUCED_MS, magnification follows the pointer
/// at once. (The bounce, the genie and the poof keep `motion()`: their
/// own effects, with their own switches.)
pub(super) fn glides() -> bool {
    crate::motion::full()
}

/// What an implicit animation runs on (7.4: by what changes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Run {
    /// The kit's ease-out over this many ms (fades, crossfades).
    Ease(i64),
    /// A straight fade over this many ms (the theme's, 7.1).
    Linear(i64),
    /// A spring from rest, sampled over this many ms (a place or a size).
    Spring(crate::motion::Spring, i64),
}

/// GLIDE: a surface's place or size (the auto-hide slide, Split View's
/// halves).
pub(super) const GLIDE: Run = Run::Spring(crate::motion::SPRING_GLIDE, crate::motion::GLIDE_MAX_MS);
/// SNAP: a small thing's state (the running dot growing for the app in
/// front).
pub(super) const SNAP: Run = Run::Spring(crate::motion::SPRING_SNAP, crate::motion::SNAP_MAX_MS);

/// What an implicit animation finally plays on (`played`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Shape {
    /// One key frame on this curve.
    Curve(crate::motion::Curve),
    /// A spring from rest, sampled over this many ms at speed 1 (the keys
    /// are shares of the way; the length stretches them).
    Spring(crate::motion::Spring, i64),
}

/// Pure: `run` of property `prop` in `mode` at animation speed `speed`,
/// as Composition plays it: (ms of real time, shape); None: nothing is
/// attached (the change applies at once). Fades take their length and
/// curve from crate::motion::fade_plan, so reduced motion keeps fades only,
/// REDUCED_MS at most, straight and never stretched by the speed (12.2);
/// nothing slides or resizes. With full motion everything is as designed,
/// at the speed.
pub(super) fn played(run: Run, prop: &str, mode: crate::motion::Mode, speed: f32) -> Option<(i64, Shape)> {
    use crate::motion::{fade_plan, Mode, LINEAR};
    let fade = |ms: i64, curve| {
        let (ms, curve) = fade_plan(mode, ms, curve, speed);
        (ms, Shape::Curve(curve))
    };
    match (mode, run) {
        (Mode::Full, Run::Spring(spring, ms)) => Some((crate::effects::ms_at(ms, speed), Shape::Spring(spring, ms))),
        (Mode::Reduced, Run::Spring(..)) => None,
        (Mode::Reduced, _) if prop != "Opacity" => None,
        (_, Run::Ease(ms)) => Some(fade(ms, implicit_curve(prop))),
        (_, Run::Linear(ms)) => Some(fade(ms, LINEAR)),
    }
}

/// Settings is recording a shortcut: every NopBar hotkey and the keyboard
/// hook are off until it ends (or KEYS_TIMER, if it never says so).
static KEYS_SUSPENDED: AtomicBool = AtomicBool::new(false);
/// The longest a recording may keep the shortcuts off.
const KEYS_TIMER: usize = 50;
const KEYS_MAX_MS: u32 = 60_000;

pub(super) fn keys_suspended() -> bool {
    KEYS_SUSPENDED.load(Ordering::SeqCst)
}

impl Dock {
    /// Pauses (`on`) or resumes every shortcut of the registry.
    pub(super) fn suspend_keys(&mut self, on: bool) {
        if KEYS_SUSPENDED.swap(on, Ordering::SeqCst) == on {
            return;
        }
        unsafe {
            if on {
                crate::shortcuts::suspend_all(self.hwnd);
                SetTimer(Some(self.hwnd), KEYS_TIMER, KEYS_MAX_MS, None);
                crate::log!("shortcuts: paused while Settings records one");
            } else {
                let _ = KillTimer(Some(self.hwnd), KEYS_TIMER);
                crate::shortcuts::apply_all(self.hwnd, &self.cfg);
                crate::log!("shortcuts: back on");
            }
        }
    }
}

/// Shortcut "focus_dock" (crate::shortcuts): the focus-the-dock hotkey.
pub fn apply_focus_hotkey(owner: HWND, spec: &str) {
    // Keep a previously chosen custom global focus key even beside Explorer.
    // Explorer owns Win+T, so only the taskbar hook handles that chord.
    let reserved = crate::shortcuts::parse(spec).ok().flatten().is_some_and(|c| c.mods == crate::shortcuts::WIN && matches!(c.vk, 0x54 | 0x42));
    let status = access::apply_hotkey(owner, if reserved { "none" } else { spec });
    crate::launchpad::wintap::apply_cycle(owner, spec, (!reserved).then_some(status));
}

/// Shortcut "window_on_top" (crate::shortcuts): "Always on top" for the
/// window in front.
pub fn apply_ontop_hotkey(owner: HWND, spec: &str) {
    ontop::apply_ontop_hotkey(owner, spec);
}

/// Shortcut "window_to_tray" (crate::shortcuts): "Hide to tray" for the
/// window in front.
pub fn apply_tray_hotkey(owner: HWND, spec: &str) {
    ontop::apply_tray_hotkey(owner, spec);
}

/// Shortcut "dock_apps" (crate::shortcuts): switching to apps by number.
pub fn apply_digits(owner: HWND, v: &str) {
    digits::apply(owner, v);
}

/// Implicit animations for several properties of one object at once
/// (a visual has a single collection, so they must be set together).
/// `props`: (property, duration ms). Empty when motion is off.
/// HSHELL_GETMINRECT: lParam points at { HWND, RECTS } (S2). The pointer
/// comes with a registered message anyone could send, so it is touched
/// only through Read/WriteProcessMemory on ourselves, which fail instead of
/// faulting on a bad address. Nothing is written unless the whole answer
/// fits in 16-bit coordinates.
unsafe fn answer_minrect(lp: isize) -> bool {
    use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
    use windows::Win32::System::Threading::GetCurrentProcess;
    if lp == 0 || lp % std::mem::align_of::<usize>() as isize != 0 {
        return false;
    }
    let me = GetCurrentProcess();
    let mut target = HWND::default();
    if ReadProcessMemory(me, lp as *const _, &mut target as *mut HWND as *mut _, std::mem::size_of::<HWND>(), None).is_err() {
        return false;
    }
    let Some(r) = minrect_for(target) else {
        return false;
    };
    let vals = [r.left, r.top, r.right, r.bottom];
    if vals.iter().any(|v| *v < i16::MIN as i32 || *v > i16::MAX as i32) {
        return false;
    }
    let shorts: [i16; 4] = vals.map(|v| v as i16);
    let ok = WriteProcessMemory(me, (lp + 8) as *const _, shorts.as_ptr() as *const _, 8, None).is_ok();
    crate::log!("minimize target {ok}: {:?}", vals);
    ok
}

/// Where window `h` (or its root window) minimizes to: its app's dock icon
/// as last drawn, screen px. None while the dock is hidden / fullscreen.
/// With a dock on every display: the dock on the window's own monitor.
pub(super) fn minrect_for(h: HWND) -> Option<RECT> {
    let root = unsafe { GetAncestor(h, GA_ROOT) };
    let on = views::window_monitor(if root.is_invalid() { h } else { root });
    MINRECTS.with(|m| views::pick_minrect(&m.borrow(), &[h, root], on))
}

fn implicit(c: &Compositor, v: &impl windows::core::Interface, props: &[(&str, i64)]) -> Result<()> {
    let runs: Vec<(&str, Run)> = props.iter().map(|&(p, ms)| (p, Run::Ease(ms))).collect();
    // (The top bar's callers, as before harmony: on with Windows'
    // animations, nothing attached without.)
    implicit_runs(c, v, &runs, motion().then_some(crate::motion::Mode::Full))
}

/// The dock's own implicit animations: each property on its `Run`, gated
/// by crate::motion::now() (reduced motion: fades only, REDUCED_MS).
pub(super) fn implicit_on(c: &Compositor, v: &impl windows::core::Interface, props: &[(&str, Run)]) -> Result<()> {
    implicit_runs(c, v, props, Some(crate::motion::now()))
}

/// `mode` None: nothing is attached (changes apply at once).
fn implicit_runs(c: &Compositor, v: &impl windows::core::Interface, props: &[(&str, Run)], mode: Option<crate::motion::Mode>) -> Result<()> {
    use windows::Foundation::TimeSpan;
    use windows::UI::Composition::{CompositionEasingFunction, CompositionObject, ICompositionAnimationBase, KeyFrameAnimation};
    let coll = c.CreateImplicitAnimationCollection()?;
    let speed = crate::effects::current().speed;
    for &(prop, run) in props {
        // The final length (at the chosen speed with full motion; never
        // stretched with reduced motion) and shape, decided in one place:
        // callers pass the base length.
        let Some((ms, shape)) = mode.and_then(|m| played(run, prop, m, speed)) else { continue };
        let key = HSTRING::from(prop);
        let lin: CompositionEasingFunction = c.CreateLinearEasingFunction()?.cast()?;
        // (Keyframes: progress, share of the way done, easing into it.)
        let keys: Vec<(f32, f32, CompositionEasingFunction)> = match shape {
            Shape::Curve(curve) if curve == crate::motion::LINEAR => vec![(1.0, 1.0, lin.clone())],
            Shape::Curve(((x1, y1), (x2, y2))) => vec![(1.0, 1.0, c.CreateCubicBezierEasingFunction(Vector2 { X: x1, Y: y1 }, Vector2 { X: x2, Y: y2 })?.cast()?)],
            Shape::Spring(spring, base) => spring.keys(base, crate::motion::SAMPLES).into_iter().skip(1).map(|(p, done)| (p, done, lin.clone())).collect(),
        };
        let d = TimeSpan { Duration: ms * 10_000 };
        let anim: KeyFrameAnimation = match prop {
            "Opacity" => c.CreateScalarKeyFrameAnimation()?.cast()?,
            "Offset" => c.CreateVector3KeyFrameAnimation()?.cast()?,
            // Size (Vector2).
            _ => c.CreateVector2KeyFrameAnimation()?.cast()?,
        };
        for (p, done, ease) in keys {
            let to = spring_key(done);
            anim.InsertExpressionKeyFrameWithEasingFunction(p, &HSTRING::from(to), &ease)?;
        }
        anim.SetDuration(d)?;
        anim.SetTarget(&key)?;
        coll.Insert(&key, &anim.cast::<ICompositionAnimationBase>()?)?;
    }
    v.cast::<CompositionObject>()?.SetImplicitAnimations(&coll)?;
    Ok(())
}

/// Pure: an implicit animation's keyframe `done` of the way (0..1 and a
/// spring's small overshoot) from where the property was to where it was
/// just set, as Composition's expression.
fn spring_key(done: f32) -> String {
    if done == 1.0 {
        "this.FinalValue".to_string()
    } else {
        format!("this.StartingValue + (this.FinalValue - this.StartingValue) * {done:.4}")
    }
}

/// Pure: the curve of an implicit animation of `prop`. One for all (fades,
/// slides and resizes ease out alike; polish audit F20): the shared
/// ease-out (motion::EASE_OUT), not a linear fade or a second resize curve.
fn implicit_curve(_prop: &str) -> ((f32, f32), (f32, f32)) {
    crate::motion::EASE_OUT
}

/// The dock's picture for the promo film (tests only). Declared down
/// here, with the tests: the tests that read this file's code read it up
/// to its first `#[cfg(test)]`.
#[cfg(test)]
pub(crate) mod promo;

/// The dock's pictures for the design audit (tests only).
#[cfg(test)]
#[path = "dock_harmony.rs"]
mod harmony;
#[cfg(test)]
pub(crate) use harmony::{paint_site_dock, render_readme_dock};
#[cfg(test)]
pub(crate) use label::font_compare_label;
#[cfg(test)]
pub(crate) use menu::font_compare_menu;

#[cfg(test)]
mod implicit_tests {
    use super::{played, Run, Shape, GLIDE, SNAP};

    /// 12.2 for the dock's implicit animations (the running dot's and the
    /// drop mark's fades ride on fade_to; the theme's cross, the plate's
    /// fades and the slides on these): with reduced motion an Opacity
    /// fades REDUCED_MS straight at any speed, nothing else moves; with
    /// full motion all is as designed at the speed.
    #[test]
    fn reduced_implicit_fades_are_60ms_straight_at_any_speed() {
        use crate::effects::ms_at;
        use crate::motion::{Mode, EASE_OUT, GLIDE_MAX_MS, LINEAR, REDUCED_MS, SPRING_GLIDE, THEME_MS};
        let reduced = Some((REDUCED_MS, Shape::Curve(LINEAR)));
        for speed in [0.25f32, 1.0, 4.0] {
            assert_eq!(played(Run::Ease(120), "Opacity", Mode::Reduced, speed), reduced, "{speed}");
            assert_eq!(played(Run::Ease(200), "Opacity", Mode::Reduced, speed), reduced, "{speed}");
            assert_eq!(played(Run::Linear(THEME_MS), "Opacity", Mode::Reduced, speed), reduced, "{speed}");
            assert_eq!(played(Run::Ease(40), "Opacity", Mode::Reduced, speed), Some((40, Shape::Curve(LINEAR))));
            for (run, prop) in [(SNAP, "Size"), (GLIDE, "Offset"), (Run::Ease(120), "Offset"), (Run::Linear(120), "Size")] {
                assert_eq!(played(run, prop, Mode::Reduced, speed), None, "{prop}");
            }
            assert_eq!(played(Run::Ease(120), "Opacity", Mode::Full, speed), Some((ms_at(120, speed), Shape::Curve(EASE_OUT))));
            assert_eq!(played(Run::Linear(THEME_MS), "Opacity", Mode::Full, speed), Some((ms_at(THEME_MS, speed), Shape::Curve(LINEAR))));
            assert_eq!(played(GLIDE, "Offset", Mode::Full, speed), Some((ms_at(GLIDE_MAX_MS, speed), Shape::Spring(SPRING_GLIDE, GLIDE_MAX_MS))));
        }
    }

    #[test]
    fn fades_slides_and_resizes_share_the_ease_out() {
        for prop in ["Opacity", "Offset", "Size"] {
            assert_eq!(super::implicit_curve(prop), crate::motion::EASE_OUT, "{prop}");
        }
    }

    /// implicit() (and fade / slide_anim over it) applies the animation
    /// speed itself: a caller passing an already scaled length would get
    /// it twice (polish audit F7).
    #[test]
    fn callers_pass_base_lengths() {
        let srcs = [
            ("mod.rs", include_str!("mod.rs")),
            ("preview.rs", include_str!("preview.rs")),
            ("stack.rs", include_str!("stack.rs")),
            ("split.rs", include_str!("split.rs")),
            ("items.rs", include_str!("items.rs")),
            ("fx.rs", include_str!("fx.rs")),
            ("gpu.rs", include_str!("gpu.rs")),
            ("drag.rs", include_str!("drag.rs")),
            ("../topbar/mod.rs", include_str!("../topbar/mod.rs")),
        ];
        let scaled = ["effects", "::ms("].concat();
        let calls = ["fade(&", "slide_anim(&", "implicit(c", "implicit_visual(c"].map(String::from);
        for (name, src) in srcs {
            for (n, line) in src.lines().enumerate() {
                if calls.iter().any(|c| line.contains(c.as_str())) {
                    assert!(!line.contains(&scaled), "{name}:{}: scaled twice: {}", n + 1, line.trim());
                }
            }
        }
    }
}

/// Compositor-driven fade: once attached, every Opacity change on the
/// visual animates over `ms` instead of jumping. Costs the UI thread nothing.
pub(super) fn fade(c: &Compositor, v: &impl windows::core::Interface, ms: i64) -> Result<()> {
    implicit(c, v, &[("Opacity", ms)])
}

/// Compositor-driven slide: Offset changes glide (GLIDE over `ms`, the
/// bar's place: 7.4). Nothing slides with reduced motion.
fn slide_anim(c: &Compositor, v: &impl windows::core::Interface, ms: i64) -> Result<()> {
    implicit_on(c, v, &[("Offset", Run::Spring(crate::motion::SPRING_GLIDE, ms))])
}

/// Shows (`on`) or hides `v` as the pointer's changes go (6.3): on over
/// FADE_IN_MS on the ease-out, off over FADE_OUT_MS on the ease-in, from
/// where it is; with reduced motion a straight REDUCED_MS fade the speed
/// does not stretch (fade_to: crate::motion::fade_plan). Call it when the
/// state changes, not on every layout (each call starts the fade again).
pub(super) fn show(c: &Compositor, v: &impl windows::core::Interface, on: bool) {
    use crate::motion::{fade_to, EASE_IN, EASE_OUT, FADE_IN_MS, FADE_OUT_MS};
    let _ = if on { fade_to(c, v, 1.0, FADE_IN_MS, EASE_OUT) } else { fade_to(c, v, 0.0, FADE_OUT_MS, EASE_IN) };
}

fn make_dot(c: &Compositor, fill: Color, style: Indicator, vertical: bool) -> Result<(SpriteVisual, Option<CompositionRoundedRectangleGeometry>)> {
    let dot = c.CreateSpriteVisual()?;
    // Appears / disappears with a fade (show(): 90 in, 120 out), grows
    // for the app in front on SNAP (a small thing's state, 7.4).
    implicit_on(c, &dot, &[("Size", SNAP)])?;
    let geom = style_indicator(c, &dot, style, fill, vertical, false, false)?;
    if let Some(g) = &geom {
        size_anim(c, g)?;
    }
    dot.SetOpacity(0.0)?;
    Ok((dot, geom))
}

/// Compositor-driven resize (the indicator growing for the app in front).
fn size_anim(c: &Compositor, v: &impl windows::core::Interface) -> Result<()> {
    implicit_on(c, v, &[("Size", SNAP)])
}

/// A material's cross-fade (the finish or the theme changed: THEME_MS,
/// straight, 7.1).
pub(super) fn theme_fade(c: &Compositor, v: &impl windows::core::Interface) -> Result<()> {
    implicit_on(c, v, &[("Opacity", Run::Linear(crate::motion::THEME_MS))])
}

impl Dock {
    /// The primary dock (loads the config, starts the workers).
    unsafe fn new(hwnd: HWND, ticker: Ticker, gfx: Rc<Gfx>) -> Result<Self> {
        let cfg = Config::load();
        crate::i18n::init(&cfg.language);
        crate::desklayer::shell::sync_current();
        crate::log!("language: {} (setting {:?})", crate::i18n::current(), cfg.language);
        let theme_raw = cfg.load_theme(&crate::config::app_dir());
        let sys = crate::syslook::current();
        fx::publish(&cfg, &sys);
        // After the effects are in force: Glass needs the blur.
        let theme = theme_raw.resolve(&sys).windows_shell();
        let (workers, events) = worker::start(hwnd.0 as isize);
        let shelf = Rc::new(RefCell::new(IconShelf::default()));
        let mut dock = Self::build(hwnd, ticker, gfx, cfg, theme_raw, theme, sys, (workers, events), shelf, true)?;
        // Start where the user is (following) or on the primary monitor.
        let start = if dock.cfg.dock.follows_mouse() { crate::monitors::under_cursor() } else { crate::monitors::primary() };
        dock.mon = start.map(|m| m.handle).unwrap_or(0);
        dock.place_window();
        dock.build_items()?;
        dock.workers.request_refresh();
        dock.finish_start();
        Ok(dock)
    }

    /// A dock for another display (views.rs): the primary's config, theme,
    /// window list and icon shelf; workers only for its own icons, stacks
    /// and wallpaper.
    unsafe fn new_view(hwnd: HWND, ticker: Ticker, seed: &Seed, mon: isize, display: Option<String>) -> Result<Self> {
        let workers = worker::start_view(hwnd.0 as isize);
        let s = &seed.snap;
        // Its own display's settings (dock.per_display) from the start.
        let cfg = s.cfg.for_display(display.as_deref());
        let mut dock = Self::build(hwnd, ticker, seed.gfx.clone(), cfg, seed.theme_raw.clone(), seed.theme.clone(), seed.sys, workers, seed.shelf.clone(), false)?;
        dock.mon = mon;
        dock.display = display;
        dock.view_previewing = s.previewing;
        dock.place_window();
        dock.build_items()?;
        dock.wins_gen = s.wins_gen;
        dock.apply_snapshot(s.wins.clone());
        dock.finish_start();
        dock.set_active(s.active);
        Ok(dock)
    }

    /// Items at rest, label and material set up, auto-hide in force.
    fn finish_start(&mut self) {
        for it in &mut self.items {
            it.pres = it.pres_t;
        }
        self.style_label();
        // Covered already? (Decided before anything shows; reveal() looks
        // again right before showing.)
        self.refresh_cover();
        // The material (and the shadow's look) first: the layout places it.
        self.apply_material();
        self.layout();
        if self.cfg.dock.autohide {
            self.set_hidden(true);
        }
    }

    /// The window's visuals and the dock's state, placed nowhere yet.
    #[allow(clippy::too_many_arguments)]
    unsafe fn build(
        hwnd: HWND,
        ticker: Ticker,
        gfx: Rc<Gfx>,
        cfg: Config,
        theme_raw: Theme,
        theme: Theme,
        sys: crate::syslook::SysLook,
        (workers, events): (Workers, worker::EventRx),
        shelf: Rc<RefCell<IconShelf>>,
        primary: bool,
    ) -> Result<Self> {
        let radius = Look::from_cfg(&cfg.dock, true).radius;
        let c = gfx.compositor.clone();

        let enable = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_HOSTBACKDROPBRUSH,
            &enable as *const _ as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );

        let interop: ICompositorDesktopInterop = c.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
        let root = c.CreateContainerVisual()?;
        target.SetRoot(&root)?;

        // Bar: blurred backdrop + tint, rounded clip, hairline border.
        let bar = c.CreateContainerVisual()?;
        let bar_geom = c.CreateRoundedRectangleGeometry()?;
        bar_geom.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
        bar.SetClip(&c.CreateGeometricClipWithGeometry(&bar_geom)?)?;
        let backdrop = c.CreateSpriteVisual()?;
        // The blur through the glass's graph (crate::glass; apply_material
        // keeps it in step with the theme).
        let glass = crate::glass::backdrop(&gfx, &theme, crate::theme::Material::Dock)?;
        backdrop.SetBrush(&glass)?;
        let tint = c.CreateSpriteVisual()?;
        tint.SetBrush(&c.CreateColorBrushWithColor(rgba(crate::glass::tint_for(&theme, crate::theme::Material::Dock, Some(&glass))))?)?;
        let mica = c.CreateSpriteVisual()?;
        mica.SetOpacity(0.0)?;
        // Material switches cross-fade instead of popping.
        theme_fade(&c, &mica)?;
        theme_fade(&c, &backdrop)?;
        bar.Children()?.InsertAtTop(&mica)?;
        bar.Children()?.InsertAtTop(&backdrop)?;
        bar.Children()?.InsertAtTop(&tint)?;
        let border_geom = c.CreateRoundedRectangleGeometry()?;
        border_geom.SetCornerRadius(Vector2 { X: radius - 0.5, Y: radius - 0.5 })?;
        border_geom.SetOffset(Vector2 { X: 0.5, Y: 0.5 })?;
        let border_shape = c.CreateSpriteShapeWithGeometry(&border_geom)?;
        border_shape.SetStrokeBrush(&c.CreateColorBrushWithColor(rgba(theme.dock_border()))?)?;
        border_shape.SetStrokeThickness(1.0)?;
        let border = c.CreateShapeVisual()?;
        border.Shapes()?.Append(&border_shape)?;
        border.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        bar.Children()?.InsertAtTop(&border)?;
        // The lit top edge, just inside the border.
        let highlight_geom = c.CreateRoundedRectangleGeometry()?;
        highlight_geom.SetCornerRadius(Vector2 { X: (radius - 1.5).max(0.0), Y: (radius - 1.5).max(0.0) })?;
        highlight_geom.SetOffset(Vector2 { X: 1.5, Y: 1.5 })?;
        let highlight_shape = c.CreateSpriteShapeWithGeometry(&highlight_geom)?;
        highlight_shape.SetStrokeThickness(1.0)?;
        // Lit from the start (a new dock, another display's), as apply_theme does.
        paint_highlight(&c, &highlight_shape, &theme, crate::theme::Material::Dock);
        let highlight = c.CreateShapeVisual()?;
        highlight.Shapes()?.Append(&highlight_shape)?;
        highlight.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        bar.Children()?.InsertAtTop(&highlight)?;
        let highlight_v = highlight;
        let shadow = shadow::BarShadow::new(&gfx)?;
        slide_anim(&c, shadow.slide(), SLIDE_MS)?;

        let layer = c.CreateContainerVisual()?;
        // Under every icon (items are added on top of it).
        let drop_glow = c.CreateSpriteVisual()?;
        let drop_glow_geom = c.CreateRoundedRectangleGeometry()?;
        drop_glow.SetClip(&c.CreateGeometricClipWithGeometry(&drop_glow_geom)?)?;
        // (Lit and put out by show(): 90 in, 120 out.)
        drop_glow.SetOpacity(0.0)?;
        layer.Children()?.InsertAtBottom(&drop_glow)?;
        let (label, label_parts) = label::make_label(&c)?;
        let slide = c.CreateContainerVisual()?;
        slide_anim(&c, &slide, SLIDE_MS)?;
        root.Children()?.InsertAtTop(&slide)?;
        slide.Children()?.InsertAtTop(&bar)?;
        slide.Children()?.InsertAtTop(&layer)?;
        let rings = ring::Rings::new(&c)?;
        slide.Children()?.InsertAtTop(rings.layer())?;
        slide.Children()?.InsertAtTop(&label)?;

        Ok(Self {
            primary,
            wins_gen: 0,
            active_top: HWND::default(),
            view_previewing: false,
            display: None,
            fx_seen: crate::effects::current(),
            hwnd,
            gfx,
            look: Look::from_cfg(&cfg.dock, crate::effects::items().magnify),
            theme_raw,
            sys,
            cfg,
            theme,
            _target: target,
            root,
            slide,
            layer,
            bar,
            bar_fill: vec![backdrop, tint],
            mica,
            bar_geom,
            border_geom,
            border_shape,
            highlight_geom,
            highlight_shape,
            highlight_v,
            rings,
            shadow,
            shadow_spec: Default::default(),
            label,
            label_parts,
            held: false,
            glow_on: false,
            label_armed: false,
            label_waiting: false,
            labels: HashMap::new(),
            items: Vec::new(),
            scale: 1.0,
            origin: POINT::default(),
            monitor: RECT::default(),
            along: 0.0,
            frame: Frame { pos: Position::Bottom, depth: 0.0 },
            mon: 0,
            bar_rect: Rect::default(),
            hidden: false,
            hover_item: None,
            hover_app: None,
            press_at: None,
            drag: None,
            press_path: None,
            ole_drag: false,
            file_hover: None,
            undo: None,
            poofs: Vec::new(),
            trash_dirty: false,
            preview: None,
            pending_edits: Default::default(),
            edit_gen: 0,
            pin_fixes: Vec::new(),
            region_expanded: false,
            fullscreen: false,
            fullscreen_window: HWND::default(),
            start_hidden: None,
            strip_ready: false,
            mouse: None,
            tracking: false,
            region: (0, 0, 0, 0),
            pressed: None,
            press_back: None,
            last_tick: Instant::now(),
            ticker,
            appbar: false,
            placed_area: RECT::default(),
            workers,
            events,
            last_seq: 0,
            last_wins: Vec::new(),
            shelf,
            icon_wait: HashSet::new(),
            rest_fails: HashMap::new(),
            first_snapshot: false,
            icons_logged: false,
            marks: HashMap::new(),
            hang: hung::HangWatch::default(),
            hang_timer: false,
            stack_seq: 0,
            stack_req: None,
            stack_plan: None,
            sound: Default::default(),
            drop_glow,
            drop_glow_geom,
            drop_stack: None,
            kb: None,
            kb_ring: false,
            kb_prev: HWND::default(),
            last_stack: String::new(),
        })
    }

    // ---- window placement & taskbar --------------------------------------

    /// NopBar hides the taskbar right now: asked for, and auto-hide not
    /// refused in this run (taskbar.rs). Otherwise the dock shares the
    /// screen with it (placement.rs).
    fn hides_taskbar(&self) -> bool {
        self.cfg.hide_taskbar && !taskbar::refused()
    }

    /// After a work-area or display change: if auto-hide was switched off
    /// while NopBar hides the taskbar (its strip reserved again: an empty
    /// band under maximized windows), ask again and run the freed check.
    unsafe fn reassert_taskbar(&mut self) {
        if !self.primary || !self.hides_taskbar() || SHUTTING_DOWN.load(Ordering::SeqCst) {
            return;
        }
        if taskbar::reassert() {
            HIDE_CHECK_STEP.store(0, Ordering::SeqCst);
            SetTimer(Some(self.hwnd), HIDE_CHECK_TIMER, HIDE_CHECK_MS, None);
        }
    }

    unsafe fn apply_taskbar_mode(&mut self) {
        let hide = self.hides_taskbar();
        if !self.primary {
            // A view keeps only its own AppBar in step; the primary hides
            // or gives back the taskbar.
            if hide && !self.appbar {
                taskbar::appbar_register(self.hwnd);
                self.appbar = true;
            } else if !hide && self.appbar {
                taskbar::appbar_remove(self.hwnd);
                self.appbar = false;
            }
            self.place_window();
            return;
        }
        HIDE_TASKBAR.store(hide, Ordering::SeqCst);
        if hide {
            // Auto-hide first; the windows go once the strip is seen free
            // (HIDE_CHECK_TIMER), or the taskbar is given back.
            taskbar::hide();
            // The top bar goes back to the top edge.
            crate::topbar::follow_taskbar();
            if !self.appbar {
                taskbar::appbar_register(self.hwnd);
                self.appbar = true;
            }
            HIDE_CHECK_STEP.store(0, Ordering::SeqCst);
            SetTimer(Some(self.hwnd), HIDE_CHECK_TIMER, HIDE_CHECK_MS, None);
        } else {
            let _ = KillTimer(Some(self.hwnd), HIDE_CHECK_TIMER);
            if self.appbar {
                taskbar::appbar_remove(self.hwnd);
                self.appbar = false;
            }
            taskbar::restore();
            taskbar::log_kept();
            crate::topbar::follow_taskbar();
            // Explorer reserves the taskbar's strip again a moment after
            // the restore: look again then (and on SPI_SETWORKAREA).
            schedule_coexist_checks(self.hwnd);
        }
        self.place_window();
        // The other displays' docks follow (their AppBars, their places).
        views::post_reconcile();
    }

    /// HIDE_CHECK_TIMER: did auto-hide free the taskbar's strip? Then its
    /// windows are hidden (taskbar.rs). Not after a while: ask once more,
    /// then give the taskbar back and sit next to it, with a notice.
    unsafe fn check_taskbar_hide(&mut self) {
        let _ = KillTimer(Some(self.hwnd), HIDE_CHECK_TIMER);
        if !self.hides_taskbar() {
            return;
        }
        match taskbar::check_hide() {
            crate::placement::HideCheck::Freed => {
                let step = HIDE_CHECK_STEP.load(Ordering::SeqCst);
                if step > 0 {
                    crate::log!("taskbar: auto-hide freed the strip after {} check(s)", step + 1);
                }
            }
            crate::placement::HideCheck::NotWanted => {}
            crate::placement::HideCheck::Waiting => {
                let step = HIDE_CHECK_STEP.fetch_add(1, Ordering::SeqCst) + 1;
                if step == HIDE_CHECKS {
                    taskbar::retry_hide();
                }
                if step < HIDE_CHECKS * 2 {
                    SetTimer(Some(self.hwnd), HIDE_CHECK_TIMER, HIDE_CHECK_MS, None);
                    return;
                }
                let first = taskbar::give_up_hiding();
                HIDE_TASKBAR.store(false, Ordering::SeqCst);
                if self.appbar {
                    taskbar::appbar_remove(self.hwnd);
                    self.appbar = false;
                }
                schedule_coexist_checks(self.hwnd);
                crate::topbar::follow_taskbar();
                self.place_window();
                self.layout();
                views::post_reconcile();
                if first {
                    crate::taskbar::notice_refused();
                }
            }
        }
    }

    /// The work area or the taskbar changed (SPI_SETWORKAREA, a check
    /// after a restore): follow the taskbar we share the screen with (or
    /// the top bar that moved). Nothing to do when the area is the same,
    /// so our own AppBar's changes cannot loop through here.
    unsafe fn follow_taskbar(&mut self) {
        let Some(info) = crate::monitors::info(self.mon).or_else(crate::monitors::primary) else { return };
        // The work area and our AppBars' strips on the log, once per change.
        taskbar::log_edges(info.rect);
        if self.dock_area(&info) != self.placed_area || info.rect != self.monitor {
            crate::log!("dock: taskbar or work area moved; placing the dock clear of it");
            self.place_window();
            self.layout();
        }
    }

    /// Where the dock may be on `info`'s monitor (placement::dock_area):
    /// clear of the top bar, at the top or the bottom.
    fn dock_area(&self, info: &crate::monitors::Monitor) -> RECT {
        let hiding = self.hides_taskbar();
        let bars = if hiding { Vec::new() } else { taskbar::bars_on(info.handle) };
        crate::placement::dock_area(info.rect, info.work, &bars, hiding, crate::topbar::space_for(info.rect))
    }

    unsafe fn place_window(&mut self) {
        // Our monitor (the primary one if it went away), with its own DPI:
        // the window may still be on another monitor while it moves.
        if !self.primary && crate::monitors::info(self.mon).is_none() {
            // A view whose display went away: reconcile closes it.
            return;
        }
        let Some(info) = crate::monitors::info(self.mon).or_else(crate::monitors::primary) else { return };
        self.mon = info.handle;
        views::note_monitor(self.hwnd, self.mon);
        let rescaled = info.scale() != self.scale;
        self.scale = info.scale();
        let mi = MONITORINFO { rcMonitor: info.rect, rcWork: info.work, ..Default::default() };
        if self.monitor != mi.rcMonitor && self.appbar {
            // Leaving a monitor: give its edge back before reserving the new one.
            taskbar::appbar_reserve(self.hwnd, self.monitor, 0, self.cfg.dock.position);
        }
        self.monitor = mi.rcMonitor;
        let pos = self.cfg.dock.position;
        // With the taskbar hidden we own the edges; otherwise stay inside
        // the work area and clear of every taskbar strip, auto-hide ones
        // included (placement.rs). Side docks start below the top bar (end
        // above it at the bottom); a bottom dock sits right on a bar at
        // the bottom, its auto-hide strip too.
        let area = self.dock_area(&info);
        self.placed_area = area;
        let headroom = if pos == Position::Bottom { HEADROOM } else { SIDE_HEADROOM };
        let depth = self.look.icon * self.look.max_scale + PAD_CROSS + EDGE_GAP + headroom;
        let depth_px = (depth * self.scale).ceil() as i32;
        self.frame = Frame { pos, depth };
        let r = self.frame.window_rect(area, depth_px);
        let along_px = if self.frame.vertical() { r.bottom - r.top } else { r.right - r.left };
        self.along = along_px as f32 / self.scale;
        self.origin = POINT { x: r.left, y: r.top };
        let _ = SetWindowPos(
            self.hwnd,
            None,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        // Topmost, or still behind a fullscreen app.
        self.restack();
        // The shadow's window: the same place, just under the dock.
        self.shadow.place(self.hwnd, r);
        self.root.SetScale(Vector3 { X: self.scale, Y: self.scale, Z: 1.0 }).ok();
        self.region = (0, 0, 0, 0);
        self.region_expanded = false;
        if self.appbar {
            // Auto-hide docks float over windows: keep the AppBar (for its
            // fullscreen notices) but reserve nothing.
            let reserve = if self.cfg.dock.autohide || !self.cfg.dock.reserve_work_area {
                0
            } else {
                ((self.look.icon + PAD_CROSS * 2.0 + EDGE_GAP) * self.scale).ceil() as i32
            };
            // Over a bar at the bottom that reserves nothing: its strip too
            // (placement::dock_reserve; counted as ours by the hide check).
            let r = crate::placement::dock_reserve(pos, reserve, crate::topbar::space_for(self.monitor));
            taskbar::appbar_reserve_dock(self.hwnd, self.monitor, reserve, pos, r);
        }
        if self.primary {
            let len = if self.bar_rect.w > 0.0 { self.bar_rect.w } else { 400.0 };
            sensors::rebuild(self.cfg.dock.follows_mouse(), pos, self.hides_taskbar(), self.mon, len);
        }
        // A new DPI (whichever way it was noticed: a work-area change can
        // place the dock before the display timer does): icons and labels
        // are drawn again at the new pixel size (sharp.rs).
        if rescaled && !self.items.is_empty() {
            self.labels.clear();
            self.rebuild_items();
        }
    }

    /// Re-place after a display or DPI change (or a move): new DPI means
    /// icons and labels must be drawn again at the new pixel size.
    unsafe fn replace_on_monitor(&mut self) {
        // Cards are placed for the old monitor / scale.
        preview::hide();
        // Another monitor may run at another rate.
        fx::display_changed();
        let before_mon = self.monitor;
        // (Draws icons and labels again if the DPI changed.)
        self.place_window();
        if self.monitor != before_mon {
            // Covered on the old monitor says nothing about this one.
            self.refresh_cover();
        }
        self.layout();
        // New resolution or monitor: the mica snapshot no longer fits.
        if self.monitor != before_mon {
            self.refresh_mica();
            // Showing only its display's apps: a new display, new apps.
            if self.filters_by_display() {
                self.sync_windows();
            }
        }
    }

    /// A sensor saw the pointer rest on monitor `mon`'s dock edge.
    unsafe fn move_to_monitor(&mut self, mon: isize) {
        if !self.primary || !self.cfg.dock.follows_mouse() || mon == self.mon || (crate::popup::is_tracking() && !crate::companion::convenience::menu_open()) || stack::is_open() || self.pressed.is_some() {
            return;
        }
        let Some(info) = crate::monitors::info(mon) else { return };
        if input::fullscreen_window(self.hwnd, Some(mon), info.rect) {
            return;
        }
        crate::log!("dock: following the mouse to monitor {:?}", info.rect);
        self.mon = mon;
        self.replace_on_monitor();
    }

    /// The primary dock goes to monitor `mon` (the main display, views.rs).
    unsafe fn go_to_monitor(&mut self, mon: isize) {
        if mon != self.mon {
            self.mon = mon;
            self.replace_on_monitor();
        }
    }

}

/// Follow the taskbar for a few seconds after it was given back.
unsafe fn schedule_coexist_checks(hwnd: HWND) {
    COEXIST_STEP.store(0, Ordering::SeqCst);
    SetTimer(Some(hwnd), COEXIST_TIMER, COEXIST_STEPS_MS[0], None);
}

/// The dock handling the current message, else the one the user touched
/// last, else the primary (views.rs). None while it is busy.
fn with_dock<R>(f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    views::with_dock_at(views::target(), f)
}

fn dip(lp: LPARAM, scale: f32) -> (f32, f32) {
    let x = (lp.0 & 0xFFFF) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
    (x / scale, y / scale)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let me = hwnd.0 as isize;
    // Its CPU counts to the dock (desklayer::load: the stats panel's split).
    let _span = crate::desklayer::load::span(crate::desklayer::load::Part::Dock);
    // While this message is handled, with_dock means this window's dock.
    let _ctx = views::enter(me);
    if matches!(msg, WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_MBUTTONDOWN | WM_RBUTTONDOWN | WM_MOUSEWHEEL) {
        // Menus, previews and stacks opened from here call back to it.
        views::set_active(me);
    }
    if me != views::primary_hwnd() {
        // A dock on another display: broadcasts and session messages are
        // the primary's (views.rs).
        if let Some(r) = views::view_message(hwnd, msg, wp, lp) {
            return r;
        }
    }
    let tb = WM_TASKBAR_CREATED.load(Ordering::SeqCst);
    if tb != 0 && msg == tb {
        // Our notification-area icon: back after an explorer restart, and
        // again for our tray spy's own broadcast (so the top bar sees it).
        if !SHUTTING_DOWN.load(Ordering::SeqCst) {
            trayicon::taskbar_created();
        }
        if SHUTTING_DOWN.load(Ordering::SeqCst) {
            return LRESULT(0);
        }
        // Explorer's broadcast always means it (re)started. Our tray's own
        // ("apps, add your icons again", tagged) does only while a new
        // explorer runs that this path has not run for yet: explorer's own
        // may come later, or not at all.
        let pid = taskbar::explorer_pid();
        let own = crate::tray::is_own_broadcast(wp, lp);
        if !own {
            // Explorer's notification area is up: the tray spy may return.
            crate::tray::explorer_announced();
        }
        if !restart_due(own, pid, HANDLED_EXPLORER.load(Ordering::SeqCst)) {
            return LRESULT(0);
        }
        if pid != 0 {
            HANDLED_EXPLORER.store(pid, Ordering::SeqCst);
        }
        crate::log!("explorer started: pid {pid} (TaskbarCreated{})", if own { ", ours" } else { "" });
        // Explorer restarted: the taskbar came back, hide it again, follow
        // the new explorer process and keep checking for a few seconds.
        // Both AppBars are forgotten by the new shell: top bar first.
        reregister_appbars();
        start_reserve_checks(hwnd);
        hook_explorer();
        worker::watch_explorer(taskbar::explorer_pid());
        // The new taskbar has new buttons: watch them for progress / badges.
        badges::resubscribe();
        REHIDE_STEP.store(0, Ordering::SeqCst);
        SetTimer(Some(hwnd), REHIDE_TIMER, REHIDE_STEPS_MS[0], None);
        return LRESULT(0);
    }
    let sh = WM_SHELLHOOK.load(Ordering::SeqCst);
    if sh != 0 && msg == sh {
        // HSHELL_GETMINRECT (sent, not posted): Windows asks where a window
        // minimizes to. Answer with its dock icon so the OS animation flies
        // there. The rect is RECTS (i16 x4) at lParam+8, physical px (S2).
        if wp.0 & 0x7FFF == 5 {
            return LRESULT(answer_minrect(lp.0) as isize);
        }
        // WINDOWCREATED/DESTROYED/ACTIVATED/REDRAW/WINDOWREPLACED and
        // RUDEAPPACTIVATED (0x8004) all mean the window list may have changed.
        if matches!(wp.0 & 0x7FFF, 1 | 2 | 4 | 6 | 13) {
            schedule_refresh();
        }
        // A window flashes for attention (HSHELL_FLASH = REDRAW | 0x8000).
        if wp.0 == 0x8006 {
            views::for_each(|d| d.attention(HWND(lp.0 as *mut _)));
        }
        // Activation: a fullscreen app may have come to the front (or left).
        if wp.0 & 0x7FFF == 4 {
            schedule_fullscreen_check();
            views::for_each(|d| d.report_active(HWND(lp.0 as *mut _)));
        }
        return LRESULT(0);
    }
    match msg {
        WM_COPYDATA => LRESULT((crate::notes::toast::receive(lp) || crate::companion::agent_toast::receive(lp)) as isize),
        WM_DEV_HANG if DEV_SIGNALS.load(Ordering::SeqCst) => {
            crate::log!("dev: hanging the UI thread for 40 s");
            std::thread::sleep(std::time::Duration::from_secs(40));
            LRESULT(0)
        }
        WM_DEV_PANIC if DEV_SIGNALS.load(Ordering::SeqCst) => {
            panic!("dev panic requested");
        }
        WM_APPLY_TASKBAR => {
            // Top bar first: side docks keep below what it reserves.
            crate::topbar::apply_appbar();
            with_dock(|d| {
                d.apply_taskbar_mode();
                d.layout();
            });
            crate::log!("startup: taskbar mode at {} ms", since_start());
            // First run ever: the Welcome page, once.
            if with_dock(|d| d.take_first_run()).unwrap_or(false) {
                crate::log!("startup: first run, opening the Welcome page");
                crate::settings::launch_welcome();
            }
            follow_foreground(GetForegroundWindow());
            schedule_fullscreen_check();
            LRESULT(0)
        }
        worker::WM_EVENTS => {
            worker::events_taken(me);
            if with_dock(|d| d.drain_events()).is_none() {
                SetTimer(Some(hwnd), EVENTS_RETRY_TIMER, 50, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == EVENTS_RETRY_TIMER => {
            let _ = KillTimer(Some(hwnd), EVENTS_RETRY_TIMER);
            if with_dock(|d| d.drain_events()).is_none() {
                SetTimer(Some(hwnd), EVENTS_RETRY_TIMER, 50, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == EXPLORER_WAIT_TIMER => {
            let _ = KillTimer(Some(hwnd), EXPLORER_WAIT_TIMER);
            let pid = taskbar::explorer_pid();
            if pid != 0 {
                // New explorer is up: follow it and hide its taskbar right away
                // instead of waiting for its TaskbarCreated broadcast.
                hook_explorer();
                if HIDE_TASKBAR.load(Ordering::SeqCst) && !SHUTTING_DOWN.load(Ordering::SeqCst) {
                    taskbar::rehide();
                    // The new taskbar is hidden once its strip is seen free
                    // (auto-hide asked again if not), even if its
                    // TaskbarCreated broadcast is late or lost.
                    HIDE_CHECK_STEP.store(0, Ordering::SeqCst);
                    SetTimer(Some(hwnd), HIDE_CHECK_TIMER, HIDE_CHECK_MS, None);
                }
                worker::watch_explorer(pid);
                crate::log!("explorer back: pid {pid}");
                // Our AppBars are re-registered on its TaskbarCreated; if
                // that is lost or too early, these checks catch it.
                start_reserve_checks(hwnd);
            } else if EXPLORER_WAIT_LEFT.fetch_sub(1, Ordering::SeqCst) > 1 {
                SetTimer(Some(hwnd), EXPLORER_WAIT_TIMER, 100, None);
            }
            LRESULT(0)
        }
        WM_TICK => {
            views::tick_taken(me);
            with_dock(|d| d.tick());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == REFRESH_TIMER => {
            let _ = KillTimer(Some(hwnd), REFRESH_TIMER);
            REFRESH_PENDING.store(false, Ordering::SeqCst);
            if with_dock(|d| d.workers.request_refresh()).is_none() {
                // Dock is busy (modal context menu): try again shortly.
                schedule_refresh();
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            with_dock(|d| {
                if d.fullscreen && !d.dragging() {
                    // Covered, yet the pointer reached us: look again (the
                    // app in front may have changed unnoticed); rest meanwhile.
                    schedule_fullscreen_check();
                    return;
                }
                if !d.tracking {
                    d.on_mouse_enter();
                }
                let (x, y) = dip(lp, d.scale);
                let (u, v) = d.frame.from_win(x, y);
                d.on_mouse_move(u, v)
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == HIDE_CHECK_TIMER => {
            if with_dock(|d| d.check_taskbar_hide()).is_none() {
                // Busy (a menu is open): look again shortly.
                SetTimer(Some(hwnd), HIDE_CHECK_TIMER, HIDE_CHECK_MS, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == WORKAREA_TIMER || wp.0 == COEXIST_TIMER => {
            let _ = KillTimer(Some(hwnd), wp.0);
            if SHUTTING_DOWN.load(Ordering::SeqCst) {
                return LRESULT(0);
            }
            // Top bar first: the dock keeps below what it reserves.
            crate::topbar::follow_taskbar();
            if !views::for_each(|d| d.follow_taskbar()) {
                SetTimer(Some(hwnd), wp.0, WORKAREA_DEBOUNCE_MS, None);
                return LRESULT(0);
            }
            if wp.0 == WORKAREA_TIMER {
                WORKAREA_PENDING.store(false, Ordering::SeqCst);
                // The strip reserved again under a hidden taskbar?
                views::with_primary(|d| d.reassert_taskbar());
            } else {
                let next = COEXIST_STEP.fetch_add(1, Ordering::SeqCst) as usize + 1;
                if let Some(&ms) = COEXIST_STEPS_MS.get(next) {
                    SetTimer(Some(hwnd), COEXIST_TIMER, ms, None);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == RESERVE_CHECK_TIMER => {
            let _ = KillTimer(Some(hwnd), RESERVE_CHECK_TIMER);
            if SHUTTING_DOWN.load(Ordering::SeqCst) {
                return LRESULT(0);
            }
            // Only with an explorer to talk to (one starting answers later).
            if taskbar::explorer_pid() != 0 {
                let lost = taskbar::lost_reservations();
                if !lost.is_empty() {
                    crate::log!("appbar: the shell does not keep {} reserved strip(s) free after an explorer restart; registering again", lost.len());
                    reregister_appbars();
                }
            }
            let next = RESERVE_CHECK_STEP.fetch_add(1, Ordering::SeqCst) as usize + 1;
            if let Some(&ms) = RESERVE_CHECK_STEPS_MS.get(next) {
                SetTimer(Some(hwnd), RESERVE_CHECK_TIMER, ms, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == REHIDE_TIMER => {
            let _ = KillTimer(Some(hwnd), REHIDE_TIMER);
            if SHUTTING_DOWN.load(Ordering::SeqCst) {
                return LRESULT(0);
            }
            if HIDE_TASKBAR.load(Ordering::SeqCst) {
                taskbar::rehide();
            }
            views::for_each(|d| d.place_window());
            let next = REHIDE_STEP.fetch_add(1, Ordering::SeqCst) as usize + 1;
            if let Some(&ms) = REHIDE_STEPS_MS.get(next) {
                SetTimer(Some(hwnd), REHIDE_TIMER, ms, None);
            }
            LRESULT(0)
        }
        crate::companion::downloads::WM_CHANGED => {crate::companion::downloads::changed(wp,lp);LRESULT(0)}
        WM_TRASH_CHANGED => {
            // New-style delivery: the notice lives in shared memory that
            // must be locked and released for every message.
            use windows::Win32::UI::Shell::{SHChangeNotification_Lock, SHChangeNotification_Unlock};
            let mut pidls: *mut *mut windows::Win32::UI::Shell::Common::ITEMIDLIST = std::ptr::null_mut();
            let mut event = 0i32;
            let lock = SHChangeNotification_Lock(HANDLE(wp.0 as *mut _), lp.0 as u32, Some(&mut pidls), Some(&mut event));
            if !lock.is_invalid() {

                let _ = SHChangeNotification_Unlock(lock);
            }
            if event & windows::Win32::UI::Shell::SHCNE_UPDATEIMAGE.0 as i32 == 0 {return LRESULT(0);}
            // One refresh per burst, at most 300 ms after the first notice
            // (a steady stream does not keep pushing it back).
            if !TRASH_PENDING.swap(true, Ordering::SeqCst) {
                SetTimer(Some(hwnd), TRASH_TIMER, 300, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TRASH_TIMER => {
            let _ = KillTimer(Some(hwnd), TRASH_TIMER);
            TRASH_PENDING.store(false, Ordering::SeqCst);
            views::for_each(|d| d.refresh_trash());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == drag::FILE_HOVER_TIMER => {
            let _ = KillTimer(Some(hwnd), drag::FILE_HOVER_TIMER);
            with_dock(|d| d.file_hover_due());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == PREVIEW_TIMER => {
            let _ = KillTimer(Some(hwnd), PREVIEW_TIMER);
            with_dock(|d| {
                if d.dragging() || d.ole_drag {
                    return;
                }
                // Find the app again by path; it must still be under the pointer.
                let i = d.hover_app.as_ref().and_then(|p| d.items.iter().position(|it| it.kind == Kind::App && it.path == *p));
                let under = d.mouse.and_then(|(u, _)| d.hit(u));
                if let (Some(i), true) = (i, i == under) {
                    d.show_preview(i);
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == stack::STACK_TIMER => {
            let _ = KillTimer(Some(hwnd), stack::STACK_TIMER);
            if with_dock(|d| d.stack_timed_out()).is_none() {
                SetTimer(Some(hwnd), stack::STACK_TIMER, 50, None);
            }
            LRESULT(0)
        }
        stack::WM_SHOW_STACK => {
            stack::show();
            // The panel may have swallowed a WM_MOUSELEAVE.
            if PENDING_LEAVE.swap(false, Ordering::SeqCst) {
                with_dock(|d| d.on_mouse_leave());
            }
            LRESULT(0)
        }
        badges::WM_BADGES => {
            // Let related changes (value + text) arrive, then apply once.
            SetTimer(Some(hwnd), badges::BADGE_TIMER, badges::BADGE_COALESCE_MS, None);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == badges::BADGE_TIMER => {
            let _ = KillTimer(Some(hwnd), badges::BADGE_TIMER);
            if !views::for_each(|d| d.take_marks()) {
                SetTimer(Some(hwnd), badges::BADGE_TIMER, badges::BADGE_COALESCE_MS, None);
            }
            LRESULT(0)
        }
        WM_SNAP_DONE => {
            // No dock state needed: works while a menu holds the dock.
            // Only if it is still what the user asked for last.
            snap::finish(HWND(wp.0 as *mut _));
            LRESULT(0)
        }
        split::WM_SPLIT_DONE => {
            // Request wp: both halves to the front, the dragged app focused
            // (no dock state needed either).
            snap::finish_split(wp.0 as u64);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == split::SPLIT_TIMER => {
            let _ = KillTimer(Some(hwnd), split::SPLIT_TIMER);
            if with_dock(|d| d.split_wait_over()).is_none() {
                SetTimer(Some(hwnd), split::SPLIT_TIMER, 50, None);
            }
            LRESULT(0)
        }
        // Send to tray: the one-shot recheck (a grace, a close, a Show).
        WM_TIMER if wp.0 == crate::traywin::RECHECK_TIMER => {
            crate::traywin::on_timer();
            LRESULT(0)
        }
        // "Always on top": did the changes asked for take (ontop.rs)?
        WM_TIMER if wp.0 == ontop::VERIFY_TIMER => {
            let _ = KillTimer(Some(hwnd), ontop::VERIFY_TIMER);
            ontop::verify();
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == ontop::ONTOP_HOTKEY => {
            with_dock(|d| d.on_top_front());
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == ontop::TRAY_HOTKEY => {
            with_dock(|d| d.tray_front());
            LRESULT(0)
        }
        // Send to tray (crate::traywin): the list changed.
        crate::traywin::WM_TRAYWIN => {
            views::for_each(|d| d.sync_windows());
            crate::topbar::traywin_changed();
            LRESULT(0)
        }
        preview::WM_PREVIEW_A11Y => {
            // The chooser scrolled under the keyboard's choice.
            with_dock(|d| d.publish_a11y());
            LRESULT(0)
        }
        preview::WM_PREVIEW_MIN => {
            // A preview card's Minimize: the dock's own way (genie / plain).
            // Only for the same process's window (the handle may have been
            // reused since the card was clicked), one the dock lists, alive,
            // responding and not minimized already. A busy dock (a menu
            // open) drops it rather than act unchecked.
            let h = HWND(wp.0 as *mut _);
            let pid = lp.0 as u32;
            let mut now = 0u32;
            GetWindowThreadProcessId(h, Some(&mut now));
            if pid != 0 && now == pid && IsWindow(Some(h)).as_bool() && !IsHungAppWindow(h).as_bool() && !IsIconic(h).as_bool() {
                let done = with_dock(|d| {
                    if d.items.iter().any(|it| it.windows.contains(&h)) {
                        // Flies to the dock on the window's own display
                        // (minrect_for).
                        d.minimize_from_dock(h);
                    }
                });
                if done.is_none() {
                    crate::log!("preview: minimize of window {:#x} dropped (dock busy)", h.0 as isize);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == hung::HUNG_TIMER => {
            // Repeating; a busy dock (menu open) just waits for the next one.
            // Nobody to tell while the session is locked or asleep or the
            // display is off (the timer stops then; a tick already queued
            // is dropped).
            if !session::idle() {
                with_dock(|d| d.check_hangs());
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == HOLD_TIMER => {
            // (Busy: the next tick looks again.)
            with_dock(|d| d.release_open());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == label::LABEL_TIMER => {
            let _ = KillTimer(Some(hwnd), label::LABEL_TIMER);
            if with_dock(|d| d.label_wait_over()).is_none() {
                // Busy (menu open): look again shortly.
                SetTimer(Some(hwnd), label::LABEL_TIMER, 50, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == drag::REMOVE_TIMER => {
            let _ = KillTimer(Some(hwnd), drag::REMOVE_TIMER);
            if with_dock(|d| d.remove_wait_over()).is_none() {
                SetTimer(Some(hwnd), drag::REMOVE_TIMER, 50, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == drag::POOF_TIMER => {
            let _ = KillTimer(Some(hwnd), drag::POOF_TIMER);
            if with_dock(|d| d.poof_done()).is_none() {
                SetTimer(Some(hwnd), drag::POOF_TIMER, 50, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == drag::UNDO_TIMER => {
            let _ = KillTimer(Some(hwnd), drag::UNDO_TIMER);
            if with_dock(|d| d.undo_expired()).is_none() {
                SetTimer(Some(hwnd), drag::UNDO_TIMER, 50, None);
            }
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == drag::ESC_HOTKEY => {
            // Remembered even if the dock is busy: the poll applies it.
            drag::escape_hit();
            with_dock(|d| d.escape_check());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == drag::ESC_POLL_TIMER => {
            // Repeats while dragging; escape_check stops it afterwards.
            with_dock(|d| d.escape_check());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == SLIDE_DONE_TIMER => {
            let _ = KillTimer(Some(hwnd), SLIDE_DONE_TIMER);
            with_dock(|d| d.slide_done());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == EDGE_CURSOR_TIMER => {
            with_dock(|d| d.poll_edge_cursor());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == FULLSCREEN_TIMER => {
            let _ = KillTimer(Some(hwnd), FULLSCREEN_TIMER);
            FULLSCREEN_PENDING.store(false, Ordering::SeqCst);
            refollow_foreground();
            // (Also where a moved window's app shows, with "only apps on
            // this display": the same foreground moves bring us here.)
            if !views::for_each(|d| {
                d.check_fullscreen();
                d.refilter();
            }) {
                schedule_fullscreen_check();
            }
            // The window in front may be on another display now (moved
            // without a focus change): the island follows it.
            crate::topbar::foreground_moved();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == AUTOHIDE_TIMER => {
            let _ = KillTimer(Some(hwnd), AUTOHIDE_TIMER);
            if with_dock(|d| d.autohide_now()) != Some(true) {
                // Busy (menu open), the pointer just beside it or Settings in
                // front: try again after the usual delay.
                let wait = with_dock(|d| d.cfg.dock.autohide_delay()).unwrap_or(AUTOHIDE_DELAY_MS);
                SetTimer(Some(hwnd), AUTOHIDE_TIMER, wait, None);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // Lost the mouse in the middle of a drag: cancel it.
            if GetCapture() != hwnd {
                with_dock(|d| {
                    if d.dragging() {
                        d.end_drag(true, None);
                        d.pressed = None;
                    }
                });
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Screen coordinates; the high word of wParam is the delta.
            let delta = (wp.0 >> 16) as u16 as i16;
            let at = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            // Shift held (MK_SHIFT): through the app's windows, not its volume.
            let shift = wp.0 & 0x0004 != 0;
            with_dock(|d| d.on_wheel(delta, at, shift));
            LRESULT(0)
        }
        sound::WM_SOUND => {
            if with_dock(|d| d.take_sound()).is_none() {
                SetTimer(Some(hwnd), sound::SOUND_RETRY_TIMER, 50, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == sound::SOUND_RETRY_TIMER => {
            let _ = KillTimer(Some(hwnd), sound::SOUND_RETRY_TIMER);
            if with_dock(|d| d.take_sound()).is_none() {
                SetTimer(Some(hwnd), sound::SOUND_RETRY_TIMER, 50, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == sound::VOL_LABEL_TIMER => {
            let _ = KillTimer(Some(hwnd), sound::VOL_LABEL_TIMER);
            if with_dock(|d| d.volume_label_over()).is_none() {
                SetTimer(Some(hwnd), sound::VOL_LABEL_TIMER, 50, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == sound::PEAK_TIMER => {
            // Busy (menu open): the next tick asks.
            with_dock(|d| d.poll_playing());
            LRESULT(0)
        }
        WM_MOUSELEAVE_MSG => {
            if with_dock(|d| d.on_mouse_leave()).is_none() {
                PENDING_LEAVE.store(true, Ordering::SeqCst);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_MBUTTONDOWN | WM_RBUTTONDOWN => {
            let up = msg + 1; // *BUTTONUP follows *BUTTONDOWN
            with_dock(|d| {
                let (x, y) = dip(lp, d.scale);
                let (u, v) = d.frame.from_win(x, y);
                d.on_down(u, v, up)
            });
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_MBUTTONUP | WM_RBUTTONUP => {
            let menu_for = with_dock(|d| {
                let (x, y) = dip(lp, d.scale);
                // Both axes of the release itself: whether a dragged pin is
                // removed is decided where the button went up.
                let (u, v) = d.frame.from_win(x, y);
                d.on_up(u, v, msg)
            })
            .flatten();
            if let Some((item, shift)) = menu_for {
                show_menu(item, shift, None);
            }
            // The context menu may have swallowed a WM_MOUSELEAVE.
            if PENDING_LEAVE.swap(false, Ordering::SeqCst) {
                with_dock(|d| d.on_mouse_leave());
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        taskbar::WM_APPBAR => {
            with_dock(|d| d.on_appbar(wp.0 as u32, lp));
            LRESULT(0)
        }
        // The taskbar moved, resized, (un)set auto-hide, or an AppBar came
        // or went: sharing the screen, follow it (coalesced).
        WM_SETTINGCHANGE if wp.0 == SPI_SETWORKAREA.0 as usize => {
            if !WORKAREA_PENDING.swap(true, Ordering::SeqCst) {
                SetTimer(Some(hwnd), WORKAREA_TIMER, WORKAREA_DEBOUNCE_MS, None);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SETTINGCHANGE if wp.0 == SPI_SETDESKWALLPAPER.0 as usize => {
            // A new picture: the mica images made for the old one are stale.
            crate::wallpaper::invalidate();
            views::for_each(|d| d.refresh_mica());
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        // Light/dark, accent, transparency, high contrast, animations.
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED_MSG => {
            // A known folder (Downloads, ...) may have moved: stack tiles
            // look them up again (icons::known_folders, registry only).
            if msg == WM_SETTINGCHANGE {
                crate::icons::forget_known_folders();
            }
            // These come in bursts: one look at the settings shortly after.
            if !SYSLOOK_PENDING.swap(true, Ordering::SeqCst) {
                SetTimer(Some(hwnd), SYSLOOK_TIMER, SYSLOOK_DEBOUNCE_MS, None);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER if wp.0 == SYSLOOK_TIMER => {
            let _ = KillTimer(Some(hwnd), SYSLOOK_TIMER);
            if views::for_each(|d| d.refresh_syslook()) {
                SYSLOOK_PENDING.store(false, Ordering::SeqCst);
                // Our tray icon follows the taskbar's light / dark.
                trayicon::look_changed();
            } else {
                // Busy (a menu is open): try again shortly; nothing is lost.
                SetTimer(Some(hwnd), SYSLOOK_TIMER, SYSLOOK_DEBOUNCE_MS, None);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            crate::companion::displays_changed();
            gpu::display_changed();
            // Coalesced, and never lost while the dock is busy (menu open).
            if !DISPLAY_PENDING.swap(true, Ordering::SeqCst) {
                SetTimer(Some(hwnd), DISPLAY_TIMER, DISPLAY_DEBOUNCE_MS, None);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER if wp.0 == DISPLAY_TIMER => {
            let _ = KillTimer(Some(hwnd), DISPLAY_TIMER);
            gpu::check();
            // A new refresh rate (crate::effects' frame target and cap).
            fx::display_changed();
            // The wallpaper is laid out anew on the new displays.
            crate::wallpaper::invalidate();
            let done = views::with_primary(|d| {
                // Monitors may have come or gone: one top bar each.
                crate::topbar::apply_config(&d.gfx, &d.cfg, &d.theme);
                crate::corners::displays_changed();
                trayicon::displays_changed();
                d.replace_on_monitor();
                d.report_active(GetForegroundWindow());
                // Explorer may turn auto-hide off when monitors change.
                d.reassert_taskbar();
            });
            if done.is_some() {
                DISPLAY_PENDING.store(false, Ordering::SeqCst);
                // One dock per display (or the main one back home); the
                // others re-placed.
                views::post_reconcile();
                schedule_fullscreen_check();
            } else {
                SetTimer(Some(hwnd), DISPLAY_TIMER, DISPLAY_DEBOUNCE_MS, None);
            }
            LRESULT(0)
        }
        sensors::WM_MOVE_TO_MONITOR => {
            with_dock(|d| d.move_to_monitor(wp.0 as isize));
            LRESULT(0)
        }
        WM_QUERYENDSESSION => {
            crate::focus_timer::shutdown();
            // Give the taskbar back now, while explorer is still running, and
            // keep it given back until the session ends or the end is cancelled.
            SHUTTING_DOWN.store(true, Ordering::SeqCst);
            HIDE_TASKBAR.store(false, Ordering::SeqCst);
            let _ = KillTimer(Some(hwnd), REHIDE_TIMER);
            views::for_each(|d| {
                d.sync_edge_cursor();
                d.shutdown();
            });
            // A change made a moment ago reaches the file before the end.
            crate::companion::flush_state(true);
            worker::wait_for_saves(std::time::Duration::from_secs(2));
            LRESULT(1)
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                crate::focus_timer::shutdown();
                views::for_each(|d| d.shutdown());
            } else {
                // Logoff/shutdown was cancelled: put back everything the
                // query took down (audit A10), each at most once.
                SHUTTING_DOWN.store(false, Ordering::SeqCst);
                crate::focus_timer::resume_after_cancel();
                crate::topbar::reregister();
                views::for_each(|d| {
                    if d.primary {
                        crate::focus_timer::apply_hotkey(d.hwnd, &d.cfg.shortcut(crate::shortcuts::FOCUS_TIMER));
                        trayicon::set_enabled(d.cfg.tray_icon);
                    }
                    d.appbar = false;
                    d.apply_taskbar_mode();
                    d.layout();
                    d.sync_edge_cursor();
                });
                hook_explorer();
                watch_trash(hwnd);
                // The bin may have changed while nobody was watching.
                views::for_each(|d| d.refresh_trash());
                crate::log!("session: logoff cancelled; appbars, taskbar mode, tray icon and Recycle Bin watch restored");
            }
            LRESULT(0)
        }
        WM_TIMECHANGE => {crate::companion::refresh();LRESULT(0)}
        WM_POWERBROADCAST => {
            session::on_message(hwnd, true, wp.0, lp.0);
            crate::companion::environment_changed();
            LRESULT(1)
        }
        session::WM_WTSSESSION_CHANGE => {
            session::on_message(hwnd, false, wp.0, 0);
            crate::companion::environment_changed();
            LRESULT(0)
        }
        WM_TIMER if matches!(wp.0, session::RESYNC_TIMER | session::RESYNC_LATE_TIMER) => {
            session::on_timer(hwnd, wp.0);
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == crate::search::hotkey::ID => {
            if crate::search::is_open() {
                crate::search::cancel();
            } else if let Some((gfx, theme)) = with_dock(|d| (d.gfx.clone(), d.theme.clone())) {
                // Outside the dock's borrow: the panel runs its own loop.
                crate::search::toggle(&gfx, &theme);
            }
            LRESULT(0)
        }
        // Launchpad (hotkey, Windows key, dock pin): outside the dock's
        // borrow, it runs its own loop.
        WM_HOTKEY if wp.0 as i32 == crate::launchpad::hotkey::ID => {
            crate::launchpad::on_hotkey();
            LRESULT(0)
        }
        crate::companion::WM_APP_TICK => {
            crate::companion::paced_tick(wp.0 as u64);
            LRESULT(0)
        }
        crate::companion::WM_REFRESH => {
            crate::companion::handle();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == crate::companion::TIMER => {
            crate::companion::handle();
            LRESULT(0)
        }
        crate::companion::WM_TOGGLE => {
            COMPANION_TOGGLE.with(|p| p.set((!p.get().0, 0)));
            retry_companion_toggle(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == crate::companion::TOGGLE_TIMER => {
            retry_companion_toggle(hwnd);
            LRESULT(0)
        }
        crate::companion::chat::mcp::WAKE | crate::companion::chat::session::CHANGED => {
            crate::companion::chat::mcp::poll();
            crate::companion::refresh();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == crate::companion::chat::mcp::TIMER => {
            crate::companion::chat::mcp::poll();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == crate::companion::chat::cli::TIMER => {
            crate::companion::chat::cli::refresh(Default::default());
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == crate::companion::chat::HOTKEY_ID => {
            if crate::companion::chat::connected() {
                crate::companion::chat::open();
            } else {
                crate::settings::launch_page("companion-ai");
            }
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == crate::companion::chat::HISTORY_HOTKEY_ID => {
            crate::companion::chat::open_history();
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == crate::companion::SNOOZE_HOTKEY_ID => {crate::companion::snooze_toggle();LRESULT(0)}
        WM_HOTKEY if wp.0 as i32 == crate::companion::HOTKEY_ID => {
            crate::companion::toggle();
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == crate::notes::HOTKEY_ID => {
            crate::notes::hotkey_pressed();
            LRESULT(0)
        }
        // A piece of the screen pinned where it was (the overlay comes up).
        WM_HOTKEY if wp.0 as i32 == crate::pieces::overlay::HOTKEY_ID => {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(windows::Win32::System::Threading::GetCurrentProcessId());
            }
            if let Some(gfx) = with_dock(|d| d.gfx.clone()) {
                crate::pieces::overlay::on_hotkey(&gfx);
            }
            LRESULT(0)
        }
        // The focus timer: start the last plan, or pause / resume.
        WM_HOTKEY if wp.0 as i32 == crate::focus_timer::HOTKEY_ID => {
            crate::focus_timer::on_hotkey();
            LRESULT(0)
        }
        crate::launchpad::WM_LAUNCHPAD => {
            crate::launchpad::on_message(wp, lp);
            LRESULT(0)
        }
        WM_TRAY_SAVE | WM_TIMER if msg == WM_TRAY_SAVE || wp.0 == TRAY_SAVE_TIMER => {
            let _ = KillTimer(Some(hwnd), TRAY_SAVE_TIMER);
            if let Flush::Retry(ms) = flush_tray_saves() {
                SetTimer(Some(hwnd), TRAY_SAVE_TIMER, ms, None);
            }
            LRESULT(0)
        }
        gpu::WM_GPU_LOST => {
            gpu::on_lost(hwnd);
            LRESULT(0)
        }
        gpu::WM_GPU_DEVICE => {
            gpu::on_device(hwnd, wp.0 as u64);
            LRESULT(0)
        }
        WM_TIMER if matches!(wp.0, gpu::GPU_RETRY_TIMER | gpu::GPU_REDRAW_TIMER | gpu::GPU_UPGRADE_TIMER | gpu::GPU_DEADLINE_TIMER) => {
            gpu::on_timer(hwnd, wp.0);
            LRESULT(0)
        }
        WM_UPDATE_QUIET => LRESULT(ui_is_quiet() as isize),
        WM_QUIT_REQUEST => {
            // Another process asks (Settings, --quit, the updater): the
            // desktop notes are saved now; never held up.
            crate::notes::prepare_quit(crate::notes::Quit::External);
            views::for_each(|d| d.shutdown());
            views::destroy_all();
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_GETOBJECT => {
            let answer = crate::a11y::on_getobject(hwnd, wp, lp, || {
                with_dock(|d| d.publish_a11y());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == crate::a11y::wm_action() => {
            for (key, act) in crate::a11y::take(hwnd) {
                if let Some(Some(i)) = with_dock(|d| d.a11y_act(&key, act)) {
                    show_menu_from_keyboard(i);
                }
            }
            LRESULT(0)
        }
        // Dock apps by number: Alt / Ctrl+Alt sets (hotkeys) and the Windows
        // key set (the keyboard hook).
        WM_HOTKEY if crate::shortcuts::DIGIT_IDS.contains(&(wp.0 as i32)) => {
            if let Some((slot, act)) = crate::shortcuts::digit_hotkey(wp.0 as i32) {
                run_digit(slot, act);
            }
            LRESULT(0)
        }
        crate::launchpad::wintap::WM_TASKBAR_FOCUS => {
            if crate::taskbar::hidden_by_us() {
                if wp.0 == 2 {
                    crate::topbar::focus_notification_area();
                } else {
                    let target = views::for_foreground();
                    views::set_active(target);
                    views::with_dock_at(target, |d| d.taskbar_cycle(wp.0 == 1));
                }
            }
            LRESULT(0)
        }
        digits::WM_DOCK_DIGIT => {
            if lp.0 & 0x100 != 0 {
                crate::log!("dock: Windows key mask not sent; Start may open too");
            }
            if let Some(act) = crate::shortcuts::DigitAct::from_code((lp.0 & 0xFF) as usize) {
                run_digit(wp.0, act);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == KEYS_TIMER => {
            // A recording that never ended: the shortcuts come back.
            with_dock(|d| d.suspend_keys(false));
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == access::FOCUS_HOTKEY => {
            // Leaves keyboard mode wherever it is on, else takes the focus
            // on the dock of the display with the window in front.
            let mut left = false;
            views::for_each(|d| {
                if d.kb.is_some() {
                    d.leave_keyboard(true);
                    left = true;
                }
            });
            if !left {
                let target = views::for_foreground();
                views::set_active(target);
                views::with_dock_at(target, |d| d.enter_keyboard(None, true));
            }
            LRESULT(0)
        }
        views::WM_VIEW_FORWARD => {
            views::on_forward(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == views::VIEW_FORWARD_TIMER => {
            views::on_forward(hwnd);
            LRESULT(0)
        }
        views::WM_VIEW_RECONCILE => {
            views::reconcile(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == views::VIEW_RECONCILE_TIMER => {
            views::reconcile(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN if with_dock(|d| d.kb.is_some()).unwrap_or(false) => {
            if let Some(Some(i)) = with_dock(|d| d.kb_key(wp.0 as u16)) {
                show_menu_from_keyboard(i);
            }
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            // Another window (or our own menu) took the focus.
            with_dock(|d| d.leave_keyboard(false));
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DESTROY => {
            crate::a11y::forget(hwnd);
            session::stop(hwnd);
            shell::remove_hooks();
            crate::companion::flush_state(true);
            worker::wait_for_saves(std::time::Duration::from_secs(2));
            access::release_hotkey();
            ontop::release_hotkeys();
            digits::release();
            crate::search::hotkey::release();
            crate::shot::shutdown();
            crate::notes::release_hotkey();
            crate::companion::shutdown();
            crate::desklayer::host::shutdown();
            crate::pieces::shutdown();
            crate::focus_timer::shutdown();
            crate::launchpad::shutdown();
            views::for_each(|d| d.shutdown());
            views::destroy_all();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// App number `slot` (Win+digit and the other sets). The numbers always
/// follow the primary dock's order (unchanged with docks on several
/// displays); the app is then acted on, with its bounce or menu, on the
/// dock of the display with the window in front if that dock shows it.
unsafe fn run_digit(slot: usize, act: crate::shortcuts::DigitAct) {
    let Some(Some(path)) = views::with_primary(|p| p.digit_path(slot)) else { return };
    let here = views::for_foreground();
    let on = if views::with_dock_at(here, |d| d.digit_item(&path).is_some()).unwrap_or(false) { here } else { views::primary_hwnd() };
    let _ctx = views::enter(on);
    // On the primary itself: by number, exactly as with one dock.
    let slot = (on == views::primary_hwnd()).then_some(slot);
    if let Some(Some(i)) = with_dock(|d| d.digit(slot, &path, act)) {
        show_menu_from_keyboard(i);
    }
}

/// DwmFlush failed this many times in a row: wait longer per frame.
const FLUSH_FAIL_SLOW_AFTER: u32 = 60;
/// The UI has not taken this many ticks in a row: it is busy or stuck, so
/// the frame loop slows right down instead of spinning beside it.
const TICK_STALL_FRAMES: u32 = 120;

/// How long the ticker waits after a frame (audit A06). DwmFlush normally
/// paces the loop; when it fails at once (DWM restarting, a driver reset,
/// remote sessions) a sleep stands in for the frame, and while the UI does
/// not take its ticks the loop nearly stops. None = no extra wait.
fn ticker_pause(flush_fails: u32, unconsumed: u32) -> Option<std::time::Duration> {
    use std::time::Duration;
    if unconsumed >= TICK_STALL_FRAMES {
        Some(Duration::from_millis(250))
    } else if flush_fails >= FLUSH_FAIL_SLOW_AFTER {
        Some(Duration::from_millis(100))
    } else if flush_fails > 0 {
        Some(Duration::from_millis(16))
    } else {
        None
    }
}

/// A ticker thread with nothing to animate ends after this long (the next
/// kick starts a new one).
const TICKER_IDLE: std::time::Duration = std::time::Duration::from_secs(10);

/// The frame ticker: a thread that posts WM_TICK once per DWM frame while
/// the dock animates. It exists only around animations: after TICKER_IDLE
/// without one it ends, and the next kick starts it again.
/// Each dock window has its own (views.rs).
pub(super) struct Ticker {
    hwnd: isize,
    /// The running ticker thread (None = none).
    slot: std::sync::Arc<std::sync::Mutex<Option<Thread>>>,
    /// Something on this dock animates: frames are posted while set.
    pub(super) animating: std::sync::Arc<AtomicBool>,
    /// A WM_TICK is on its way to this dock.
    pub(super) pending: std::sync::Arc<AtomicBool>,
}

impl Ticker {
    fn new(hwnd: HWND) -> Self {
        Self { hwnd: hwnd.0 as isize, slot: Default::default(), animating: Default::default(), pending: Default::default() }
    }

    /// Something animates (again).
    pub(super) fn is_animating(&self) -> bool {
        self.animating.load(Ordering::SeqCst)
    }

    /// `animating` was just set: wake the ticker, starting it if needed.
    pub(super) fn unpark(&self) {
        let mut g = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = g.as_ref() {
            t.unpark();
            return;
        }
        let (raw, slot) = (self.hwnd, self.slot.clone());
        let (animating, pending) = (self.animating.clone(), self.pending.clone());
        match std::thread::Builder::new().name("ticker".into()).spawn(move || ticker_loop(raw, &slot, &animating, &pending)) {
            Ok(h) => *g = Some(h.thread().clone()),
            Err(e) => crate::log!("ticker: cannot start ({e}); animations wait for the next kick"),
        }
    }
}

impl Drop for Ticker {
    /// The dock is gone (a display's dock closed): its thread ends.
    fn drop(&mut self) {
        self.animating.store(false, Ordering::SeqCst);
        if let Some(t) = self.slot.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            t.unpark();
        }
    }
}

fn ticker_loop(raw: isize, slot: &std::sync::Mutex<Option<Thread>>, animating: &AtomicBool, pending: &AtomicBool) {
    let hwnd = HWND(raw as *mut _);
    let (mut flush_fails, mut unconsumed) = (0u32, 0u32);
    // Display frames seen (the frame cap ticks on every n-th).
    let mut frames = 0u32;
    loop {
        if animating.load(Ordering::SeqCst) {
            unsafe {
                flush_fails = if DwmFlush().is_ok() { 0 } else { flush_fails.saturating_add(1) };
                if flush_fails == 1 {
                    crate::log!("ticker: DwmFlush failed; pacing frames by sleeping");
                }
                frames = frames.wrapping_add(1);
                let capped = flush_fails == 0 && !frames.is_multiple_of(fx::tick_div());
                if capped {
                    // Skipped for the frame cap (crate::effects).
                } else if !pending.swap(true, Ordering::SeqCst) {
                    unconsumed = 0;
                    if PostMessageW(Some(hwnd), WM_TICK, WPARAM(0), LPARAM(0)).is_err() {
                        // Not delivered: the next frame tries again.
                        pending.store(false, Ordering::SeqCst);
                    }
                } else {
                    unconsumed = unconsumed.saturating_add(1);
                }
            }
            if let Some(d) = ticker_pause(flush_fails, unconsumed) {
                // A plain sleep: unpark() (sent on every kick) must not
                // cut it short, or a failing DwmFlush would spin again.
                std::thread::sleep(d);
            }
        } else {
            flush_fails = 0;
            unconsumed = 0;
            frames = 0;
            std::thread::park_timeout(TICKER_IDLE);
            if animating.load(Ordering::SeqCst) {
                continue;
            }
            // Still nothing to animate: end. Checked again under the lock
            // that kicks take, so a kick either sees this thread gone and
            // starts a new one, or is seen here.
            let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
            if !animating.load(Ordering::SeqCst) {
                *g = None;
                return;
            }
        }
    }
}

/// Cancel outstanding cooperative shutdowns and allow up to two seconds
/// for their RM sessions to close before the process exits.
pub fn finish_full_quit() { full_quit::finish_pending(); }

/// A dock that died mid-genie may have left another app's window with its
/// minimize/restore animations off: put them back (guardian and startup).
pub fn recover_genie() {
    genie_shared::recover_pending();
}

pub unsafe fn run() -> Result<()> {
    recover_genie();
    let hinst = GetModuleHandleW(None)?;
    let class = w!("NopBarWindow");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: class,
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    WM_TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::SeqCst);
    // Startup registers the AppBars with this explorer (WM_APPLY_TASKBAR):
    // our tray's first "add your icons" broadcast is not a restart.
    HANDLED_EXPLORER.store(taskbar::explorer_pid(), Ordering::SeqCst);
    WM_SHELLHOOK.store(RegisterWindowMessageW(w!("SHELLHOOK")), Ordering::SeqCst);

    let hwnd = CreateWindowExW(
        WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
        class,
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
    )?;
    SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
    DOCK_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    // Send to tray (crate::traywin): the list's changes come to this window.
    crate::traywin::attach(hwnd);

    // Started by the first animation (and again after it ends when idle).
    let ticker = Ticker::new(hwnd);
    // The Effects page's rendering choice, before the first device.
    if let Some(c) = crate::config::read_current() {
        crate::gfx::set_render_choice(&c.effects.render);
    }
    // One compositor / D3D device for the dock and the top bar.
    let gfx = Rc::new(Gfx::new()?);
    // Device loss (driver reset): recover and redraw instead of going blank.
    gpu::init(&gfx, hwnd);
    let pending = ticker.pending.clone();
    let dock = {
        let _ctx = views::enter(hwnd.0 as isize);
        Dock::new(hwnd, ticker, gfx.clone())?
    };
    let (cfg, theme, mon) = (dock.cfg.clone(), dock.theme.clone(), dock.mon);
    views::register(hwnd, mon, true, dock, pending);
    crate::topbar::create(gfx.clone(), &cfg, &theme);
    // Spans count the dock's thread's CPU to its parts (desklayer::load).
    crate::desklayer::load::init_main();
    crate::desklayer::privacy::set_hidden(cfg.widgets.hide_from_capture);
    // Desktop notes (crate::notes): their own thread, before the shortcuts.
    crate::notes::set_per_desktop(cfg.widgets.notes_per_desktop);
    crate::notes::configure(&cfg.widgets);
    if cfg.widgets.notes {
        crate::notes::init(theme.non_shell());
    }
    // The image pieces and what the widgets on this thread share (the
    // host takes the desktop's Ctrl+V, a shortcut applied just below).
    crate::desklayer::host::init(&cfg);
    crate::device_battery::runtime::apply(&cfg, theme.non_shell());
    crate::pieces::init(&gfx, theme.non_shell(), &cfg);
    // On this (the UI) thread: WM_HOTKEY comes to the dock window. Every
    // shortcut comes from the registry (crate::shortcuts).
    crate::shortcuts::apply_all(hwnd, &cfg);
    crate::ime::apply_config(&cfg.input);
    // The focus timer's settings.
    crate::focus_timer::apply_config(&cfg);
    crate::agenda::apply_config(&cfg);
    crate::agents::apply_config(&cfg);
    crate::companion::init(&gfx, hwnd, &cfg);
    with_dock(|d| d.companion_ground());
    // Finds the other launchers / previewers (helper thread).
    crate::launchers::set_choice(&cfg.search.with);
    crate::corners::apply(&cfg.hot_corners);
    install_hooks();
    // NopBar's own notification-area icon (after the top bar: its tray spy
    // may already be up to see it; it asks again in a moment anyway).
    // Our icon's calls are mirrored from the first one, whatever the
    // top bar and its tray do now (they may come on later).
    crate::tray::dock_started();
    trayicon::init(cfg.tray_icon);
    if std::env::var("NOPDOCK_DEV").as_deref() == Ok("1") {
        DEV_SIGNALS.store(true, Ordering::SeqCst);
        crate::log!("dev: test signals enabled");
    }
    hook_explorer();
    worker::watch_explorer(taskbar::explorer_pid());
    let _ = RegisterShellHookWindow(hwnd);
    session::start(hwnd);

    // In its place (behind a fullscreen app already in front), then shown
    // with its shadow.
    if with_dock(|d| d.reveal()).is_none() {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    crate::log!("startup: shown at {} ms", since_start());
    // Files dropped from Explorer: open with an app / pin.
    drag::register(hwnd);
    // Jump lists are read in the background; the first menu has them.
    crate::jumplist::refresh();
    // Taskbar progress and badges (UIA, its own thread; off on failure).
    badges::start(hwnd);
    watch_trash(hwnd);
    let _ = PostMessageW(Some(hwnd), WM_APPLY_TASKBAR, WPARAM(0), LPARAM(0));

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    Ok(())
}

#[cfg(test)]
mod ticker_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn healthy_frames_need_no_extra_wait() {
        assert_eq!(ticker_pause(0, 0), None);
        assert_eq!(ticker_pause(0, TICK_STALL_FRAMES - 1), None);
    }

    #[test]
    fn failing_flush_never_spins() {
        // Every failure waits about a frame, then longer.
        for n in 1..FLUSH_FAIL_SLOW_AFTER {
            assert_eq!(ticker_pause(n, 0), Some(Duration::from_millis(16)));
        }
        assert_eq!(ticker_pause(FLUSH_FAIL_SLOW_AFTER, 0), Some(Duration::from_millis(100)));
        assert_eq!(ticker_pause(u32::MAX, 0), Some(Duration::from_millis(100)));
    }

    #[test]
    fn a_ui_that_takes_no_ticks_slows_the_loop() {
        assert_eq!(ticker_pause(0, TICK_STALL_FRAMES), Some(Duration::from_millis(250)));
        assert_eq!(ticker_pause(5, u32::MAX), Some(Duration::from_millis(250)));
    }
}

#[cfg(test)]
mod tray_save_tests {
    use super::merge_requeue;

    #[test]
    fn failed_tray_saves_come_back_unless_superseded() {
        let c = |id: &str, h: bool| (id.to_string(), h);
        let out = merge_requeue(vec![c("a", true), c("b", false)], vec![c("b", true), c("c", false)]);
        // "a" is retried; "b" keeps its newer choice; "c" stays queued.
        assert_eq!(out, vec![c("a", true), c("b", true), c("c", false)]);
        assert_eq!(merge_requeue(vec![c("a", true)], vec![]), vec![c("a", true)]);
    }
}

#[cfg(test)]
mod timer_id_tests {
    /// Timers on the dock window must not share an id (label and tray
    /// saves once both used 12, and tray-save retries went to the label).
    #[test]
    fn dock_window_timers_and_hotkeys_are_distinct() {
        let ids = [
            crate::companion::TIMER,
            crate::companion::TOGGLE_TIMER,
            super::REFRESH_TIMER,
            super::REHIDE_TIMER,
            super::EVENTS_RETRY_TIMER,
            super::EXPLORER_WAIT_TIMER,
            super::AUTOHIDE_TIMER,
            super::SLIDE_DONE_TIMER,
            super::FULLSCREEN_TIMER,
            super::SYSLOOK_TIMER,
            super::DISPLAY_TIMER,
            super::PREVIEW_TIMER,
            super::TRASH_TIMER,
            super::label::LABEL_TIMER,
            super::TRAY_SAVE_TIMER,
            super::badges::BADGE_TIMER,
            super::stack::STACK_TIMER,
            super::drag::REMOVE_TIMER,
            super::drag::POOF_TIMER,
            super::drag::UNDO_TIMER,
            super::drag::ESC_POLL_TIMER,
            super::split::SPLIT_TIMER,
            super::gpu::GPU_RETRY_TIMER,
            super::gpu::GPU_REDRAW_TIMER,
            super::gpu::GPU_UPGRADE_TIMER,
            super::gpu::GPU_DEADLINE_TIMER,
            super::session::RESYNC_TIMER,
            super::session::RESYNC_LATE_TIMER,
            super::hung::HUNG_TIMER,
            super::sound::VOL_LABEL_TIMER,
            super::sound::PEAK_TIMER,
            super::sound::SOUND_RETRY_TIMER,
            super::KEYS_TIMER,
            super::HIDE_CHECK_TIMER,
            super::WORKAREA_TIMER,
            super::COEXIST_TIMER,
            super::RESERVE_CHECK_TIMER,
            super::views::VIEW_SYNC_TIMER,
            super::views::VIEW_FORWARD_TIMER,
            super::views::VIEW_RECONCILE_TIMER,
            super::ontop::VERIFY_TIMER,
            crate::traywin::RECHECK_TIMER,
        ];
        for (i, a) in ids.iter().enumerate() {
            assert!(!ids[i + 1..].contains(a), "duplicate dock timer id {a}");
        }
        let mut keys = vec![crate::search::hotkey::ID, crate::launchpad::hotkey::ID, super::drag::ESC_HOTKEY, super::access::FOCUS_HOTKEY, crate::notes::HOTKEY_ID, crate::focus_timer::HOTKEY_ID, crate::companion::HOTKEY_ID,crate::companion::SNOOZE_HOTKEY_ID,
            super::ontop::ONTOP_HOTKEY, super::ontop::TRAY_HOTKEY];
        keys.extend(crate::shortcuts::DIGIT_IDS);
        for (i, a) in keys.iter().enumerate() {
            assert!(!keys[i + 1..].contains(a), "duplicate hotkey id {a}");
        }
    }
}

#[cfg(test)]
mod message_id_tests {
    /// Messages posted to the dock window must not share a number (two
    /// branches once both took WM_APP + 72 and tray saves went to badges).
    #[test]
    fn dock_window_messages_are_distinct() {
        let ids = [
            crate::companion::WM_APP_TICK,
            crate::companion::WM_REFRESH,
            crate::companion::WM_TOGGLE,
            super::WM_TICK,
            super::WM_APPLY_TASKBAR,
            super::sensors::WM_MOVE_TO_MONITOR,
            super::WM_TRASH_CHANGED,
            super::WM_QUIT_REQUEST,
            super::WM_UPDATE_QUIET,
            super::badges::WM_BADGES,
            super::WM_TRAY_SAVE,
            super::WM_SNAP_DONE,
            super::split::WM_SPLIT_DONE,
            super::preview::WM_PREVIEW_MIN,
            super::preview::WM_PREVIEW_A11Y,
            crate::traywin::WM_TRAYWIN,
            super::stack::WM_SHOW_STACK,
            super::sound::WM_SOUND,
            super::WM_DEV_HANG,
            super::WM_DEV_PANIC,
            super::digits::WM_DOCK_DIGIT,
            crate::launchpad::WM_LAUNCHPAD,
            crate::worker::WM_EVENTS,
            super::gpu::WM_GPU_LOST,
            super::gpu::WM_GPU_DEVICE,
            super::views::WM_VIEW_SYNC,
            super::views::WM_VIEW_FORWARD,
            super::views::WM_VIEW_RECONCILE,
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b, "duplicate dock message id {a}");
            }
        }
    }

    /// Timers on the dock window must not share an id either (label and
    /// tray saves both once used 12: a label timer ate the save retry).
    #[test]
    fn dock_window_timers_are_distinct() {
        let ids = [
            crate::companion::TIMER,
            crate::companion::TOGGLE_TIMER,
            super::REFRESH_TIMER,
            super::REHIDE_TIMER,
            super::EVENTS_RETRY_TIMER,
            super::EXPLORER_WAIT_TIMER,
            super::AUTOHIDE_TIMER,
            super::SLIDE_DONE_TIMER,
            super::FULLSCREEN_TIMER,
            super::SYSLOOK_TIMER,
            super::DISPLAY_TIMER,
            super::PREVIEW_TIMER,
            super::TRASH_TIMER,
            super::label::LABEL_TIMER,
            super::badges::BADGE_TIMER,
            super::stack::STACK_TIMER,
            super::hung::HUNG_TIMER,
            super::sound::VOL_LABEL_TIMER,
            super::sound::PEAK_TIMER,
            super::sound::SOUND_RETRY_TIMER,
            super::TRAY_SAVE_TIMER,
            super::gpu::GPU_RETRY_TIMER,
            super::gpu::GPU_REDRAW_TIMER,
            super::gpu::GPU_UPGRADE_TIMER,
            super::gpu::GPU_DEADLINE_TIMER,
            super::KEYS_TIMER,
            super::ontop::VERIFY_TIMER,
            crate::traywin::RECHECK_TIMER,
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b, "duplicate dock timer id {a}");
            }
        }
    }
}

/// Widget editor actions use the same configuration transaction as Settings.
pub fn set_widget_enabled(kind:crate::desklayer::edit::Kind,on:bool){
    with_dock(|d|d.update_config(move|c|match kind {
        crate::desklayer::edit::Kind::Battery=>c.widgets.device_battery=on,
        crate::desklayer::edit::Kind::Inbox=>{c.widgets.inbox=on;if on{c.widgets.notes=true;}},
        crate::desklayer::edit::Kind::Note=>c.widgets.notes=on,
    }));
}

/// Existing taskbar progress snapshot; no new UI Automation reader.
pub fn companion_working()->bool {
    with_dock(|d|d.marks.values().any(|m|m.progress.is_some())).unwrap_or(false)
}

pub fn save_companion_state(r:crate::companion::routine::Routine,b:crate::companion::purpose::Budget)->bool {
    with_dock(|d|d.save_companion_state(r,b)).is_some()
}

/// Bounded existing Explorer lookup; caller is the download worker, never the UI thread.
pub fn companion_foreground_folders(h:HWND)->Vec<String>{stack::explorer_folders(&[h],100).into_iter().map(|(p,_)|p).collect()}
