//! NopBar's own notification-area (tray) icon.
//!
//! Owned by a hidden message-only window on the dock's UI thread (class
//! `NopBarTrayIcon`), so its callbacks never mix with the dock window's
//! messages and nothing activates the dock window when an icon is clicked.
//! Message-only windows do not get broadcasts: the dock window's
//! TaskbarCreated handler calls `taskbar_created` for us.
//!
//! The icon registers with NOTIFYICON_VERSION_4 and the fixed GUID
//! `ICON_GUID`. Windows ties a GUID icon to the exe path that first
//! registered it: if that NIM_ADD fails (the exe moved, a dev build in
//! another folder), the icon falls back to hWnd + uID and says so in
//! dock.log.
//!
//! Our top bar mirrors tray icons through crate::tray's spy window, which
//! sits in front of explorer's taskbar and relays apps' Shell_NotifyIcon
//! calls. Ours do not go through it: relayed from this same process,
//! explorer turned down every NIM_ADD (GUID and hWnd + uID alike, while
//! the spy was in front; AppBar calls relayed that way were never honored
//! either, see taskbar::appbar_msg). So each call steps the spy aside and
//! talks to explorer directly, and what explorer accepted is handed to the
//! spy's cache (tray::mirror_own), so the icon still shows (and is
//! clicked) in our own top bar like any other app's icon.
//!
//! An add that fails (explorer still starting, or not answering) is tried
//! again a few times on a timer, besides on the next TaskbarCreated.
//!
//! A click (or Enter / Space) on the icon opens the system stats panel
//! (topbar/stats.rs) hanging from it, and closes it again; right-click
//! (Shift+F10, the menu key) shows our menu, Settings first. Where the icon
//! is: its cell in our top bar when it was clicked there (found by the
//! click's anchor point, topbar::own_tray_cell), else what
//! Shell_NotifyIconGetRect last said for the identity it was added with
//! (asked on the shell helper after each add and each click, never waited
//! for: explorer may be slow), else the click's anchor point.

use super::menu::{tray_menu, DockAction};
use super::*;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconGetRect, Shell_NotifyIconW, NIF_GUID, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICONIDENTIFIER, NOTIFYICON_VERSION_4, NOTIFY_ICON_MESSAGE,
};

/// The icon's identity: {B8DC297C-7493-4333-A5D2-20769F1EE9D8}. Never
/// change it: Windows keeps the user's "show / hide in the overflow" choice
/// under this GUID (for this exe path).
pub const ICON_GUID: windows::core::GUID = windows::core::GUID::from_u128(0xB8DC297C_7493_4333_A5D2_20769F1EE9D8);
/// uID of the fallback (non-GUID) registration.
const ICON_UID: u32 = 1;
/// Callback message of the icon (to our own window, so WM_APP + 1 is free).
const WM_TRAYICON: u32 = WM_APP + 1;
/// NIN_SELECT | NINF_KEY (not in the windows crate).
const NIN_KEYSELECT: u32 = windows::Win32::UI::Shell::NIN_SELECT + 1;
const NIN_SELECT: u32 = windows::Win32::UI::Shell::NIN_SELECT;
/// A mouse select on the icon this soon after a press on it closed the
/// stats panel is that same click (the press was eaten, the release
/// reached the tray): it leaves the panel closed. A keyboard select this
/// soon after one opened it is Enter's second NIN_KEYSELECT.
const RECLICK_MS: u32 = 500;
/// A press this close (px, each way) to a mouse select's anchor is on the
/// same icon, when explorer has not told where the icon is.
const SAME_ICON_PX: i32 = 16;
/// On the owner window: try a failed add again.
const RETRY_TIMER: usize = 1;
/// On the owner window: watches for the left button that closed the stats
/// panel to come up (Recent::press_closed), every RELEASE_POLL_MS.
const RELEASE_TIMER: usize = 2;
const RELEASE_POLL_MS: u32 = 15;
/// A closing press whose button is not seen up within this long is given
/// up: the next select is a new one.
const HELD_MAX_MS: u128 = 30_000;
/// When a failed add is tried again (after the previous try).
const RETRY_MS: [u32; 4] = [1500, 3000, 6000, 12000];
static RETRY_STEP: AtomicU32 = AtomicU32::new(0);
/// Explorer refuses the icon: it is shown in our own top bar only (put
/// into the tray spy's cache by hWnd + uID) while adds are tried again.
static BAR_ONLY: AtomicBool = AtomicBool::new(false);

/// The hidden owner window (0 until `init`).
static OWNER: AtomicIsize = AtomicIsize::new(0);
/// The user wants the icon (cfg.tray_icon).
static WANTED: AtomicBool = AtomicBool::new(false);
/// The icon is registered right now.
static ADDED: AtomicBool = AtomicBool::new(false);
/// Registered by GUID (false: by hWnd + uID).
static BY_GUID: AtomicBool = AtomicBool::new(true);
/// Our HICON (sized for the DPI in ICON_DPI, for the look in ICON_LOOK,
/// MarkLook::key) and what it was made for.
static ICON: AtomicIsize = AtomicIsize::new(0);
static ICON_DPI: AtomicU32 = AtomicU32::new(0);
static ICON_LOOK: AtomicU32 = AtomicU32::new(0);

/// The gnome mark for the notification area (assets/brand/tray, made by
/// scripts/brand-icons.py), by side in px: the brand's two colors (a brick
/// hat; the beard light grey for a dark taskbar - "white" -, dark grey for
/// a light one - "black").
const TRAY_WHITE: [(i32, &[u8]); 6] = [
    (16, include_bytes!("../../../assets/brand/tray/tray-white-16.png")),
    (20, include_bytes!("../../../assets/brand/tray/tray-white-20.png")),
    (24, include_bytes!("../../../assets/brand/tray/tray-white-24.png")),
    (32, include_bytes!("../../../assets/brand/tray/tray-white-32.png")),
    (40, include_bytes!("../../../assets/brand/tray/tray-white-40.png")),
    (48, include_bytes!("../../../assets/brand/tray/tray-white-48.png")),
];
const TRAY_BLACK: [(i32, &[u8]); 6] = [
    (16, include_bytes!("../../../assets/brand/tray/tray-black-16.png")),
    (20, include_bytes!("../../../assets/brand/tray/tray-black-20.png")),
    (24, include_bytes!("../../../assets/brand/tray/tray-black-24.png")),
    (32, include_bytes!("../../../assets/brand/tray/tray-black-32.png")),
    (40, include_bytes!("../../../assets/brand/tray/tray-black-40.png")),
    (48, include_bytes!("../../../assets/brand/tray/tray-black-48.png")),
];

/// Pure: the picture for a `light` taskbar at `px` a side: the smallest
/// one at least that big (drawn 1:1 at the usual DPIs), else the largest.
pub fn tray_png(light: bool, px: i32) -> (i32, &'static [u8]) {
    let set = if light { &TRAY_BLACK } else { &TRAY_WHITE };
    *set.iter().find(|(side, _)| *side >= px).unwrap_or(&set[set.len() - 1])
}

/// What the mark is drawn for: the taskbar it sits on (Windows mode,
/// SystemUsesLightTheme, not NopBar's own light / dark), or high
/// contrast, where it takes the scheme's text color (0xRRGGBB).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MarkLook {
    Dark,
    Light,
    Contrast(u32),
}

impl MarkLook {
    /// Pure: the look for Windows' settings `l`.
    pub(super) fn of(l: &crate::syslook::SysLook) -> Self {
        match (l.high_contrast, l.light) {
            (Some(h), _) => MarkLook::Contrast(((h.text.r as u32) << 16) | ((h.text.g as u32) << 8) | h.text.b as u32),
            (None, true) => MarkLook::Light,
            (None, false) => MarkLook::Dark,
        }
    }

    /// Pure: one number per look, for the icon cache (never 0: "none yet").
    pub(super) fn key(self) -> u32 {
        match self {
            MarkLook::Dark => 1,
            MarkLook::Light => 2,
            MarkLook::Contrast(rgb) => 0x0100_0000 | (rgb & 0xFF_FFFF),
        }
    }
}

fn mark_look() -> MarkLook {
    MarkLook::of(&crate::syslook::current())
}

/// Whether `owner` (a tray entry's window, crate::tray::Entry) is our own
/// icon's: the top bar draws that one in its own colors.
pub fn is_own(owner: u32) -> bool {
    let own = OWNER.load(Ordering::SeqCst);
    own != 0 && own as u32 == owner
}

