//! Tray icons for the top bar (spike S1, spec §7.3).
//!
//! A hidden top-level window of class `Shell_TrayWnd` on its own thread
//! receives what apps send to "the taskbar" through Shell_NotifyIcon. Every
//! WM_COPYDATA is forwarded to explorer's real taskbar and explorer's answer
//! returned, so apps and explorer keep working exactly as before; tray
//! messages (dwData = 1) are also kept in our cache for the top bar.
//!
//! If this process dies, the window goes with it and Shell_NotifyIcon finds
//! explorer's taskbar again, which already has every icon (we forwarded).
//!
//! The spy exists only while explorer's notification area is up: an
//! explorer that starts while another `Shell_TrayWnd` exists never brings
//! its notification area up, and refuses every app's icon (measured on
//! build 26300: NIM_ADD fails with E_FAIL for any process, NopBar running
//! or not, until explorer is restarted without the spy). So the spy is
//! destroyed the moment explorer goes away and built again only once the
//! new explorer has announced its tray (its own TaskbarCreated, which it
//! sends when its notification area is ready; a spy built before that, even
//! with TrayNotifyWnd already there, kept it from ever being ready). See
//! SpyLife. A hidden window of our own on the spy thread hears that
//! broadcast while the spy is gone.
//!
//! The spy exists only while it is wanted: `topbar.tray` on (`set_enabled`)
//! and a top bar to show the icons (`start` / `detach`). Off, the spy
//! thread closes the spy, forgets the icons and sleeps. While the session
//! is locked, asleep or the display is off (`set_quiet`), the watchdog
//! sleeps too (the spy stays); once the spy has kept its place a while the
//! watchdog looks less often (Pace). When the tray is off, or the spy is
//! Stuck behind explorer or stands aside (Bypass), the top bar shows one
//! "notification area" button instead of icons (`health`, `open_overflow`).

pub mod fit;
mod menu_place;
pub mod proto;

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use proto::*;
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetObjectW, SelectObject, BITMAP,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Posted to the top bar when the icon set or an icon changed.
pub const WM_TRAY_CHANGED: u32 = WM_APP + 50;
/// To the spy thread: render every icon again (new size).
const WM_RERENDER: u32 = WM_APP + 1;
/// To the spy thread: after sleep / unlock / reconnect, follow explorer and
/// take the front again (as after TaskbarCreated, without asking apps).
const WM_RESYNC: u32 = WM_APP + 2;
const NIN_SELECT: u32 = WM_USER;
const REASSERT_TIMER: usize = 1;
const COLLECT_TIMER: usize = 2;
const COLLECT_DELAY_MS: u32 = 1500;
/// Logs how many icons we know a moment after asking apps for them.
const COUNT_TIMER: usize = 3;
const COUNT_DELAY_MS: u32 = 3000;
const REASSERT_DELAYS: [u32; 5] = [500, 1000, 1500, 2000, 5000];

/// One icon as the cache keeps it.
#[derive(Clone, Debug, Default)]
pub struct Entry {
    /// The app's window that gets the callbacks (32-bit handle value).
    pub owner: u32,
    pub uid: u32,
    pub callback: u32,
    pub version: u32,
    /// Last explicit NIM_SETVERSION, including version 0.
    set_version: Option<u32>,
    pub tip: String,
    pub show_tip: bool,
    pub hidden: bool,
    /// Its NIM_ADD reached us (not only later MODIFYs): what it registered
    /// is all there is, so a missing callback is the app's choice.
    pub added: bool,
    /// guidItem identity (if the app uses GUIDs).
    pub guid: Option<[u8; 16]>,
    /// Our own copy of the icon (CopyIcon at receipt; the sender may destroy
    /// its handle right after). Owned by the cache; 0 = none.
    own_icon: isize,
    /// Bumped whenever the picture changes.
    pub gen: u64,
    /// Premultiplied BGRA, ICON_PX square.
    pub pixels: Option<Arc<Vec<u8>>>,
}

/// What a message needs done after `apply`.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    None,
    /// Entry `i` got a new picture (the sender's handle, to copy now).
    Icon(usize, u32),
    /// An entry was removed; its own icon copy must be destroyed.
    Removed(isize),
}

fn same_icon(e: &Entry, m: &TrayMsg) -> bool {
    match (&e.guid, &m.guid) {
        (Some(a), Some(b)) => a == b,
        (None, None) => e.owner == m.hwnd && e.uid == m.uid,
        _ => false,
    }
}

/// Applies one Shell_NotifyIcon message to the cache.
pub fn apply(list: &mut Vec<Entry>, m: &TrayMsg) -> Change {
    let pos = list.iter().position(|e| same_icon(e, m));
    let set_fields = |e: &mut Entry, m: &TrayMsg| {
        // Callbacks always go to the latest hWnd/uID.
        e.owner = m.hwnd;
        e.uid = m.uid;
        if m.flags & NIF_MESSAGE != 0 {
            e.callback = m.callback_msg;
        }
        if m.flags & NIF_TIP != 0 {
            e.tip = m.tip.clone();
            e.show_tip = m.flags & NIF_SHOWTIP != 0;
        } else if m.flags & NIF_SHOWTIP != 0 {
            e.show_tip = true;
        }
        if m.flags & NIF_STATE != 0 && m.state_mask & NIS_HIDDEN != 0 {
            e.hidden = m.state & NIS_HIDDEN != 0;
        }
        // NIF_SHOWTIP exists only for version 4, and such an app fills the
        // version in the same struct before its NIM_SETVERSION: take it at
        // the ADD, in case the SETVERSION goes past us (seen with Google
        // Drive: re-added with 0xA7 and version 4, no SETVERSION reached us,
        // and clicks packed for version 0 did nothing).
        // The Epic Games Launcher re-adds with NIF_SHOWTIP and the field
        // left 0 (its add fails: explorer still has the icon, version 4
        // from its first SETVERSION), every few seconds: NIF_SHOWTIP alone
        // says version 4 (clicks packed for 0 opened no menu).
        if m.message == NIM_ADD && e.set_version.is_none() && m.flags & NIF_SHOWTIP != 0 {
            e.version = if (3..=4).contains(&m.version) { m.version } else { 4 };
        }
    };
    let icon = m.flags & NIF_ICON != 0 && m.hicon != 0;
    match (m.message, pos) {
        // Refresh the registration and preserve the last explicit version.
        (NIM_ADD, Some(i)) => {
            let old = std::mem::take(&mut list[i]);
            list[i] = Entry {
                guid: m.guid, own_icon: old.own_icon, pixels: old.pixels, gen: old.gen, added: true,
                set_version: old.set_version, version: old.set_version.unwrap_or_default(), ..Default::default()
            };
            set_fields(&mut list[i], m);
            if icon { Change::Icon(i, m.hicon) } else { Change::None }
        }
        // MODIFY for an unknown icon adds it: explorer's own icons and icons
        // added before we started arrive as MODIFY first (S1).
        (NIM_ADD | NIM_MODIFY, None) => {
            let mut e = Entry { guid: m.guid, added: m.message == NIM_ADD, ..Default::default() };
            set_fields(&mut e, m);
            list.push(e);
            if icon { Change::Icon(list.len() - 1, m.hicon) } else { Change::None }
        }
        // The same handle number may carry a new picture: always re-render.
        (NIM_MODIFY, Some(i)) => {
            set_fields(&mut list[i], m);
            if icon { Change::Icon(i, m.hicon) } else { Change::None }
        }
        (NIM_DELETE, Some(i)) => Change::Removed(list.remove(i).own_icon),
        (NIM_SETVERSION, Some(i)) => {
            list[i].version = m.version;
            list[i].set_version = Some(m.version);
            Change::None
        }
        (NIM_SETVERSION, None) => {
            // Its registration may have gone to Explorer. Remember the
            // version without showing anything until an icon image arrives.
            list.push(Entry {
                owner: m.hwnd, uid: m.uid, guid: m.guid,
                version: m.version, set_version: Some(m.version), ..Default::default()
            });
            Change::None
        }
        _ => Change::None,
    }
}

static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static SPY: AtomicIsize = AtomicIsize::new(0);
static NOTIFY: AtomicIsize = AtomicIsize::new(0);
static NOTIFY_PENDING: AtomicBool = AtomicBool::new(false);
static ICON_PX: AtomicU32 = AtomicU32::new(32);
static GEN: AtomicU64 = AtomicU64::new(0);
static EXPLORER_PID: AtomicU32 = AtomicU32::new(0);
static HOOKS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
static REASSERT_STEP: AtomicU32 = AtomicU32::new(0);
/// The spy thread (0 until started): explorer-gone notices go to it.
static TRAY_TID: AtomicU32 = AtomicU32::new(0);
/// Where the top bars show tray icons: (bar, owner, uid, guid, screen rect
/// as left, top, right, bottom px). Apps ask where their icon is
/// (Shell_NotifyIconGetRect) to put their menu by it: explorer's answer is
/// its own hidden taskbar's, where the Epic Games Launcher's menu never
/// showed.
static SHOWN: Mutex<Vec<(isize, u32, u32, Option<[u8; 16]>, (i32, i32, i32, i32))>> = Mutex::new(Vec::new());

/// The icons bar `bar` shows now, with their cells (replaces what it showed).
pub fn set_shown(bar: isize, cells: Vec<(u32, u32, Option<[u8; 16]>, (i32, i32, i32, i32))>) {
    if let Ok(mut l) = SHOWN.lock() {
        l.retain(|e| e.0 != bar);
        l.extend(cells.into_iter().map(|(o, u, g, r)| (bar, o, u, g, r)));
    }
}

/// Where the icon `q` asks about is shown: the cell under the pointer when
/// more than one bar shows it, else the first.
fn shown_rect(q: &proto::RectQuery) -> Option<(i32, i32, i32, i32)> {
    let l = SHOWN.lock().ok()?;
    let mine: Vec<(i32, i32, i32, i32)> = l
        .iter()
        .filter(|e| match (q.guid, e.3) {
            (Some(a), Some(b)) => a == b,
            _ => e.1 == q.hwnd && e.2 == q.uid,
        })
        .map(|e| e.4)
        .collect();
    let mut p = POINT::default();
    let _ = unsafe { GetCursorPos(&mut p) };
    mine.iter().copied().find(|r| p.x >= r.0 && p.x < r.2 && p.y >= r.1 && p.y < r.3).or_else(|| mine.first().copied())
}
static STARTED: AtomicBool = AtomicBool::new(false);
/// Explorer broadcast TaskbarCreated (set by the dock, read by the spy
/// thread while it waits to build the spy).
static EXPLORER_ANNOUNCED: AtomicBool = AtomicBool::new(false);
/// To the spy thread (thread message): explorer exited.
const WM_EXPLORER_GONE: u32 = WM_APP + 3;
static WM_TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// The explorer that was running when we last broadcast TaskbarCreated
/// (the settle-time ask goes out once per explorer).
static ASKED_PID: AtomicU32 = AtomicU32::new(0);
/// Forwards to explorer that failed in a row; after FAIL_LIMIT the spy steps
/// back (goes to the bottom of the Z-order) so apps reach explorer directly.
static FAILS: AtomicU32 = AtomicU32::new(0);
static BYPASS: AtomicBool = AtomicBool::new(false);
const FAIL_LIMIT: u32 = 2;
/// topbar.tray: off = no spy at all (the bar shows the button instead).
static ENABLED: AtomicBool = AtomicBool::new(true);
/// Locked, asleep or the display off: the watchdog sleeps.
static QUIET: AtomicBool = AtomicBool::new(false);
/// Explorer's taskbar has stayed in front of the spy (FrontWatch::stuck).
static STUCK: AtomicBool = AtomicBool::new(false);
/// This process is the dock (not the guardian): our own tray icon's calls
/// are kept in the cache from the start, also while the mirror is off or
/// the top bar is, so the icon is there once the mirror comes (back) on.
static USED: AtomicBool = AtomicBool::new(false);
/// topbar.tray_hotplug: explorer's Safely Remove icon is shown (off by
/// default: see hotplug_shown).
static SHOW_HOTPLUG: AtomicBool = AtomicBool::new(false);

/// The dock is starting (before its own tray icon is added): see USED.
pub fn dock_started() {
    USED.store(true, Ordering::SeqCst);
}
/// To the spy thread (thread message): ENABLED, NOTIFY or QUIET changed.
const WM_TRAY_STATE: u32 = WM_APP + 4;

fn hwnd(v: isize) -> HWND {
    HWND(v as *mut _)
}

/// Our own TaskbarCreated broadcasts carry this in wParam and lParam.
/// Explorer's own broadcast carries zeros, and apps ignore both (the
/// message has no parameters), so a receiver in this process tells our
/// "add your icons again" apart from a real explorer restart exactly. (A
/// time window could not: explorer's broadcast after a restart often comes
/// a moment after ours, and was taken for ours.)
const OWN_TAG: usize = 0x4E42_5452; // "NBTR"

/// This TaskbarCreated (its wParam and lParam) is our tray spy's own
/// broadcast, not explorer's: explorer did not restart for it.
pub fn is_own_broadcast(wp: WPARAM, lp: LPARAM) -> bool {
    wp.0 == OWN_TAG && lp.0 as usize == OWN_TAG
}

/// Who our "add your icons again" goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    /// Every top-level window (the spy is new: we know no icon yet).
    Everyone,
    /// Every top-level window except those of apps whose icons we already
    /// know whole. TaskbarCreated means "explorer restarted" to apps, and
    /// some do more than re-add an icon: Wallpaper Engine rebuilds its
    /// wallpaper (it flickered on every ask, and we asked after every
    /// edge reservation, retake of the front and resume).
    Missing,
}