/// Pure: an icon's 32-bit color bitmap (BGRA) recolored to `rgb`
/// (0xRRGGBB): the same shape and soft edges (alpha kept), one color.
/// An icon's color bitmap holds straight (not premultiplied) alpha: that
/// is what CreateIconFromResourceEx makes of our PNG and what DrawIconEx
/// (the shell's tray, our own via tray::draw_icon) blends, multiplying by
/// alpha itself, so an edge pixel keeps the full color and its alpha (it
/// is drawn as color * alpha / 255; premultiplying here would darken edges
/// twice). A fully transparent pixel becomes 0, 0, 0, 0: no color hides
/// in it for anything that reads the bits as premultiplied.
pub(super) fn tint_straight(bgra: &mut [u8], rgb: u32) {
    let (r, g, b) = ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);
    for p in bgra.chunks_exact_mut(4) {
        let c = if p[3] == 0 { [0, 0, 0] } else { [b, g, r] };
        p[..3].copy_from_slice(&c);
    }
}

/// How a select reached the icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Via {
    /// A click (NIN_SELECT).
    Mouse,
    /// Enter / Space (NIN_KEYSELECT).
    Key,
}

/// What a callback from the shell asks for.
#[derive(Debug, PartialEq)]
pub(super) enum TrayEvent {
    /// A mouse button went down on the icon: a new press (the one that
    /// closed the stats panel was eaten, so explorer never saw it).
    Press,
    /// Left click, or Enter / Space on the icon: the stats panel opens (or
    /// closes), anchored at this point (screen px).
    Stats(POINT, Via),
    /// Right click, or Shift+F10 / the menu key: our menu at this point
    /// (screen px).
    Menu(POINT),
}

/// Decodes a NOTIFYICON_VERSION_4 callback: LOWORD(lParam) is the event,
/// wParam holds the anchor point (x, y as signed 16-bit).
pub(super) fn decode(wp: usize, lp: isize) -> Option<TrayEvent> {
    let at = || POINT { x: (wp & 0xFFFF) as u16 as i16 as i32, y: ((wp >> 16) & 0xFFFF) as u16 as i16 as i32 };
    match (lp & 0xFFFF) as u32 {
        NIN_SELECT => Some(TrayEvent::Stats(at(), Via::Mouse)),
        NIN_KEYSELECT => Some(TrayEvent::Stats(at(), Via::Key)),
        WM_CONTEXTMENU => Some(TrayEvent::Menu(at())),
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => Some(TrayEvent::Press),
        _ => None,
    }
}

/// What a click on the icon does to the stats panel.
#[derive(Debug, PartialEq)]
pub(super) enum Toggle {
    Open,
    Close,
    /// The release of the press that just closed it, or Enter's second
    /// NIN_KEYSELECT after the one that just opened it: nothing (and that
    /// duplicate is used up).
    Leave,
}

/// What came just before a select, for telling a duplicate callback of the
/// same input from a new one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Recent {
    /// ms since a keyboard select opened the panel (not used up yet).
    pub key_opened: Option<u128>,
    /// The press that closed a panel the icon had opened in Windows' tray
    /// (not used up yet).
    pub press_closed: Option<Closed>,
}

/// A press outside that closed the stats panel, as a select sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Closed {
    pub button: crate::topbar::stats::Button,
    /// Where it was (screen px).
    pub at: POINT,
    /// ms since it closed the panel.
    pub ms: u128,
    /// ms since its button came up; None: not seen up yet (still held, or
    /// the select is that very release).
    pub up_ms: Option<u128>,
}

impl Closed {
    /// Pure: a mouse select now is this press's own release: a left press
    /// (another button's release is no select), not seen up yet (a long
    /// press included) or up just now.
    fn released_now(&self) -> bool {
        self.button == crate::topbar::stats::Button::Left
            && match self.up_ms {
                None => self.ms < HELD_MAX_MS,
                Some(up) => up < RECLICK_MS as u128,
            }
    }
}

/// Pure: a select by `via` on the icon (`icon`: where it is, screen px).
/// An open panel closes, unless this is Enter's second NIN_KEYSELECT (a
/// keyboard select just after a keyboard select opened it). A closed one
/// opens, unless this is the release of the press that just closed it (a
/// mouse select as the left press on the icon itself comes up: a press
/// elsewhere, another button's, or Enter / Space, opens it).
pub(super) fn toggle(open: bool, via: Via, icon: RECT, r: Recent) -> Toggle {
    let soon = |ms: u128| ms < RECLICK_MS as u128;
    if open {
        if via == Via::Key && r.key_opened.is_some_and(soon) { Toggle::Leave } else { Toggle::Close }
    } else if via == Via::Mouse && r.press_closed.is_some_and(|c| c.released_now() && inside(icon, c.at, 2)) {
        Toggle::Leave
    } else {
        Toggle::Open
    }
}

/// `p` is in `r` grown by `pad` px each way.
fn inside(r: RECT, p: POINT, pad: i32) -> bool {
    p.x >= r.left - pad && p.x < r.right + pad && p.y >= r.top - pad && p.y < r.bottom + pad
}

/// Pure: where the icon of a select anchored at `at` is: what explorer
/// last said (`known`) when the anchor is on it (the icon may have moved
/// since: then a 1 px rect at the anchor).
pub(super) fn icon_at(known: Option<RECT>, at: POINT) -> RECT {
    match known {
        Some(r) if inside(r, at, 1) => r,
        _ => RECT { left: at.x, top: at.y, right: at.x + 1, bottom: at.y + 1 },
    }
}

thread_local! {
    /// When a keyboard select last opened the stats panel (Recent::key_opened).
    static KEY_OPENED: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
    /// The press that closed it (Recent::press_closed): when it closed it,
    /// what it was, and when its button came up (RELEASE_TIMER).
    static PRESS_CLOSED: std::cell::Cell<Option<PressRecord>> = const { std::cell::Cell::new(None) };
}

#[derive(Clone, Copy)]
struct PressRecord {
    closed: std::time::Instant,
    press: crate::topbar::stats::ClosingPress,
    up: Option<std::time::Instant>,
}

/// The logical left button (swapped buttons applied) is down now.
fn left_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
    // GetAsyncKeyState reads the physical buttons.
    let vk = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 { VK_RBUTTON } else { VK_LBUTTON };
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

/// RELEASE_TIMER: notes when the closing left press comes up; ends when
/// it has, or after HELD_MAX_MS, or once the record is gone.
unsafe fn release_tick(hwnd: HWND) {
    let mut r = PRESS_CLOSED.with(|c| c.get());
    let done = match r.as_mut() {
        Some(p) if p.up.is_none() && p.closed.elapsed().as_millis() < HELD_MAX_MS => {
            if !left_down() {
                p.up = Some(std::time::Instant::now());
            }
            p.up.is_some()
        }
        Some(p) if p.up.is_none() => {
            r = None;
            true
        }
        _ => true,
    };
    PRESS_CLOSED.with(|c| c.set(r));
    if done {
        let _ = KillTimer(Some(hwnd), RELEASE_TIMER);
    }
}

/// Where explorer last said the icon is (screen px), and the request that
/// said so (later requests only replace it).
type Rect4 = (i32, i32, i32, i32);
static ICON_RECT: std::sync::Mutex<(u64, Option<Rect4>)> = std::sync::Mutex::new((0, None));
/// The last rect request made.
static RECT_ASKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Pure: the answer to request `seq` replaces the one from request
/// `stored` (an older answer coming in late never does).
pub(super) fn newer(stored: u64, seq: u64) -> bool {
    seq >= stored
}

fn store_rect(seq: u64, r: Option<Rect4>) {
    if let Ok(mut g) = ICON_RECT.lock() {
        if newer(g.0, seq) {
            *g = (seq, r);
        }
    }
}

/// What explorer last said (None: not asked yet, not in its tray, or an
/// empty rect).
fn known_rect() -> Option<RECT> {
    let r = ICON_RECT.lock().ok()?.1?;
    Some(RECT { left: r.0, top: r.1, right: r.2, bottom: r.3 })
}

pub(crate) fn companion_news_anchor(allow_cursor: bool) -> Option<(crate::companion::SlotAnchor, RECT)> {
    use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONEAREST};
    let icon = ADDED.load(Ordering::SeqCst).then(known_rect).flatten();
    let (rect, monitor) = if let Some(icon) = icon {
        let handle = unsafe { MonitorFromRect(&icon, MONITOR_DEFAULTTONEAREST) };
        let monitor = crate::monitors::info(handle.0 as isize)?;
        (tray_anchor(icon, monitor.rect, monitor.work).0, monitor)
    } else {
        if !allow_cursor {
            return None;
        }
        let monitor = crate::monitors::under_cursor().or_else(crate::monitors::primary)?;
        let mut cursor = POINT::default();
        unsafe { GetCursorPos(&mut cursor) }.ok()?;
        (RECT { left: cursor.x, top: cursor.y, right: cursor.x + 1, bottom: cursor.y + 1 }, monitor)
    };
    Some((crate::companion::SlotAnchor { rect, dpi: monitor.scale() }, monitor.work))
}