/// The processes not to ask again: they own an icon we know whole.
fn known_pids() -> Vec<u32> {
    let owners: Vec<u32> = ENTRIES.lock().map(|l| l.iter().filter(|e| e.added && !incomplete(e)).map(|e| e.owner).collect()).unwrap_or_default();
    let mut pids: Vec<u32> = owners
        .into_iter()
        .filter_map(|o| {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd(o as isize), Some(&mut pid)) };
            (pid != 0).then_some(pid)
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Whether a window of `pid` gets a Missing ask.
fn ask_missing(pid: u32, known: &[u32], me: u32) -> bool {
    pid != 0 && pid != me && known.binary_search(&pid).is_err()
}

unsafe extern "system" fn collect_top(h: HWND, lp: LPARAM) -> windows::core::BOOL {
    let out = &mut *(lp.0 as *mut Vec<HWND>);
    out.push(h);
    true.into()
}

/// The collection after a spy is built: everyone only the first time in
/// this process (we know no icon yet); every later spy (explorer
/// restarted, the spy rebuilt, the tray switched back on) asks only the
/// apps whose icons we miss. Every other ask (front retaken, explorer
/// answering again, an edge reservation, a settled explorer) is Missing
/// too, and a single app is asked alone (ask_owner_again).
fn collect_ask(asked_everyone_before: bool) -> Ask {
    if asked_everyone_before { Ask::Missing } else { Ask::Everyone }
}

fn broadcast_taskbar_created(ask: Ask) {
    ASKED_PID.store(crate::taskbar::explorer_pid(), Ordering::SeqCst);
    if let Ok(mut r) = READDS.lock() {
        r.asked(Instant::now());
    }
    unsafe {
        let msg = WM_TASKBAR_CREATED.load(Ordering::SeqCst);
        match ask {
            Ask::Everyone => {
                let _ = PostMessageW(Some(HWND_BROADCAST), msg, WPARAM(OWN_TAG), LPARAM(OWN_TAG as isize));
            }
            Ask::Missing => {
                let known = known_pids();
                let me = windows::Win32::System::Threading::GetCurrentProcessId();
                let mut tops: Vec<HWND> = Vec::new();
                let _ = EnumWindows(Some(collect_top), LPARAM(&mut tops as *mut _ as isize));
                for t in tops {
                    let mut pid = 0u32;
                    GetWindowThreadProcessId(t, Some(&mut pid));
                    if ask_missing(pid, &known, me) {
                        let _ = PostMessageW(Some(t), msg, WPARAM(OWN_TAG), LPARAM(OWN_TAG as isize));
                    }
                }
            }
        }
        // Say in the log what came back (one line per ask, coalesced).
        let spy = SPY.load(Ordering::SeqCst);
        if spy != 0 {
            SetTimer(Some(hwnd(spy)), COUNT_TIMER, COUNT_DELAY_MS, None);
        }
    }
}

/// Starts the spy thread (once per process; never while `enabled`, the
/// bar's topbar.tray, is off: the setting comes with every call, so no
/// start path can build a spy before it is applied). Later calls retarget
/// the notifications and the icon size (top bar re-created, DPI changed)
/// and bring a spy that `detach` put away back.
pub fn start(notify: HWND, icon_px: u32, enabled: bool) {
    NOTIFY.store(notify.0 as isize, Ordering::SeqCst);
    // A notice pending for an old bar must not silence the new one.
    NOTIFY_PENDING.store(false, Ordering::SeqCst);
    USED.store(true, Ordering::SeqCst);
    if ENABLED.swap(enabled, Ordering::SeqCst) != enabled {
        crate::log!("tray: mirror switched {}", if enabled { "on" } else { "off" });
    }
    let old_px = ICON_PX.swap(icon_px.clamp(12, 64), Ordering::SeqCst);
    if STARTED.load(Ordering::SeqCst) {
        let spy = SPY.load(Ordering::SeqCst);
        if spy != 0 && old_px != icon_px {
            unsafe {
                let _ = PostMessageW(Some(hwnd(spy)), WM_RERENDER, WPARAM(0), LPARAM(0));
            }
        }
        post_state();
        changed();
        return;
    }
    spawn_if_wanted();
    changed();
}

/// The spy thread, once there is a bar and the tray is on.
fn spawn_if_wanted() {
    if !ENABLED.load(Ordering::SeqCst) || NOTIFY.load(Ordering::SeqCst) == 0 || STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    if std::thread::Builder::new().name("tray".into()).spawn(|| unsafe { run() }).is_err() {
        STARTED.store(false, Ordering::SeqCst);
        crate::log!("tray: cannot start the spy thread");
    }
}

/// Tells the spy thread to look at ENABLED / NOTIFY / QUIET again.
fn post_state() {
    let tid = TRAY_TID.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_TRAY_STATE, WPARAM(0), LPARAM(0));
        }
    }
}

/// topbar.tray_hotplug: show explorer's Safely Remove icon in the tray
/// (the bars take the new set at once).
pub fn set_show_hotplug(on: bool) {
    if SHOW_HOTPLUG.swap(on, Ordering::SeqCst) != on {
        changed();
    }
}

/// topbar.tray: mirror apps' tray icons (on) or not at all (off: no spy,
/// the bar shows the notification area button).
pub fn set_enabled(on: bool) {
    if ENABLED.swap(on, Ordering::SeqCst) == on {
        return;
    }
    crate::log!("tray: mirror switched {}", if on { "on" } else { "off" });
    if on {
        spawn_if_wanted();
    }
    post_state();
    changed();
}

/// The session is locked, asleep or disconnected, or the display is off
/// (`quiet`), or back: the watchdog sleeps meanwhile (the spy stays).
pub fn set_quiet(quiet: bool) {
    if QUIET.swap(quiet, Ordering::SeqCst) != quiet {
        post_state();
    }
}

/// How the tray mirror is doing, for the top bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Switched off (topbar.tray).
    Off,
    /// No spy yet (explorer's notification area is not ready).
    Waiting,
    Up,
    /// Explorer's taskbar stays in front of the spy: apps' icons reach
    /// explorer only.
    Stuck,
    /// Explorer did not answer; the spy stands aside until it does.
    Bypass,
}

pub fn health() -> Health {
    if !ENABLED.load(Ordering::SeqCst) {
        Health::Off
    } else if SPY.load(Ordering::SeqCst) == 0 {
        Health::Waiting
    } else if BYPASS.load(Ordering::SeqCst) {
        Health::Bypass
    } else if STUCK.load(Ordering::SeqCst) {
        Health::Stuck
    } else {
        Health::Up
    }
}

/// The bar shows one "notification area" button instead of icons: the
/// mirror is off, or its icons cannot be trusted to be all there.
pub fn needs_button(h: Health) -> bool {
    matches!(h, Health::Off | Health::Stuck | Health::Bypass)
}

/// Takes the keyboard to Windows' own notification area, for the bar's
/// button: Win+B, Windows' documented "set focus in the notification
/// area", which puts the focus on the taskbar's "Show hidden icons"
/// button; Enter or Space there opens it. That is all: no key is sent
/// after it (SendInput has no target window, so an Enter could land in
/// whatever took the front meanwhile), and the button is not invoked
/// through UI Automation either (its automation id is undocumented and
/// differs between Windows builds, its name is localized). Nothing is sent
/// while the user holds a modifier (keys::unmixed).
pub fn open_overflow() {
    let sent = crate::keys::unmixed(&crate::keys::NOTIFICATION_AREA);
    crate::log!("tray: notification area button: Win+B ({sent:?})");
}

/// While alive, our spy is out of FindWindow's way (bottom of the
/// Z-order), so our own SHAppBarMessage calls reach explorer directly.
/// Nests; the spy goes back on top when the last one drops.
pub struct Aside;

static ASIDE_DEPTH: AtomicU32 = AtomicU32::new(0);
/// An Aside (not a quiet one) was taken since the spy last went back on
/// top: ask apps again once it does.
static ASIDE_RECOLLECT: AtomicBool = AtomicBool::new(false);

impl Aside {
    pub fn new() -> Self {
        ASIDE_RECOLLECT.store(true, Ordering::SeqCst);
        Self::take()
    }

    /// Same, but apps are not asked for their icons again afterwards: for
    /// our own tray icon's calls (crate::dock::trayicon), which run again
    /// on that broadcast and would ask again, and again.
    pub fn quiet() -> Self {
        Self::take()
    }

    fn take() -> Self {
        let spy = SPY.load(Ordering::SeqCst);
        if spy != 0 && ASIDE_DEPTH.fetch_add(1, Ordering::SeqCst) == 0 {
            unsafe {
                let h = hwnd(spy);
                let _ = SetWindowPos(h, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                let _ = SetWindowPos(h, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        } else if spy == 0 {
            ASIDE_DEPTH.fetch_add(1, Ordering::SeqCst);
        }
        Aside
    }
}

impl Drop for Aside {
    fn drop(&mut self) {
        if ASIDE_DEPTH.fetch_sub(1, Ordering::SeqCst) == 1 {
            unsafe {
                assert_top();
                // Icons apps changed in that moment went to explorer only:
                // ask once more a little later (coalesced).
                let spy = SPY.load(Ordering::SeqCst);
                if ASIDE_RECOLLECT.swap(false, Ordering::SeqCst) && spy != 0 && COLLECT_ARMED.load(Ordering::SeqCst) {
                    SetTimer(Some(hwnd(spy)), COLLECT_TIMER, RECOLLECT_MS, None);
                }
            }
        }
    }
}

/// The startup collection has run; later Aside()s re-collect.
static COLLECT_ARMED: AtomicBool = AtomicBool::new(false);
const RECOLLECT_MS: u32 = 1000;

/// After sleep, unlock or a reconnect (audit A09): the spy takes the front
/// again a few times while explorer settles, and the bar is told to look
/// at the icons once more.
pub fn resync() {
    let spy = SPY.load(Ordering::SeqCst);
    if spy != 0 {
        unsafe {
            let _ = PostMessageW(Some(hwnd(spy)), WM_RESYNC, WPARAM(0), LPARAM(0));
        }
    }
    changed();
}

/// The watched explorer exited (dock/shell.rs): the spy goes away now, so
/// the next explorer starts without another `Shell_TrayWnd` around.
pub fn explorer_gone() {
    EXPLORER_ANNOUNCED.store(false, Ordering::SeqCst);
    let tid = TRAY_TID.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_EXPLORER_GONE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Explorer itself broadcast TaskbarCreated (not our tray): its
/// notification area is up, the spy may come back.
pub fn explorer_announced() {
    EXPLORER_ANNOUNCED.store(true, Ordering::SeqCst);
}

/// What the spy thread does about its window.
#[derive(Debug, PartialEq, Eq)]
pub enum SpyAction {
    None,
    /// Build the spy (take the front, ask apps for their icons).
    Create,
    /// Same, after waiting SPY_FALLBACK for explorer's announcement in vain.
    CreateLate,
    /// Destroy the spy: explorer is gone.
    Destroy,
}

/// An explorer this old (or whose taskbar we have seen this long) without
/// an announcement we heard: build the spy anyway (the top bar must not
/// lose its tray for good). Also how old an explorer found at our start
/// must be to count as settled.
pub const SPY_FALLBACK: Duration = Duration::from_secs(20);

/// When the spy may exist (see the module notes).
#[derive(Debug)]
pub struct SpyLife {
    up: bool,
    /// No look taken yet (startup: an explorer already up gets the spy now).
    first: bool,
    tray_since: Option<Instant>,
    announced: bool,
}

impl Default for SpyLife {
    fn default() -> Self {
        SpyLife { up: false, first: true, tray_since: None, announced: false }
    }
}

impl SpyLife {
    pub fn up(&self) -> bool {
        self.up
    }

    /// Explorer exited.
    pub fn explorer_gone(&mut self) -> SpyAction {
        self.first = false;
        self.tray_since = None;
        self.announced = false;
        if std::mem::replace(&mut self.up, false) {
            SpyAction::Destroy
        } else {
            SpyAction::None
        }
    }

    /// Explorer broadcast TaskbarCreated.
    pub fn announced(&mut self) {
        if !self.up {
            self.announced = true;
        }
    }

    /// A look at explorer: its taskbar exists (`tray`), with its
    /// notification area (`notify`); the explorer process runs for `age`
    /// (None: unknown).
    ///
    /// The spy is built when explorer announced its tray; at our own start,
    /// also for an explorer long settled (its announcement was before us);
    /// and after SPY_FALLBACK without one, late. Never just because
    /// TrayNotifyWnd exists: that is there before the tray is ready.
    pub fn tick(&mut self, tray: bool, notify: bool, age: Option<Duration>, now: Instant) -> SpyAction {
        let first = std::mem::replace(&mut self.first, false);
        if self.up {
            // The spy's own look: explorer's taskbar is gone.
            return if tray { SpyAction::None } else { self.explorer_gone() };
        }
        if !tray {
            self.tray_since = None;
            return SpyAction::None;
        }
        let seen = now.duration_since(*self.tray_since.get_or_insert(now));
        let waited = age.unwrap_or_default().max(seen);
        let action = if self.announced || (first && notify && age.is_some_and(|a| a >= SPY_FALLBACK)) {
            SpyAction::Create
        } else if waited >= SPY_FALLBACK {
            SpyAction::CreateLate
        } else {
            return SpyAction::None;
        };
        self.up = true;
        self.announced = false;
        action
    }

    /// The spy could not be built: look again later.
    pub fn create_failed(&mut self) {
        self.up = false;
    }
}

/// Explorer's taskbar exists, and with its notification area child.
unsafe fn explorer_tray_state() -> (bool, bool) {
    match crate::taskbar::explorer_tray() {
        Some(t) => (true, FindWindowExW(Some(t), None, w!("TrayNotifyWnd"), windows::core::PCWSTR::null()).is_ok()),
        None => (false, false),
    }
}

/// How long explorer (the process owning its taskbar) has been running.
unsafe fn explorer_age() -> Option<Duration> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let pid = crate::taskbar::explorer_pid();
    if pid == 0 {
        return None;
    }
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok();
    let _ = CloseHandle(h);
    if !ok {
        return None;
    }
    let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    let now = GetSystemTimeAsFileTime();
    // 100 ns units.
    Some(Duration::from_nanos(ft(now).saturating_sub(ft(c)).saturating_mul(100)))
}

/// Hears explorer's TaskbarCreated on the spy thread while the spy is gone
/// (a hidden top-level window: broadcasts reach only those).
unsafe extern "system" fn watch_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let tb = WM_TASKBAR_CREATED.load(Ordering::SeqCst);
    if tb != 0 && msg == tb {
        if !is_own_broadcast(wp, lp) {
            explorer_announced();
        }
        return LRESULT(0);
    }
    DefWindowProcW(h, msg, wp, lp)
}

unsafe fn create_watch(hinst: windows::Win32::Foundation::HINSTANCE) {
    let class = w!("NopBarTrayWatch");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(watch_proc), hInstance: hinst, lpszClassName: class, ..Default::default() };
    RegisterClassExW(&wc);
    match CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, class, w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None) {
        Ok(h) => {
            // Explorer's broadcast must get through even if we run elevated.
            let _ = ChangeWindowMessageFilterEx(h, WM_TASKBAR_CREATED.load(Ordering::SeqCst), MSGFLT_ALLOW, None);
        }
        Err(e) => crate::log!("tray: cannot create the TaskbarCreated listener: {e}"),
    }
}