/// Pure: what the stats panel hangs from for our icon at `icon` (screen
/// px) in Windows' notification area, on monitor `mon` with work area
/// `work`, and whether it opens upward (the icon in the monitor's lower
/// half). The icon is moved out to the taskbar's edge (where the work area
/// ends): one in the overflow flyout, which closes as the panel opens,
/// hangs from the taskbar like the others.
pub(super) fn tray_anchor(icon: RECT, mon: RECT, work: RECT) -> (RECT, bool) {
    let up = (icon.top + icon.bottom) / 2 > (mon.top + mon.bottom) / 2;
    let mut a = icon;
    if up && work.bottom < mon.bottom {
        a.top = a.top.max(work.bottom);
        a.bottom = a.bottom.max(a.top + 1);
    } else if !up && work.top > mon.top {
        a.bottom = a.bottom.min(work.top);
        a.top = a.top.min(a.bottom - 1);
    }
    (a, up)
}

/// Creates the owner window and shows the icon if `on`. Once, at startup,
/// on the dock's UI thread.
pub(super) unsafe fn init(on: bool) {
    let Ok(hinst) = GetModuleHandleW(None) else { return };
    let class = w!("NopBarTrayIcon");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: class,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    match CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("NopBar"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None) {
        Ok(h) => OWNER.store(h.0 as isize, Ordering::SeqCst),
        Err(e) => {
            crate::log!("trayicon: cannot create the owner window: {e}");
            return;
        }
    }
    set_enabled(on);
}

/// cfg.tray_icon changed (or startup): add or remove the icon.
pub(super) unsafe fn set_enabled(on: bool) {
    WANTED.store(on, Ordering::SeqCst);
    if on && !ADDED.load(Ordering::SeqCst) {
        RETRY_STEP.store(0, Ordering::SeqCst);
        add_or_retry();
    } else if !on && ADDED.load(Ordering::SeqCst) {
        remove();
    }
}

/// Explorer restarted (its icons are gone), or our tray spy asked apps to
/// register again: put the icon back. An icon that is still there is kept
/// (MODIFY), so nothing flickers.
pub(super) unsafe fn taskbar_created() {
    if !WANTED.load(Ordering::SeqCst) || OWNER.load(Ordering::SeqCst) == 0 {
        return;
    }
    if ADDED.load(Ordering::SeqCst) {
        let mut d = data(BY_GUID.load(Ordering::SeqCst));
        fill(&mut d);
        if notify(NIM_MODIFY, &d).is_ok() {
            set_version(&d);
            return;
        }
        // Not there any more (a new explorer): add it again.
        ADDED.store(false, Ordering::SeqCst);
    }
    RETRY_STEP.store(0, Ordering::SeqCst);
    add_or_retry();
}

/// Adds the icon; if that fails, tries again a few times on a timer.
unsafe fn add_or_retry() {
    let owner = HWND(OWNER.load(Ordering::SeqCst) as *mut _);
    if owner.is_invalid() {
        return;
    }
    if add() {
        let _ = KillTimer(Some(owner), RETRY_TIMER);
        return;
    }
    // What the user sees while the taskbar is hidden: the top bar.
    bar_only(true);
    let step = RETRY_STEP.fetch_add(1, Ordering::SeqCst) as usize;
    match RETRY_MS.get(step) {
        Some(&ms) => {
            SetTimer(Some(owner), RETRY_TIMER, ms, None);
        }
        None => crate::log!("trayicon: giving up until the taskbar is created again"),
    }
}

/// Shows (true) or drops (false) the top-bar-only copy of the icon.
unsafe fn bar_only(on: bool) {
    if BAR_ONLY.swap(on, Ordering::SeqCst) == on {
        return;
    }
    let mut d = data(false);
    if on {
        fill(&mut d);
        crate::tray::mirror_own(&tray_msg(NIM_ADD.0, &d));
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        crate::tray::mirror_own(&tray_msg(NIM_SETVERSION.0, &d));
        crate::log!("trayicon: explorer refuses the icon; shown in the top bar only, still trying");
    } else {
        crate::tray::mirror_own(&tray_msg(NIM_DELETE.0, &d));
    }
}

/// Displays changed: a new DPI on the primary monitor needs a new picture.
pub(super) unsafe fn displays_changed() {
    picture_changed();
}

/// Windows' light / dark or high contrast changed: the taskbar's icon
/// follows it.
pub(super) unsafe fn look_changed() {
    picture_changed();
}

/// A new DPI or taskbar mode: the shell gets the new picture.
unsafe fn picture_changed() {
    if icon_dpi() == ICON_DPI.load(Ordering::SeqCst) && mark_look().key() == ICON_LOOK.load(Ordering::SeqCst) {
        return;
    }
    if BAR_ONLY.load(Ordering::SeqCst) {
        let mut d = data(false);
        d.uFlags |= NIF_ICON;
        d.hIcon = icon();
        crate::tray::mirror_own(&tray_msg(NIM_MODIFY.0, &d));
    }
    if !ADDED.load(Ordering::SeqCst) {
        return;
    }
    let mut d = data(BY_GUID.load(Ordering::SeqCst));
    d.uFlags |= NIF_ICON;
    d.hIcon = icon();
    let _ = notify(NIM_MODIFY, &d);
}

/// Removes the icon (quit, logoff, the setting turned off). Always safe.
pub(super) unsafe fn remove() {
    let owner = HWND(OWNER.load(Ordering::SeqCst) as *mut _);
    let _ = KillTimer(Some(owner), RETRY_TIMER);
    // A closing press still watched goes with the icon.
    let _ = KillTimer(Some(owner), RELEASE_TIMER);
    PRESS_CLOSED.with(|c| c.set(None));
    bar_only(false);
    if !ADDED.swap(false, Ordering::SeqCst) {
        return;
    }
    let _lock = Lock::take();
    let d = data(BY_GUID.load(Ordering::SeqCst));
    if let Err(e) = notify(NIM_DELETE, &d) {
        crate::log!("trayicon: NIM_DELETE failed ({e})");
    }
    // Gone (or explorer is, and the icon with it): nothing to clean up.
    let _ = std::fs::remove_file(state_file());
}

// ---- cleanup from another process -------------------------------------------
//
// A dock that crashed or was killed leaves its icon in explorer (a GUID icon
// outlives its window). Its guardian, and uninstall, remove it from outside
// using what the dock recorded when it registered: which identity it used
// (GUID, or hWnd + uID) and which dock it was (pid + process start time).
// Recording, add / remove and cleanup all run under one named mutex, so an
// old dock's guardian never removes a newer dock's icon: once the newer dock
// has registered, the record names it, not the dead one.

/// The registration on record (`<app dir>\trayicon.state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub pid: u32,
    /// Process start time (FILETIME as u64): tells a reused pid apart.
    pub started: u64,
    pub by_guid: bool,
    pub hwnd: isize,
    pub uid: u32,
}

impl Record {
    pub fn to_line(&self) -> String {
        format!("{} {} {} {} {}", self.pid, self.started, if self.by_guid { "guid" } else { "hwnd" }, self.hwnd, self.uid)
    }

    pub fn parse(s: &str) -> Option<Record> {
        let p: Vec<&str> = s.split_whitespace().collect();
        if p.len() != 5 {
            return None;
        }
        let by_guid = match p[2] {
            "guid" => true,
            "hwnd" => false,
            _ => return None,
        };
        Some(Record { pid: p[0].parse().ok()?, started: p[1].parse().ok()?, by_guid, hwnd: p[3].parse().ok()?, uid: p[4].parse().ok()? })
    }
}

/// Whose icon a cleanup is for.
#[derive(Clone, Copy, Debug)]
pub enum Gone {
    /// This dock exited (its guardian): (pid, process start time).
    Dock(u32, u64),
    /// Whichever dock registered, if it no longer runs (uninstall).
    Any,
}

/// Whether a cleanup for `gone` may remove the icon on `rec`. `alive` says
/// whether a (pid, start time) process still runs.
pub fn should_remove(rec: &Record, gone: Gone, alive: impl Fn(u32, u64) -> bool) -> bool {
    match gone {
        Gone::Dock(pid, started) => rec.pid == pid && rec.started == started,
        Gone::Any => !alive(rec.pid, rec.started),
    }
}

fn state_file() -> std::path::PathBuf {
    crate::config::app_dir().join("trayicon.state")
}

/// The named mutex around registering, removing and cleaning up.
struct Lock(HANDLE);