/// Destroys the spy (explorer is gone). On the spy thread.
unsafe fn destroy_spy(why: &str) {
    let spy = SPY.swap(0, Ordering::SeqCst);
    if spy != 0 {
        crate::log!("tray: {why}; spy closed");
        let _ = DestroyWindow(hwnd(spy));
    }
    STUCK.store(false, Ordering::SeqCst);
    changed();
}

/// The entries to keep when the mirror goes off: our own process's
/// (`is_own` by owner window). A later spy asks other apps again, but
/// never our own process (ask_missing), and our tray icon's calls go to
/// explorer directly (mirror_own), so its entry must survive. The rest is
/// returned to be dropped.
fn keep_own(list: &mut Vec<Entry>, is_own: impl Fn(u32) -> bool) -> Vec<Entry> {
    let (keep, drop): (Vec<Entry>, Vec<Entry>) = std::mem::take(list).into_iter().partition(|e| is_own(e.owner));
    *list = keep;
    drop
}

/// The mirror is off or the bar gone: no hooks on explorer, no icons of
/// other apps kept (a later spy asks them again); our own stays.
unsafe fn forget_all() {
    if let Ok(mut hooks) = HOOKS.lock() {
        for h in hooks.drain(..) {
            let _ = UnhookWinEvent(HWINEVENTHOOK(h as *mut _));
        }
    }
    EXPLORER_PID.store(0, Ordering::SeqCst);
    let me = windows::Win32::System::Threading::GetCurrentProcessId();
    let is_own = |owner: u32| {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd(owner as isize), Some(&mut pid));
        pid == me
    };
    let owns: Vec<isize> = ENTRIES.lock().map(|mut l| keep_own(&mut l, is_own).into_iter().map(|e| e.own_icon).collect()).unwrap_or_default();
    for own in owns.into_iter().filter(|&o| o != 0) {
        let _ = DestroyIcon(HICON(own as *mut _));
    }
    if let Ok(mut r) = READDS.lock() {
        *r = Readds::default();
    }
    BYPASS.store(false, Ordering::SeqCst);
    FAILS.store(0, Ordering::SeqCst);
    changed();
}

/// The top bar is gone: nobody shows the icons, so the spy goes away
/// too (`start` brings it back).
pub fn detach() {
    menu_place::stop();
    NOTIFY.store(0, Ordering::SeqCst);
    NOTIFY_PENDING.store(false, Ordering::SeqCst);
    post_state();
}

fn changed() {
    let h = NOTIFY.load(Ordering::SeqCst);
    if h != 0 && !NOTIFY_PENDING.swap(true, Ordering::SeqCst) {
        unsafe {
            if PostMessageW(Some(hwnd(h)), WM_TRAY_CHANGED, WPARAM(0), LPARAM(0)).is_err() {
                NOTIFY_PENDING.store(false, Ordering::SeqCst);
            }
        }
    }
}

/// Visible icons of live apps, in the order they were added. Clears the
/// "changed" flag (call when handling WM_TRAY_CHANGED).
pub fn snapshot() -> Vec<Entry> {
    NOTIFY_PENDING.store(false, Ordering::SeqCst);
    if !ENABLED.load(Ordering::SeqCst) {
        return Vec::new();
    }
    // Nothing that can send a window message runs under the lock.
    let explorer = crate::taskbar::explorer_pid();
    let all: Vec<Entry> = {
        let Ok(mut list) = ENTRIES.lock() else { return Vec::new() };
        // Apps that died without NIM_DELETE.
        list.retain(|e| {
            let alive = unsafe { IsWindow(Some(hwnd(e.owner as isize))).as_bool() };
            if !alive && e.own_icon != 0 {
                unsafe {
                    let _ = DestroyIcon(HICON(e.own_icon as *mut _));
                }
            }
            alive
        });
        list.clone()
    };
    // Of explorer's own icons only those the bar's status area shows
    // already are left out (see explorer_icon_kept), and Safely Remove
    // Hardware unless asked for (hotplug_shown); location in use and the
    // like stay.
    let hotplug = SHOW_HOTPLUG.load(Ordering::SeqCst);
    all.into_iter()
        .filter(|e| !e.hidden && e.pixels.is_some())
        .filter(|e| {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd(e.owner as isize), Some(&mut pid)) };
            explorer == 0 || pid != explorer || (explorer_icon_kept(e) && hotplug_shown(e, hotplug))
        })
        .collect()
}

/// A GUID as Shell_NotifyIcon carries it (Data1..3 little-endian, Data4 as
/// is), from its usual parts.
const fn guid_bytes(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}

/// Explorer's system icons share one GUID family, {7820AE7x-23E3-4229-
/// 82C1-E41CB67D5B9C} (Windows 10 and 11; the same ids shells such as
/// Cairo's ManagedShell match on). The last byte of Data1 tells them apart.
const fn system_icon(n: u8) -> [u8; 16] {
    guid_bytes(0x7820_AE00 | n as u32, 0x23E3, 0x4229, [0x82, 0xC1, 0xE4, 0x1C, 0xB6, 0x7D, 0x5B, 0x9C])
}

/// Explorer's icons the bar's status area replaces: volume (73), network
/// (74) and power / battery (75). The others of the family stay in the
/// tray: 76 (security and health), 77 (location in use), 78 (Safely Remove
/// Hardware and Eject Media), 81 (Windows Update), 82 (microphone in use),
/// 83 (Meet Now), and any we do not know.
pub const BAR_SHOWS: [[u8; 16]; 3] = [system_icon(0x73), system_icon(0x74), system_icon(0x75)];

/// Whether one of explorer's own tray icons is shown in the tray: every
/// one but those the bar already shows (BAR_SHOWS, by GUID), and those
/// whose callback message we do not know. Shown means clickable: explorer
/// is never asked to add its icons again (a TaskbarCreated to explorer is
/// not ours to send, see ask_owner_again), so a click on an icon without
/// a callback would go nowhere. That covers icons explorer registered
/// with no callback at all (its own UI handles them; uID 100 on build
/// 26300) and icons whose NIM_ADD went past us while only MODIFYs without
/// NIF_MESSAGE reached us; such an icon shows as soon as a message naming
/// its callback arrives (explorer adds its icons again whenever the spy
/// asks every app, at the spy's start and after explorer restarts).
/// Anything else, GUID or not, is kept: hotplug registers uID 1226 with
/// callback 0x4CA, so it is shown exactly when it can be clicked.
pub fn explorer_icon_kept(e: &Entry) -> bool {
    if e.guid.is_some_and(|g| BAR_SHOWS.contains(&g)) {
        return false;
    }
    !incomplete(e)
}

/// Explorer's "Safely Remove Hardware and Eject Media" icon: its GUID
/// ({7820AE78-...}), or on builds without GUIDs its uID 1226.
const HOTPLUG: [u8; 16] = system_icon(0x78);
const HOTPLUG_UID: u32 = 1226;

pub fn is_hotplug(e: &Entry) -> bool {
    match e.guid {
        Some(g) => g == HOTPLUG,
        None => e.uid == HOTPLUG_UID,
    }
}

/// Pure: whether one of explorer's icons passes the Safely Remove rule:
/// that icon only when `show` (topbar.tray_hotplug), every other icon as
/// explorer_icon_kept decides. Explorer ignores clicks that do not come
/// from its own taskbar, so its menu never opened from ours; the Control
/// Center's removable storage section does the job instead.
pub fn hotplug_shown(e: &Entry, show: bool) -> bool {
    show || !is_hotplug(e)
}

/// A tray icon's id that survives restarts (the config keeps the user's
/// choices by it): the icon's GUID if it registered one, else its app's exe
/// path (lowercased) and its uID. None if neither is known: a window class
/// and uID are not unique across apps, so such an icon is never saved (see
/// session_id). See TopbarCfg::tray_hidden.
pub fn stable_id(guid: Option<[u8; 16]>, exe: Option<&str>, uid: u32) -> Option<String> {
    if let Some(g) = guid {
        // GUID bytes as stored: Data1..3 little-endian, Data4 as is.
        let d1 = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
        let d2 = u16::from_le_bytes([g[4], g[5]]);
        let d3 = u16::from_le_bytes([g[6], g[7]]);
        let tail: String = g[10..].iter().map(|b| format!("{b:02X}")).collect();
        return Some(format!("guid:{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{tail}}}", g[8], g[9]));
    }
    exe.map(str::trim).filter(|e| !e.is_empty()).map(|e| format!("exe:{}#{uid}", e.to_lowercase()))
}

/// The id of an icon with no stable id: unique while its owner window
/// lives, never written to the config.
pub fn session_id(owner: u32, uid: u32) -> String {
    format!("session:{owner:08x}#{uid}")
}

pub fn is_session_id(id: &str) -> bool {
    id.starts_with("session:")
}

/// Which tray icons sit behind the chevron (true = hidden), in `ids` order.
/// The user's choices win; the rest are shown while fewer than `auto_max`
/// icons are shown (counting the user's always-shown ones), then hidden.
pub fn split(ids: &[String], hidden: &[String], visible: &[String], auto_max: usize) -> Vec<bool> {
    let mut shown = ids.iter().filter(|id| visible.contains(id) && !hidden.contains(id)).count();
    ids.iter()
        .map(|id| {
            if hidden.contains(id) {
                true
            } else if visible.contains(id) {
                false
            } else if shown < auto_max {
                shown += 1;
                false
            } else {
                true
            }
        })
        .collect()
}

/// The tray's display order (indexes into `ids`): icons by their place in
/// `order` (the user's), then the rest by when they were first seen
/// (`seen`; an app that deletes and re-adds its icon keeps its place),
/// then by index.
pub fn rank(ids: &[String], order: &[String], seen: &std::collections::HashMap<String, u64>) -> Vec<usize> {
    let mut ks: Vec<usize> = (0..ids.len()).collect();
    ks.sort_by_key(|&k| {
        let id = &ids[k];
        (order.iter().position(|o| o == id).unwrap_or(usize::MAX), seen.get(id).copied().unwrap_or(u64::MAX), k)
    });
    ks
}

/// `ranked` (ids in display order) with `id` moved to its side (`hidden`)
/// at position `slot` among that side's other icons; `side_of` says which
/// side every other icon is on.
pub fn reorder(ranked: &[String], side_of: impl Fn(&str) -> bool, id: &str, hidden: bool, slot: usize) -> Vec<String> {
    let mut out: Vec<String> = ranked.iter().filter(|x| *x != id).cloned().collect();
    let same: Vec<usize> = (0..out.len()).filter(|&i| side_of(&out[i]) == hidden).collect();
    let at = match same.get(slot) {
        Some(&i) => i,
        None => same.last().map(|&i| i + 1).unwrap_or(out.len()),
    };
    out.insert(at, id.to_string());
    out
}

/// The owner window's exe path (for stable_id); None if it cannot be read.
pub fn owner_exe(owner: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    let h = hwnd(owner as isize);
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(p);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize])).filter(|e| !e.trim().is_empty())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Keyboard,
    Left,
    Right,
    Double,
}

/// Apps that answered "add your icons again" with a delete and whose add
/// never reached us (explorer's taskbar was ahead for that moment; seen
/// for Notion, Orca and Google Drive while Slack's add came through): they
/// are asked once more, alone.
#[derive(Debug, Default)]
pub struct Readds {
    asked_at: Option<Instant>,
    /// (owner, deleted at, asks so far)
    waiting: Vec<(u32, Instant, u8)>,
}

/// A delete this soon after asking is the first half of a re-add.
const READD_DELETE_WINDOW: Duration = Duration::from_secs(4);
/// The add normally follows its delete at once; past this it went elsewhere.
const READD_ADD_GRACE: Duration = Duration::from_millis(1500);
const READD_MAX_ASKS: u8 = 2;

impl Readds {
    pub fn asked(&mut self, now: Instant) {
        self.asked_at = Some(now);
    }

    pub fn deleted(&mut self, owner: u32, now: Instant) {
        let after_ask = self.asked_at.is_some_and(|t| now.saturating_duration_since(t) < READD_DELETE_WINDOW);
        if after_ask && !self.waiting.iter().any(|w| w.0 == owner) {
            self.waiting.push((owner, now, 0));
        }
    }

    pub fn added(&mut self, owner: u32) {
        self.waiting.retain(|w| w.0 != owner);
    }

    /// Owners to ask again now (each at most READD_MAX_ASKS times).
    pub fn due(&mut self, now: Instant) -> Vec<u32> {
        let mut out = Vec::new();
        self.waiting.retain_mut(|w| {
            if now.saturating_duration_since(w.1) < READD_ADD_GRACE {
                return true;
            }
            if w.2 >= READD_MAX_ASKS {
                return false;
            }
            out.push(w.0);
            w.1 = now;
            w.2 += 1;
            true
        });
        out
    }
}

static READDS: Mutex<Readds> = Mutex::new(Readds { asked_at: None, waiting: Vec::new() });

/// Owners asked to add their icons again (see `ask_owner_again`), and when.
static READD_ASKED: Mutex<Vec<(u32, Instant)>> = Mutex::new(Vec::new());
/// An app is asked again at most this often on its own (a click asks now).
const READD_EVERY: Duration = Duration::from_secs(10);
/// After asking an app to add its icon again, the click goes out this
/// much later (the app answers at once; this is plenty).
const READD_RETRY_MS: u32 = 400;

/// We cannot click this icon: the NIM_ADD that names its callback message
/// never reached us (it went to explorer while explorer's taskbar was found
/// first, or while the spy stood aside), only later MODIFYs did. Posting
/// message 0 to the app does nothing (seen with Slack: a click opened no
/// window).
pub fn incomplete(e: &Entry) -> bool {
    e.callback == 0
}

/// Only MODIFYs of this icon reached us: its callback (if any) and its
/// version may be wrong (a missed NIM_SETVERSION makes us pack the click
/// for version 0 while the app reads version 4 and ignores it; seen with
/// Google Drive: right-click opened nothing).
pub fn partly_known(e: &Entry) -> bool {
    !e.added
}

/// Whether `owner` may be asked again now; records the ask. `force` (a
/// click) asks regardless of the last ask.
fn readd_due(asked: &mut Vec<(u32, Instant)>, owner: u32, now: Instant, force: bool) -> bool {
    asked.retain(|(_, t)| now.saturating_duration_since(*t) < READD_EVERY);
    if !force && asked.iter().any(|(o, _)| *o == owner) {
        return false;
    }
    asked.retain(|(o, _)| *o != owner);
    asked.push((owner, now));
    true
}

/// Asks one app to add its icons again, as after an explorer restart: a
/// TaskbarCreated posted to its icon window alone (apps delete and re-add,
/// with the callback message this time, and the spy is in front to hear
/// it). Not while the spy stands aside or is behind explorer (the ADD
/// would miss us again), nor to explorer's or our own windows.
unsafe fn ask_owner_again(owner: u32, force: bool) -> bool {
    let spy = SPY.load(Ordering::SeqCst);
    if spy == 0 || BYPASS.load(Ordering::SeqCst) || ASIDE_DEPTH.load(Ordering::SeqCst) > 0 || lost_front(hwnd(spy)) {
        return false;
    }
    let h = hwnd(owner as isize);
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, Some(&mut pid));
    let me = windows::Win32::System::Threading::GetCurrentProcessId();
    if pid == 0 || pid == me || pid == crate::taskbar::explorer_pid() || !IsWindow(Some(h)).as_bool() {
        return false;
    }
    let due = READD_ASKED.lock().map(|mut a| readd_due(&mut a, owner, Instant::now(), force)).unwrap_or(false);
    if !due {
        return false;
    }
    let msg = WM_TASKBAR_CREATED.load(Ordering::SeqCst);
    msg != 0 && PostMessageW(Some(h), msg, WPARAM(OWN_TAG), LPARAM(OWN_TAG as isize)).is_ok()
}

/// Sends a click to the icon's app, the way the taskbar does (S1).
/// `at` is the screen point the app should treat as the click (it anchors
/// menus and flyouts there). An icon whose callback we never saw is first
/// registered again by its app, then clicked.
pub fn click(owner: u32, uid: u32, kind: Click, at: POINT) {
    let Some(e) = ENTRIES.lock().ok().and_then(|l| l.iter().find(|e| e.owner == owner && e.uid == uid).cloned()) else {
        crate::log!("tray: {kind:?} click on an icon that is gone");
        return;
    };
    // Explorer is never asked to add its icons again (ask_owner_again):
    // its own icons are clicked with what is known, at once.
    let from_explorer = unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd(owner as isize), Some(&mut pid));
        pid != 0 && pid == crate::taskbar::explorer_pid()
    };
    if from_explorer {
        if incomplete(&e) {
            crate::log!("tray: {kind:?} click on an explorer icon with no callback (uid {}); nothing to send", e.uid);
        } else {
            send_click(&e, kind, at);
        }
        return;
    }
    if incomplete(&e) || partly_known(&e) {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd(owner as isize), Some(&mut pid));
            // Granted now, while this click is the last input.
            let _ = AllowSetForegroundWindow(pid);
            let asked = ask_owner_again(owner, true);
            crate::log!("tray: {kind:?} click to pid {pid}: its registration was never seen (callback 0x{:X}); asked the app to add its icon again ({asked})", e.callback);
        }
        let guid = e.guid;
        crate::motion::after(READD_RETRY_MS, Box::new(move || {
            // Found by GUID when it has one (the app may re-add from another window).
            let again = ENTRIES.lock().ok().and_then(|l| {
                l.iter().find(|x| match (guid, x.guid) {
                    (Some(a), Some(b)) => a == b,
                    _ => x.owner == owner && x.uid == uid,
                })
                .cloned()
            });
            match again {
                // Not added again but a callback known: send it as it is.
                Some(x) if !incomplete(&x) => send_click(&x, kind, at),
                _ => crate::log!("tray: the app did not add its icon again; the click went nowhere"),
            }
        }));
        return;
    }
    send_click(&e, kind, at);
}

impl Entry {
    pub fn standard_tip(&self) -> bool {
        (self.version < 4 || self.show_tip) && !self.tip.is_empty()
    }

    pub fn pointer_message(&self, message: u32, at: POINT) {
        if self.callback == 0 { return; }
        let (w, l) = pack_callback(self.version, self.uid, message, at.x, at.y);
        unsafe { let _ = PostMessageW(Some(hwnd(self.owner as isize)), self.callback, WPARAM(w), LPARAM(l)); }
    }
}

fn send_click(e: &Entry, kind: Click, at: POINT) {
    let h = hwnd(e.owner as isize);
    unsafe {
        let posted = std::cell::Cell::new(0u32);
        let send = |ev: u32| {
            let (w, l) = pack_callback(e.version, e.uid, ev, at.x, at.y);
            if PostMessageW(Some(h), e.callback, WPARAM(w), LPARAM(l)).is_ok() {
                posted.set(posted.get() + 1);
            }
        };
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        // Let the app bring up its window / menu in front.
        let allowed = AllowSetForegroundWindow(pid).is_ok();
        let alive = IsWindow(Some(h)).as_bool();
        match kind {
            Click::Right => {
                // Context menus need their owner in front (KB135788).
                let _ = SetForegroundWindow(h);
                menu_place::watch(pid, at);
                send(WM_RBUTTONDOWN);
                send(WM_RBUTTONUP);
                if e.version >= 4 {
                    send(WM_CONTEXTMENU);
                }
                if posted.get() == 0 {
                    menu_place::stop();
                }
            }
            Click::Double => {
                // The first click already went out as DOWN/UP: the second
                // press of a real double-click is DBLCLK then UP.
                send(WM_LBUTTONDBLCLK);
                send(WM_LBUTTONUP);
            }
            Click::Keyboard if e.version >= 4 => send(NIN_SELECT + 1),
            Click::Keyboard | Click::Left => {
                send(WM_LBUTTONDOWN);
                send(WM_LBUTTONUP);
                if e.version >= 4 {
                    send(NIN_SELECT);
                }
            }
        }
        crate::log!(
            "tray: {kind:?} click to pid {pid} (window alive {alive}, uid {}, version {}, callback 0x{:X}, {} message(s) posted, foreground allowed {allowed})",
            e.uid,
            e.version,
            e.callback,
            posted.get()
        );
    }
}

// ---- the spy thread ---------------------------------------------------------