impl Lock {
    /// Waits up to 5 s; without it, goes on anyway (a stuck holder must not
    /// keep the dock from quitting).
    fn take() -> Option<Lock> {
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        unsafe {
            let h = CreateMutexW(None, false, w!("Local\\NopBar.TrayIcon")).ok()?;
            let r = WaitForSingleObject(h, 5000);
            // WAIT_OBJECT_0, or WAIT_ABANDONED (the holder died): ours now.
            if r.0 == 0 || r.0 == 0x80 {
                Some(Lock(h))
            } else {
                crate::log!("trayicon: lock not taken in 5 s; going on without it");
                let _ = windows::Win32::Foundation::CloseHandle(h);
                None
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::ReleaseMutex(self.0);
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// A process's start time (FILETIME as u64; 0 if unknown).
pub fn started(process: HANDLE) -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    if unsafe { GetProcessTimes(process, &mut c, &mut e, &mut k, &mut u) }.is_err() {
        return 0;
    }
    ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64
}

/// Process `pid` still runs and is the one that started at `at`.
fn alive(pid: u32, at: u64) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let same = started(h) == at;
        let _ = windows::Win32::Foundation::CloseHandle(h);
        same
    }
}

/// From another process (the guardian after its dock exited, uninstall):
/// removes the icon a dead dock left, by the identity it registered with,
/// and only if that dock is the one `gone` names.
pub fn remove_stale(gone: Gone) {
    let _lock = Lock::take();
    let Some(rec) = crate::config::read_capped(&state_file(), 64 << 10).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|s| Record::parse(&s)) else { return };
    if !should_remove(&rec, gone, alive) {
        return;
    }
    let mut d = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, ..Default::default() };
    if rec.by_guid {
        d.uFlags = NIF_GUID;
        d.guidItem = ICON_GUID;
    } else {
        d.hWnd = HWND(rec.hwnd as *mut _);
        d.uID = rec.uid;
    }
    let removed = notify(NIM_DELETE, &d).is_ok();
    let _ = std::fs::remove_file(state_file());
    let how = if rec.by_guid { "GUID" } else { "hWnd + uID" };
    crate::log!("trayicon: cleaned up after dock {} ({how}; removed: {removed})", rec.pid);
}

/// Records this dock's registration (called under the lock).
fn record(by_guid: bool) {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId};
    let rec = Record {
        pid: unsafe { GetCurrentProcessId() },
        started: started(unsafe { GetCurrentProcess() }),
        by_guid,
        hwnd: OWNER.load(Ordering::SeqCst),
        uid: ICON_UID,
    };
    let path = state_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, rec.to_line()) {
        crate::log!("trayicon: cannot record the registration: {e}");
    }
}

/// Adds the icon (GUID first, then hWnd + uID). True if it is there.
unsafe fn add() -> bool {
    if OWNER.load(Ordering::SeqCst) == 0 || !WANTED.load(Ordering::SeqCst) {
        return false;
    }
    let _lock = Lock::take();
    // GUID first; a leftover from a crashed run blocks NIM_ADD, so remove
    // that and try once more.
    for by_guid in [true, false] {
        let mut d = data(by_guid);
        fill(&mut d);
        let first = notify(NIM_ADD, &d);
        let ok = first.is_ok() || (notify(NIM_DELETE, &data(by_guid)).is_ok() && notify(NIM_ADD, &d).is_ok());
        if ok {
            BY_GUID.store(by_guid, Ordering::SeqCst);
            ADDED.store(true, Ordering::SeqCst);
            record(by_guid);
            set_version(&d);
            // Where explorer puts it, ready for the first click: asked
            // after the calls the icon needs, on the optional lane (never
            // in front of them or any later waited call).
            refresh_icon_rect();
            if BAR_ONLY.load(Ordering::SeqCst) {
                crate::log!("trayicon: explorer took the icon");
                // The same identity was just added for real: keep it.
                if by_guid {
                    bar_only(false);
                } else {
                    BAR_ONLY.store(false, Ordering::SeqCst);
                }
            }
            return true;
        }
        // Said once per refusal spell, not on every retry.
        if !BAR_ONLY.load(Ordering::SeqCst) {
            let why = first.err().map(|e| e.to_string()).unwrap_or_default();
            let how = if by_guid { "the GUID" } else { "hWnd + uID" };
            crate::log!("trayicon: NIM_ADD with {how} failed ({why}); the call reached {}", shell_target());
        }
    }
    false
}

/// For the log: which window our Shell_NotifyIcon reaches with the spy
/// stepped aside (explorer's taskbar, its notification area child, or not).
unsafe fn shell_target() -> String {
    let _aside = crate::tray::Aside::quiet();
    let Ok(h) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) else { return "no Shell_TrayWnd".into() };
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, Some(&mut pid));
    if pid == std::process::id() {
        return "our own tray spy (not stepped aside)".into();
    }
    let notify_area = FindWindowExW(Some(h), None, w!("TrayNotifyWnd"), PCWSTR::null()).is_ok();
    let visible = IsWindowVisible(h).as_bool();
    let explorer = crate::taskbar::explorer_pid();
    format!("Shell_TrayWnd of pid {pid} (explorer {explorer}), visible {visible}, TrayNotifyWnd {notify_area}")
}

/// Everything that names the icon: the GUID, or hWnd + uID.
fn data(by_guid: bool) -> NOTIFYICONDATAW {
    let mut d = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(OWNER.load(Ordering::SeqCst) as *mut _),
        ..Default::default()
    };
    if by_guid {
        d.uFlags = NIF_GUID;
        d.guidItem = ICON_GUID;
    } else {
        d.uID = ICON_UID;
    }
    d
}

/// Adds the callback, picture and tooltip.
unsafe fn fill(d: &mut NOTIFYICONDATAW) {
    d.uFlags |= NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    d.uCallbackMessage = WM_TRAYICON;
    d.hIcon = icon();
    let tip: Vec<u16> = "NopBar".encode_utf16().collect();
    d.szTip[..tip.len()].copy_from_slice(&tip);
}

unsafe fn set_version(d: &NOTIFYICONDATAW) {
    let mut v = *d;
    v.uFlags = v.uFlags & NIF_GUID;
    v.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    if let Err(e) = notify(NIM_SETVERSION, &v) {
        crate::log!("trayicon: NIM_SETVERSION failed ({e})");
    }
}

/// Why a Shell_NotifyIcon call did not succeed.
enum NotifyError {
    /// Explorer said no (the helper thread's last error then; often 0).
    Refused(windows::Win32::Foundation::WIN32_ERROR),
    /// Not answered within the deadline (it still runs, in order).
    NoAnswer,
}

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotifyError::Refused(e) => write!(f, "refused, error {}", e.0),
            NotifyError::NoAnswer => write!(f, "no answer in time"),
        }
    }
}

/// Shell_NotifyIconW is a synchronous round trip to explorer: it runs on
/// the shell-call helper with a deadline (audit A03). Not done in time:
/// reported as failed (it still happens, in order, when explorer answers).
/// It goes to explorer directly, with our tray spy stepped aside (see the
/// module notes), and what explorer accepted is mirrored into the spy.
fn notify(msg: NOTIFY_ICON_MESSAGE, d: &NOTIFYICONDATAW) -> std::result::Result<(), NotifyError> {
    /// The request with its own copy of the icon: a call that waits in the
    /// queue (explorer stuck) must not use an HICON a DPI change destroyed
    /// meanwhile. The copy goes when the request is done or dropped.
    struct Data(NOTIFYICONDATAW);
    // Plain data: our window handle, GUID, text and an icon this owns.
    unsafe impl Send for Data {}
    impl Drop for Data {
        fn drop(&mut self) {
            if !self.0.hIcon.is_invalid() {
                unsafe {
                    let _ = DestroyIcon(self.0.hIcon);
                }
            }
        }
    }
    let mut copy = *d;
    if (copy.uFlags & NIF_ICON).0 != 0 && !copy.hIcon.is_invalid() {
        match unsafe { CopyIcon(copy.hIcon) } {
            Ok(h) => copy.hIcon = h,
            Err(_) => {
                // No copy: send it without the picture rather than a
                // handle that may die before it is used.
                copy.uFlags &= !NIF_ICON;
                copy.hIcon = Default::default();
            }
        }
    } else {
        copy.hIcon = Default::default();
    }
    let data = Data(copy);
    crate::taskbar::shell::call("tray icon", crate::taskbar::shell::UI_DEADLINE, move || {
        let data = data;
        let ok = {
            // Quiet: a plain Aside asks apps for their icons again after,
            // and that broadcast brings us back here.
            let _aside = crate::tray::Aside::quiet();
            unsafe { Shell_NotifyIconW(msg, &data.0).as_bool() }
        };
        if !ok {
            return Err(NotifyError::Refused(unsafe { windows::Win32::Foundation::GetLastError() }));
        }
        // Still holding our copy of the icon: the spy copies it now.
        crate::tray::mirror_own(&tray_msg(msg.0, &data.0));
        Ok(())
    })
    .unwrap_or(Err(NotifyError::NoAnswer))
}