unsafe fn run() {
    WM_TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::SeqCst);
    let Ok(hinst) = GetModuleHandleW(None) else { return };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: w!("Shell_TrayWnd"),
        ..Default::default()
    };
    if RegisterClassExW(&wc) == 0 {
        crate::log!("tray: cannot register the spy class");
        return;
    }
    TRAY_TID.store(windows::Win32::System::Threading::GetCurrentThreadId(), Ordering::SeqCst);
    create_watch(hinst.into());
    // The spy waits for explorer's notification area (early sign-in).
    let mut life = SpyLife::default();
    let look = |life: &mut SpyLife| {
        if EXPLORER_ANNOUNCED.swap(false, Ordering::SeqCst) {
            life.announced();
        }
        let (tray, notify) = explorer_tray_state();
        let age = if tray && !life.up() { explorer_age() } else { None };
        match life.tick(tray, notify, age, Instant::now()) {
            a @ (SpyAction::Create | SpyAction::CreateLate) => {
                if a == SpyAction::CreateLate {
                    crate::log!("tray: explorer did not announce its tray within {} s; starting the spy anyway", SPY_FALLBACK.as_secs());
                }
                if create_spy(hinst.into()).is_some() {
                    REASSERT_STEP.store(0, Ordering::SeqCst);
                    crate::log!("tray: spy ready{}", if notify { "" } else { " (explorer's taskbar has no notification area child)" });
                } else {
                    life.create_failed();
                }
                true
            }
            SpyAction::Destroy => {
                destroy_spy("explorer's taskbar is gone");
                true
            }
            SpyAction::None => !life.up(),
        }
    };
    // Watchdog: the spy has been seen gone while the process lived (someone
    // destroyed it), which silently empties the top bar's tray. Check now
    // and then on this thread and build it again. It also keeps the spy the
    // Shell_TrayWnd apps find first: nothing tells us when explorer's
    // taskbar moves ahead (no WinEvent fires for a top-level window's
    // Z-order), so look often (less often once calm, see Pace); FindWindow
    // is cheap. It runs only while the spy is wanted and the user is here.
    let mut watchdog = 0usize;
    let mut pace = Pace::default();
    let mut front = FrontWatch::default();
    let mut bypass = BypassWatch::default();
    let reconcile = |life: &mut SpyLife, watchdog: &mut usize, pace: &mut Pace| -> bool {
        let want_spy = ENABLED.load(Ordering::SeqCst) && NOTIFY.load(Ordering::SeqCst) != 0;
        if !want_spy {
            if life.up() || SPY.load(Ordering::SeqCst) != 0 {
                destroy_spy(if ENABLED.load(Ordering::SeqCst) { "the top bar is gone" } else { "the tray mirror is switched off" });
            }
            let me = windows::Win32::System::Threading::GetCurrentProcessId();
            let others = ENTRIES.lock().map(|l| {
                l.iter().any(|e| {
                    let mut pid = 0u32;
                    GetWindowThreadProcessId(hwnd(e.owner as isize), Some(&mut pid));
                    pid != me
                })
            });
            let kept = others.unwrap_or(false) || HOOKS.lock().map(|h| !h.is_empty()).unwrap_or(false);
            if kept {
                forget_all();
            }
            // Back on later: an explorer long settled gets the spy at once.
            *life = SpyLife::default();
        }
        let want_timer = want_spy && !QUIET.load(Ordering::SeqCst);
        if want_timer && *watchdog == 0 {
            *watchdog = SetTimer(None, 0, SPY_WATCHDOG_MS, None);
            pace.reset();
            return true;
        }
        if !want_timer && *watchdog != 0 {
            let _ = KillTimer(None, *watchdog);
            *watchdog = 0;
        }
        false
    };
    if reconcile(&mut life, &mut watchdog, &mut pace) {
        look(&mut life);
    }
    if !life.up() {
        crate::log!("tray: waiting for explorer's notification area before the spy starts");
    }

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        if msg.hwnd.is_invalid() && msg.message == WM_EXPLORER_GONE {
            if life.explorer_gone() == SpyAction::Destroy {
                destroy_spy("explorer exited; until its notification area is back");
            }
            continue;
        }
        if msg.hwnd.is_invalid() && msg.message == WM_TRAY_STATE {
            let woke = reconcile(&mut life, &mut watchdog, &mut pace);
            if woke {
                // Back (unlocked, display on, switched on): look now.
                front.in_front();
                look(&mut life);
            }
            continue;
        }
        if !msg.hwnd.is_invalid() && (msg.message == WM_RESYNC || msg.message == WM_TASKBAR_CREATED.load(Ordering::SeqCst)) {
            // Explorer moves: the watchdog looks often again.
            if pace.kick() && watchdog != 0 {
                let _ = KillTimer(None, watchdog);
                watchdog = SetTimer(None, 0, SPY_WATCHDOG_MS, None);
            }
        }
        if watchdog != 0 && msg.hwnd.is_invalid() && msg.message == WM_TIMER && msg.wParam.0 == watchdog {
            // Spy down (or explorer's taskbar just went): only the
            // lifecycle looks; nothing takes the front.
            let due = READDS.lock().map(|mut r| r.due(Instant::now())).unwrap_or_default();
            let mut calm = due.is_empty();
            for owner in due {
                if ask_owner_again(owner, true) {
                    crate::log!("tray: an app deleted its icon to add it again and the add missed us; asked it once more");
                    if let Ok(mut r) = READDS.lock() {
                        r.asked(Instant::now());
                    }
                }
            }
            let looked = look(&mut life);
            // Calm: the spy is up, in front, nothing pending. Then the
            // watchdog slows down; any trouble speeds it up again.
            calm &= !looked && life.up() && !BYPASS.load(Ordering::SeqCst) && SPY.load(Ordering::SeqCst) != 0 && {
                let spy = hwnd(SPY.load(Ordering::SeqCst));
                IsWindow(Some(spy)).as_bool() && !lost_front(spy)
            };
            if let Some(ms) = pace.tick(calm) {
                let _ = KillTimer(None, watchdog);
                watchdog = SetTimer(None, 0, ms, None);
            }
            if looked {
                continue;
            }
            if BYPASS.load(Ordering::SeqCst) {
                // Stepped aside for a stuck explorer (after sleep it can
                // hang for a few seconds): come back once it answers again,
                // instead of missing every new icon until explorer restarts.
                let answers = crate::taskbar::explorer_tray().is_some_and(|real| {
                    let mut r = 0usize;
                    SendMessageTimeoutW(real, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_NORMAL, 500, Some(&mut r)).0 != 0
                });
                if bypass.tick(answers) {
                    BYPASS.store(false, Ordering::SeqCst);
                    FAILS.store(0, Ordering::SeqCst);
                    assert_top();
                    crate::log!("tray: explorer answers again; the spy is back in front and asked apps we miss for their icons");
                    broadcast_taskbar_created(Ask::Missing);
                    changed();
                }
                continue;
            }
            bypass = BypassWatch::default();
            let spy = hwnd(SPY.load(Ordering::SeqCst));
            if !IsWindow(Some(spy)).as_bool() {
                crate::log!("tray: spy window was gone; creating it again");
                if create_spy(hinst.into()).is_some() {
                    REASSERT_STEP.store(0, Ordering::SeqCst);
                }
            } else if lost_front(spy) {
                // Explorer's taskbar got ahead of us: apps' Shell_NotifyIcon
                // finds explorer's window first and the top bar misses their
                // icons. Take the front back, then ask apps to add their
                // icons again (a few times a minute at most).
                assert_top();
                match front.lost(!lost_front(spy), Instant::now()) {
                    FrontAction::Recollect => {
                        crate::log!("tray: explorer's taskbar was in front of the spy; took it back and asked apps we miss for their icons");
                        broadcast_taskbar_created(Ask::Missing);
                    }
                    FrontAction::Stuck => {
                        crate::log!("tray: explorer's taskbar is in front of the spy and stays there; apps' tray icons reach explorer only");
                    }
                    FrontAction::Quiet => {}
                }
                set_stuck(front.stuck());
            } else {
                front.in_front();
                set_stuck(false);
            }
            continue;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// Explorer answered this many watchdog ticks in a row (500 ms each): the
/// spy comes back from stepping aside.
const BYPASS_RECOVER_TICKS: u32 = 6;

/// Counts explorer's answers while the spy stands aside for it.
#[derive(Debug, Default)]
pub struct BypassWatch {
    answered: u32,
}

impl BypassWatch {
    /// One watchdog tick; true = come back now.
    pub fn tick(&mut self, answers: bool) -> bool {
        self.answered = if answers { self.answered + 1 } else { 0 };
        if self.answered >= BYPASS_RECOVER_TICKS {
            self.answered = 0;
            return true;
        }
        false
    }
}

/// How often the spy thread checks that its window still exists and is the
/// Shell_TrayWnd apps find first.
const SPY_WATCHDOG_MS: u32 = 500;
/// Asking apps again after taking the front back: at most this many times
/// per RETAKE_WINDOW (explorer could keep jumping ahead).
const MAX_RETAKE_RECOLLECTS: usize = 3;
const RETAKE_WINDOW: Duration = Duration::from_secs(60);

/// What the watchdog does after finding explorer's taskbar in front.
#[derive(Debug, PartialEq, Eq)]
pub enum FrontAction {
    /// Front taken back: ask apps for their icons again.
    Recollect,
    /// Could not take the front back (logged once until it recovers).
    Stuck,
    /// Nothing to do or say.
    Quiet,
}

/// The watchdog's memory about the spy's place in front.
#[derive(Debug, Default)]
pub struct FrontWatch {
    stuck_logged: bool,
    recollects: Vec<Instant>,
    /// Watchdog ticks in a row that could not take the front back.
    stuck_ticks: u32,
}

/// Ticks in a row behind explorer before the bar gives up on its icons
/// and shows the notification area button (a moment of it is normal while
/// explorer re-lays out its tray).
const STUCK_TICKS: u32 = 4;

impl FrontWatch {
    /// Explorer's taskbar was found first; `retaken`: after asserting the
    /// spy's place it is first again.
    pub fn lost(&mut self, retaken: bool, now: Instant) -> FrontAction {
        if !retaken {
            self.stuck_ticks = self.stuck_ticks.saturating_add(1);
            if std::mem::replace(&mut self.stuck_logged, true) {
                return FrontAction::Quiet;
            }
            return FrontAction::Stuck;
        }
        self.stuck_logged = false;
        self.stuck_ticks = 0;
        self.recollects.retain(|t| now.saturating_duration_since(*t) < RETAKE_WINDOW);
        if self.recollects.len() >= MAX_RETAKE_RECOLLECTS {
            return FrontAction::Quiet;
        }
        self.recollects.push(now);
        FrontAction::Recollect
    }

    /// The spy is first (again): a later failure is worth a log line.
    pub fn in_front(&mut self) {
        self.stuck_logged = false;
        self.stuck_ticks = 0;
    }

    /// Explorer has stayed in front long enough to count as stuck.
    pub fn stuck(&self) -> bool {
        self.stuck_ticks >= STUCK_TICKS
    }
}

fn set_stuck(on: bool) {
    if STUCK.swap(on, Ordering::SeqCst) != on {
        if on {
            crate::log!("tray: the top bar shows the notification area button until the spy is back in front");
        }
        changed();
    }
}

/// The watchdog's interval after this many calm ticks in a row (spy up,
/// in front, nothing pending: a minute at SPY_WATCHDOG_MS).
const CALM_TICKS: u32 = 120;
const SPY_WATCHDOG_CALM_MS: u32 = 2000;

/// How often the watchdog looks: SPY_WATCHDOG_MS while things move,
/// SPY_WATCHDOG_CALM_MS once the spy has kept its place a while.
#[derive(Debug, Default)]
pub struct Pace {
    calm: u32,
}

impl Pace {
    /// One watchdog tick; `calm` as above. Some(ms): set the timer to it.
    pub fn tick(&mut self, calm: bool) -> Option<u32> {
        if !calm {
            return self.kick().then_some(SPY_WATCHDOG_MS);
        }
        if self.calm < CALM_TICKS {
            self.calm += 1;
            if self.calm == CALM_TICKS {
                return Some(SPY_WATCHDOG_CALM_MS);
            }
        }
        None
    }

    /// Something moved (explorer restarted, a resync): count again. True
    /// if the watchdog was slowed down and must speed up.
    pub fn kick(&mut self) -> bool {
        std::mem::replace(&mut self.calm, 0) >= CALM_TICKS
    }

    /// The watchdog (re)starts at SPY_WATCHDOG_MS.
    pub fn reset(&mut self) {
        self.calm = 0;
    }
}

/// The first Shell_TrayWnd an app's FindWindow sees is not ours (and we are
/// not deliberately out of the way).
unsafe fn lost_front(spy: HWND) -> bool {
    if BYPASS.load(Ordering::SeqCst) || ASIDE_DEPTH.load(Ordering::SeqCst) > 0 {
        return false;
    }
    FindWindowW(w!("Shell_TrayWnd"), windows::core::PCWSTR::null()).is_ok_and(|first| first != spy)
}

/// Creates the spy window, puts it in front, and (after a moment) asks
/// running apps to add their icons again.
unsafe fn create_spy(hinst: windows::Win32::Foundation::HINSTANCE) -> Option<HWND> {
    let title = HSTRING::from(crate::taskbar::SPY_TITLE);
    let Ok(spy) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("Shell_TrayWnd"), &title, WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None) else {
        crate::log!("tray: cannot create the spy window");
        return None;
    };
    SPY.store(spy.0 as isize, Ordering::SeqCst);
    assert_top();
    hook_explorer();
    // Ask running apps to add their icons again (they reach us now), once
    // the dock and top bar have reserved their screen edges: those calls
    // step the spy aside for a moment, and icons added then would miss us.
    SetTimer(Some(spy), COLLECT_TIMER, COLLECT_DELAY_MS, None);
    Some(spy)
}

/// Keep our window ahead of explorer's in Z-order so FindWindow sees ours.
unsafe fn assert_top() {
    if BYPASS.load(Ordering::SeqCst) || ASIDE_DEPTH.load(Ordering::SeqCst) > 0 {
        return;
    }
    let spy = hwnd(SPY.load(Ordering::SeqCst));
    let _ = SetWindowPos(spy, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
}

/// Follow explorer's taskbar being created / shown (explorer only). Its
/// Z-order changes raise no WinEvent (EVENT_OBJECT_REORDER is not sent for
/// top-level windows), so the watchdog in `run` watches those.
unsafe fn hook_explorer() {
    let pid = crate::taskbar::explorer_pid();
    if pid == 0 || pid == EXPLORER_PID.swap(pid, Ordering::SeqCst) && !HOOKS.lock().map(|h| h.is_empty()).unwrap_or(true) {
        return;
    }
    if let Ok(mut hooks) = HOOKS.lock() {
        for h in hooks.drain(..) {
            let _ = UnhookWinEvent(HWINEVENTHOOK(h as *mut _));
        }
        let h = SetWinEventHook(EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, None, Some(explorer_event), pid, 0, WINEVENT_OUTOFCONTEXT);
        hooks.push(h.0 as isize);
    }
}

unsafe extern "system" fn explorer_event(_: HWINEVENTHOOK, _: u32, h: HWND, id_object: i32, _: i32, _: u32, _: u32) {
    if id_object != OBJID_WINDOW.0 || h.is_invalid() {
        return;
    }
    let mut buf = [0u16; 32];
    let n = GetClassNameW(h, &mut buf).max(0) as usize;
    if String::from_utf16_lossy(&buf[..n]) == "Shell_TrayWnd" {
        assert_top();
    }
}

/// Keeps one Shell_NotifyIcon message in the cache for the top bar.
/// (Copies the icon now, while the sender's handle is surely alive.)
unsafe fn cache(m: &TrayMsg) {
    if let Ok(mut r) = READDS.lock() {
        match m.message {
            NIM_DELETE => r.deleted(m.hwnd, Instant::now()),
            NIM_ADD => r.added(m.hwnd),
            _ => {}
        }
    }
    // Registrations are rare (MODIFYs are not): worth a line each.
    if matches!(m.message, NIM_ADD | NIM_DELETE | NIM_SETVERSION) {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd(m.hwnd as isize), Some(&mut pid));
        let what = match m.message {
            NIM_ADD => "add",
            NIM_DELETE => "delete",
            _ => "set version",
        };
        // Explorer's own icons with their GUID and tip (system icons, no
        // user data): which of them the tray keeps can be read off the log.
        let explorer = pid != 0 && pid == crate::taskbar::explorer_pid();
        let detail = match (explorer, m.guid) {
            (true, Some(g)) => format!(", explorer's {}, tip {:?}", stable_id(Some(g), None, 0).unwrap_or_default(), m.tip),
            (true, None) => format!(", explorer's, tip {:?}", m.tip),
            _ => String::new(),
        };
        crate::log!(
            "tray: {what} from pid {pid} (uid {}, flags 0x{:X}, callback 0x{:X}, version {}, guid {}{detail})",
            m.uid,
            m.flags,
            m.callback_msg,
            m.version,
            m.guid.is_some()
        );
    }
    let change = ENTRIES.lock().map(|mut l| apply(&mut l, m)).unwrap_or(Change::None);
    match change {
        Change::Icon(_, raw) => {
            let own = CopyIcon(HICON(raw as isize as *mut _)).map(|h| h.0 as isize).unwrap_or(0);
            let pixels = render_icon(own, ICON_PX.load(Ordering::SeqCst));
            let mut stale = own;
            if let Ok(mut l) = ENTRIES.lock() {
                // Found again by identity, not by the index `apply` saw:
                // the UI may have removed dead owners' entries in between
                // (audit A19). Gone meanwhile: our copy is destroyed below.
                if let Some(e) = l.iter_mut().find(|e| same_icon(e, m)) {
                    stale = std::mem::replace(&mut e.own_icon, own);
                    e.pixels = pixels.map(Arc::new);
                    e.gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
                }
            }
            if stale != 0 {
                let _ = DestroyIcon(HICON(stale as *mut _));
            }
        }
        Change::Removed(own) if own != 0 => {
            let _ = DestroyIcon(HICON(own as *mut _));
        }
        _ => {}
    }
    // Known only from a MODIFY (its ADD went to explorer): ask the app for
    // the whole registration now, so a click later reaches it.
    let partial = m.message == NIM_MODIFY
        && ENTRIES.lock().map(|l| l.iter().any(|e| same_icon(e, m) && partly_known(e))).unwrap_or(false);
    if partial && ask_owner_again(m.hwnd, false) {
        crate::log!("tray: an icon arrived without its registration (uid {}); asked its app to add it again", m.uid);
    }
    changed();
}

/// Our own tray icon's Shell_NotifyIcon call, which went to explorer
/// directly (with the spy stepped aside, see Aside::quiet) and succeeded:
/// shown in the top bar like any app's icon. Any thread.
pub fn mirror_own(m: &TrayMsg) {
    // (Not in a process without the tray, e.g. the guardian. Kept while
    // the mirror is off too: shown again once it is back on.)
    if USED.load(Ordering::SeqCst) {
        unsafe { cache(m) };
    }
}

fn log_uncached_copydata(cb_data: u32, bytes: &[u8]) {
    static LOGGED: AtomicU32 = AtomicU32::new(0);
    let message = proto::u32_at(bytes, 4);
    let size = proto::u32_at(bytes, 8);
    let kind = match message {
        Some(n @ NIM_ADD..=NIM_SETVERSION) => n,
        Some(_) => NIM_SETVERSION + 1,
        None => NIM_SETVERSION + 2,
    };
    let bit = 1 << kind;
    if LOGGED.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        crate::log!("tray: skipping cache for WM_COPYDATA cbData={cb_data} cbSize={size:?} dwMessage={message:?}");
    }
}

unsafe fn on_copydata(wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(cds) = (lp.0 as *const COPYDATASTRUCT).as_ref() else { return LRESULT(0) };
    if cds.dwData == 1 {
        // The whole buffer the sender passed (it may be longer than the
        // declared NOTIFYICONDATAW: parse_copydata reads the declared size).
        let bytes = if cds.lpData.is_null() {
            &[]
        } else {
            std::slice::from_raw_parts(cds.lpData.cast::<u8>(), cds.cbData as usize)
        };
        if let Some(m) = parse_copydata(cds.dwData, bytes) {
            cache(&m);
        } else {
            log_uncached_copydata(cds.cbData, bytes);
        }
    }
    if cds.dwData == 3 && !cds.lpData.is_null() && matches!(cds.cbData, 40 | 48) {
        let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
        // Where is my icon: the top bar's cell when a bar shows it.
        if let Some(q) = proto::parse_rect_query(cds.dwData, bytes) {
            let r = shown_rect(&q);
            if q.corner == proto::RECT_TOP_LEFT {
                crate::log!("tray: where is icon (window 0x{:X}, uid {}, {} bytes): {}", q.hwnd, q.uid, bytes.len(), if r.is_some() { "its top bar cell" } else { "not on a bar; explorer answers" });
            }
            if let Some(r) = r {
                return LRESULT(proto::pack_corner(q.corner, r));
            }
        }
    }
    // Forward everything (tray, AppBar dwData=0, icon rect dwData=3) and
    // hand explorer's answer back to the caller.
    let Some(real) = crate::taskbar::explorer_tray() else { return LRESULT(0) };
    let mut result = 0usize;
    let sent = SendMessageTimeoutW(real, WM_COPYDATA, wp, lp, SMTO_ABORTIFHUNG | SMTO_NORMAL, 3000, Some(&mut result));
    if sent.0 == 0 {
        // Explorer is stuck: after a couple of failures step aside so apps
        // talk to explorer directly instead of waiting on us each time.
        let n = FAILS.fetch_add(1, Ordering::SeqCst) + 1;
        crate::log!("tray: forward to explorer failed ({n} in a row)");
        if n >= FAIL_LIMIT && !BYPASS.swap(true, Ordering::SeqCst) {
            let spy = hwnd(SPY.load(Ordering::SeqCst));
            let _ = SetWindowPos(spy, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            let _ = SetWindowPos(spy, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            crate::log!("tray: explorer not answering; spy steps aside until it answers again");
            changed();
        }
    } else {
        FAILS.store(0, Ordering::SeqCst);
    }
    // Explorer may bring its taskbar ahead of the spy while it handles the
    // forward (an icon deleted re-lays out its tray). Apps re-adding an icon
    // send NIM_DELETE then NIM_ADD right after our answer: with explorer
    // first, the ADD went past us (seen for Slack, Notion, Docker, Drive and
    // Orca at once: delete logged, add never). Take the front back before
    // answering.
    let spy = hwnd(SPY.load(Ordering::SeqCst));
    if lost_front(spy) {
        assert_top();
    }
    LRESULT(result as isize)
}

unsafe fn rerender_all() {
    let px = ICON_PX.load(Ordering::SeqCst);
    // Each icon is copied under the lock and drawn from the copy: the UI
    // (dead owners) and a fresh NIM_MODIFY may destroy the entry's own
    // handle meanwhile, and a destroyed handle's value can be reused.
    // (CopyIcon sends no window message.)
    let copies: Vec<(isize, u64, isize)> = ENTRIES
        .lock()
        .map(|l| {
            l.iter()
                .filter(|e| e.own_icon != 0)
                .filter_map(|e| CopyIcon(HICON(e.own_icon as *mut _)).ok().map(|c| (e.own_icon, e.gen, c.0 as isize)))
                .collect()
        })
        .unwrap_or_default();
    for (own, gen, copy) in copies {
        let pixels = render_icon(copy, px);
        let _ = DestroyIcon(HICON(copy as *mut _));
        if let Ok(mut l) = ENTRIES.lock() {
            // Still the icon that was copied (not replaced or re-drawn since).
            if let Some(e) = l.iter_mut().find(|e| e.own_icon == own && e.gen == gen) {
                e.pixels = pixels.map(Arc::new);
                e.gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
            }
        }
    }
    changed();
}

/// Alpha from two renders (on black and on white): works for alpha icons
/// and for old mask-only icons alike. Result is premultiplied BGRA.
pub fn alpha_from_pair(black: &[u8], white: &[u8]) -> Vec<u8> {
    black
        .chunks_exact(4)
        .zip(white.chunks_exact(4))
        .flat_map(|(b, w)| {
            let a = 255 - (w[1] as i32 - b[1] as i32).clamp(0, 255);
            let a = a as u8;
            [b[0].min(a), b[1].min(a), b[2].min(a), a]
        })
        .collect()
}

/// The icon's own pixel size (square; 0 if unknown). A copied HICON holds
/// one image: rendering it at that size is exact, and our resampler then
/// scales it better than DrawIconEx's stretch would.
unsafe fn native_px(icon: HICON) -> u32 {
    let mut info = ICONINFO::default();
    if GetIconInfo(icon, &mut info).is_err() {
        return 0;
    }
    let mut bm = BITMAP::default();
    let size = std::mem::size_of::<BITMAP>() as i32;
    let n = if !info.hbmColor.is_invalid() && GetObjectW(info.hbmColor.into(), size, Some(&mut bm as *mut _ as *mut _)) != 0 {
        bm.bmWidth.max(bm.bmHeight)
    } else if !info.hbmMask.is_invalid() && GetObjectW(info.hbmMask.into(), size, Some(&mut bm as *mut _ as *mut _)) != 0 {
        // Monochrome: AND and XOR masks stacked.
        bm.bmWidth.max(bm.bmHeight / 2)
    } else {
        0
    };
    if !info.hbmColor.is_invalid() {
        let _ = DeleteObject(info.hbmColor.into());
    }
    if !info.hbmMask.is_invalid() {
        let _ = DeleteObject(info.hbmMask.into());
    }
    n.max(0) as u32
}

/// Renders our copy of an icon for a `px` x `px` slot: drawn at its own
/// size, then its visible content scaled to fill the slot evenly (fit.rs),
/// so padded and full-bleed icons look the same size. Done once per icon
/// change (the result is kept in the Entry).
unsafe fn render_icon(own: isize, px: u32) -> Option<Vec<u8>> {
    if own == 0 {
        return None;
    }
    let icon = HICON(own as *mut _);
    // Its own size, within reason (huge icons are drawn at 128 px).
    let n = match native_px(icon) {
        0 => px,
        n => n.clamp(8, 128),
    };
    let raw = draw_icon(icon, n)?;
    Some(fit::normalize(&raw, n, px))
}

/// Draws an icon at `px` x `px`, premultiplied BGRA.
pub(crate) unsafe fn draw_icon(icon: HICON, px: u32) -> Option<Vec<u8>> {
    let dc = CreateCompatibleDC(None);
    if dc.is_invalid() {
        return None;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: px as i32,
            biHeight: -(px as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let Ok(bmp) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(dc);
        return None;
    };
    let old = SelectObject(dc, bmp.into());
    let n = (px * px * 4) as usize;
    let pass = |fill: u8| -> Option<Vec<u8>> {
        std::ptr::write_bytes(bits as *mut u8, fill, n);
        DrawIconEx(dc, 0, 0, icon, px as i32, px as i32, 0, None, DI_NORMAL).ok()?;
        let _ = GdiFlush();
        Some(std::slice::from_raw_parts(bits as *const u8, n).to_vec())
    };
    let out = match (pass(0), pass(255)) {
        (Some(b), Some(w)) => Some(alpha_from_pair(&b, &w)),
        _ => None,
    };
    SelectObject(dc, old);
    let _ = DeleteObject(bmp.into());
    let _ = DeleteDC(dc);
    out
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => on_copydata(wp, lp),
        WM_TIMER if wp.0 == COLLECT_TIMER => {
            let _ = KillTimer(Some(h), COLLECT_TIMER);
            if ASIDE_DEPTH.load(Ordering::SeqCst) > 0 || (lost_front(h) && {
                assert_top();
                lost_front(h)
            }) {
                // Still stepped aside, or explorer's taskbar is the one apps
                // would answer to: try again in a moment (the watchdog logs
                // a front that cannot be taken back).
                SetTimer(Some(h), COLLECT_TIMER, RECOLLECT_MS, None);
            } else if BYPASS.load(Ordering::SeqCst) {
                // Stepped aside for a stuck explorer: apps would answer
                // explorer, not us. The recovery asks once it is back.
            } else {
                // The first ask after the spy was built goes to everyone;
                // later ones (after an edge reservation) only to apps
                // whose icons we miss.
                broadcast_taskbar_created(collect_ask(COLLECT_ARMED.swap(true, Ordering::SeqCst)));
            }
            LRESULT(0)
        }
        // Every Z-order change of the spy passes here, on its own thread:
        // while stepped aside, nothing may put it back on top.
        WM_WINDOWPOSCHANGING if ASIDE_DEPTH.load(Ordering::SeqCst) > 0 => {
            let wp_ = &mut *(lp.0 as *mut WINDOWPOS);
            if wp_.hwndInsertAfter == HWND_TOPMOST || wp_.hwndInsertAfter == HWND_TOP {
                wp_.flags |= SWP_NOZORDER;
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == COUNT_TIMER => {
            let _ = KillTimer(Some(h), COUNT_TIMER);
            let (all, shown) = ENTRIES
                .lock()
                .map(|l| (l.len(), l.iter().filter(|e| !e.hidden && e.pixels.is_some()).count()))
                .unwrap_or((0, 0));
            let first = if lost_front(h) { "explorer's taskbar" } else { "the spy" };
            crate::log!("tray: {all} icon(s) known ({shown} drawable) after asking apps; apps reach {first}");
            LRESULT(0)
        }
        WM_RERENDER => {
            rerender_all();
            LRESULT(0)
        }
        WM_RESYNC => {
            hook_explorer();
            assert_top();
            REASSERT_STEP.store(0, Ordering::SeqCst);
            SetTimer(Some(h), REASSERT_TIMER, REASSERT_DELAYS[0], None);
            LRESULT(0)
        }
        // Programs that "close the taskbar" send WM_CLOSE to every
        // Shell_TrayWnd; ours must not die from it (the watchdog would
        // rebuild it, but icons would be gone until apps re-add them).
        WM_CLOSE => {
            crate::log!("tray: WM_CLOSE sent to the spy; ignored");
            LRESULT(0)
        }
        WM_DESTROY => {
            crate::log!("tray: spy window destroyed");
            LRESULT(0)
        }
        m if m != 0 && m == WM_TASKBAR_CREATED.load(Ordering::SeqCst) => {
            // Explorer restarted (or our own broadcast): follow the new
            // explorer and take the Z-order back a few times while it
            // settles. Apps re-add their icons on their own.
            if !is_own_broadcast(wp, lp) {
                // A new explorer: it is answering again.
                FAILS.store(0, Ordering::SeqCst);
                if BYPASS.swap(false, Ordering::SeqCst) {
                    changed();
                }
            }
            hook_explorer();
            assert_top();
            REASSERT_STEP.store(0, Ordering::SeqCst);
            SetTimer(Some(h), REASSERT_TIMER, REASSERT_DELAYS[0], None);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == REASSERT_TIMER => {
            let _ = KillTimer(Some(h), REASSERT_TIMER);
            hook_explorer();
            assert_top();
            let next = REASSERT_STEP.fetch_add(1, Ordering::SeqCst) as usize + 1;
            // Once the new explorer settled and we are in front again, ask
            // apps once more: some re-added their icons to explorer before
            // we got back in front. Once per explorer.
            let pid = crate::taskbar::explorer_pid();
            let asked = ASKED_PID.load(Ordering::SeqCst);
            if next == 2 && pid != 0 && asked != pid && !BYPASS.load(Ordering::SeqCst) {
                broadcast_taskbar_created(Ask::Missing);
            }
            if let Some(&d) = REASSERT_DELAYS.get(next) {
                SetTimer(Some(h), REASSERT_TIMER, d, None);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    const OLD: Option<Duration> = Some(Duration::from_secs(3600));

    fn young(n: u64) -> Option<Duration> {
        Some(Duration::from_millis(n))
    }

    #[test]
    fn spy_starts_at_once_with_a_settled_explorer() {
        let t = Instant::now();
        let mut l = SpyLife::default();
        assert_eq!(l.tick(true, true, OLD, t), SpyAction::Create);
        assert!(l.up());
        assert_eq!(l.tick(true, true, None, ms(t, 500)), SpyAction::None);
    }

    #[test]
    fn spy_waits_for_explorer_at_sign_in() {
        // Early sign-in: no taskbar, then a young explorer with its
        // notification area child: still not ready until it announces.
        let t = Instant::now();
        let mut l = SpyLife::default();
        assert_eq!(l.tick(false, false, None, t), SpyAction::None);
        assert_eq!(l.tick(true, false, young(300), ms(t, 500)), SpyAction::None);
        assert_eq!(l.tick(true, true, young(3000), ms(t, 3000)), SpyAction::None);
        l.announced();
        assert_eq!(l.tick(true, true, young(3500), ms(t, 3500)), SpyAction::Create);
    }

    #[test]
    fn a_notification_area_alone_is_not_enough_after_a_restart() {
        // Measured: a spy built 1.5 s after TrayNotifyWnd appeared, before
        // explorer's TaskbarCreated, left explorer's tray broken.
        let t = Instant::now();
        let mut l = SpyLife::default();
        assert_eq!(l.tick(true, true, OLD, t), SpyAction::Create);
        assert_eq!(l.explorer_gone(), SpyAction::Destroy);
        assert!(!l.up());
        assert_eq!(l.explorer_gone(), SpyAction::None);
        assert_eq!(l.tick(false, false, None, ms(t, 100)), SpyAction::None);
        for n in [200u64, 1000, 2000, 5000, 15000] {
            assert_eq!(l.tick(true, true, young(n), ms(t, n)), SpyAction::None, "at {n} ms");
        }
        l.announced();
        assert_eq!(l.tick(true, true, young(16000), ms(t, 16000)), SpyAction::Create);
    }

    #[test]
    fn no_announcement_at_all_falls_back_late() {
        let t = Instant::now();
        let mut l = SpyLife::default();
        l.tick(true, true, OLD, t);
        l.explorer_gone();
        assert_eq!(l.tick(true, true, young(500), ms(t, 500)), SpyAction::None);
        assert_eq!(l.tick(true, true, SPY_FALLBACK.checked_add(Duration::from_millis(1)), ms(t, 21000)), SpyAction::CreateLate);
        // Age unknown: counted from when the taskbar was first seen.
        let mut l = SpyLife::default();
        l.explorer_gone();
        assert_eq!(l.tick(true, false, None, t), SpyAction::None);
        assert_eq!(l.tick(true, false, None, t + SPY_FALLBACK), SpyAction::CreateLate);
    }

    #[test]
    fn an_old_announcement_does_not_count_for_the_next_explorer() {
        let t = Instant::now();
        let mut l = SpyLife::default();
        l.tick(true, true, OLD, t);
        // TaskbarCreated while the spy is up (a stray one).
        l.announced();
        l.explorer_gone();
        assert_eq!(l.tick(true, true, young(100), ms(t, 100)), SpyAction::None);
    }

    #[test]
    fn repeated_restarts() {
        let t = Instant::now();
        let mut l = SpyLife::default();
        l.tick(true, true, OLD, t);
        for i in 1..=5u64 {
            let at = ms(t, i * 30_000);
            assert_eq!(l.explorer_gone(), SpyAction::Destroy);
            assert_eq!(l.tick(true, false, young(100), at), SpyAction::None);
            assert_eq!(l.tick(true, true, young(2000), ms(at, 2000)), SpyAction::None);
            l.announced();
            assert_eq!(l.tick(true, true, young(2500), ms(at, 2500)), SpyAction::Create);
        }
    }

    #[test]
    fn spy_notices_explorer_gone_itself() {
        let t = Instant::now();
        let mut l = SpyLife::default();
        l.tick(true, true, OLD, t);
        assert_eq!(l.tick(false, false, None, ms(t, 500)), SpyAction::Destroy);
        assert_eq!(l.tick(false, false, None, ms(t, 1000)), SpyAction::None);
    }

    #[test]
    fn our_broadcast_is_told_from_explorers() {
        assert!(is_own_broadcast(WPARAM(OWN_TAG), LPARAM(OWN_TAG as isize)));
        // Explorer's own broadcast carries no parameters.
        assert!(!is_own_broadcast(WPARAM(0), LPARAM(0)));
        assert!(!is_own_broadcast(WPARAM(OWN_TAG), LPARAM(0)));
    }

    #[test]
    fn an_icon_is_found_again_by_identity_not_index() {
        let mut l = Vec::new();
        apply(&mut l, &msg(NIM_ADD, 1, NIF_ICON, 0x10));
        let m2 = msg(NIM_ADD, 2, NIF_ICON, 0x20);
        assert_eq!(apply(&mut l, &m2), Change::Icon(1, 0x20));
        l.remove(0); // the UI dropped a dead owner's icon meanwhile
        assert_eq!(l.iter().position(|e| same_icon(e, &m2)), Some(0));
    }

    #[test]
    fn standard_tip_follows_version_and_showtip_modifications() {
        let mut icons = Vec::new();
        apply(&mut icons, &msg(NIM_ADD, 1, NIF_TIP, 0));
        assert!(icons[0].standard_tip());
        icons[0].version = 4;
        assert!(!icons[0].standard_tip());
        apply(&mut icons, &msg(NIM_MODIFY, 1, NIF_TIP | NIF_SHOWTIP, 0));
        assert!(icons[0].standard_tip());
        apply(&mut icons, &msg(NIM_MODIFY, 1, NIF_ICON, 1));
        assert!(icons[0].standard_tip());
        apply(&mut icons, &msg(NIM_MODIFY, 1, NIF_TIP, 0));
        assert!(!icons[0].standard_tip());
    }

    fn msg(message: u32, uid: u32, flags: u32, hicon: u32) -> TrayMsg {
        TrayMsg {
            signature: 0x3475_3423,
            message,
            hwnd: 0x100,
            uid,
            flags,
            callback_msg: 0x8001,
            hicon,
            tip: "tip".into(),
            version: 0,
            state: 0,
            state_mask: 0,
            guid: None,
        }
    }

    #[test]
    fn an_icon_known_only_from_a_modify_is_incomplete_until_its_app_adds_it() {
        let mut l = Vec::new();
        // Its ADD went to explorer; a tooltip MODIFY reached us.
        apply(&mut l, &msg(NIM_MODIFY, 3, NIF_TIP, 0));
        assert!(incomplete(&l[0]) && !l[0].added);
        // Asked again, the app deletes and adds it with its callback.
        apply(&mut l, &msg(NIM_DELETE, 3, 0, 0));
        apply(&mut l, &msg(NIM_ADD, 3, NIF_ICON | NIF_MESSAGE, 0x10));
        assert!(!incomplete(&l[0]) && l[0].added);
        assert_eq!(l[0].callback, 0x8001);
        // An app that adds without a callback is not asked again.
        apply(&mut l, &msg(NIM_ADD, 4, NIF_ICON, 0x20));
        assert!(incomplete(&l[1]) && l[1].added);
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_users_order_first_then_first_seen() {
        let all = ids(&["c", "a", "b", "d"]);
        let seen: std::collections::HashMap<String, u64> = [("a", 0), ("b", 1), ("c", 2), ("d", 3)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
        // No order: as first seen (c was re-added last but seen third).
        assert_eq!(rank(&all, &[], &seen), vec![1, 2, 0, 3]);
        // The user's order wins; the rest follow as seen.
        assert_eq!(rank(&all, &ids(&["d", "c"]), &seen), vec![3, 0, 1, 2]);
    }

    #[test]
    fn reorder_moves_within_a_side_and_across() {
        let r = ids(&["h1", "h2", "s1", "s2", "s3"]);
        let side = |x: &str| x.starts_with('h');
        // s3 to the front of the shown side.
        assert_eq!(reorder(&r, side, "s3", false, 0), ids(&["h1", "h2", "s3", "s1", "s2"]));
        // s1 to the end of the shown side.
        assert_eq!(reorder(&r, side, "s1", false, 9), ids(&["h1", "h2", "s2", "s3", "s1"]));
        // s2 behind the chevron, between h1 and h2.
        assert_eq!(reorder(&r, side, "s2", true, 1), ids(&["h1", "s2", "h2", "s1", "s3"]));
        // Onto an empty side.
        assert_eq!(reorder(&ids(&["s1", "s2"]), |_| false, "s2", true, 0), ids(&["s1", "s2"]));
    }

    #[test]
    fn the_spy_comes_back_after_explorer_answers_three_seconds_in_a_row() {
        let mut b = BypassWatch::default();
        for _ in 0..5 {
            assert!(!b.tick(true));
        }
        assert!(!b.tick(false), "a miss starts over");
        for _ in 0..5 {
            assert!(!b.tick(true));
        }
        assert!(b.tick(true));
    }

    #[test]
    fn apps_with_known_icons_are_not_asked_again() {
        let known = vec![10, 20, 30];
        assert!(!ask_missing(20, &known, 99), "Wallpaper Engine with its icon known");
        assert!(ask_missing(25, &known, 99));
        assert!(!ask_missing(99, &known, 99), "not ourselves");
        assert!(!ask_missing(0, &known, 99));
    }

    #[test]
    fn a_version_4_add_is_taken_without_its_setversion() {
        let mut l = Vec::new();
        let mut a = msg(NIM_ADD, 1, NIF_ICON | NIF_MESSAGE | NIF_SHOWTIP, 0x10);
        a.version = 4;
        apply(&mut l, &a);
        assert_eq!(l[0].version, 4);
        // Without NIF_SHOWTIP the field is uTimeout: not a version.
        let mut b = msg(NIM_ADD, 2, NIF_ICON | NIF_MESSAGE, 0x10);
        b.version = 4;
        apply(&mut l, &b);
        assert_eq!(l[1].version, 0);
        // NIF_SHOWTIP with the field 0 (the Epic Games Launcher): 4.
        let c = msg(NIM_ADD, 3, NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP, 0x10);
        apply(&mut l, &c);
        assert_eq!(l[2].version, 4);
    }

    #[test]
    fn a_delete_without_its_add_is_asked_again_twice_at_most() {
        let t = Instant::now();
        let mut r = Readds::default();
        // A delete with nobody having asked: the app removed its icon.
        r.deleted(1, t);
        assert!(r.due(ms(t, 5000)).is_empty());
        r.asked(ms(t, 10_000));
        r.deleted(2, ms(t, 10_100)); // its add arrives: nothing to do
        r.added(2);
        r.deleted(3, ms(t, 10_100)); // its add missed us
        assert!(r.due(ms(t, 10_500)).is_empty());
        assert_eq!(r.due(ms(t, 11_700)), vec![3]);
        assert_eq!(r.due(ms(t, 13_300)), vec![3]);
        assert!(r.due(ms(t, 15_000)).is_empty());
        assert!(r.waiting.is_empty());
    }

    #[test]
    fn an_app_is_asked_again_at_most_every_ten_seconds_unless_clicked() {
        let t = Instant::now();
        let mut a = Vec::new();
        assert!(readd_due(&mut a, 7, t, false));
        assert!(!readd_due(&mut a, 7, ms(t, 2000), false));
        assert!(readd_due(&mut a, 8, ms(t, 2000), false));
        assert!(readd_due(&mut a, 7, ms(t, 3000), true));
        assert!(!readd_due(&mut a, 7, ms(t, 12_000), false));
        assert!(readd_due(&mut a, 7, ms(t, 13_001), false));
        assert_eq!(a.iter().filter(|(o, _)| *o == 7).count(), 1);
    }

    #[test]
    fn tray_off_at_startup_builds_no_spy_and_asks_nobody() {
        // The bar starts with topbar.tray off (a fake bar window: the
        // notice to it just fails).
        start(HWND(0x7FFF_0010 as *mut _), 32, false);
        assert!(!STARTED.load(Ordering::SeqCst), "no spy thread");
        assert_eq!(SPY.load(Ordering::SeqCst), 0, "no Shell_TrayWnd");
        assert!(!COLLECT_ARMED.load(Ordering::SeqCst), "nobody asked for icons");
        assert_eq!(health(), Health::Off);
        assert!(needs_button(health()));
        assert!(snapshot().is_empty());
        // Back as it was (no bar: nothing starts).
        NOTIFY.store(0, Ordering::SeqCst);
        ENABLED.store(true, Ordering::SeqCst);
        spawn_if_wanted();
        assert!(!STARTED.load(Ordering::SeqCst));
    }

    #[test]
    fn explorer_keeps_the_icons_the_bar_does_not_show() {
        let g = |n: u8| Some(system_icon(n));
        let e = |guid: Option<[u8; 16]>, uid: u32, callback: u32| Entry { owner: 1, uid, callback, guid, added: true, ..Default::default() };
        // The family's bytes as the registry spells them (NotifyIconSettings
        // IconGuid {7820AE78-23E3-4229-82C1-E41CB67D5B9C} is the hotplug icon).
        assert_eq!(stable_id(g(0x78), None, 0).as_deref(), Some("guid:{7820AE78-23E3-4229-82C1-E41CB67D5B9C}"));
        assert_eq!(stable_id(g(0x74), None, 0).as_deref(), Some("guid:{7820AE74-23E3-4229-82C1-E41CB67D5B9C}"));
        // Volume, network, power: the status area shows them.
        for n in [0x73, 0x74, 0x75] {
            assert!(!explorer_icon_kept(&e(g(n), 0, 0x400)), "{n:X}");
        }
        // Safely Remove Hardware (as registered on build 26300: uID 1226,
        // callback 0x4CA), location, microphone, health, update, Meet Now.
        for n in [0x76, 0x77, 0x78, 0x81, 0x82, 0x83] {
            assert!(explorer_icon_kept(&e(g(n), 1226, 0x4CA)), "{n:X}");
        }
        // Unknown GUIDs, or none (older builds' hotplug by uID): kept.
        let other = guid_bytes(0x6DA6_8F06, 0x00F1, 0x4E6E, [0x81, 0x58, 0x7F, 0x1F, 0xFD, 0x4F, 0x9D, 0xB9]);
        assert!(explorer_icon_kept(&e(Some(other), 0, 0x403)));
        assert!(explorer_icon_kept(&e(None, 1226, 0x4CA)));
        // Registered with no callback: explorer's own UI handles it.
        assert!(!explorer_icon_kept(&e(Some(other), 100, 0)));
        // A volume GUID is left out even when its add was missed.
        assert!(!explorer_icon_kept(&Entry { added: false, ..e(g(0x73), 100, 0x400) }));
        // Only MODIFYs seen, but one named the callback: clickable, shown.
        assert!(explorer_icon_kept(&Entry { added: false, ..e(None, 1226, 0x4CA) }));
    }

    #[test]
    fn the_safely_remove_icon_is_dropped_unless_asked_for() {
        let e = |guid: Option<[u8; 16]>, uid: u32| Entry { owner: 1, uid, callback: 0x4CA, guid, added: true, ..Default::default() };
        let hotplug = e(Some(system_icon(0x78)), 1226);
        assert!(is_hotplug(&hotplug) && is_hotplug(&e(None, 1226)), "by GUID, or by uID without one");
        assert!(!hotplug_shown(&hotplug, false) && !hotplug_shown(&e(None, 1226), false));
        assert!(hotplug_shown(&hotplug, true), "the setting brings it back");
        // The rest of explorer's family and other icons are not touched.
        for n in [0x76, 0x77, 0x81, 0x82, 0x83] {
            assert!(hotplug_shown(&e(Some(system_icon(n)), 1226), false), "{n:X}");
        }
        assert!(hotplug_shown(&e(None, 100), false));
        // Explorer's policy is unchanged: the hotplug icon is still "kept"
        // by it (clickable); only the setting drops it.
        assert!(explorer_icon_kept(&hotplug));
    }

    #[test]
    fn an_explorer_icon_is_shown_only_once_it_can_be_clicked() {
        // Its NIM_ADD went past us: a MODIFY with picture and tip only
        // makes the entry, callback unknown.
        let mut l = Vec::new();
        let mut m = msg(NIM_MODIFY, 1226, NIF_ICON | NIF_TIP, 0x10);
        m.callback_msg = 0x4CA;
        apply(&mut l, &m);
        assert!(!l[0].added && l[0].callback == 0);
        assert!(!explorer_icon_kept(&l[0]), "a click would go nowhere: not shown");
        // A later message naming the callback (or explorer adding it
        // again when the spy asks every app): shown, and clickable.
        apply(&mut l, &msg(NIM_MODIFY, 1226, NIF_ICON | NIF_MESSAGE, 0x10));
        assert!(explorer_icon_kept(&l[0]) && !incomplete(&l[0]));
        let mut l = Vec::new();
        apply(&mut l, &msg(NIM_ADD, 1226, NIF_ICON | NIF_MESSAGE | NIF_TIP, 0x10));
        assert!(explorer_icon_kept(&l[0]) && !incomplete(&l[0]));
        // Whatever explorer sent, a shown icon always has a callback.
        for flags in [NIF_ICON, NIF_ICON | NIF_TIP, NIF_ICON | NIF_MESSAGE, NIF_ICON | NIF_STATE | NIF_GUID] {
            for message in [NIM_ADD, NIM_MODIFY] {
                let mut l = Vec::new();
                apply(&mut l, &msg(message, 7, flags, 0x10));
                assert!(!explorer_icon_kept(&l[0]) || !incomplete(&l[0]), "{message} {flags:X}");
            }
        }
    }

    #[test]
    fn our_own_icon_survives_the_mirror_going_off() {
        let e = |owner: u32, uid: u32| Entry { owner, uid, callback: 0x400, added: true, ..Default::default() };
        // Tray switched off, or the top bar off: other apps' icons go (a
        // later spy asks them again), ours stays for when it is back on.
        let mut list = vec![e(1, 1), e(7, 1), e(2, 5), e(7, 2)];
        let dropped = keep_own(&mut list, |owner| owner == 7);
        assert_eq!(list.iter().map(|x| (x.owner, x.uid)).collect::<Vec<_>>(), [(7, 1), (7, 2)]);
        assert_eq!(dropped.len(), 2);
        // Off again with only ours left: nothing to drop.
        assert!(keep_own(&mut list, |owner| owner == 7).is_empty());
        assert_eq!(list.len(), 2);
        // And the Missing ask never goes to our own process (it would
        // not answer it with its icon anyway).
        assert!(!ask_missing(7, &[], 7));
    }

    #[test]
    fn everyone_is_asked_once_then_only_apps_we_miss() {
        assert_eq!(collect_ask(false), Ask::Everyone, "the first spy knows no icon");
        assert_eq!(collect_ask(true), Ask::Missing, "every later spy (explorer restart, rebuilt, switched on again)");
    }

    #[test]
    fn the_watchdog_slows_down_when_calm_and_speeds_up_on_trouble() {
        let mut p = Pace::default();
        for _ in 1..CALM_TICKS {
            assert_eq!(p.tick(true), None);
        }
        assert_eq!(p.tick(true), Some(SPY_WATCHDOG_CALM_MS), "a calm minute");
        assert_eq!(p.tick(true), None, "stays slow");
        assert_eq!(p.tick(false), Some(SPY_WATCHDOG_MS), "trouble: quick again");
        assert_eq!(p.tick(false), None, "already quick");
        for _ in 0..CALM_TICKS {
            p.tick(true);
        }
        assert!(p.kick(), "explorer moved while slow: speed up");
        assert!(!p.kick());
    }

    #[test]
    fn stuck_only_after_a_few_ticks_behind_explorer() {
        let t0 = Instant::now();
        let mut f = FrontWatch::default();
        for _ in 1..STUCK_TICKS {
            f.lost(false, t0);
            assert!(!f.stuck(), "a moment behind is normal");
        }
        f.lost(false, t0);
        assert!(f.stuck());
        f.lost(true, t0);
        assert!(!f.stuck(), "taken back");
        for _ in 0..STUCK_TICKS {
            f.lost(false, t0);
        }
        f.in_front();
        assert!(!f.stuck());
        assert!(needs_button(Health::Off) && needs_button(Health::Stuck) && needs_button(Health::Bypass));
        assert!(!needs_button(Health::Up) && !needs_button(Health::Waiting));
        // The spy thread's own thread messages.
        assert_ne!(WM_TRAY_STATE, WM_EXPLORER_GONE);
    }

    #[test]
    fn front_watch_recollects_a_few_times_and_logs_stuck_once() {
        let t0 = Instant::now();
        let mut f = FrontWatch::default();
        // Taken back: ask apps again, but not forever in the same minute.
        for i in 0..MAX_RETAKE_RECOLLECTS as u64 {
            assert_eq!(f.lost(true, t0 + Duration::from_secs(i)), FrontAction::Recollect);
        }
        assert_eq!(f.lost(true, t0 + Duration::from_secs(10)), FrontAction::Quiet);
        // A minute later it may ask again.
        assert_eq!(f.lost(true, t0 + RETAKE_WINDOW + Duration::from_secs(1)), FrontAction::Recollect);
        // Cannot take it back: one log line, then quiet until it recovers.
        assert_eq!(f.lost(false, t0), FrontAction::Stuck);
        assert_eq!(f.lost(false, t0), FrontAction::Quiet);
        f.in_front();
        assert_eq!(f.lost(false, t0), FrontAction::Stuck);
    }

    #[test]
    fn add_modify_delete() {
        let mut l = Vec::new();
        assert_eq!(apply(&mut l, &msg(NIM_ADD, 1, NIF_ICON | NIF_MESSAGE | NIF_TIP, 0x10)), Change::Icon(0, 0x10));
        // Same handle number still re-renders (the picture may have changed).
        assert_eq!(apply(&mut l, &msg(NIM_MODIFY, 1, NIF_ICON, 0x10)), Change::Icon(0, 0x10));
        assert_eq!(apply(&mut l, &msg(NIM_MODIFY, 1, NIF_TIP, 0)), Change::None);
        // MODIFY for an unknown icon adds it (icons from before we started).
        assert_eq!(apply(&mut l, &msg(NIM_MODIFY, 2, NIF_ICON, 0x20)), Change::Icon(1, 0x20));
        assert_eq!(l.len(), 2);
        assert_eq!(apply(&mut l, &msg(NIM_DELETE, 1, 0, 0)), Change::Removed(0));
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].uid, 2);
    }

    #[test]
    fn guid_icons_are_separate() {
        let mut l = Vec::new();
        let mut a = msg(NIM_ADD, 0, NIF_ICON | NIF_GUID, 0x10);
        a.guid = Some([1; 16]);
        let mut b = msg(NIM_ADD, 0, NIF_ICON | NIF_GUID, 0x20);
        b.guid = Some([2; 16]);
        apply(&mut l, &a);
        apply(&mut l, &b);
        assert_eq!(l.len(), 2);
        let mut del = msg(NIM_DELETE, 0, NIF_GUID, 0);
        del.guid = Some([1; 16]);
        apply(&mut l, &del);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].guid, Some([2; 16]));
    }

    #[test]
    fn explicit_version_survives_updates_until_delete() {
        let mut l = Vec::new();
        let mut v = msg(NIM_SETVERSION, 1, 0, 0);
        v.version = 3;
        apply(&mut l, &v);
        let add = msg(NIM_ADD, 1, NIF_ICON | NIF_SHOWTIP, 0x10);
        apply(&mut l, &add);
        apply(&mut l, &msg(NIM_MODIFY, 1, NIF_ICON, 0x11));
        assert_eq!(l[0].version, 3);
        v.version = 0;
        apply(&mut l, &v);
        apply(&mut l, &add);
        assert_eq!(l[0].version, 0);
        apply(&mut l, &msg(NIM_DELETE, 1, 0, 0));
        apply(&mut l, &add);
        assert_eq!(l[0].version, 4);
    }

    #[test]
    fn hidden_state_and_version() {
        let mut l = Vec::new();
        apply(&mut l, &msg(NIM_ADD, 1, NIF_ICON, 0x10));
        let mut hide = msg(NIM_MODIFY, 1, NIF_STATE, 0);
        hide.state = NIS_HIDDEN;
        hide.state_mask = NIS_HIDDEN;
        apply(&mut l, &hide);
        assert!(l[0].hidden);
        let mut v = msg(NIM_SETVERSION, 1, 0, 0);
        v.version = 4;
        apply(&mut l, &v);
        assert_eq!(l[0].version, 4);
    }

    #[test]
    fn stable_ids() {
        let g = [0x78, 0x56, 0x34, 0x12, 0xBC, 0x9A, 0xF0, 0xDE, 1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(stable_id(Some(g), Some(r"C:\x.exe"), 5).as_deref(), Some("guid:{12345678-9ABC-DEF0-0102-030405060708}"));
        assert_eq!(stable_id(None, Some(r"C:\Program Files\App\App.EXE"), 7).as_deref(), Some(r"exe:c:\program files\app\app.exe#7"));
        // No GUID and no exe: nothing that could be saved (a class name
        // would collide across apps).
        assert_eq!(stable_id(None, None, 1), None);
        assert_eq!(stable_id(None, Some("  "), 1), None);
        // Same app, two icons: different ids.
        assert_ne!(stable_id(None, Some("a.exe"), 1), stable_id(None, Some("a.exe"), 2));
        // Session ids are told apart and differ per owner window.
        let sid = session_id(0x1234, 1);
        assert!(is_session_id(&sid));
        assert_ne!(sid, session_id(0x1235, 1));
        assert!(!is_session_id("exe:a.exe#1") && !is_session_id("guid:{0}"));
    }

    #[test]
    fn split_keeps_choices_and_caps_new_icons() {
        let ids: Vec<String> = (0..9).map(|i| format!("i{i}")).collect();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // Nothing chosen: the first 6 shown, the rest hidden.
        let out = split(&ids, &[], &[], 6);
        assert_eq!(out.iter().filter(|h| !**h).count(), 6);
        assert!(out[..6].iter().all(|h| !h) && out[6..].iter().all(|h| *h));
        // The user's choices win, and always-shown icons count toward the cap.
        let out = split(&ids, &s(&["i0"]), &s(&["i8"]), 6);
        assert!(out[0]);
        assert!(!out[8]);
        assert_eq!(out.iter().filter(|h| !**h).count(), 6);
        assert!(!out[5] && out[6] && out[7]);
        // More always-shown than the cap: all stay, new ones hide.
        let many = s(&["i1", "i2", "i3", "i4", "i5", "i6", "i7"]);
        let out = split(&ids, &[], &many, 6);
        assert!(out[0] && out[8]);
        assert_eq!(out.iter().filter(|h| !**h).count(), 7);
    }

    #[test]
    fn alpha_from_black_and_white() {
        // Opaque red, half-transparent white, fully transparent.
        let black = [0, 0, 255, 0, 128, 128, 128, 0, 0, 0, 0, 0];
        let white = [0, 0, 255, 0, 255, 255, 255, 0, 255, 255, 255, 0];
        let out = alpha_from_pair(&black, &white);
        assert_eq!(&out[0..4], &[0, 0, 255, 255]);
        assert_eq!(&out[4..8], &[128, 128, 128, 128]);
        assert_eq!(&out[8..12], &[0, 0, 0, 0]);
    }
}