/// The call as the spy would have received it (crate::tray::proto).
fn tray_msg(message: u32, d: &NOTIFYICONDATAW) -> crate::tray::proto::TrayMsg {
    let flags = d.uFlags.0;
    let tip_len = d.szTip.iter().position(|&c| c == 0).unwrap_or(d.szTip.len());
    let g = d.guidItem;
    let mut guid = [0u8; 16];
    guid[..4].copy_from_slice(&g.data1.to_le_bytes());
    guid[4..6].copy_from_slice(&g.data2.to_le_bytes());
    guid[6..8].copy_from_slice(&g.data3.to_le_bytes());
    guid[8..].copy_from_slice(&g.data4);
    crate::tray::proto::TrayMsg {
        signature: 0x3475_3423,
        message,
        // Handles as the 32-bit values the shell passes along.
        hwnd: d.hWnd.0 as usize as u32,
        uid: d.uID,
        flags,
        callback_msg: d.uCallbackMessage,
        hicon: d.hIcon.0 as usize as u32,
        tip: String::from_utf16_lossy(&d.szTip[..tip_len]),
        version: unsafe { d.Anonymous.uVersion },
        state: d.dwState.0,
        state_mask: d.dwStateMask.0,
        guid: (flags & NIF_GUID.0 != 0).then_some(guid),
    }
}

/// The taskbar draws tray icons for the primary monitor's DPI.
fn icon_dpi() -> u32 {
    crate::monitors::primary().map(|m| m.dpi).filter(|&d| d > 0).unwrap_or(96)
}

/// The gnome mark at the small-icon size for the DPI (SM_CXSMICON for that
/// DPI, which is what LoadIconMetric(LIM_SMALL) uses: LoadIconMetric lives
/// only in comctl32 v6, which this exe does not bind to), white or black
/// for the taskbar (tray_png), or in high contrast's text color. If that
/// picture cannot be made: the exe's own icon (resource id 1,
/// res/nopbar.rc).
unsafe fn icon() -> HICON {
    let (dpi, look) = (icon_dpi(), mark_look());
    let old = ICON.load(Ordering::SeqCst);
    if old != 0 && ICON_DPI.load(Ordering::SeqCst) == dpi && ICON_LOOK.load(Ordering::SeqCst) == look.key() {
        return HICON(old as *mut _);
    }
    let px = windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16);
    let (_, png) = tray_png(look == MarkLook::Light, px);
    // PNG image data is a valid icon resource (version 3.0) since Vista.
    let made = CreateIconFromResourceEx(png, true, 0x0003_0000, px, px, LR_DEFAULTCOLOR).ok();
    // High contrast: the white mark recolored (the white one if that fails).
    let made = match (made, look) {
        (Some(white), MarkLook::Contrast(rgb)) => match recolor(white, rgb) {
            Some(h) => {
                let _ = DestroyIcon(white);
                Some(h)
            }
            None => Some(white),
        },
        (made, _) => made,
    };
    let made = made.or_else(|| {
        let hinst = GetModuleHandleW(None).ok()?;
        LoadImageW(Some(hinst.into()), PCWSTR(1 as *const u16), IMAGE_ICON, px, px, LR_DEFAULTCOLOR).ok().map(|h| HICON(h.0))
    });
    let Some(h) = made else {
        crate::log!("trayicon: cannot make the icon at {px} px");
        return HICON(old as *mut _);
    };
    ICON.store(h.0 as isize, Ordering::SeqCst);
    ICON_DPI.store(dpi, Ordering::SeqCst);
    ICON_LOOK.store(look.key(), Ordering::SeqCst);
    // The shell copies the picture on every call: the old one can go.
    if old != 0 {
        let _ = DestroyIcon(HICON(old as *mut _));
    }
    HICON(h.0)
}

/// A copy of `icon` with every pixel in color `rgb` (0xRRGGBB) and its
/// alpha kept (tint_straight: straight alpha, as icons hold it, with the
/// mask kept): the mark in another color. None if GDI cannot read or make
/// it.
unsafe fn recolor(icon: HICON, rgb: u32) -> Option<HICON> {
    use windows::Win32::Graphics::Gdi::{CreateDIBSection, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, GetIconInfo, ICONINFO};
    let mut info = ICONINFO::default();
    GetIconInfo(icon, &mut info).ok()?;
    let mut out = None;
    let mut bm = BITMAP::default();
    let size = std::mem::size_of::<BITMAP>() as i32;
    if !info.hbmColor.is_invalid() && GetObjectW(info.hbmColor.into(), size, Some(&mut bm as *mut _ as *mut _)) != 0 && bm.bmWidth > 0 && bm.bmHeight > 0 {
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        if GetDIBits(dc, info.hbmColor, 0, h as u32, Some(pixels.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS) == h {
            tint_straight(&mut pixels, rgb);
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            if let Ok(bmp) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                if !bits.is_null() {
                    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
                    let ii = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: info.hbmMask, hbmColor: bmp };
                    // The icon gets its own copies of both bitmaps.
                    out = CreateIconIndirect(&ii).ok();
                }
                let _ = DeleteObject(bmp.into());
            }
        }
        ReleaseDC(None, dc);
    }
    if !info.hbmColor.is_invalid() {
        let _ = DeleteObject(info.hbmColor.into());
    }
    if !info.hbmMask.is_invalid() {
        let _ = DeleteObject(info.hbmMask.into());
    }
    out
}

/// Our menu, drawn like the dock's (crate::popup), at `at`.
unsafe fn show_menu(at: POINT) {
    let Some((gfx, theme)) = with_dock(|d| (d.gfx.clone(), d.theme.clone())) else { return };
    let mut at = at;
    if at.x == 0 && at.y == 0 {
        let _ = GetCursorPos(&mut at);
    }
    let mon = windows::Win32::Graphics::Gdi::MonitorFromPoint(at, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST);
    let info = crate::monitors::info(mon.0 as isize);
    let bar = crate::topbar::own_tray_cell(at);
    let scale = bar.as_ref().map(|b| b.3).unwrap_or_else(|| info.as_ref().map(|m| m.scale()).unwrap_or(1.0));
    let (gfx, theme) = bar.as_ref().map(|b| (b.0.clone(), b.1.clone())).unwrap_or((gfx, theme));
    // A tray at the top (our bar) opens downward, one at the bottom upward.
    let upper = bar.as_ref().map(|b| !b.4).unwrap_or_else(|| info.is_some_and(|m| at.y < (m.rect.top + m.rect.bottom) / 2));
    // Right to left it hangs to the left of the pointer.
    let align = match (upper, crate::i18n::rtl()) {
        (true, false) => crate::popup::Align::TopLeft,
        (true, true) => crate::popup::Align::TopRight,
        (false, _) => crate::popup::Align::Above,
    };
    let (rows, actions) = tray_menu(crate::notes::running());
    let out = crate::popup::track(crate::popup::Request {
        gfx: &gfx,
        theme: &theme,
        items: rows,
        at,
        align,
        scale,
        titles: crate::popup::Titles { anchor: bar.as_ref().map(|b| b.2), ..Default::default() },
        on_slide: None,
        drag_ids: 0..0,
        on_space: None,
        on_pick: None,
        hangs: false,
    });
    if let crate::popup::Outcome::Picked(id) = out {
        if let Some(a) = actions.get((id as usize).wrapping_sub(1)).cloned() {
            run(a);
        }
    }
}

/// The unified bar cell shares the shell icon's left/right/keyboard behavior.
pub fn bar_click(at: POINT, right: bool, keyboard: bool) {
    unsafe {
        match bar_event(at, right, keyboard) {
            TrayEvent::Menu(at) => show_menu(at),
            TrayEvent::Stats(at, via) => click_stats(at, via),
            TrayEvent::Press => unreachable!(),
        }
    }
}

fn bar_event(at: POINT, right: bool, keyboard: bool) -> TrayEvent {
    if right { TrayEvent::Menu(at) }
    else { TrayEvent::Stats(at, if keyboard { Via::Key } else { Via::Mouse }) }
}

#[cfg(test)]
#[test]
fn unified_bar_selects_the_same_events_as_the_shell_icon() {
    let at = POINT { x: -123, y: 456 };
    let packed = (at.x as u16 as usize) | ((at.y as u16 as usize) << 16);
    for (right, keyboard, event) in [(false, false, NIN_SELECT), (false, true, NIN_KEYSELECT), (true, false, WM_CONTEXTMENU), (true, true, WM_CONTEXTMENU)] {
        assert_eq!(Some(bar_event(at, right, keyboard)), decode(packed, event as isize));
    }
}

/// Asks explorer where it shows the icon (Shell_NotifyIconGetRect for the
/// identity it was added with) on the shell helper's optional lane (one
/// waiting at most: the newest), the tray spy stepped aside (notify()),
/// without waiting: the answer goes to
/// ICON_RECT for the clicks after it. Explorer being slow never holds the
/// UI thread, and a late answer only ever updates that cache (it opens or
/// moves nothing). Not in explorer's tray (top bar only): nothing known.
fn refresh_icon_rect() {
    let seq = RECT_ASKED.fetch_add(1, Ordering::SeqCst) + 1;
    if !ADDED.load(Ordering::SeqCst) {
        store_rect(seq, None);
        return;
    }
    struct Id(NOTIFYICONIDENTIFIER);
    // Plain data: our window handle and the GUID.
    unsafe impl Send for Id {}
    let mut id = NOTIFYICONIDENTIFIER { cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32, ..Default::default() };
    if BY_GUID.load(Ordering::SeqCst) {
        id.guidItem = ICON_GUID;
    } else {
        id.hWnd = HWND(OWNER.load(Ordering::SeqCst) as *mut _);
        id.uID = ICON_UID;
    }
    let id = Id(id);
    crate::taskbar::shell::post_latest("tray icon rect", move || {
        let id = id;
        let _aside = crate::tray::Aside::quiet();
        let r = unsafe { Shell_NotifyIconGetRect(&id.0) }.ok().filter(|r| r.right > r.left && r.bottom > r.top);
        store_rect(seq, r.map(|r| (r.left, r.top, r.right, r.bottom)));
    });
}

/// A select on the icon (anchor point `at`, by `via`): the stats panel
/// opens hanging from it, or closes (toggle).
unsafe fn click_stats(at: POINT, via: Via) {
    use crate::topbar::stats;
    let at = anchor_point(at);
    let ms = |t: std::time::Instant| t.elapsed().as_millis();
    let press_closed = PRESS_CLOSED.with(|c| c.get()).map(|r| Closed { button: r.press.button, at: r.press.at, ms: ms(r.closed), up_ms: r.up.map(ms) });
    let recent = Recent { key_opened: KEY_OPENED.with(|c| c.get()).map(ms), press_closed };
    let icon = match crate::topbar::own_tray_cell(at) {
        Some((_, _, cell, _, _)) => cell,
        None => match known_rect() {
            Some(r) if inside(r, at, 1) => r,
            _ => RECT { left: at.x - SAME_ICON_PX, top: at.y - SAME_ICON_PX, right: at.x + SAME_ICON_PX + 1, bottom: at.y + SAME_ICON_PX + 1 },
        },
    };
    match toggle(stats::is_open(), via, icon, recent) {
        Toggle::Close => stats::cancel(),
        Toggle::Leave => {
            // That duplicate is used up: the next select is a new one.
            KEY_OPENED.with(|c| c.set(None));
            PRESS_CLOSED.with(|c| c.set(None));
        }
        Toggle::Open => {
            PRESS_CLOSED.with(|c| c.set(None));
            let opened = std::time::Instant::now();
            KEY_OPENED.with(|c| c.set((via == Via::Key).then_some(opened)));
            // Until it closes (the panel's loop).
            let in_explorer = open_stats(at);
            // Closed by a press (after this opening): in Windows' tray,
            // that press's release still comes as a select. Our own bar
            // eats it (the panel had the press), so nothing comes there.
            // Its button and place come from the press's own message; a
            // left one is watched until it comes up (a long press too).
            let since = opened.elapsed().as_millis().min(u32::MAX as u128) as u32;
            let press = stats::closed_by_press(since).filter(|_| in_explorer);
            let left = press.is_some_and(|p| p.button == stats::Button::Left);
            PRESS_CLOSED.with(|c| c.set(press.map(|press| PressRecord { closed: std::time::Instant::now(), press, up: None })));
            if left {
                SetTimer(Some(HWND(OWNER.load(Ordering::SeqCst) as *mut _)), RELEASE_TIMER, RELEASE_POLL_MS, None);
            }
        }
    }
}

/// A select's anchor point; (0, 0) (not given) is the pointer.
unsafe fn anchor_point(at: POINT) -> POINT {
    let mut p = at;
    if p.x == 0 && p.y == 0 {
        let _ = GetCursorPos(&mut p);
    }
    p
}

/// The stats panel under (or over) the icon: its cell in our top bar, its
/// place in Windows' tray as explorer last told it, or the anchor point,
/// in that order. True if it hung in Windows' tray (not our bar).
unsafe fn open_stats(at: POINT) -> bool {
    // Its "Details" open or shut as it was left (the setting), and kept
    // again if that changed while it was open.
    let details = with_dock(|d| d.cfg.topbar.stats_details).unwrap_or(false);
    let keep = |left: Option<bool>| {
        if let Some(open) = left {
            let _ = with_dock(|d| d.update_config(move |c| c.topbar.stats_details = open));
        }
    };
    if let Some((gfx, theme, cell, scale, up)) = crate::topbar::own_tray_cell(at) {
        keep(crate::topbar::stats::show(&gfx, &theme, cell, scale, up, details));
        return false;
    }
    let Some((gfx, theme)) = with_dock(|d| (d.gfx.clone(), d.theme.clone())) else {
        crate::log!("trayicon: dock busy, stats not opened");
        return false;
    };
    let icon = icon_at(known_rect(), at);
    // For the next time (the icon may have moved); not waited for.
    refresh_icon_rect();
    let mid = POINT { x: (icon.left + icon.right) / 2, y: (icon.top + icon.bottom) / 2 };
    let mon = windows::Win32::Graphics::Gdi::MonitorFromPoint(mid, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST);
    let Some(info) = crate::monitors::info(mon.0 as isize) else { return false };
    let (button, up) = tray_anchor(icon, info.rect, info.work);
    keep(crate::topbar::stats::show(&gfx, &theme, button, info.scale(), up, details));
    true
}

/// Runs a picked action through the dock's own menu actions.
unsafe fn run(a: DockAction) {
    if super::views::with_primary(|d| d.run_action(a.clone())).is_none() {
        crate::log!("trayicon: dock busy, menu action {a:?} dropped");
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_TIMER && wp.0 == RETRY_TIMER {
        let _ = KillTimer(Some(hwnd), RETRY_TIMER);
        if WANTED.load(Ordering::SeqCst) && !ADDED.load(Ordering::SeqCst) {
            add_or_retry();
        }
        return LRESULT(0);
    }
    if msg == WM_TIMER && wp.0 == RELEASE_TIMER {
        release_tick(hwnd);
        return LRESULT(0);
    }
    if msg == WM_TRAYICON {
        match decode(wp.0, lp.0) {
            // A new press: the closing one's release is past.
            Some(TrayEvent::Press) => PRESS_CLOSED.with(|c| c.set(None)),
            Some(TrayEvent::Stats(at, via)) => click_stats(at, via),
            Some(TrayEvent::Menu(at)) => show_menu(at),
            None => {}
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_follows_high_contrast_too() {
        use crate::syslook::{HighContrast, SysLook};
        let c = |r, g, b| crate::theme::Rgba { r, g, b, a: 255 };
        assert_eq!(MarkLook::of(&SysLook { light: false, ..Default::default() }), MarkLook::Dark);
        assert_eq!(MarkLook::of(&SysLook { light: true, ..Default::default() }), MarkLook::Light);
        // High contrast wins over light / dark: the scheme's text color.
        let hc = |text| HighContrast { window: c(0, 0, 0), text, highlight: c(0x1A, 0xEB, 0xFF), highlight_text: c(0, 0, 0), gray_text: crate::syslook::GRAY_TEXT };
        let aquatic = SysLook { light: true, high_contrast: Some(hc(c(0xFF, 0xFF, 0x00))), ..Default::default() };
        assert_eq!(MarkLook::of(&aquatic), MarkLook::Contrast(0xFFFF00));
        // Every look is its own cache key (a new scheme makes a new icon).
        let keys = [MarkLook::Dark, MarkLook::Light, MarkLook::Contrast(0), MarkLook::Contrast(0xFFFFFF), MarkLook::Contrast(0xFFFF00)].map(MarkLook::key);
        for (i, a) in keys.iter().enumerate() {
            assert_ne!(*a, 0, "0 is \"none yet\"");
            assert!(keys[i + 1..].iter().all(|b| b != a), "{keys:?}");
        }
        // The mark recolored (straight alpha, an icon's): the color where
        // it shows, its alpha kept; a transparent pixel stays all zero
        // (also when its color bits were not: the PNG's are white).
        let mut px = vec![255, 255, 255, 255, 255, 255, 255, 128, 0, 0, 0, 0, 255, 255, 255, 0];
        tint_straight(&mut px, 0x12_34_56);
        assert_eq!(px, vec![0x56, 0x34, 0x12, 255, 0x56, 0x34, 0x12, 128, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn tray_picture_follows_the_taskbar_and_the_size() {
        // A dark taskbar gets the white mark, a light one the black.
        assert_eq!(tray_png(false, 16).1, TRAY_WHITE[0].1);
        assert_eq!(tray_png(true, 16).1, TRAY_BLACK[0].1);
        // Each DPI's small-icon size has its own picture (96..288 DPI).
        for (px, want) in [(16, 16), (20, 20), (24, 24), (32, 32), (40, 40), (48, 48)] {
            assert_eq!(tray_png(false, px).0, want);
        }
        // In between: the next one up; past the largest: the largest.
        assert_eq!(tray_png(true, 18).0, 20);
        assert_eq!(tray_png(true, 64).0, 48);
        // Real PNGs.
        for (_, b) in TRAY_WHITE.iter().chain(TRAY_BLACK.iter()) {
            assert_eq!(&b[..4], &[0x89, b'P', b'N', b'G']);
        }
    }

    #[test]
    fn callbacks_decode_like_version_4() {
        // Left click and keyboard select: the stats panel, at the anchor
        // point (wParam, signed: monitors left of the primary).
        let wp = (0xFFF6usize) | (200usize << 16);
        assert_eq!(decode(wp, NIN_SELECT as isize | (1 << 16)), Some(TrayEvent::Stats(POINT { x: -10, y: 200 }, Via::Mouse)));
        assert_eq!(decode(0, NIN_KEYSELECT as isize), Some(TrayEvent::Stats(POINT { x: 0, y: 0 }, Via::Key)));
        // Right click / context menu key: our menu at the anchor.
        assert_eq!(decode(wp, WM_CONTEXTMENU as isize), Some(TrayEvent::Menu(POINT { x: -10, y: 200 })));
        assert_eq!(decode(0, (WM_USER + 5) as isize), None, "no balloons: a balloon click is nothing");
        // Plain mouse traffic is ignored (v4 sends NIN_SELECT after it),
        // but a press going down marks a new click.
        assert_eq!(decode(0, WM_LBUTTONUP as isize), None);
        assert_eq!(decode(0, WM_LBUTTONDOWN as isize), Some(TrayEvent::Press));
        assert_eq!(decode(0, WM_MBUTTONDOWN as isize), Some(TrayEvent::Press));
        assert_eq!(decode(0, WM_RBUTTONUP as isize), None);
        assert_eq!(decode(0, WM_MOUSEMOVE as isize), None);
    }

    #[test]
    fn a_click_toggles_the_stats_panel() {
        let icon = RECT { left: 100, top: 1040, right: 124, bottom: 1064 };
        let on = POINT { x: 110, y: 1050 };
        let none = Recent::default();
        assert_eq!(toggle(false, Via::Mouse, icon, none), Toggle::Open);
        assert_eq!(toggle(false, Via::Key, icon, none), Toggle::Open);
        assert_eq!(toggle(true, Via::Mouse, icon, none), Toggle::Close, "a click while open closes it");
        assert_eq!(toggle(true, Via::Key, icon, none), Toggle::Close);
        // The press on the icon closed it (eaten); its release reached the
        // tray as a click: the panel stays closed.
        let closer = Recent { press_closed: Some(left(on, 100, None)), ..none };
        assert_eq!(toggle(false, Via::Mouse, icon, closer), Toggle::Leave);
        // Up long ago: a new click.
        assert_eq!(toggle(false, Via::Mouse, icon, Recent { press_closed: Some(left(on, 900, Some(800))), ..none }), Toggle::Open);
        // A select while open is never swallowed by a press record.
        assert_eq!(toggle(true, Via::Mouse, icon, closer), Toggle::Close);
        // Enter's second NIN_KEYSELECT right after the first: left open;
        // a click then still closes it.
        let enter = Recent { key_opened: Some(50), ..none };
        assert_eq!(toggle(true, Via::Key, icon, enter), Toggle::Leave);
        assert_eq!(toggle(true, Via::Mouse, icon, enter), Toggle::Close, "a click is not Enter's duplicate");
        assert_eq!(toggle(false, Via::Key, icon, enter), Toggle::Open, "closed by Escape meanwhile: opens again");
    }

    /// A press outside closed the panel somewhere else than on the icon:
    /// the next click on the icon, or Enter on it, opens it at once.
    #[test]
    fn a_press_elsewhere_or_a_key_select_is_not_the_closing_click() {
        let icon = RECT { left: 100, top: 1040, right: 124, bottom: 1064 };
        let elsewhere = Recent { press_closed: Some(left(POINT { x: 600, y: 300 }, 100, None)), ..Default::default() };
        assert_eq!(toggle(false, Via::Mouse, icon, elsewhere), Toggle::Open);
        // Pressed on the icon, then Enter / Space: a keyboard select opens.
        let on = Recent { press_closed: Some(left(POINT { x: 110, y: 1050 }, 100, None)), ..Default::default() };
        assert_eq!(toggle(false, Via::Key, icon, on), Toggle::Open);
    }

    /// A left press closing the panel at `at`, `ms` ago, its button up
    /// `up` ms ago (None: not seen up yet).
    fn left(at: POINT, ms: u128, up: Option<u128>) -> Closed {
        Closed { button: crate::topbar::stats::Button::Left, at, ms, up_ms: up }
    }

    /// Only the closing left press's own release is swallowed: a middle or
    /// right press on the icon closes the panel, and the next left click
    /// opens it at once.
    #[test]
    fn another_buttons_close_never_eats_the_next_left_click() {
        use crate::topbar::stats::Button;
        let icon = RECT { left: 100, top: 1040, right: 124, bottom: 1064 };
        let on = POINT { x: 110, y: 1050 };
        for button in [Button::Middle, Button::Right] {
            for up in [None, Some(0), Some(300)] {
                let r = Recent { press_closed: Some(Closed { button, at: on, ms: 300, up_ms: up }), ..Default::default() };
                assert_eq!(toggle(false, Via::Mouse, icon, r), Toggle::Open, "{button:?} up {up:?}");
            }
        }
    }

    /// A long press on the icon (held past RECLICK_MS) that closed the
    /// panel: its late release is still that press, not a new click; once
    /// its button has been up a while, a click is a new one.
    #[test]
    fn a_long_press_is_still_one_click() {
        let icon = RECT { left: 100, top: 1040, right: 124, bottom: 1064 };
        let on = POINT { x: 110, y: 1050 };
        let held = |ms, up| Recent { press_closed: Some(left(on, ms, up)), ..Default::default() };
        assert_eq!(toggle(false, Via::Mouse, icon, held(2_000, None)), Toggle::Leave, "held 2 s, its release");
        assert_eq!(toggle(false, Via::Mouse, icon, held(2_000, Some(20))), Toggle::Leave, "up just now");
        assert_eq!(toggle(false, Via::Mouse, icon, held(3_000, Some(1_000))), Toggle::Open, "up a while ago: new");
        assert_eq!(toggle(false, Via::Mouse, icon, held(HELD_MAX_MS, None)), Toggle::Open, "never seen up: given up");
    }

    /// The selects as they come, with click_stats' bookkeeping (a Leave
    /// uses the record up, an open clears the press one). Each event: how
    /// it came, and where a press closed the open panel just before it.
    fn run(events: &[(Via, Option<POINT>)], icon: RECT) -> Vec<Toggle> {
        let mut open = false;
        let mut r = Recent::default();
        let mut out = Vec::new();
        for &(via, closed_by) in events {
            if let Some(p) = closed_by {
                assert!(open, "only an open panel closes");
                open = false;
                r = Recent { key_opened: None, press_closed: Some(left(p, 10, None)) };
            }
            let t = toggle(open, via, icon, r);
            match t {
                Toggle::Open => {
                    open = true;
                    r = Recent { key_opened: (via == Via::Key).then_some(10), press_closed: None };
                }
                Toggle::Close => open = false,
                Toggle::Leave => r = Recent::default(),
            }
            out.push(t);
        }
        out
    }

    #[test]
    fn two_clicks_in_a_row_are_two_clicks() {
        let icon = RECT { left: 100, top: 1040, right: 124, bottom: 1064 };
        let on = POINT { x: 112, y: 1052 };
        // Open; a click on the icon (its press closes, its release is
        // swallowed); a second, separate click soon after opens it again.
        let got = run(&[(Via::Mouse, None), (Via::Mouse, Some(on)), (Via::Mouse, None)], icon);
        assert_eq!(got, vec![Toggle::Open, Toggle::Leave, Toggle::Open]);
        // Enter (two NIN_KEYSELECTs) opens it once; a press outside, then a
        // keyboard select: opens again.
        let got = run(&[(Via::Key, None), (Via::Key, None), (Via::Key, Some(POINT { x: 700, y: 200 }))], icon);
        assert_eq!(got, vec![Toggle::Open, Toggle::Leave, Toggle::Open]);
        // Enter's duplicate is used up: another select closes it.
        let got = run(&[(Via::Key, None), (Via::Key, None), (Via::Key, None)], icon);
        assert_eq!(got, vec![Toggle::Open, Toggle::Leave, Toggle::Close]);
    }

    #[test]
    fn the_icon_rect_is_used_only_where_the_click_is() {
        let known = RECT { left: 100, top: 1040, right: 124, bottom: 1064 };
        assert_eq!(icon_at(Some(known), POINT { x: 110, y: 1050 }), known);
        // The icon moved (or another anchor): the anchor itself.
        let far = POINT { x: 400, y: 1050 };
        assert_eq!(icon_at(Some(known), far), RECT { left: 400, top: 1050, right: 401, bottom: 1051 });
        assert_eq!(icon_at(None, far), RECT { left: 400, top: 1050, right: 401, bottom: 1051 });
        // A late answer never replaces a newer one.
        assert!(newer(3, 4));
        assert!(newer(4, 4));
        assert!(!newer(5, 4));
    }

    #[test]
    fn the_panel_hangs_from_windows_tray() {
        let mon = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let work = RECT { bottom: 1032, ..mon };
        // An icon on a bottom taskbar: kept as it is, opens upward.
        let icon = RECT { left: 1700, top: 1040, right: 1724, bottom: 1072 };
        assert_eq!(tray_anchor(icon, mon, work), (icon, true));
        // In the overflow flyout above the taskbar: from the taskbar's edge.
        let flyout = RECT { left: 1650, top: 960, right: 1690, bottom: 1000 };
        let (a, up) = tray_anchor(flyout, mon, work);
        assert!(up);
        assert_eq!((a.left, a.right, a.top), (1650, 1690, 1032));
        assert!(a.bottom > a.top);
        // Auto-hide (the work area is the whole monitor): the icon as is.
        assert_eq!(tray_anchor(icon, mon, mon), (icon, true));
        // A taskbar at the top: opens downward, from its edge.
        let top_work = RECT { top: 48, ..mon };
        let top_icon = RECT { left: 1700, top: 8, right: 1724, bottom: 40 };
        assert_eq!(tray_anchor(top_icon, mon, top_work), (top_icon, false));
        let below = RECT { left: 1650, top: 60, right: 1690, bottom: 100 };
        let (a, up) = tray_anchor(below, mon, top_work);
        assert!(!up);
        assert_eq!(a.bottom, 48);
        // A monitor left of the primary (negative x) and a second one below.
        let left = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        let li = RECT { left: -200, top: 1040, right: -176, bottom: 1072 };
        assert_eq!(tray_anchor(li, left, RECT { bottom: 1032, ..left }), (li, true));
        let below_mon = RECT { left: 0, top: 1080, right: 1920, bottom: 2160 };
        let bi = RECT { left: 1700, top: 2120, right: 1724, bottom: 2152 };
        assert!(tray_anchor(bi, below_mon, RECT { bottom: 2112, ..below_mon }).1, "opens upward on the lower monitor too");
    }

    #[test]
    fn records_round_trip() {
        let r = Record { pid: 4242, started: 133_000_000_000_000_000, by_guid: false, hwnd: 0x0012_0456, uid: 1 };
        assert_eq!(Record::parse(&r.to_line()), Some(r));
        let g = Record { by_guid: true, ..r };
        assert_eq!(Record::parse(&g.to_line()), Some(g));
        assert_eq!(Record::parse(""), None);
        assert_eq!(Record::parse("1 2 maybe 3 4"), None);
        assert_eq!(Record::parse("1 2 guid 3"), None);
    }

    #[test]
    fn removing_the_icon_stops_watching_the_closing_press() {
        let src = include_str!("trayicon.rs");
        let body = src.split("pub(super) unsafe fn remove()").nth(1).and_then(|b| b.split("
}
").next()).unwrap();
        assert!(body.contains("RETRY_TIMER") && body.contains("RELEASE_TIMER"));
        assert!(body.contains("PRESS_CLOSED.with(|c| c.set(None))"));
    }

    #[test]
    fn cleanup_only_for_the_dock_that_registered() {
        let rec = Record { pid: 10, started: 500, by_guid: true, hwnd: 0, uid: 1 };
        let never = |_: u32, _: u64| false;
        let always = |_: u32, _: u64| true;
        // The guardian of the dock on record cleans up after it.
        assert!(should_remove(&rec, Gone::Dock(10, 500), always));
        // A newer dock is on record: an older dock's guardian keeps off,
        // a reused pid included.
        assert!(!should_remove(&rec, Gone::Dock(9, 400), never));
        assert!(!should_remove(&rec, Gone::Dock(10, 499), never));
        // Uninstall: only once that dock no longer runs.
        assert!(should_remove(&rec, Gone::Any, never));
        assert!(!should_remove(&rec, Gone::Any, always));
    }

    #[test]
    fn our_calls_reach_the_spy_cache_as_the_shell_would_send_them() {
        // The GUID in the byte order of the shell's TRAYNOTIFYDATA.
        let mut d = data(true);
        d.hWnd = HWND(0x0012_0456 as *mut _);
        d.uFlags |= NIF_MESSAGE | NIF_TIP;
        d.uCallbackMessage = WM_TRAYICON;
        let tip: Vec<u16> = "NopBar".encode_utf16().collect();
        d.szTip[..tip.len()].copy_from_slice(&tip);
        let m = tray_msg(NIM_ADD.0, &d);
        assert_eq!(m.message, NIM_ADD.0);
        assert_eq!(m.hwnd, 0x0012_0456);
        assert_eq!(m.tip, "NopBar");
        assert_eq!(m.callback_msg, WM_TRAYICON);
        let g = m.guid.expect("by GUID");
        assert_eq!(&g[..4], &0xB8DC297Cu32.to_le_bytes());
        assert_eq!(&g[4..6], &0x7493u16.to_le_bytes());
        assert_eq!(&g[6..8], &0x4333u16.to_le_bytes());
        assert_eq!(&g[8..], &[0xA5, 0xD2, 0x20, 0x76, 0x9F, 0x1E, 0xE9, 0xD8]);
        // By hWnd + uID: no GUID, so the spy matches it by window and uID.
        let mut h = data(false);
        h.hWnd = HWND(0x0012_0456 as *mut _);
        let m = tray_msg(NIM_MODIFY.0, &h);
        assert_eq!((m.guid, m.uid), (None, ICON_UID));
        // Version: the union's uVersion.
        let mut v = data(false);
        v.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        assert_eq!(tray_msg(NIM_SETVERSION.0, &v).version, NOTIFYICON_VERSION_4);
    }

    #[test]
    fn guid_is_the_documented_one() {
        assert_eq!(format!("{ICON_GUID:?}"), "B8DC297C-7493-4333-A5D2-20769F1EE9D8");
    }

    /// An icon's color bits (straight BGRA, top-down).
    unsafe fn color_bits(icon: HICON) -> Vec<u8> {
        use windows::Win32::Graphics::Gdi::{DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
        use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).unwrap();
        let mut bm = BITMAP::default();
        assert_ne!(GetObjectW(info.hbmColor.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)), 0);
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        assert_eq!(GetDIBits(dc, info.hbmColor, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS), h);
        ReleaseDC(None, dc);
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        px
    }

    #[test]
    fn the_recolored_mark_keeps_its_transparency_and_edges() {
        // The real marks at every tray size, made into icons as the tray
        // makes them, recolored as high contrast does, then drawn the way
        // icons are (DrawIconEx, on black and on white: tray::draw_icon)
        // and their alpha read back. Offscreen GDI only.
        let (r, g, b) = (0xFFu32, 0xC0u32, 0x40u32);
        let rgb = r << 16 | g << 8 | b;
        for (px, png) in TRAY_WHITE {
            unsafe {
                let src = CreateIconFromResourceEx(png, true, 0x0003_0000, px, px, LR_DEFAULTCOLOR).unwrap();
                let (src_bits, src_drawn) = (color_bits(src), crate::tray::draw_icon(src, px as u32).unwrap());
                let tinted = recolor(src, rgb).expect("recolored");
                let (bits, drawn) = (color_bits(tinted), crate::tray::draw_icon(tinted, px as u32).unwrap());
                let _ = DestroyIcon(src);
                let _ = DestroyIcon(tinted);
                let (mut clear, mut edges) = (0, 0);
                for ((s, t), (sd, d)) in src_bits.chunks_exact(4).zip(bits.chunks_exact(4)).zip(src_drawn.chunks_exact(4).zip(drawn.chunks_exact(4))) {
                    let a = s[3];
                    assert_eq!(t[3], a, "{px}: the alpha is kept");
                    assert_eq!(d[3], sd[3], "{px}: drawn just as opaque as the original");
                    if a == 0 {
                        clear += 1;
                        assert_eq!(t, [0, 0, 0, 0], "{px}: a transparent pixel holds no color");
                        assert_eq!(d, [0, 0, 0, 0], "{px}: and is drawn as nothing");
                    } else {
                        // Straight color in the bits; drawn premultiplied:
                        // the tint scaled by the pixel's coverage.
                        assert_eq!(t[..3], [b as u8, g as u8, r as u8], "{px}");
                        let want = |c: u32| (c * a as u32 + 127) / 255;
                        for (got, c) in d[..3].iter().zip([b, g, r]) {
                            assert!((*got as i32 - want(c) as i32).abs() <= 1, "{px}: alpha {a}: {d:?}");
                        }
                        edges += (a < 255) as usize;
                    }
                }
                assert!(clear > 0, "{px}: the mark has a transparent background");
                if px >= 20 {
                    assert!(edges > 0, "{px}: and soft edges");
                }
            }
        }
    }
}

