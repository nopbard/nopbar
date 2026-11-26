//! The settings window (`dock.exe --settings`): its own process, gone when
//! closed. Changes show on the dock right away (Preview); Save keeps them
//! (Commit), Cancel / closing the window goes back (Revert).
//!
//! The whole window is one D2D surface drawn immediate-mode on a Mica
//! backdrop, redrawn only on input (plus a short timer while the About
//! photo crossfades).

mod access;

pub(crate) const SPONSORS_URL: &str = "https://github.com/sponsors/nopbard";
mod apply;
mod calendar;
mod cards;
mod companion_ai;
mod agent_notifications;
pub(crate) mod client;
mod corners;
mod dialogs;
mod displays_view;
mod extra;
mod howto;
mod input;
mod layout;
#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod snapshots;
#[cfg(test)]
pub(crate) use snapshots::font_compare;
mod model;
mod pins;
mod record;
mod report;
mod reset;
mod rows;
mod search_box;
mod status;
mod tooltip;
mod topbar_view;
mod update_card;
mod welcome;
mod widgets;

pub use model::apply_keys;

use std::cell::RefCell;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::UI::Composition::{CompositionStretch, Desktop::DesktopWindowTarget, SpriteVisual};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use crate::config::Config;
use crate::gfx::Gfx;
use crate::i18n::t;
use model::Ctl;

/// The window's client area (DIP): first opened at W0 x H0, resizable down
/// to MIN_W x MIN_H (review P1), the last size remembered in the config
/// (`settings_window`, no row).
const W0: f32 = 820.0;
const H0: f32 = 620.0;
pub(super) const MIN_W: f32 = 720.0;
pub(super) const MIN_H: f32 = 520.0;
/// The window's style: resizable and maximizable.
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0 | WS_THICKFRAME.0);

thread_local! {
    /// The client area now (DIP), and its last size neither minimized nor
    /// maximized (what is remembered).
    static SIZE: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((W0, H0)) };
    static NORMAL_SIZE: std::cell::Cell<Option<(f32, f32)>> = const { std::cell::Cell::new(None) };
    /// The size it opened at (not resized: nothing to remember).
    static OPENED: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((W0, H0)) };
}

/// The window's width and height now (DIP); every page lays out in them.
pub(super) fn win_w() -> f32 {
    SIZE.with(|s| s.get().0)
}

pub(super) fn win_h() -> f32 {
    SIZE.with(|s| s.get().1)
}

/// The size to open at: the remembered one, kept to the minimum and to
/// the work area (`work`: its size in DIP).
pub(super) fn opening_size(saved: Option<[u32; 2]>, work: (f32, f32)) -> (f32, f32) {
    let (w, h) = saved.map(|[w, h]| (w as f32, h as f32)).unwrap_or((W0, H0));
    (w.clamp(MIN_W, work.0.max(MIN_W)), h.clamp(MIN_H, work.1.max(MIN_H)))
}

const SIDEBAR: f32 = 220.0;
/// Space under the page (no footer: every change applies and saves at
/// once). A note, when there is one, is drawn over the page's foot.
const FOOTER: f32 = 16.0;
const NOTE_H: f32 = 36.0;
const PAD: f32 = 24.0;
/// The page's title (its top: one 20/28 line), and where the page starts
/// under it (24 below).
const TITLE_TOP: f32 = 20.0;
const CONTENT_TOP: f32 = 72.0;
/// The sidebar's entries: their pitch and height, the space between two
/// clusters of them, and an entry's icon (a rounded square, its corners
/// and the glyph in it) and where its name starts.
const TAB_PITCH: f32 = 32.0;
const TAB_H: f32 = 28.0;
const CLUSTER_GAP: f32 = 12.0;
const TAB_ICON: f32 = 20.0;
const TAB_ICON_R: f32 = 4.0;
const TAB_GLYPH: f32 = 12.0;
const TAB_TEXT_X: f32 = 50.0;
/// How far in the sidebar list's edge fade reaches (DIP).
const SIDE_FADE: f32 = crate::theme::metrics::space::S4;
/// The timer that runs the switches' and the page's motion (stopped as
/// soon as nothing moves).
const ANIM_TIMER: usize = 0x6A11;
/// A switch's knob crossing (motion::SPRING_A), and a page coming in
/// (motion::SPRING_C), ms at speed 1.
const SWITCH_MS: u64 = 260;
const PAGE_MS: u64 = 280;
/// How far a page rises as it comes in (DIP).
const PAGE_RISE: f32 = 12.0;
/// A tab of more than one page: the switch between its pages under the
/// title (its top and height), and what it adds over the content.
const SUBTABS_TOP: f32 = 60.0;
const SUBTABS_H: f32 = 28.0;
const SUBTABS_SPACE: f32 = 40.0;
/// "The UI font" (the user's choice, else Windows'): fonts::map turns it into
/// the family drawn with.
const FONT: &str = crate::fonts::THEME_DEFAULT;
/// The pointer left the window (asked for with TrackMouseEvent).
const WM_MOUSELEAVE: u32 = 0x02A3;

thread_local! {
    /// line_h's answers, by size and text size (the font is the same).
    static LINE_H: RefCell<Vec<(u32, u32, f32)>> = const { RefCell::new(Vec::new()) };
}

/// How tall one line of the settings font at `size` is drawn (DIP; at the
/// user's text size), measured once per size.
fn line_h(dw: &windows::Win32::Graphics::DirectWrite::IDWriteFactory, size: f32) -> f32 {
    let key = (size.to_bits(), crate::fonts::scale().to_bits());
    if let Some(h) = LINE_H.with(|c| c.borrow().iter().find(|e| (e.0, e.1) == key).map(|e| e.2)) {
        return h;
    }
    let h = extra::wrapped_h(dw, "Ag", size, false, 4000.0);
    LINE_H.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 32 {
            c.clear();
        }
        c.push((key.0, key.1, h));
    });
    h
}

/// The corners of the control `h` (`size` w x h DIP) the focus ring goes
/// round: round ones (switches, colors, the "i") a capsule, the light /
/// dark pictures and the first-run cards a card's, the rest a control's.
/// Pure: tested.
/// The focus ring's stroke (its centerline's rectangle and corner) for
/// `h` drawn at `r`: RING_GAP outside the control, concentric with its
/// corners; an "i"'s around its glyph (not its larger target),
/// INFO_RING_GAP out. Pure: tested.
fn focus_ring_rect(h: Hit, r: D2D_RECT_F, ctl_r: f32, card_r: f32) -> (D2D_RECT_F, f32) {
    let (r, g) = match h {
        Hit::Info(_) => (rows::info_glyph(r), rows::tok::INFO_RING_GAP),
        _ => (r, rows::tok::RING_GAP),
    };
    let o = g + rows::tok::RING_W / 2.0;
    let rr = D2D_RECT_F { left: r.left - o, top: r.top - o, right: r.right + o, bottom: r.bottom + o };
    (rr, focus_radius(h, (r.right - r.left, r.bottom - r.top), ctl_r, card_r) + o)
}

fn focus_radius(h: Hit, size: (f32, f32), ctl_r: f32, card_r: f32) -> f32 {
    let capsule = crate::theme::capsule(size.0.min(size.1));
    match h {
        Hit::Toggle(_) | Hit::Swatch(..) | Hit::Info(_) => capsule,
        Hit::Theme(_) | Hit::WelcomeCard(_) | Hit::Display(_) | Hit::PinRow(_) => card_r.min(capsule),
        _ => ctl_r.min(capsule),
    }
}
/// "Put Launchpad back in name order?" (its button), shown outside the
/// app's borrow.
pub(crate) const WM_ASK_LP_RESET: u32 = WM_APP + 17;
/// Show a page (wParam = page_code), posted to ourselves.
const WM_SHOW_PAGE_NOW: u32 = WM_APP + 10;
/// The GPU device was lost (from Gfx); a new one is made with backoff.
const WM_GPU_LOST: u32 = WM_APP + 13;
/// Close now: posted, so the window goes outside any borrow of the app.
const WM_LEAVE_NOW: u32 = WM_APP + 14;
/// The launchers were found (crate::launchers): the "Search with" list
/// is rebuilt.
const WM_LAUNCHERS_FOUND: u32 = WM_APP + 15;
/// The accent's "Custom…" (wParam = its row): Windows' color dialog,
/// shown outside the app's borrow (corners.rs, pick_menu).
pub(super) const WM_PICK_ACCENT: u32 = WM_APP + 19;
/// The window, for the launchers' detection thread to tell.
static SETTINGS_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// The theme the window draws itself with: the dock's finish (owner,
/// 2026-11-12: Flat and Glass tell apart everywhere). Flat is an opaque
/// window of Flat's greys; Glass is DWM's acrylic behind the page, the
/// cards glass. The blur is `cfg`'s (this window's own process: the
/// dock's effects state is not here), as the dock's falls back.
fn settings_look(raw: crate::theme::Theme, sys: &crate::syslook::SysLook, cfg: &Config) -> crate::theme::Theme {
    raw.resolve_fx(sys, settings_blur(sys, cfg)).windows_shell()
}

/// The blur as the dock has it in force for `cfg` (effects::resolve: the
/// low spec switch, the preset, Windows' animations and high contrast),
/// off on software rendering (chosen, or a device fallen back to WARP).
fn settings_blur(sys: &crate::syslook::SysLook, cfg: &Config) -> bool {
    let env = crate::effects::Env { animations: sys.animations, high_contrast: sys.high_contrast.is_some() };
    let software = crate::gfx::software_in_force() || cfg.effects.render.trim().eq_ignore_ascii_case("software");
    crate::theme::glass_blur(crate::effects::resolve(cfg, env).items.blur, software)
}

fn launchers_found() {
    let h = SETTINGS_HWND.load(std::sync::atomic::Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_LAUNCHERS_FOUND, WPARAM(0), LPARAM(0));
        }
    }
}
/// Next recovery attempt after a device loss.
const GPU_RETRY_TIMER: usize = 0x6770;

thread_local! {
    static GPU_PLAN: RefCell<crate::gfx::RecoveryPlan> = RefCell::new(crate::gfx::RecoveryPlan::default());
    /// A retry is scheduled: further loss notices of the same incident wait for it.
    static GPU_RETRYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A device loss was noticed (or a retry is due): try a new device, redraw
/// the page on success, else wait (bounded); after that the window stays as
/// it is (closing and reopening it starts over).
unsafe fn gpu_recover(hwnd: HWND, fresh: bool) {
    if fresh && GPU_RETRYING.get() {
        return;
    }
    GPU_RETRYING.set(false);
    let Some(lost) = with_app(|a| a.gfx.check_device()) else {
        // Busy (re-entered): look again shortly.
        GPU_RETRYING.set(SetTimer(Some(hwnd), GPU_RETRY_TIMER, 100, None) != 0);
        return;
    };
    if !lost {
        return;
    }
    let now = crate::START.get().map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
    if fresh && !GPU_PLAN.with(|p| p.borrow_mut().lost(now)) {
        crate::log!("settings: GPU lost too often; not trying again");
        return;
    }
    let warp = GPU_PLAN.with(|p| p.borrow().allow_warp());
    match with_app(|a| a.gfx.recover(warp)) {
        Some(Ok(_)) => {
            GPU_PLAN.with(|p| p.borrow_mut().succeeded());
            crate::log!("settings: new rendering device; redrawing");
            with_app(|a| a.redraw());
        }
        _ => {
            let next = GPU_PLAN.with(|p| {
                let mut p = p.borrow_mut();
                p.failed();
                p.next_delay()
            });
            match next {
                Some(ms) => {
                    GPU_RETRYING.set(SetTimer(Some(hwnd), GPU_RETRY_TIMER, ms.max(1), None) != 0);
                }
                None => crate::log!("settings: no GPU device after every try"),
            }
        }
    }
}

/// Asks a running dock to quit over its pipe; true once it is gone (its
/// single-instance mutex is free), waiting up to `secs`.
pub fn stop_running_dock(secs: u64) -> bool {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::System::Threading::{OpenMutexW, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE};
    // Bounded like every pipe call (a dock that stops answering cannot
    // hold the installer or the settings window up).
    client::Client::connect().quit();
    unsafe {
        let Ok(m) = OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\NopBar.Single")) else {
            return true; // no dock running
        };
        // The dock holds the mutex for its whole life: we get it when it exits.
        let r = WaitForSingleObject(m, (secs * 1000) as u32);
        let _ = CloseHandle(m);
        r == WAIT_OBJECT_0 || r.0 == 0x80
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Chat(&'static str),
    /// A sidebar tab (index into model::Tab::ALL).
    Tab(usize),
    /// A page: one of the tab's pages under its title.
    Section(usize),
    Toggle(usize),
    Slider(usize, f32, f32),
    Choice(usize, usize),
    Button(usize),
    NewNote(usize),
    /// Pinned apps: a row (press to drag it), its buttons, the add buttons
    /// and an app in the running apps list.
    PinRow(usize),
    PinIcon(usize),
    PinReset(usize),
    PinRemove(usize),
    PinAdd,
    PinAddRunning,
    AppItem(usize),
    /// The sidebar's search box, its ×, a result; "Show all" over a page
    /// the search narrows.
    Search,
    SearchClear,
    SearchResult(usize),
    FilterAll,
    /// "Reset all settings" (General) and the answers to its question.
    ResetAll,
    ResetYes,
    ResetNo,
    /// First-run flow: step 1's cards (0 hides the taskbar), the buttons.
    WelcomeCard(usize),
    WelcomeBack,
    WelcomeNext,
    WelcomeSkip,
    /// First-run flow's last step: a theme card.
    Theme(usize),
    /// "Advanced" / "More options" (by row): opens or closes it.
    Fold(usize),
    /// Top bar page: an item in the picture of the bar (by its row).
    BarCell(usize),
    /// Dock page: a display on the map (Pinned apps page: its button), by
    /// index into App::screens; and "Identify".
    Display(usize),
    Identify,
    /// Dock page: "Undo" after a dock was made the same as the main one.
    SameUndo,
    /// Pinned apps page: "Customize this dock" (a dock taking the main
    /// dock's apps gets a list of its own).
    PinsCustomize,
    /// About page: a link (index into extra::LINKS).
    Link(usize),
    /// A list row's button (Ctl::Menu): opens its list.
    Menu(usize),
    /// An option in the open list: (row, option).
    MenuItem(usize, usize),
    /// Anywhere outside the open list: closes it.
    MenuDismiss,
    /// About page: "Report a problem".
    ReportProblem,
    /// About page, Updates: "Check now", "Download", the auto-check switch.
    UpdateCheck,
    UpdateDownload,
    UpdateAuto,
    /// "Update now" / "Restart to update".
    UpdateInstall,
    /// The link under a row's note (status::Fix), by row.
    RowFix(usize),
    /// The "i" after a row's label (or a group label): its description in
    /// a tooltip (tooltip.rs), by row.
    Info(usize),
    /// The accent row's colors (Appearance): (row, option), the last one
    /// "Custom…" (Windows' color dialog).
    Swatch(usize, usize),
    /// A shortcut row's key field (record / flip) and its Reset.
    Shortcut(usize),
    ShortcutReset(usize),
    /// Input page: "Add app…" (running apps), a suggestion, a rule's mode
    /// (cycles) and its ×.
    InputAdd,
    InputSuggest(usize),
    InputMode(usize),
    InputRemove(usize),
    /// Calendar page (calendar.rs): the link field and Add; a link's
    /// name field (renaming), Test, Rename and ×.
    CalUrl,
    CalAdd,
    /// "Add calendar…": the field shows in its place.
    CalOpen,
    CalName,
    CalRename(usize),
    CalRemove(usize),
}

/// The row a control belongs to (None: not a row's).
fn row_of(h: Hit) -> Option<usize> {
    match h {
        Hit::Info(r) if r >= tooltip::CARD_TIP => None,
        Hit::NewNote(r) | Hit::Toggle(r) | Hit::Slider(r, ..) | Hit::Choice(r, _) | Hit::Menu(r) | Hit::Button(r) | Hit::Shortcut(r) | Hit::ShortcutReset(r) | Hit::RowFix(r) | Hit::Info(r) | Hit::Swatch(r, _) => Some(r),
        _ => None,
    }
}

/// How a page is reached: opened (a click in the sidebar, a tab, a search
/// result picked) or previewed by the search while typing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Nav {
    Open,
    Preview,
}

/// Pure: whether going from page `from` to page `to` ends the undo
/// offered on the page left. A preview while typing a search ends nothing
/// (the typing is not a decision to leave); opening another page does.
/// Tested.
fn page_change(from: usize, to: usize, by: Nav) -> bool {
    match by {
        Nav::Preview => false,
        Nav::Open => from != to,
    }
}

/// Where the pointer and the focus were when an "i" was asked for.
type TipSeen = (Option<Hit>, Option<Hit>);

struct App {
    hwnd: HWND,
    gfx: Gfx,
    _target: DesktopWindowTarget,
    sprite: SpriteVisual,
    sections: Vec<model::Section>,
    section: usize,
    scroll: f32,
    cfg: Config,
    /// Fields edited and not saved yet (only these are sent): previewed on
    /// the dock at once, saved a moment later (apply.rs).
    touched: Vec<String>,
    /// Closed with a save that failed: hidden, trying again (the tries so
    /// far; apply.rs). None while the window is up.
    exit_tries: Option<u32>,
    /// Edits taken from (or kept in) the journal: it goes once all is saved.
    journaled: bool,
    client: client::Client,
    hits: Vec<(D2D_RECT_F, Hit)>,
    hover: Option<Hit>,
    dragging: Option<Hit>,
    scale: f32,
    light: bool,
    /// The resolved theme: the shared text, tile and radius tokens.
    look: crate::theme::Theme,
    /// What `look` was resolved from (refresh_look): the theme choice and
    /// Windows' appearance.
    look_for: (crate::config::ThemeRef, crate::syslook::SysLook),
    /// The font and text size `look` was resolved with.
    look_text: crate::config::AppearanceCfg,
    /// The blur `look` was resolved with (settings_blur: the low spec
    /// switch, the preset, the rendering).
    look_blur: bool,
    solid: bool,
    /// High contrast colors in force.
    hc: Option<crate::syslook::HighContrast>,
    /// Keyboard focus (shown with a ring once the keyboard is used).
    focus: Option<Hit>,
    keyboard: bool,
    /// The "on" color (the theme's accent) and what is drawn on it.
    accent: D2D1_COLOR_F,
    accent_ink: D2D1_COLOR_F,
    /// Last thing to tell the user (bottom left).
    note: String,
    content_h: f32,
    /// Pin icons for the pinned apps page, loaded once (path -> pixels).
    pin_icons: RefCell<std::collections::HashMap<String, Option<crate::icons::Icon>>>,
    icon_pending: RefCell<std::collections::HashSet<String>>,
    icon_done: std::sync::Arc<std::sync::Mutex<Vec<(String, Option<crate::icons::Icon>)>>>,
    /// Themes on the appearance page: (source, name).
    themes: Vec<(String, String)>,
    /// The first-run flow, while it is shown (over the whole window).
    welcome: Option<welcome::Welcome>,
    /// The open list of a Ctl::Menu row: (row, highlighted option), and
    /// the first option shown (long lists scroll).
    menu: Option<(usize, usize)>,
    menu_first: usize,
    /// How many items the open list had room for (last drawn).
    list_shown: std::cell::Cell<usize>,
    /// Pinned apps: the pin held by the mouse, the list's top and pitch
    /// (last drawn), the running apps list and its button.
    pin_drag: Option<pins::PinDrag>,
    /// The dock saved new pins while one was held here: merged on the drop.
    pins_pending: bool,
    pin_list: std::cell::Cell<(f32, f32)>,
    apps_menu: Option<pins::AppsMenu>,
    apps_anchor: std::cell::Cell<Option<D2D_RECT_F>>,
    /// The sidebar's search: what is typed, the IME's composition still
    /// open, the caret's x (for the IME), half a surrogate pair.
    search: String,
    search_comp: String,
    search_caret: std::cell::Cell<f32>,
    surrogate: Option<u16>,
    /// A row found by the search: (section, row, since), lit for a moment;
    /// and the row to scroll into view once drawn.
    flash: Option<(usize, usize, std::time::Instant)>,
    reveal: Option<usize>,
    /// Each row's top and bottom as last drawn (window DIPs).
    row_tops: RefCell<Vec<(f32, f32)>>,
    /// A reset waiting for its "Reset" on the page shown.
    confirm: Option<reset::Confirm>,
    /// Windows' "Animation effects" (off: changes are instant).
    motion: bool,
    /// About page, Updates: who updates this build, what update.json
    /// says, and whether "Check now" is running.
    upd_channel: crate::updates::Channel,
    upd: crate::updates::State,
    upd_checking: bool,
    /// This copy installs updates itself (crate::autoupdate::eligible).
    upd_self: bool,
    /// The card polls update.json while an update moves (UPD_POLL_TIMER).
    upd_polling: std::cell::Cell<bool>,
    /// "Start with Windows": (what was saved, what Windows does), None
    /// when it could not be read (status.rs).
    startup: Option<(bool, bool)>,
    /// The shortcut being recorded: (row, modifiers held so far); after
    /// it, the modifiers still down are swallowed (record.rs).
    recording: Option<(usize, u32)>,
    rec_tail: bool,
    /// What the recorder refused last: (shortcut id, why).
    rec_note: Option<(&'static str, String)>,
    /// Free keys found for a taken chord (status.rs), per chord.
    alts: RefCell<Vec<(crate::shortcuts::Chord, Vec<crate::shortcuts::Chord>)>>,
    /// The Calendar page (calendar.rs).
    cal: calendar::Page,
    /// The folds opened ("Advanced", "More options"), by row key.
    folds: Vec<&'static str>,
    /// The displays now (crate::displays: main first) and the one whose
    /// dock the Dock page sets (displays_view.rs).
    screens: Vec<crate::displays::Display>,
    display_sel: usize,
    /// A dock just made "Same as Dock 1": its own settings as they were
    /// (display key), for "Undo"; and every dock's own settings dropped
    /// this way, put back if its switch is turned off again.
    same_undo: Option<(String, crate::config::DisplayDock)>,
    /// The dock a question about one dock was asked for (display key, main?):
    /// reset::Confirm::SameAsMain acts only while it is still picked.
    confirm_target: Option<(String, bool)>,
    own_stash: std::collections::BTreeMap<String, crate::config::DisplayDock>,
    /// The first search result listed (the list scrolls).
    results_first: usize,
    /// Choice rows drawn as a list button (their options too wide), last
    /// paint.
    compact: RefCell<Vec<usize>>,
    /// The sidebar's list scrolled this far (it is taller than a short
    /// window).
    side_scroll: f32,
    /// Switches moving (their row key, or the update switches' own) and
    /// since when; the page coming in since when.
    anims: RefCell<Vec<(String, std::time::Instant)>>,
    page_anim: Option<std::time::Instant>,
    /// The search narrows the page shown to its matches (while typing;
    /// a result picked shows its page whole).
    filtering: bool,
    /// The tooltip shown, waiting or fading (tooltip.rs); the row whose tip
    /// Esc put away (until another is wanted); the next tip shows at once
    /// (its "i" clicked).
    tip: Option<tooltip::Tip>,
    tip_quiet: Option<usize>,
    tip_now: bool,
    /// An "i" asked for (show_tip), with where the pointer and the focus
    /// were when it was (noted at the next sync): it holds until they move.
    tip_ask: Option<(usize, Option<TipSeen>)>,
    /// The keyboard moved last (not the pointer): its "i" comes first.
    tip_key: bool,
    /// A widget row's how-to card (howto.rs): shown, waiting or fading
    /// (a Tip, its row); the widget row under the pointer; the row whose
    /// card Esc or a scroll put away (until another is wanted); where each
    /// widget row was drawn last (window DIPs).
    howto: Option<tooltip::Tip>,
    howto_hover: Option<usize>,
    howto_quiet: Option<usize>,
    howto_rows: RefCell<Vec<(usize, D2D_RECT_F)>>,
    /// The window is not in front (switched away, minimized, hidden): no
    /// how-to card plays (it would not be seen).
    howto_away: bool,
    /// The row whose card the last paint drew (None: none drawn).
    howto_drawn: std::cell::Cell<Option<usize>>,
    /// Where the pointer was at the last WM_MOUSEMOVE (client px; None
    /// once it left): a move to the same place is not the pointer moving.
    last_pt: Option<(i32, i32)>,
    /// The control the mouse button is held down on (drawn pressed).
    pressed: Option<Hit>,
    /// What the page drew last, for the space before the next block
    /// (layout::gap_before).
    flow: std::cell::Cell<layout::Block>,
    /// What a page drew and where (layout.rs): Some only while a test
    /// draws a page.
    #[cfg(test)]
    log: RefCell<Option<Vec<layout::Item>>>,
    /// A text logged already as its box (text_box): not again as drawn.
    #[cfg(test)]
    quiet_log: std::cell::Cell<bool>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

/// Starts the settings window as its own process (one at a time).
pub fn launch() {
    launch_with(&[]);
}

/// The settings window opened on the first-run Welcome page.
pub fn launch_welcome() {
    launch_with(&["--welcome"]);
}

/// The settings window on a page (`--settings --page <name>`): "about", or
/// "report" (the About page with "Report a problem" run, as if clicked).
pub fn launch_page(page: &str) {
    launch_with(&["--page", page]);
}

/// `--page` names: 1 = About, 2 = About and report; 0 = the usual start.
fn page_code(page: Option<&str>) -> usize {
    match page {
        Some("about") => 1,
        Some("report") => 2,
        Some("input") => 3,
        Some("topbar") => PAGE_TOPBAR,
        Some("general") => PAGE_GENERAL,
        Some("dock") => PAGE_DOCK,
        Some("bar") => PAGE_BAR,
        Some("calendar") => PAGE_CALENDAR,
        Some("updates") => PAGE_UPDATES,
        Some("appearance") => PAGE_APPEARANCE,
        Some("shortcuts") => PAGE_SHORTCUTS,
        Some("companion") => 15,
        Some("companion-ai") => 16,
        // Older links to the workspaces (gone): their page.
        Some(p) if p == "workspaces" || p.starts_with("workspaces-") => PAGE_WINDOWS,
        _ => 0,
    }
}

/// page_code() of Window management (older links to the workspaces).
const PAGE_WINDOWS: usize = 4;
/// page_code() of the focus timer's settings ("--page topbar", the clock
/// menu's "Focus timer settings…"; the name is older than its card).
pub(super) const PAGE_TOPBAR: usize = 7;
/// page_code()s of the General, Dock and Top bar pages (the top bar's
/// right-click menu; "--page topbar" was taken by the focus timer).
pub(super) const PAGE_GENERAL: usize = 8;
pub(super) const PAGE_DOCK: usize = 9;
pub(super) const PAGE_BAR: usize = 10;
/// page_code() of the calendars (the clock panel's "Add a calendar in
/// Settings…"): the Top bar's Clock sub-page.
pub(super) const PAGE_CALENDAR: usize = 11;
/// page_code()s of the updates (the About page), Appearance and
/// Shortcuts.
pub(super) const PAGE_UPDATES: usize = 12;
pub(super) const PAGE_APPEARANCE: usize = 13;
pub(super) const PAGE_SHORTCUTS: usize = 14;

/// The page (its title key) a plain page code opens, if it is one of them.
/// (The calendar is on the Top bar's Clock sub-page; updates are on the
/// About page's first card.)
fn plain_page(code: usize) -> Option<&'static str> {
    match code {
        15 => Some("companion.title"),
        16 => Some("chat.ai.title"),
        PAGE_GENERAL => Some("settings.section.general"),
        PAGE_DOCK => Some("settings.section.dock"),
        PAGE_BAR => Some("settings.section.topbar"),
        PAGE_CALENDAR => Some("settings.group.clock"),
        PAGE_UPDATES => Some("settings.section.about"),
        PAGE_APPEARANCE => Some("settings.section.appearance"),
        PAGE_SHORTCUTS => Some("settings.section.shortcuts"),
        PAGE_WINDOWS => Some("settings.tab.windows"),
        _ => None,
    }
}
/// The row "--page topbar" shows: the focus timer's first option, on the
/// Top bar's Clock sub-page.
const TIMER_ROW: &str = "focus_timer.custom_focus";

/// Every `--page` name (launch_page): none is ever dropped, callers
/// outside this build (the site, older builds' notices) may still use
/// one. "workspaces-rename-<i>" stands for each such link. Tested.
#[cfg(test)]
pub(super) const PAGE_NAMES: [&str; 16] =
    ["companion", "about", "report", "input", "topbar", "general", "dock", "bar", "calendar", "updates", "appearance", "shortcuts", "workspaces", "workspaces-save", "workspaces-work", "workspaces-rename-0"];

/// The page page code `code` opens (page_code). None: the usual start,
/// or the timer's row (opened by its key: App::open_row). Pure: tested.
pub(super) fn page_target(sections: &[model::Section], code: usize) -> Option<usize> {
    if code == 0 || code == PAGE_TOPBAR {
        return None;
    }
    if let Some(title) = plain_page(code) {
        return sections.iter().position(|s| s.title == title);
    }
    let kind = if code == 3 { model::Kind::Input } else { model::Kind::About };
    sections.iter().position(|s| s.kind == kind)
}

fn launch_with(extra: &[&str]) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--settings").args(extra).spawn();
    }
}

/// Start at login (Run key, or the Store build's StartupTask).
pub fn set_run_at_login(on: bool) -> bool {
    crate::release::set_run_at_login(on)
}

fn rgba(c: crate::theme::Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

/// The settings window's own control washes (the ink at this alpha, (dark,
/// light): rest, + hover, + pressed): the tile fills as they were before
/// harmony v2.1 made cards white fills (theme::fills::CARD_*). A control
/// here sits on a white card, where a white fill would not show; the
/// settings batch (8) moves these to Theme::fill_of and deletes them.
const CTL_REST: (f32, f32) = (0.06, 0.05);
const CTL_HOVER: (f32, f32) = (0.04, 0.03);
const CTL_PRESSED: (f32, f32) = (0.07, 0.05);

/// A card's fill over the window (card_fill without high contrast). Pure:
/// tested.
pub(super) fn card_layer(light: bool) -> D2D1_COLOR_F {
    if light {
        col(1.0, 1.0, 1.0, 0.55)
    } else {
        col(1.0, 1.0, 1.0, 0.07)
    }
}

fn col(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

/// An opaque grey `v`.
fn grey(v: u8) -> D2D1_COLOR_F {
    let c = v as f32 / 255.0;
    col(c, c, c, 1.0)
}

impl App {
    /// The window's state around its parts (`hwnd` a top-level window,
    /// `client` linked or offline, `cfg` what to show): the settings window
    /// and the layout tests build it the same way.
    #[allow(clippy::too_many_arguments)]
    unsafe fn assemble(
        hwnd: HWND,
        gfx: Gfx,
        client: client::Client,
        cfg: Config,
        cfg_now: &Config,
        sys: crate::syslook::SysLook,
        scale: f32,
        theme: &crate::theme::Theme,
        light: bool,
        solid: bool,
        welcome: bool,
    ) -> Result<App> {
        let c = gfx.compositor.clone();
        let interop: ICompositorDesktopInterop = windows::core::Interface::cast(&c)?;
        let target = interop.CreateDesktopWindowTarget(hwnd, false)?;
        let sprite = c.CreateSpriteVisual()?;
        target.SetRoot(&sprite)?;

        let note = String::new();
        let sections = model::sections();
        let themes = extra::list_themes();
        let welcome = welcome.then(|| welcome::Welcome::new(true, &themes));
        Ok(App {
            hwnd,
            gfx,
            _target: target,
            sprite,
            sections,
            section: 0,
            scroll: 0.0,
            touched: Vec::new(),
            cfg,
            exit_tries: None,
            journaled: false,
            client,
            hits: Vec::new(),
            hover: None,
            dragging: None,
            scale,
            light,
            look: theme.clone(),
            look_for: (cfg_now.theme.clone(), sys),
            look_text: cfg_now.appearance.clone(),
            look_blur: settings_blur(&sys, cfg_now),
            solid,
            hc: sys.high_contrast,
            // Enter goes on through the first-run flow.
            focus: welcome.is_some().then_some(Hit::WelcomeNext),
            keyboard: false,
            accent: rgba(theme.accent()),
            accent_ink: rgba(theme.on_accent()),
            note,
            content_h: 0.0,
            pin_icons: RefCell::new(Default::default()),
            icon_pending: RefCell::new(Default::default()),
            icon_done: Default::default(),
            themes,
            welcome,
            menu: None,
            menu_first: 0,
            list_shown: Default::default(),
            pin_drag: None,
            pins_pending: false,
            pin_list: Default::default(),
            apps_menu: None,
            apps_anchor: Default::default(),
            search: String::new(),
            search_comp: String::new(),
            search_caret: Default::default(),
            surrogate: None,
            flash: None,
            reveal: None,
            row_tops: Default::default(),
            confirm: None,
            motion: sys.animations,
            upd_channel: crate::updates::channel(),
            upd: crate::updates::load_state(),
            upd_checking: false,
            upd_self: crate::autoupdate::eligible().is_some(),
            upd_polling: std::cell::Cell::new(false),
            startup: None,
            recording: None,
            rec_tail: false,
            rec_note: None,
            alts: RefCell::new(Vec::new()),
            cal: Default::default(),
            folds: Vec::new(),
            screens: Vec::new(),
            display_sel: 0,
            same_undo: None,
            confirm_target: None,
            own_stash: Default::default(),
            results_first: 0,
            compact: RefCell::new(Vec::new()),
            side_scroll: 0.0,
            anims: RefCell::new(Vec::new()),
            page_anim: None,
            filtering: false,
            tip: None,
            tip_quiet: None,
            tip_now: false,
            tip_ask: None,
            tip_key: false,
            howto: None,
            howto_hover: None,
            howto_quiet: None,
            howto_rows: RefCell::new(Vec::new()),
            howto_away: false,
            howto_drawn: std::cell::Cell::new(None),
            last_pt: None,
            pressed: None,
            flow: std::cell::Cell::new(layout::Block::Start),
            #[cfg(test)]
            log: RefCell::new(None),
            #[cfg(test)]
            quiet_log: std::cell::Cell::new(false),
        })
    }
}

/// `welcome`: first run, open on the Welcome page (only then is it listed).
/// `page`: open on that page (see page_code).
pub fn run(welcome: bool, page: Option<&str>) -> Result<()> {
    let page = page_code(page);
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        WM_SHOW_WELCOME.store(RegisterWindowMessageW(w!("NopBar.Settings.ShowWelcome")), std::sync::atomic::Ordering::SeqCst);
        WM_SHOW_PAGE.store(RegisterWindowMessageW(w!("NopBar.Settings.ShowPage")), std::sync::atomic::Ordering::SeqCst);
        WM_CONFIG_SAVED.store(RegisterWindowMessageW(CONFIG_SAVED_MSG), std::sync::atomic::Ordering::SeqCst);
        let mutex = CreateMutexW(None, true, w!("Local\\NopBar.Settings"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Bring the open one forward (on the Welcome page if asked).
            if welcome {
                if !ask_open_window(&WM_SHOW_WELCOME, 0) {
                    crate::log!("welcome: the open settings window did not take the Welcome page");
                }
            } else if page != 0 {
                if !ask_open_window(&WM_SHOW_PAGE, page) {
                    crate::log!("settings: the open settings window did not take page {page}");
                }
            } else if let Ok(h) = FindWindowW(w!("NopBarSettings"), PCWSTR::null()) {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
            }
            let _ = CloseHandle(mutex);
            return Ok(());
        }
        windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_SINGLETHREADED)?;
        crate::i18n::init(&Config::load().language);
        let _queue = windows::Win32::System::WinRT::CreateDispatcherQueueController(windows::Win32::System::WinRT::DispatcherQueueOptions {
            dwSize: std::mem::size_of::<windows::Win32::System::WinRT::DispatcherQueueOptions>() as u32,
            threadType: windows::Win32::System::WinRT::DQTYPE_THREAD_CURRENT,
            apartmentType: windows::Win32::System::WinRT::DQTAT_COM_STA,
        })?;

        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // The NopBar icon from the exe's resources (res/nopbar.rc, id 1).
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(1 as *const u16)).unwrap_or_default(),
            lpszClassName: w!("NopBarSettings"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            w!("NopBarSettings"),
            &windows::core::HSTRING::from(t("settings.window_title")),
            STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let sys = crate::syslook::current();
        let cfg_now = crate::config::Config::load();
        size_window(hwnd, scale, cfg_now.settings_window);
        // The Effects page's rendering choice, before the first device.
        crate::gfx::set_render_choice(&cfg_now.effects.render);
        let theme = settings_look(cfg_now.load_theme(&crate::config::app_dir()), &sys, &cfg_now);
        let light = theme.is_light;
        let solid = !theme.glass();
        crate::icons::set_flat_tiles(solid);
        window_look(hwnd, light, solid);

        let gfx = Gfx::new()?;
        // Driver reset: a new device, then the page is drawn again.
        gfx.watch_device_loss(hwnd, WM_GPU_LOST);
        let mut client = client::Client::connect();
        let cfg = client.state();
        // The running dock may show a language not saved yet (a preview).
        crate::i18n::init(&cfg.language);
        let _ = SetWindowTextW(hwnd, &windows::core::HSTRING::from(t("settings.window_title")));
        let app = App::assemble(hwnd, gfx, client, cfg, &cfg_now, sys, scale, &theme, light, solid, welcome)?;
        APP.with(|a| *a.borrow_mut() = Some(app));
        // The "Search with" list starts from what is cached (nothing yet in
        // this process) and is rebuilt when the helper thread has looked.
        SETTINGS_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::SeqCst);
        crate::launchers::on_found(launchers_found);
        crate::launchers::refresh_soon();
        with_app(|a| {
            // The switch shows what Windows really does at sign-in.
            let wanted = a.cfg.start_with_windows;
            a.verify_startup(wanted);
            a.refresh_screens();
            // What the last window could not save (apply.rs).
            a.recover_journal();
            a.redraw()
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        if page != 0 {
            let _ = PostMessageW(Some(hwnd), WM_SHOW_PAGE_NOW, WPARAM(page), LPARAM(0));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|a| a.borrow_mut().take());
        // A "Check now" in flight must not hold the exit up.
        crate::updates::cancel();
        let _ = CloseHandle(mutex);
    }
    Ok(())
}

/// Registered message: "show the Welcome page" (from a second
/// `--settings --welcome` to the open window). Answered with 1 once shown.
static WM_SHOW_WELCOME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The dock saved the config (registered message, posted to the open
/// settings window).
static WM_CONFIG_SAVED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const CONFIG_SAVED_MSG: PCWSTR = w!("NopBar.Settings.ConfigSaved");

/// The dock saved a change of its own (e.g. a pin dragged on or off it):
/// an open settings window picks up the new pin list. Posted; nothing
/// happens when no settings window is open.
pub fn notify_config_saved() {
    unsafe {
        let Ok(h) = FindWindowW(w!("NopBarSettings"), PCWSTR::null()) else { return };
        let msg = RegisterWindowMessageW(CONFIG_SAVED_MSG);
        if msg != 0 {
            let _ = PostMessageW(Some(h), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// Registered message: "show page wParam" (page_code, from a second
/// `--settings --page`). Answered with 1 once taken.
static WM_SHOW_PAGE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Tells the open settings window to show a page (`which`: WM_SHOW_WELCOME
/// or WM_SHOW_PAGE with `wp`). It may still be starting (the mutex is taken
/// before the window exists, and it answers only once ready), so this asks
/// again for a few seconds. True once it answered.
unsafe fn ask_open_window(which: &std::sync::atomic::AtomicU32, wp: usize) -> bool {
    let msg = which.load(std::sync::atomic::Ordering::SeqCst);
    if msg == 0 {
        return false;
    }
    for _ in 0..50 {
        if let Ok(h) = FindWindowW(w!("NopBarSettings"), PCWSTR::null()) {
            let mut answer = 0usize;
            let sent = SendMessageTimeoutW(h, msg, WPARAM(wp), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, 2000, Some(&mut answer));
            if sent.0 != 0 && answer == 1 {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
                return true;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// The window's outer size (px) for a client area of `w` x `h` DIPs.
unsafe fn outer_size(w: f32, h: f32, scale: f32) -> (i32, i32) {
    let mut r = RECT { left: 0, top: 0, right: (w * scale).round() as i32, bottom: (h * scale).round() as i32 };
    let dpi = (scale * 96.0) as u32;
    let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(&mut r, STYLE, false, WINDOW_EX_STYLE(0), dpi);
    (r.right - r.left, r.bottom - r.top)
}

/// The window frame for the look: dark title bar or not; Glass: DWM's
/// acrylic behind the page and Windows' round corners; Flat (`solid`):
/// no backdrop (the page paints its own opaque grey) and the small
/// corners.
unsafe fn window_look(hwnd: HWND, light: bool, solid: bool) {
    let dark: i32 = (!light).into();
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const i32 as *const _, 4);
    // DWMSBT_TRANSIENTWINDOW (acrylic) or DWMSBT_NONE.
    let backdrop: i32 = if solid { 1 } else { 3 };
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop as *const i32 as *const _, 4);
    // DWMWCP_ROUNDSMALL (Flat) or DWMWCP_ROUND (Glass).
    let corner: i32 = if solid { 3 } else { 2 };
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const i32 as *const _, 4);
    let m = if solid { 0 } else { -1 };
    let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: m, cxRightWidth: m, cyTopHeight: m, cyBottomHeight: m });
}

/// The client area: the remembered size (else W0 x H0) at `scale`,
/// centered on the work area.
unsafe fn size_window(hwnd: HWND, scale: f32, saved: Option<[u32; 2]>) {
    let mut work = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut RECT as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    let room = ((work.right - work.left) as f32 / scale - 32.0, (work.bottom - work.top) as f32 / scale - 48.0);
    let (cw, ch) = opening_size(saved, room);
    SIZE.with(|s| s.set((cw, ch)));
    OPENED.with(|s| s.set((cw, ch)));
    let (w, h) = outer_size(cw, ch, scale);
    let x = work.left + ((work.right - work.left) - w) / 2;
    let y = work.top + ((work.bottom - work.top) - h) / 2;
    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
}

impl App {
    fn ink(&self) -> D2D1_COLOR_F {
        if let Some(h) = self.hc {
            return rgba(h.text);
        }
        rgba(self.look.ink())
    }
    fn dim(&self) -> D2D1_COLOR_F {
        if let Some(h) = self.hc {
            return rgba(h.text);
        }
        rgba(self.look.text_dim(false))
    }
    fn wash(&self, a: f32) -> D2D1_COLOR_F {
        if let Some(h) = self.hc {
            // Shapes stay readable: a faint text-colored fill.
            let t = rgba(h.text);
            return col(t.r, t.g, t.b, (a * 1.5).min(0.25));
        }
        rgba(self.look.wash(a))
    }
    /// Faint text and marks (placeholders, a list's scroll thumb): the
    /// tertiary text level; the text color in high contrast.
    fn faint(&self) -> D2D1_COLOR_F {
        if let Some(h) = self.hc {
            return rgba(h.text);
        }
        rgba(self.look.text_tertiary())
    }
    /// Where a line of text at `size` starts for its box to be centered
    /// on `cy` (measured in the settings font: no fixed offsets that drift
    /// with the text size or the font).
    fn text_top(&self, cy: f32, size: f32) -> f32 {
        cy - line_h(self.gfx.dwrite(), size) / 2.0
    }
    /// A control's fill (buttons, fields, segmented tracks, list buttons,
    /// small icon buttons) in state `st`: theme::fills' tile washes (rest,
    /// + hover, + pressed), the same every page (design B3).
    fn ctl_fill(&self, st: crate::theme::State) -> D2D1_COLOR_F {
        use crate::theme::{fills, State};
        let k = |(d, l): (f32, f32)| if self.light { l } else { d };
        let rest = k(CTL_REST);
        self.wash(match st {
            State::Rest | State::Disabled | State::Cursor => rest,
            State::Hover => rest + k(CTL_HOVER),
            State::Pressed | State::Open => rest + k(CTL_PRESSED),
            State::Selected => k(fills::ROW_SELECTED),
        })
    }
    /// A row's or list item's fill (the sidebar's pages, an open list's
    /// items, a card under the pointer) in state `st`: theme::fills' row
    /// washes (none at rest).
    fn row_fill(&self, st: crate::theme::State) -> D2D1_COLOR_F {
        use crate::theme::{fills, State};
        let k = |(d, l): (f32, f32)| if self.light { l } else { d };
        match st {
            State::Rest | State::Disabled => self.wash(0.0),
            State::Hover => self.wash(k(fills::ROW_HOVER)),
            State::Pressed | State::Open => self.wash(k(fills::ROW_PRESSED)),
            State::Selected | State::Cursor => self.wash(k(fills::ROW_SELECTED)),
        }
    }
    /// A slider's or switch's track, an "off" part (theme::fills::TRACK).
    fn track_fill(&self) -> D2D1_COLOR_F {
        let (d, l) = crate::theme::fills::TRACK;
        self.wash(if self.light { l } else { d })
    }
    /// A switch's "off" track, a step deeper under the pointer (`hot`).
    fn track_fill_at(&self, hot: bool) -> D2D1_COLOR_F {
        use crate::theme::fills;
        let k = |(d, l): (f32, f32)| if self.light { l } else { d };
        self.wash(k(fills::TRACK) + if hot { k(CTL_HOVER) } else { 0.0 })
    }
    /// What control `h` shows now: pressed while the button is held on it,
    /// hovered under the pointer, else at rest.
    fn state_of(&self, h: Hit) -> crate::theme::State {
        use crate::theme::State;
        if self.pressed == Some(h) {
            State::Pressed
        } else if self.hover == Some(h) {
            State::Hover
        } else {
            State::Rest
        }
    }

    /// A setting's card: Flat's opaque card grey a step off the page;
    /// on Glass a sheet of white over the acrylic (card_layer), its top
    /// edge lit (card_segment); the system's window color in high contrast
    /// (cards show by their text and outlines there).
    fn card_fill(&self) -> D2D1_COLOR_F {
        if let Some(h) = self.hc {
            return rgba(h.window);
        }
        match self.look.flat_card() {
            Some(c) if self.solid => grey(c.rest),
            _ => card_layer(self.light),
        }
    }

    /// Glass A's tone over the window's acrylic: white at the panel
    /// glass's top light (Coat::spec: .06 dark, .18 light) at the top,
    /// gone at the bottom (glass::SPEC_LINE). Nothing off a glass.
    unsafe fn glass_fall(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F) -> Result<()> {
        use windows::core::Interface;
        use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
        use windows::Win32::Graphics::Direct2D::{ID2D1RenderTarget, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES};
        let Some(fx) = self.look.glass_fx(crate::theme::Material::Panel) else { return Ok(()) };
        if fx.spec <= 0.0 {
            return Ok(());
        }
        let h = r.bottom - r.top;
        let stops: Vec<D2D1_GRADIENT_STOP> = crate::glass::SPEC_LINE.iter().map(|(at, a)| D2D1_GRADIENT_STOP { position: *at, color: col(1.0, 1.0, 1.0, fx.spec * a) }).collect();
        let rt: ID2D1RenderTarget = dc.cast()?;
        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: 0.0, Y: r.top }, endPoint: Vector2 { X: 0.0, Y: r.top + h } };
        dc.FillRectangle(&r, &rt.CreateLinearGradientBrush(&props, None, &coll)?);
        Ok(())
    }

    /// The page behind everything when the window has no backdrop (Flat,
    /// transparency off): Flat's panel grey (the built-in theme), else a
    /// plain grey.
    fn page_fill(&self) -> D2D1_COLOR_F {
        match self.look.flat_fill(crate::theme::Material::Panel) {
            Some((top, bottom)) => grey(((top.r as u16 + bottom.r as u16) / 2) as u8),
            None if self.light => col(0.95, 0.95, 0.95, 1.0),
            None => col(0.13, 0.13, 0.13, 1.0),
        }
    }

    /// Resolves the theme being edited (`cfg.theme`) under Windows'
    /// appearance `sys` again: text, tiles, corners, light / dark, high
    /// contrast, the accent and the window's backdrop follow (the caller
    /// redraws). Used when the theme, its mode or accent change here (a
    /// pick, a reset, a revert: redraw checks) and when Windows' appearance
    /// changes.
    fn refresh_look(&mut self, sys: crate::syslook::SysLook) {
        let theme = settings_look(self.cfg.load_theme(&crate::config::app_dir()), &sys, &self.cfg);
        // Drawn pin tiles (the Launchpad pin) are drawn for a light or
        // dark, flat or glass dock: drawn again when that changes.
        if (self.light, self.solid) != (theme.is_light, !theme.glass()) {
            self.pin_icons.borrow_mut().retain(|src, _| !crate::launchpad::is_pin(src));
        }
        self.light = theme.is_light;
        self.solid = !theme.glass();
        self.hc = sys.high_contrast;
        // Its "on" is the dock's accent (Windows' on light by default,
        // white on dark; Highlight in high contrast).
        self.accent = rgba(theme.accent());
        self.accent_ink = rgba(theme.on_accent());
        self.motion = sys.animations;
        self.look = theme;
        self.look_for = (self.cfg.theme.clone(), sys);
        self.look_text = self.cfg.appearance.clone();
        self.look_blur = settings_blur(&sys, &self.cfg);
        crate::icons::set_flat_tiles(self.solid);
        unsafe { window_look(self.hwnd, self.light, self.solid) };
    }
    /// Cards' corners: the theme's tile radius (8).
    fn card_r(&self) -> f32 {
        self.look.radii().tile
    }
    /// Small controls' corners (buttons, fields, segments: 4).
    fn ctl_r(&self) -> f32 {
        self.look.radii().control
    }
    /// An open list's corners (a menu: 12).
    fn menu_r(&self) -> f32 {
        self.look.radii().menu
    }

    /// Text / knob drawn on the "on" tone (dark on the light one, light
    /// on the dark one; HighlightText in high contrast).
    fn on_accent(&self) -> D2D1_COLOR_F {
        self.on_ink()
    }

    /// The "on" state's fill: the accent (Theme::accent: Windows' or the
    /// chosen color on light, near-white on dark by default); Highlight in
    /// high contrast.
    fn on_tone(&self) -> D2D1_COLOR_F {
        if self.tab() == model::Tab::Companion || self.tab().companion_child() {
            rgba(self.look.active())
        } else {
            self.accent
        }
    }

    /// What is drawn on on_tone().
    fn on_ink(&self) -> D2D1_COLOR_F {
        if self.tab() == model::Tab::Companion || self.tab().companion_child() {
            rgba(self.look.on_active())
        } else {
            self.accent_ink
        }
    }

    /// The motion now (crate::motion::mode_for: the dock's own answer for
    /// these settings and Windows' animation effects, whatever the preset).
    fn motion_mode(&self) -> crate::motion::Mode {
        crate::motion::mode_for(&self.cfg, self.fx_env())
    }

    /// Full motion (else switches jump and a page only fades in, quickly).
    fn full_motion(&self) -> bool {
        self.motion_mode() == crate::motion::Mode::Full
    }

    /// The switch kept under `key` moved: its knob springs across.
    fn animate_switch(&self, key: &str) {
        if !self.full_motion() {
            return;
        }
        let now = std::time::Instant::now();
        {
            let mut a = self.anims.borrow_mut();
            a.retain(|(k, _)| k != key);
            a.push((key.to_string(), now));
        }
        self.anim_timer();
    }

    /// How far the switch under `key` has gone toward where it is now
    /// (1: there; a spring, so a little past it on the way).
    fn switch_progress(&self, key: &str) -> f32 {
        let a = self.anims.borrow();
        match a.iter().find(|(k, _)| k == key) {
            Some((_, t0)) => spring_done(crate::motion::SPRING_A, t0.elapsed(), SWITCH_MS),
            None => 1.0,
        }
    }

    /// A new page comes in: it rises a little and fades in (reduced
    /// motion: a short fade only).
    fn animate_page(&mut self) {
        self.page_anim = Some(std::time::Instant::now());
        self.anim_timer();
    }

    /// The page's motion now: (how far it still sits below its place, its
    /// opacity).
    fn page_motion(&self) -> (f32, f32) {
        match self.page_anim {
            Some(t0) => page_frame(t0.elapsed(), self.motion_mode()),
            None => (0.0, 1.0),
        }
    }

    /// Runs the motion timer while anything moves.
    fn anim_timer(&self) {
        unsafe {
            SetTimer(Some(self.hwnd), ANIM_TIMER, 15, None);
        }
    }

    /// One frame of motion: what has finished is dropped, the timer stops
    /// when nothing moves.
    fn anim_tick(&mut self) {
        self.anims.borrow_mut().retain(|(_, t0)| t0.elapsed() < std::time::Duration::from_millis(SWITCH_MS));
        if self.page_anim.is_some_and(|t0| t0.elapsed() >= std::time::Duration::from_millis(page_ms(self.motion_mode()))) {
            self.page_anim = None;
        }
        if self.anims.borrow().is_empty() && self.page_anim.is_none() && !self.tip_moving() && !self.howto_moving() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
            }
        }
        self.redraw();
    }

    /// Something still to save: settings edited.
    fn dirty(&self) -> bool {
        !self.touched.is_empty()
    }

    fn redraw(&mut self) {
        // The theme picked, its mode or accent, the font or the text size
        // changed here (by a row, a reset or a revert): this window follows.
        if self.look_for.0 != self.cfg.theme || self.look_text != self.cfg.appearance || self.look_blur != settings_blur(&self.look_for.1, &self.cfg) {
            self.refresh_look(self.look_for.1);
        }
        // The rows set the picked display's dock only while there is one
        // per display (the displays setting may have just changed).
        self.sync_editing();
        // A question about a dock that is no longer the one picked goes.
        self.drop_stale_confirm();
        // The tooltip follows the pointer and the focus.
        self.sync_tip();
        self.sync_howto();
        let s = self.scale;
        let (pw, ph) = ((win_w() * s).round() as u32, (win_h() * s).round() as u32);
        let mut hits = Vec::new();
        let mut content_h = 0.0;
        let gfx = &self.gfx;
        let me = &*self;
        let surface = gfx.draw(pw, ph, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let t = Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy };
            dc.SetTransform(&t);
            content_h = me.paint(dc, &mut hits)?;
            Ok(())
        });
        self.hits = hits;
        self.content_h = content_h;
        if let Ok(surface) = surface {
            if let Ok(brush) = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) {
                let _ = brush.SetStretch(CompositionStretch::Fill);
                let _ = self.sprite.SetBrush(&brush);
            }
        }
        let _ = self.sprite.SetSize(Vector2 { X: pw as f32, Y: ph as f32 });
        let _ = self.sprite.SetOffset(Vector3 { X: 0.0, Y: 0.0, Z: 0.0 });
        // A search result's row: now that it is laid out, into view.
        if let Some(ri) = self.reveal.take() {
            if self.reveal_row(ri) {
                self.redraw();
                return;
            }
        }
        self.sync_capture();
        // Screen readers hear what is shown now (only while one reads).
        self.publish_a11y();
    }

    /// The tab of the page shown.
    fn tab(&self) -> model::Tab {
        self.sections.get(self.section).map(|s| s.tab).unwrap_or(model::Tab::General)
    }

    /// The pages of the tab shown (more than one: a switch under the title).
    fn tab_pages(&self) -> Vec<usize> {
        model::pages_of(&self.sections, self.tab())
    }

    /// Where the page starts under the title (and the switch between the
    /// tab's pages, if any).
    fn page_top(&self) -> f32 {
        let subtabs = self.welcome.is_none() && self.tab_pages().len() > 1;
        CONTENT_TOP + if subtabs { SUBTABS_SPACE } else { 0.0 }
    }

    /// Where the page's scrolling rows start.
    fn content_top(&self) -> f32 {
        self.page_top()
    }

    /// A list is open (a row's, or the running apps).
    fn list_open(&self) -> bool {
        self.menu.is_some() || self.apps_menu.is_some()
    }

    /// An open list holds the mouse, so a click anywhere (other apps, the
    /// dock, the title bar) reaches this window and closes it. Released
    /// when it closes (a slider or pin drag keeps its own capture).
    fn sync_capture(&self) {
        unsafe {
            let ours = GetCapture() == self.hwnd;
            if self.list_open() && !ours {
                SetCapture(self.hwnd);
            } else if !self.list_open() && self.dragging.is_none() && self.pin_drag.is_none() && ours {
                // WM_CAPTURECHANGED comes back while we are borrowed: ignored.
                let _ = ReleaseCapture();
            }
        }
    }

    /// Closes the open list (if any) and shows it.
    fn close_menu(&mut self) {
        if self.menu.take().is_some() | self.apps_menu.take().is_some() {
            self.redraw();
        }
    }

    /// The rows shown: the first-run step's, or the page's.
    fn rows(&self) -> &[model::Row] {
        match &self.welcome {
            Some(w) => &w.rows,
            None => self.sections.get(self.section).map(|s| s.rows.as_slice()).unwrap_or(&[]),
        }
    }

    /// The height of the scrolling area (the page, or the flow's step).
    fn view_h(&self) -> f32 {
        match &self.welcome {
            Some(w) => w.body.get().1,
            None => win_h() - FOOTER - self.content_top(),
        }
    }

    /// Draws everything in DIPs; returns the content height (for scrolling).
    unsafe fn paint(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>) -> Result<f32> {
        let fill = |c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32| self.fill_round(dc, c, r, rad);
        // Text at (x, y) top-left in DIPs; returns its width.
        let measure = |t: &str, size: f32| -> f32 { self.text_w(t, size, false) };
        let rect = |l: f32, t: f32, r: f32, b: f32| D2D_RECT_F { left: l, top: t, right: r, bottom: b };

        if let Some(h) = self.hc {
            fill(rgba(h.window), rect(0.0, 0.0, win_w(), win_h()), 0.0)?;
        } else if self.solid {
            fill(self.page_fill(), rect(0.0, 0.0, win_w(), win_h()), 0.0)?;
        } else {
            self.glass_fall(dc, rect(0.0, 0.0, win_w(), win_h()))?;
        }
        // Where the open list hangs from (its button).
        let mut menu_anchor: Option<D2D_RECT_F> = None;

        // The first-run flow takes the whole window.
        if self.welcome.is_some() {
            let content_h = self.paint_welcome(dc, hits, &mut menu_anchor)?;
            self.focus_ring(dc, hits)?;
            self.paint_open_list(dc, hits, menu_anchor)?;
            self.paint_tip(dc, hits)?;
            return Ok(content_h);
        }

        // Sidebar: the search box, then the pages in their clusters (or
        // what the search found). Right to left it is on the right: drawn
        // in its own DIPs, moved over (its hits too), mirrored inside.
        let rtl = crate::i18n::rtl();
        let side_dx = self.side_dx();
        let first_hit = hits.len();
        let mut base = Matrix3x2::default();
        dc.GetTransform(&mut base);
        dc.SetTransform(&Matrix3x2 { M31: base.M31 + side_dx * base.M11, ..base });
        let side = (|| -> Result<()> {
            let sm = rows::Mirror { on: rtl, left: 0.0, right: SIDEBAR };
            // Glass: a lighter sheet of the glass; Flat: the page's own
            // grey, one hairline on its inner edge.
            fill(self.sidebar_fill(), rect(0.0, 0.0, SIDEBAR, win_h()), 0.0)?;
            if self.solid && self.hc.is_none() {
                let (lx, lw) = crate::popup::hairline(if rtl { 0.0 } else { SIDEBAR - 1.0 }, self.scale);
                fill(rgba(self.look.hairline()), rect(lx, 0.0, lx + lw, win_h()), 0.0)?;
            }
            self.paint_search_box(dc, hits)?;
            if self.searching() {
                self.paint_results(dc, hits)?;
            } else {
                let shown = self.tab();
                let (tops, _) = model::sidebar_tops(&model::Tab::ALL, TAB_PITCH, CLUSTER_GAP);
                let list = rect(0.0, search_box::LIST_TOP - 4.0, SIDEBAR, win_h() - 6.0);
                dc.PushAxisAlignedClip(&list, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                for (i, tab) in model::Tab::ALL.iter().enumerate() {
                    let y = search_box::LIST_TOP + tops[i] - self.side_scroll;
                    let r = rect(10.0, y, SIDEBAR - 10.0, y + TAB_H);
                    if r.bottom < list.top || r.top > list.bottom {
                        continue;
                    }
                    let current = *tab == shown;
                    // The page shown: the finish's pick (Flat: inverted;
                    // Glass: a bright glass pill; no accent: the sidebar is
                    // chrome; its name keeps its weight, so it does not
                    // change width); Highlight in high contrast.
                    match (current, self.hc) {
                        (true, Some(h)) => fill(rgba(h.highlight), r, self.ctl_r())?,
                        (true, None) => self.pick(dc, r, self.ctl_r())?,
                        _ if self.hover == Some(Hit::Tab(i)) => fill(self.row_fill(self.state_of(Hit::Tab(i))), r, self.ctl_r())?,
                        _ => {}
                    }
                    let ink = if current { self.pick_ink() } else { self.ink() };
                    // Its icon: an achromatic rounded square, the glyph on it.
                    let cy = y + TAB_H / 2.0;
                    let indent=if tab.companion_child() {16.0}else{0.0};
                    let ix = sm.x(18.0 + TAB_ICON / 2.0 + indent);
                    if self.hc.is_none() && !(current && self.solid) {
                        fill(self.ctl_fill(crate::theme::State::Hover), rect(ix - TAB_ICON / 2.0, cy - TAB_ICON / 2.0, ix + TAB_ICON / 2.0, cy + TAB_ICON / 2.0), TAB_ICON_R)?;
                    }
                    self.glyph(dc, tab.glyph(), TAB_GLYPH, ix, cy, ink)?;
                    let room = SIDEBAR - 10.0 - TAB_TEXT_X - 8.0 - indent;
                    // A long translation ends in an ellipsis, not cut at the edge.
                    let name = self.ellipsized(t(tab.title()), rows::tok::BODY, false, room);
                    self.text_box(dc, &name, sm.span(TAB_TEXT_X + indent, room), self.text_top(cy, rows::tok::BODY), rows::tok::BODY, false, room, ink, rtl)?;
                    if let Some(c) = clip_to(r, list) {
                        hits.push((c, Hit::Tab(i)));
                    }
                }
                dc.PopAxisAlignedClip();
                // More of the list above or below: a faint fade says so.
                let max = self.side_scroll_max();
                if self.side_scroll > 0.5 {
                    self.side_fade(dc, list.top, true)?;
                }
                if self.side_scroll < max - 0.5 {
                    self.side_fade(dc, list.bottom, false)?;
                }
            }
            Ok(())
        })();
        dc.SetTransform(&base);
        side?;
        for (r, _) in &mut hits[first_hit..] {
            r.left += side_dx;
            r.right += side_dx;
        }

        // Content, clipped between the title and the footer, scrolled.
        let (left, right) = if rtl { (PAD, win_w() - SIDEBAR - PAD) } else { (SIDEBAR + PAD, win_w() - PAD) };
        let cm = rows::Mirror { on: rtl, left, right };
        let sec = &self.sections[self.section];
        // The page's title (large), then (more than one page in its tab)
        // the switch between them.
        let (rise, alpha) = self.page_motion();
        let pages = self.tab_pages();
        // Over sub-pages the tab's name (they are its switch's names).
        let title = if pages.len() > 1 { t(sec.tab.title()) } else { t(sec.title) };
        let title_x = if rtl { right - self.text_w(title, rows::tok::TITLE_SIZE, true).min(right - left) } else { left };
        if sec.tab != model::Tab::Companion || self.filter_now().is_some() {
            self.text_box(dc, title, title_x, TITLE_TOP, rows::tok::TITLE_SIZE, true, right - title_x, self.ink(), false)?;
        }
        if sec.tab == model::Tab::CompanionAi && self.filter_now().is_none() {
            let privacy = if self.cfg.chat.connected(crate::companion::chat::auth::connected()) && self.cfg.chat.provider == crate::companion::chat::state::Provider::Antigravity { "chat.ai.privacy.agy" } else { "chat.ai.privacy" };
            let ix = if rtl { title_x - 18. } else { title_x + self.text_w(title, rows::tok::TITLE_SIZE, true) + 18. };
            self.paint_info(dc, hits, tooltip::card_tip(privacy), ix, TITLE_TOP + 13., rect(left, 0., right, self.content_top()))?;
        }
        if sec.tab == model::Tab::Agents && self.filter_now().is_none() {
            let ix = if rtl { title_x - 18. } else { title_x + self.text_w(title, rows::tok::TITLE_SIZE, true) + 18. };
            self.paint_info(dc, hits, tooltip::card_tip("settings.intro.agents"), ix, TITLE_TOP + 13., rect(left, 0., right, self.content_top()))?;
        }
        if pages.len() > 1 {
            self.paint_subtabs(dc, hits, &pages, cm)?;
        }
        let content = if rtl { (0.0, win_w() - SIDEBAR) } else { (SIDEBAR, win_w()) };
        let top = self.content_top();
        let clip = rect(content.0, top - 8.0, content.1, win_h() - FOOTER);
        dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        // A page coming in rises into place and fades in (its hits follow
        // the drawing; the motion is over in a moment).
        let fading = alpha < 0.999;
        if fading {
            let lp = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS1 {
                contentBounds: clip,
                geometricMask: std::mem::ManuallyDrop::new(None),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: alpha,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: windows::Win32::Graphics::Direct2D::D2D1_LAYER_OPTIONS1_NONE,
            };
            dc.PushLayer(&lp, None::<&windows::Win32::Graphics::Direct2D::ID2D1Layer>);
        }
        let mut y = top - self.scroll + rise;
        let start = y;
        self.flow.set(layout::Block::Start);
        // (A test logs the page's content only: not the sidebar or title.)
        #[cfg(test)]
        if let Some(v) = self.log.borrow_mut().as_mut() {
            v.clear();
        }
        // No sentence over the page (a screen reader still hears it:
        // access.rs); while the search narrows it, what it shows and how to
        // see all.
        if self.filter_now().is_some() {
            y = self.paint_filter_note(dc, hits, y, left, right, clip)?;
        }
        let filtering = self.filter_now().is_some();
        if sec.kind == model::Kind::TopBar && !filtering {
            y = self.paint_bar_view(dc, hits, y, left, right, clip)?;
        }
        if sec.kind == model::Kind::CompanionAi && !filtering {
            y = self.paint_companion_ai(dc,hits,y,left,right,clip)?;
        }
        y = self.paint_rows(dc, hits, &sec.rows, y, left, right, clip, rtl, &mut menu_anchor)?;
        y = match sec.kind {
            _ if filtering => y,
            model::Kind::Pins => {
                // Whose apps: the dock picked (with a dock on every display).
                let y = self.paint_display_picker(dc, hits, y, left, right, clip)?;
                self.paint_pins(dc, hits, y, left, right, clip)?
            }
            model::Kind::About => self.paint_about(dc, hits, y, left, right, clip)?,
            model::Kind::Input => self.paint_input(dc, hits, y, left, right, clip)?,
            model::Kind::Calendar => self.paint_calendar(dc, hits, y, left, right, clip)?,
            model::Kind::CompanionAi | model::Kind::Rows | model::Kind::TopBar | model::Kind::Dock | model::Kind::Windows | model::Kind::Shortcuts => y,
        };
        // One reset in the whole window: "Reset all settings" under General.
        if !filtering && sec.tab == model::Tab::General {
            y = self.paint_reset_all(dc, hits, y, left, right, clip)?;
        }
        // The same space under every page's last card.
        if self.flow.get() != layout::Block::Start {
            y += layout::PAGE_END;
        }
        let content_h = y - start + rise;
        if fading {
            dc.PopLayer();
        }
        dc.PopAxisAlignedClip();

        // No footer: changes apply and save as they are made. Something
        // that went wrong (a save refused) shows over the page's foot.
        self.focus_ring(dc, hits)?;
        if !self.note.is_empty() {
            let note = self.note.as_str();
            fill(self.footer_fill(), rect(content.0, win_h() - NOTE_H, content.1, win_h()), 0.0)?;
            let by = win_h() - NOTE_H / 2.0;
            let maxw = right - left;
            let nx = cm.span(left, maxw);
            let small = rows::tok::FOOTNOTE;
            dc.PushAxisAlignedClip(&rect(nx, win_h() - NOTE_H + 2.0, nx + maxw, win_h() - 2.0), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let r = if measure(note, small) <= maxw {
                self.text_box(dc, note, nx, self.text_top(by, small), small, false, maxw, self.dim(), rtl)
            } else {
                // Long notes (some translations) wrap to two lines.
                self.para_dir(dc, note, nx, by - line_h(self.gfx.dwrite(), small), small, false, maxw, self.dim(), rtl).map(|_| ())
            };
            dc.PopAxisAlignedClip();
            r?;
        }
        self.paint_open_list(dc, hits, menu_anchor)?;
        if !self.list_open() {
            self.paint_bar_tip(dc, hits)?;
        }
        self.paint_tip(dc, hits)?;
        self.paint_howto(dc)?;
        self.paint_install(dc,hits)?;
        Ok(content_h)
    }

    /// The sidebar's tone: Glass a lighter sheet of the glass; Flat none
    /// (the page's grey: a hairline parts them); the window color in high
    /// contrast.
    fn sidebar_fill(&self) -> D2D1_COLOR_F {
        match self.hc {
            Some(h) => rgba(h.window),
            None if self.solid => col(0.0, 0.0, 0.0, 0.0),
            None => col(1.0, 1.0, 1.0, if self.light { 0.28 } else { 0.04 }),
        }
    }

    /// The footer's tone (a smaller step than the sidebar's).
    fn footer_fill(&self) -> D2D1_COLOR_F {
        match self.hc {
            Some(h) => rgba(h.window),
            None => self.wash(0.02),
        }
    }

    /// The sidebar list's whole height and how far it can scroll in the
    /// window's height now.
    fn side_scroll_max(&self) -> f32 {
        let (_, h) = model::sidebar_tops(&model::Tab::ALL, TAB_PITCH, CLUSTER_GAP);
        side_scroll_max(h, win_h())
    }

    /// A fade at the sidebar list's top or bottom edge `y`: more of it
    /// that way.
    unsafe fn side_fade(&self, dc: &ID2D1DeviceContext, y: f32, top: bool) -> Result<()> {
        if self.hc.is_some() {
            return Ok(());
        }
        // One smooth gradient (not stacked bands): the row hover's tone at
        // the edge, nothing SIDE_FADE further in.
        use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
        use windows::Win32::Graphics::Direct2D::{D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_COLOR_INTERPOLATION_MODE_STRAIGHT, D2D1_COLOR_SPACE_SRGB, D2D1_EXTEND_MODE_CLAMP, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES};
        let c = self.row_fill(crate::theme::State::Hover);
        let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: c }, D2D1_GRADIENT_STOP { position: 1.0, color: D2D1_COLOR_F { a: 0.0, ..c } }];
        let coll = dc.CreateGradientStopCollection(&stops, D2D1_COLOR_SPACE_SRGB, D2D1_COLOR_SPACE_SRGB, D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_EXTEND_MODE_CLAMP, D2D1_COLOR_INTERPOLATION_MODE_STRAIGHT)?;
        let far = if top { y + SIDE_FADE } else { y - SIDE_FADE };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: 0.0, Y: y }, endPoint: Vector2 { X: 0.0, Y: far } };
        let b = dc.CreateLinearGradientBrush(&props, None, &coll)?;
        dc.FillRectangle(&D2D_RECT_F { left: 0.0, top: y.min(far), right: SIDEBAR, bottom: y.max(far) }, &b);
        Ok(())
    }

    /// The sidebar entry `k` scrolled into view (the keyboard reached it).
    fn reveal_tab(&mut self, k: usize) {
        let (tops, _) = model::sidebar_tops(&model::Tab::ALL, TAB_PITCH, CLUSTER_GAP);
        let Some(&top) = tops.get(k) else { return };
        let room = win_h() - 6.0 - search_box::LIST_TOP;
        let s = reveal_span(self.side_scroll, top, TAB_H, room).clamp(0.0, self.side_scroll_max());
        self.side_scroll = s;
    }

    /// The tab's pages under its title, as a segmented switch (mirrored
    /// right to left).
    unsafe fn paint_subtabs(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, pages: &[usize], cm: rows::Mirror) -> Result<()> {
        let rect = |l: f32, t: f32, r: f32, b: f32| D2D_RECT_F { left: l, top: t, right: r, bottom: b };
        let (top, bottom) = (SUBTABS_TOP, SUBTABS_TOP + SUBTABS_H);
        // The names, their padding narrowing (28 down to 12) and then the
        // text a ramp step smaller (body to footnote, never in between);
        // still too wide (long translations at the smallest window), the
        // longest names are cut to a shared width with an ellipsis: the
        // switch and its click areas never run past the page.
        let room = cm.right - cm.left - 4.0;
        let names: Vec<&str> = pages.iter().map(|&i| t(self.sections[i].title)).collect();
        let mut size = rows::tok::BODY;
        let mut text: Vec<f32> = names.iter().map(|n| self.text_w(n, size, true)).collect();
        if text.iter().sum::<f32>() + 12.0 * names.len() as f32 > room {
            size = rows::tok::FOOTNOTE;
            text = names.iter().map(|n| self.text_w(n, size, true)).collect();
        }
        let (fit, pad) = fit_subtabs(&text, room);
        let widths: Vec<f32> = fit.iter().map(|w| w + pad).collect();
        let total: f32 = widths.iter().sum();
        let mut x = cm.left;
        self.fill_round(dc, self.wash(0.06), cm.r(rect(x - 2.0, top - 2.0, x + total + 2.0, bottom + 2.0)), self.ctl_r())?;
        for (k, &i) in pages.iter().enumerate() {
            let r = cm.r(rect(x, top, x + widths[k], bottom));
            let cur = i == self.section;
            let inner = self.ctl_r() - rows::tok::SEGMENT_INSET;
            if cur {
                self.pick(dc, r, inner)?;
            } else if self.hover == Some(Hit::Section(i)) {
                self.fill_round(dc, self.wash(0.05), r, inner)?;
            }
            let fg = if cur { self.pick_ink() } else { self.dim() };
            let tw = fit[k];
            let name = if text[k] > tw + 0.5 { self.ellipsized(names[k], size, true, tw) } else { names[k].to_string() };
            self.text_box(dc, &name, r.left + pad / 2.0, top + 5.0 + (rows::tok::BODY - size) / 2.0, size, cur, tw + 2.0, fg, false)?;
            hits.push((r, Hit::Section(i)));
            x += widths[k];
        }
        Ok(())
    }

    /// `s` cut to `maxw` DIPs with an ellipsis (whole if it fits).
    fn ellipsized(&self, s: &str, size: f32, bold: bool, maxw: f32) -> String {
        if self.text_w(s, size, bold) <= maxw {
            return s.to_string();
        }
        let chars: Vec<char> = s.chars().collect();
        for n in (0..chars.len()).rev() {
            let cut: String = chars[..n].iter().collect::<String>().trim_end().to_string() + "\u{2026}";
            if self.text_w(&cut, size, bold) <= maxw || n == 0 {
                return cut;
            }
        }
        "\u{2026}".into()
    }

    /// Shows the page and row of setting `key` (its fold opened, the row
    /// scrolled into view and lit). False if no page lists it.
    fn open_row(&mut self, key: &str) -> bool {
        match model::find_row(&self.sections, key) {
            Some((section, row)) => {
                self.jump(model::Found { section, row: Some(row) });
                true
            }
            None => false,
        }
    }

    /// The keyboard focus ring (once the keyboard is used).
    unsafe fn focus_ring(&self, dc: &ID2D1DeviceContext, hits: &[(D2D_RECT_F, Hit)]) -> Result<()> {
        if let (true, Some(f)) = (self.keyboard, self.focus) {
            if let Some((r, _)) = hits.iter().find(|(_, h)| same(*h, f)) {
                let ring = if let Some(h) = self.hc { rgba(h.highlight) } else { self.ink() };
                let b = dc.CreateSolidColorBrush(&ring, None)?;
                // One ring everywhere (design B3): RING_W, RING_GAP outside
                // the control, concentric with its own corners.
                let (rr, rad) = focus_ring_rect(f, *r, self.ctl_r(), self.card_r());
                self.note(layout::Kind::Ring, &format!("{f:?}"), D2D_RECT_F { left: rr.left - rows::tok::RING_W / 2.0, top: rr.top - rows::tok::RING_W / 2.0, right: rr.right + rows::tok::RING_W / 2.0, bottom: rr.bottom + rows::tok::RING_W / 2.0 });
                // On whole device pixels (states-issues 1).
                let (rr, rad, w) = crate::gfx::parts::on_device_pixels(dc, rr, rad, rows::tok::RING_W);
                dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rr, radiusX: rad, radiusY: rad }, &b, w, None);
            }
        }
        Ok(())
    }

    /// The open list, over everything (its hits, pushed last, win).
    unsafe fn paint_open_list(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, anchor: Option<D2D_RECT_F>) -> Result<()> {
        if let (Some((ri, hi)), Some(anchor)) = (self.menu, anchor) {
            if let Some((opts, get, _)) = self.rows().get(ri).and_then(|r| r.ctl.options()) {
                let labels: Vec<&str> = opts.iter().map(|o| model::opt_text(o)).collect();
                let shown = self.paint_list(dc, hits, anchor, &labels, None, Some(get(&self.cfg)), hi, self.menu_first, &|oi| Hit::MenuItem(ri, oi), 180.0, crate::i18n::rtl())?;
                self.list_shown.set(shown);
            }
        }
        self.paint_apps_menu(dc, hits)
    }

    /// Where the sidebar is drawn from (DIP): the left edge, or (right to
    /// left, outside the first-run flow) the right side.
    fn side_dx(&self) -> f32 {
        if self.welcome.is_none() && crate::i18n::rtl() { win_w() - SIDEBAR } else { 0.0 }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(_, h)| *h)
    }

    /// Row `ri` changed in the window: show it on the dock.
    fn changed(&mut self, ri: usize) {
        if let Some(key) = self.rows().get(ri).map(|r| r.key) {
            // A display's own dock (or its switch): sent as the map of
            // per-display docks, never as the main dock's field.
            let own = key == model::SAME_AS_MAIN || (model::DISPLAY_ROWS.contains(&key) && model::editing_own(&self.cfg));
            self.touch(if own { "dock.per_display" } else { key });
            // The first-run flow's language row: this window speaks it too.
            if key == "language" {
                self.language_changed();
            }
            if key.starts_with("shortcuts.") {
                // Free keys are looked for again with the new set.
                self.alts.borrow_mut().clear();
            }
        }
        self.send_preview();
    }

    /// The dock saved new pins (dragged on, off or around it): show them.
    /// Pin edits made here and not saved yet stay on top (same merge as a
    /// save); nothing else in the window changes.
    fn pins_saved_elsewhere(&mut self) {
        // Not under a pin being dragged: once it is dropped (pin_drag_end).
        if self.pin_drag.is_some() {
            self.pins_pending = true;
            return;
        }
        self.pins_pending = false;
        let Some(mut disk) = crate::config::read_current() else { return };
        let keys: Vec<String> = self.touched.iter().filter(|k| *k == "dock.pins" || *k == "dock.per_display" || k.starts_with("-pin") || k.starts_with("+pin")).cloned().collect();
        model::apply_keys(&self.cfg, &mut disk, &keys);
        let mut changed = false;
        if disk.dock.pins != self.cfg.dock.pins {
            self.cfg.dock.pins = disk.dock.pins;
            changed = true;
        }
        // Each display's own apps (its other settings stay as edited here).
        for (k, d) in self.cfg.dock.per_display.iter_mut() {
            if let Some(p) = disk.dock.per_display.get(k).and_then(|x| x.pins.clone()) {
                if d.pins.as_ref() != Some(&p) {
                    d.pins = Some(p);
                    changed = true;
                }
            }
        }
        if changed {
            self.redraw();
        }
    }

    /// `key` was edited (sent with the next preview and save). A pin added
    /// back is no longer removed, and the other way round.
    fn touch(&mut self, key: &str) {
        // "+pin:x" / "-pin:x" (and a display's "+pin@<key>:x").
        let undo = match (key.strip_prefix("+pin"), key.strip_prefix("-pin")) {
            (Some(p), _) if p.starts_with([':', '@']) => Some(format!("-pin{p}")),
            (_, Some(p)) if p.starts_with([':', '@']) => Some(format!("+pin{p}")),
            _ => None,
        };
        if let Some(u) = undo {
            self.touched.retain(|k| *k != u);
        }
        if !self.touched.iter().any(|k| k == key) {
            self.touched.push(key.to_string());
        }
        // Saved a moment after the last change (apply.rs).
        self.save_soon();
    }

    /// `cfg.language` changed: this window speaks it too (the redraw follows).
    fn language_changed(&mut self) {
        crate::i18n::init(&self.cfg.language);
        // A note in the old language would linger.
        self.note.clear();
        // The built-in theme's name is text.
        self.themes = extra::list_themes();
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &windows::core::HSTRING::from(t("settings.window_title")));
        }
    }

    /// The edits on the dock at once (the save follows: save_soon). A
    /// preview the dock refused is not a loss: the save carries them.
    fn send_preview(&mut self) {
        if self.client.online() && !self.client.preview(&self.cfg, &self.touched) {
            crate::log!("settings: preview not applied; the save carries the change");
        }
        self.redraw();
    }

    fn row_ctl(&self, ri: usize) -> Option<&Ctl> {
        self.rows().get(ri).map(|r| &r.ctl)
    }

    fn set_slider(&mut self, ri: usize, x0: f32, tw: f32, x: f32) {
        if let Some(Ctl::Slider { set, min, max, step, get, .. }) = self.row_ctl(ri) {
            let (set, get, min, max, step) = (*set, *get, *min, *max, *step);
            let v = model::snap(min + (x - x0) / tw * (max - min), min, max, step);
            if v != get(&self.cfg) {
                set(&mut self.cfg, v);
                self.changed(ri);
            }
        }
    }

    fn click(&mut self, h: Hit, x: f32) {
        // A click anywhere else ends renaming a calendar (the name typed
        // is in the config already: Save, a page change or closing keep it).
        if h != Hit::CalName {
            self.cal_end_rename();
        }
        // A click anywhere else ends recording a shortcut.
        if self.recording.is_some() && !self.keeps_recording(h) {
            self.stop_recording();
        }
        if !matches!(h, Hit::Shortcut(_) | Hit::ShortcutReset(_)) {
            self.rec_note = None;
        }
        // A pending reset question goes away when anything else is used.
        if self.confirm.is_some() && !matches!(h, Hit::ResetYes | Hit::ResetNo | Hit::ResetAll) {
            self.confirm = None;
        }
        match h {
            Hit::Chat(action) => self.chat_action(action),
            // An "i": its tip at once (the pointer's wait skipped).
            Hit::Info(ri) => {
                self.show_tip(ri);
                self.redraw();
            }
            // An accent color: picked like the list's option ("Custom…"
            // opens Windows' color dialog).
            Hit::Swatch(ri, oi) => self.pick_swatch(ri, oi),
            Hit::Shortcut(ri) => self.click_shortcut(ri),
            Hit::ShortcutReset(ri) => self.reset_shortcut(ri),
            Hit::RowFix(ri) => {
                if let Some((_, Some(fix))) = self.rows().get(ri).and_then(|r| self.row_note(r.key)) {
                    fix.run(self.hwnd);
                }
            }
            Hit::Tab(k) => {
                // A tab opens its first page (staying on the page shown if
                // it is one of the tab's).
                let Some(tab) = model::Tab::ALL.get(k).copied() else { return };
                if self.tab() != tab {
                    if let Some(&first) = model::pages_of(&self.sections, tab).first() {
                        self.click(Hit::Section(first), 0.0);
                    }
                }
                self.focus = Some(Hit::Tab(k));
                self.reveal_tab(k);
                self.redraw();
            }
            Hit::FilterAll => {
                // The page whole again (the search and its results stay).
                self.filtering = false;
                self.scroll = 0.0;
                self.focus = Some(Hit::Search);
                self.redraw();
            }
            Hit::Fold(ri) => {
                let open = self.fold_open(self.rows(), ri);
                if let Some(key) = self.rows().get(ri).map(|r| r.key) {
                    if open {
                        self.folds.retain(|k| *k != key);
                    } else if !self.folds.contains(&key) {
                        self.folds.push(key);
                    }
                }
                self.redraw();
            }
            Hit::Display(i) => self.select_display(i),
            Hit::Identify => {
                self.focus = Some(Hit::Identify);
                self.identify();
                self.redraw();
            }
            Hit::BarCell(ri) => {
                if let Some(key) = self.rows().get(ri).map(|r| r.key) {
                    self.open_row(key);
                }
            }
            Hit::Section(i) => self.go_to_section(i, Nav::Open),
            Hit::Toggle(ri) if self.welcome.is_none() && self.rows().get(ri).map(|r| r.key) == Some(model::SAME_AS_MAIN) => self.toggle_same_as_main(ri),
            Hit::SameUndo => self.same_as_main_undo(),
            Hit::PinsCustomize => self.click_pins(h),
            Hit::Toggle(ri) if self.rows().get(ri).is_some_and(|r| self.row_locked(r.key)) => {}
            Hit::Toggle(ri)
                if self
                    .rows()
                    .get(ri)
                    .is_some_and(|r| agent_notifications::provider(r.key).is_some()) =>
            {
                let (provider,on)=match self.rows()[ri].key {"chat.claude_mcp"=>(0,!self.cfg.chat.claude_mcp),"chat.codex_mcp"=>(1,!(self.cfg.agents.codex || self.cfg.chat.codex_mcp)),_=>(2,!self.cfg.chat.antigravity_mcp)};
                crate::companion::chat::settings::set_mcp(self.hwnd,provider,on);
                self.redraw();
            }
            Hit::Toggle(ri) => {
                if let Some(Ctl::Toggle { get, set }) = self.row_ctl(ri) {
                    let (get, set) = (*get, *set);
                    let v = !get(&self.cfg);
                    set(&mut self.cfg, v);
                    if let Some(key) = self.rows().get(ri).map(|r| r.key) {
                        self.animate_switch(key);
                    }
                    self.changed(ri);
                }
            }
            Hit::Choice(ri, oi) => {
                if let Some(Ctl::Choice { set, .. }) = self.row_ctl(ri) {
                    let set = *set;
                    set(&mut self.cfg, oi);
                    self.changed(ri);
                }
            }
            Hit::Slider(ri, x0, tw) => self.set_slider(ri, x0, tw, x),
            Hit::Menu(ri) => {
                self.apps_menu = None;
                pins::cancel_scan();
                self.menu = match self.menu {
                    Some((r, _)) if r == ri => None,
                    _ => self.row_ctl(ri).and_then(|c| c.options()).map(|(_, get, _)| (ri, get(&self.cfg))),
                };
                // The chosen option in view (long lists scroll).
                self.menu_first = self.menu.map(|(_, cur)| cur.saturating_sub(3)).unwrap_or(0);
                self.redraw();
            }
            Hit::MenuItem(ri, oi) => self.pick_menu(ri, oi),
            Hit::MenuDismiss => self.close_menu(),
            Hit::ReportProblem => {
                if let Some(note) = report::open(self.hwnd) {
                    self.note = note;
                }
                self.redraw();
            }
            Hit::UpdateCheck | Hit::UpdateDownload | Hit::UpdateAuto | Hit::UpdateInstall => self.click_update(h),
            Hit::PinRow(i) => {
                self.focus = Some(Hit::PinRow(i));
                self.redraw();
            }
            Hit::PinIcon(_) | Hit::PinReset(_) | Hit::PinRemove(_) | Hit::PinAdd | Hit::PinAddRunning | Hit::AppItem(_) => self.click_pins(h),
            Hit::InputAdd | Hit::InputSuggest(_) | Hit::InputMode(_) | Hit::InputRemove(_) => self.click_input(h),
            Hit::CalUrl | Hit::CalAdd | Hit::CalOpen | Hit::CalName | Hit::CalRename(_) | Hit::CalRemove(_) => self.click_calendar(h),
            Hit::Search => {
                self.focus = Some(Hit::Search);
                self.place_ime();
                self.redraw();
            }
            Hit::SearchClear => {
                self.focus = Some(Hit::Search);
                self.clear_search();
            }
            Hit::SearchResult(k) => {
                if let Some(f) = self.results().get(k).copied() {
                    self.jump(f);
                }
            }
            Hit::NewNote(_) => { if self.cfg.widgets.notes { self.client.new_note(); } },
            Hit::ResetAll | Hit::ResetYes | Hit::ResetNo => self.click_reset(h),
            Hit::WelcomeCard(k) => {
                self.focus = Some(Hit::WelcomeCard(k));
                self.pick_taskbar(k);
            }
            Hit::WelcomeBack | Hit::WelcomeNext | Hit::WelcomeSkip => self.welcome_move(h),
            Hit::Theme(_) | Hit::Link(_) => self.click_extra(h),
            Hit::Button(ri) if matches!(self.row_ctl(ri), Some(Ctl::Navigate { .. })) => {
                if let Some(Ctl::Navigate { tab, .. }) = self.row_ctl(ri) {
                    if let Some(i) = model::Tab::ALL.iter().position(|candidate| candidate == tab) {
                        self.click(Hit::Tab(i), 0.0);
                    }
                }
            }
            // Theme files go in this folder (made if it is not there yet).
            Hit::Button(ri) if self.rows().get(ri).map(|r| r.key) == Some("action.themes_folder") => {
                let dir = crate::config::app_dir().join("themes");
                let _ = std::fs::create_dir_all(&dir);
                extra::open_url(&dir.to_string_lossy());
            }
            // Launchpad's clean-up banner again (nothing is lost: no question).
            // Quit: at once (General, "Quit NopBar"). Launchpad's reset is
            // asked first; the box runs its own message loop, so it shows
            // outside this borrow (WM_ASK_LP_RESET).
            Hit::Button(ri)
                if self
                    .rows()
                    .get(ri)
                    .is_some_and(|r| r.key.starts_with("chat.")) =>
            {
                let key = self.rows()[ri].key;
                match key {
                    "chat.history" | "chat.model" | "chat.account" | "chat.disconnect" | "chat.login" => self.chat_action(key),
                    _ => crate::companion::chat::settings::begin(self.hwnd, key),
                }
                self.redraw();
            }
            Hit::Button(ri) => match self.rows().get(ri).map(|r| r.key) {
                Some("action.quit") => self.quit_and_restore(),
                Some("action.capture_privacy") => {
                    self.open_row("widgets.hide_from_capture");
                }
                Some("action.launchpad_reset") => unsafe {
                    let _ = PostMessageW(Some(self.hwnd), WM_ASK_LP_RESET, WPARAM(0), LPARAM(0));
                },
                _ => {}
            },
        }
    }

    /// Page `i` opened (`Nav::Open`: it rises in, and what page_change
    /// says ends, ends) or previewed by the search while typing (only the
    /// view changes).
    fn go_to_section(&mut self, i: usize, by: Nav) {
        if page_change(self.section, i, by) {
            // An undo offered on another page goes with it.
            self.same_undo = None;
        }
        self.show_section(i, by == Nav::Open);
    }

    /// Shows page `i` (`motion`: it rises in). Only the view changes here:
    /// what is being edited ends in go_to_section.
    fn show_section(&mut self, i: usize, motion: bool) {
        if i >= self.sections.len() {
            return;
        }
        // Another page: the widget row under the pointer is not there.
        if self.section != i {
            self.howto_hover = None;
        }
        let kind = self.sections[i].kind;
        if kind == model::Kind::About && self.section != i {
            // The dock may have checked meanwhile.
            if !self.upd_checking {
                self.upd = crate::updates::load_state();
            }
        }
        self.menu = None;
        self.apps_menu = None;
        if self.section != i {
            // A list of running apps asked for there is not opened here.
            pins::cancel_scan();
        }
        self.confirm = None;
        self.stop_recording();
        self.rec_note = None;
        if self.section != i && motion {
            self.animate_page();
        }
        self.section = i;
        self.scroll = 0.0;
        self.redraw();
    }

    /// Saves what is still waiting: the edited settings (apply.rs runs it
    /// a moment after a change, and on closing). What failed stays to save
    /// again (and says so). True when nothing is left.
    fn save(&mut self) -> bool {
        self.save_timer_off();
        if !self.dirty() {
            return true;
        }
        if !self.touched.is_empty() {
            let startup = self.touched.iter().any(|k| k == "start_with_windows");
            let wanted = self.cfg.start_with_windows;
            match self.client.save(&self.cfg, &mut self.touched) {
                Ok(_) => {
                    if startup {
                        // Checked, not assumed: the switch and its note
                        // show what Windows does now.
                        self.verify_startup(wanted);
                    }
                }
                Err(e) => {
                    crate::log!("settings: save failed: {e}");
                    self.note = t("settings.note.save_failed").into();
                    self.save_later();
                    self.redraw();
                    return false;
                }
            }
        }
        // A save that worked clears what a failed one said.
        if self.note == t("settings.note.save_failed") {
            self.note.clear();
        }
        self.journal_done();
        self.redraw();
        true
    }

    /// "Turn off and restore" after the user said yes: stops the dock (it gives the
    /// taskbar back itself) and closes. The taskbar is restored from here
    /// only once the dock is known to be gone; if it is still running, the
    /// window stays and says so.
    /// Settings > Search > Launchpad, "Reset to alphabetical" (after a
    /// yes): its pages and folders go, every app in name order again. Saved
    /// at once (only that field).
    fn reset_launchpad(&mut self) {
        // Home empty and schema 0: the next open makes every app Home again,
        // by name (hidden apps, categories and the Library switch stay).
        self.cfg.launchpad.layout.clear();
        self.cfg.launchpad.schema = 0;
        match self.client.commit(&self.cfg, &["launchpad.layout".to_string()]) {
            Ok(_) => {
                if !self.touched.is_empty() {
                    let _ = self.client.preview(&self.cfg, &self.touched);
                }
                self.note = t("settings.launchpad.reset.done").into();
            }
            Err(e) => {
                crate::log!("settings: Launchpad reset not saved: {e}");
                self.note = t("settings.note.save_failed").into();
            }
        }
        self.redraw();
    }

    fn quit_and_restore(&mut self) {
        let saved = self.close();
        if stop_running_dock(5) {
            // A save the dock refused goes to the file now that it is gone
            // (the link drops: offline); what still fails is journaled.
            if !saved && !self.flush_now() {
                self.write_journal();
            }
            crate::taskbar::restore_before_exit(std::time::Duration::from_secs(10));
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        } else {
            self.note = t("settings.note.quit_failed").into();
            self.redraw();
        }
    }

    /// Closes the window soon (outside this borrow): WM_LEAVE_NOW.
    fn post_leave_now(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_LEAVE_NOW, WPARAM(0), LPARAM(0));
        }
    }

    /// Controls the keyboard can reach, in order (only the chosen option of
    /// a choice row).
    fn focusables(&self) -> Vec<Hit> {
        let rows = self.rows();
        // In the order drawn: a row's "i" before its control.
        self.hits
            .iter()
            .map(|(_, h)| *h)
            .filter(|h| {
                let picked = row_of(*h).and_then(|ri| rows.get(ri)).and_then(|r| r.ctl.options().map(|(_, get, _)| get(&self.cfg)));
                focusable(*h, picked)
            })
            .collect()
    }

    /// Where control `h` was last drawn (cut to what is in sight).
    fn hit_rect(&self, h: Hit) -> Option<D2D_RECT_F> {
        self.hits.iter().find(|(_, x)| same(*x, h)).map(|(r, _)| *r)
    }

    /// The scrolling part of the window (the page under its title, or the
    /// first-run step), as drawn.
    fn body_rect(&self) -> D2D_RECT_F {
        if let Some(w) = &self.welcome {
            let (top, h) = w.body.get();
            return D2D_RECT_F { left: 0.0, top: top - 6.0, right: win_w(), bottom: top + h };
        }
        let (l, r) = if crate::i18n::rtl() { (0.0, win_w() - SIDEBAR) } else { (SIDEBAR, win_w()) };
        D2D_RECT_F { left: l, top: self.content_top() - 8.0, right: r, bottom: win_h() - FOOTER }
    }

    /// Tab / Shift+Tab: the next control in reading order (`list`, the
    /// focus at `pos`). Leaving the page while more of it is out of sight
    /// that way scrolls it on instead, and the next setting that comes
    /// into view takes the focus: the keyboard reaches every setting, not
    /// only those on screen.
    fn tab_step(&mut self, list: &[Hit], pos: Option<usize>, fwd: bool) {
        let n = list.len();
        if n == 0 {
            return;
        }
        let body = self.body_rect();
        let inside = |r: &D2D_RECT_F| r.left >= body.left - 0.5 && r.right <= body.right + 0.5 && r.top >= body.top - 0.5 && r.bottom <= body.bottom + 0.5;
        let next_i = match pos {
            Some(i) if fwd => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None if fwd => 0,
            None => n - 1,
        };
        let cur = pos.map(|i| list[i]).and_then(|h| self.hit_rect(h)).filter(|r| inside(r));
        let next_in = self.hit_rect(list[next_i]).is_some_and(|r| inside(&r));
        let wraps = pos.is_some_and(|i| if fwd { next_i <= i } else { next_i >= i });
        let max = (self.content_h - self.view_h() + 16.0).max(0.0);
        let more = if fwd { self.scroll < max - 0.5 } else { self.scroll > 0.5 };
        if let (Some(cur), true, true) = (cur, more, !next_in || wraps) {
            let step = (self.view_h() - 64.0).max(48.0);
            // Where the control left is now (it moves up as the page scrolls).
            let mut was = cur.top;
            // On until a control comes into view past it, or the page ends
            // (a long stretch of text has none).
            loop {
                let max = (self.content_h - self.view_h() + 16.0).max(0.0);
                let old = self.scroll;
                self.scroll = if fwd { (old + step).min(max) } else { (old - step).max(0.0) };
                let d = self.scroll - old;
                if d.abs() < 0.5 {
                    break;
                }
                self.redraw();
                was -= d;
                let after = |r: &D2D_RECT_F| if fwd { r.top > was + 0.5 } else { r.top < was - 0.5 };
                let found: Vec<Hit> = self.focusables().into_iter().filter(|h| self.hit_rect(*h).is_some_and(|r| inside(&r) && after(&r))).collect();
                if let Some(h) = if fwd { found.first() } else { found.last() }.copied() {
                    self.focus = Some(h);
                    self.reveal_focus();
                    return;
                }
            }
            // The end of the page and nothing more on it: on to what comes
            // after the page (the footer; backwards the sidebar), never left
            // on a control out of sight.
            let list = self.focusables();
            let body_at: Vec<usize> = list.iter().enumerate().filter(|(_, h)| self.hit_rect(**h).is_some_and(|r| inside(&r))).map(|(i, _)| i).collect();
            let at = if fwd {
                body_at.last().map(|i| i + 1).filter(|i| *i < list.len()).unwrap_or(0)
            } else {
                body_at.first().and_then(|i| i.checked_sub(1)).unwrap_or(list.len().saturating_sub(1))
            };
            self.focus = list.get(at).copied();
            return;
        }
        self.focus = Some(list[next_i]);
        self.reveal_focus();
    }

    /// The focused control is cut by the top or bottom of the page: the
    /// page scrolls until it is whole.
    fn reveal_focus(&mut self) {
        let Some(f) = self.focus else { return };
        for _ in 0..8 {
            let body = self.body_rect();
            let Some(r) = self.hit_rect(f) else { return };
            // Not on the page (the sidebar, the footer, an open list).
            if r.top < body.top || r.top >= body.bottom || r.right <= body.left || r.left >= body.right {
                return;
            }
            let max = (self.content_h - self.view_h() + 16.0).max(0.0);
            let s = if r.bottom >= body.bottom - 0.5 && self.scroll < max {
                (self.scroll + 40.0).min(max)
            } else if r.top <= body.top + 8.5 && self.scroll > 0.0 {
                (self.scroll - 40.0).max(0.0)
            } else {
                return;
            };
            self.scroll = s;
            self.redraw();
        }
    }

    /// A press of the pointer: the keyboard's ring goes (crate::input).
    fn pointer_down(&mut self) {
        if std::mem::replace(&mut self.keyboard, false) {
            self.redraw();
        }
    }

    fn key(&mut self, vk: u16, shift: bool, ctrl: bool, repeat: bool) -> bool {
        const TAB: u16 = 0x09;
        const ENTER: u16 = 0x0D;
        const ESC: u16 = 0x1B;
        const SPACE: u16 = 0x20;
        const HOME: u16 = 0x24;
        const END: u16 = 0x23;
        const LEFT: u16 = 0x25;
        const UP: u16 = 0x26;
        const RIGHT: u16 = 0x27;
        const DOWN: u16 = 0x28;
        if crate::companion::chat::install::snapshot().is_some() {
            self.keyboard = true;
            match vk {
                ESC => crate::companion::chat::install::close(),
                TAB => {
                    let list: Vec<_> = self.focusables().into_iter().filter(|h| matches!(h, Hit::Chat(a) if a.starts_with("chat.install."))).collect();
                    let pos = self.focus.and_then(|f| list.iter().position(|h| same(*h, f)));
                    self.tab_step(&list, pos, !shift);
                }
                ENTER | SPACE if !repeat => {
                    if let Some(Hit::Chat(a)) = self.focus.filter(|h| matches!(h, Hit::Chat(a) if a.starts_with("chat.install."))) {self.chat_action(a);}
                }
                0x21 | 0x22 => crate::companion::chat::install::scroll(if vk == 0x21 {-120.} else {120.}),
                _ => {}
            }
            self.redraw();
            return true;
        }
        // The ring (and the focus's tooltip) shows once the keyboard moves
        // the focus, not on a letter typed or a shortcut (crate::input).
        if crate::input::navigates(vk) {
            self.keyboard = true;
            self.tip_key = true;
        }
        if self.menu.is_some() {
            return self.menu_key(vk, repeat);
        }
        if self.apps_menu.is_some() {
            return self.apps_menu_key(vk, repeat);
        }
        // The displays: arrows by place (the map is never mirrored).
        if let Some(Hit::Display(i)) = self.focus {
            if !ctrl {
                if let Some(done) = self.display_key(i, vk) {
                    return done;
                }
            }
        }
        // Right to left (the pages are mirrored), left and right swap.
        let vk = match vk {
            LEFT if crate::i18n::rtl() => RIGHT,
            RIGHT if crate::i18n::rtl() => LEFT,
            v => v,
        };
        // Ctrl+F: the search box.
        if ctrl && vk == 0x46 && self.welcome.is_none() {
            self.focus = Some(Hit::Search);
            self.place_ime();
            self.redraw();
            return true;
        }
        // The search box and its results take some keys first.
        let own = match self.focus {
            Some(Hit::Search) if !(ctrl && vk == 0x53) && vk != TAB => self.search_key(vk),
            Some(Hit::SearchResult(k)) => self.result_key(k, vk),
            Some(Hit::PinRow(i)) => self.pin_row_key(i, vk, ctrl),
            Some(Hit::CalUrl | Hit::CalName) if vk != TAB => self.cal_key(vk),
            _ => None,
        };
        if let Some(done) = own {
            return done;
        }
        if matches!(self.focus, Some(Hit::CalUrl | Hit::CalName)) && vk != TAB {
            // Typing a name: the character comes as WM_CHAR.
            return false;
        }
        if self.focus == Some(Hit::Search) && vk != TAB && !(ctrl && vk == 0x53) {
            // Typing: the character comes as WM_CHAR.
            return false;
        }
        let list = self.focusables();
        let pos = self.focus.and_then(|f| list.iter().position(|h| same(*h, f)));
        // Scrolled out of sight: nothing is focused (keys must not change
        // what cannot be seen).
        if pos.is_none() {
            self.focus = None;
        }
        match vk {
            TAB => {
                self.tab_step(&list, pos, !shift);
                if self.focus == Some(Hit::Search) {
                    self.place_ime();
                }
                if let Some(Hit::Tab(k)) = self.focus {
                    self.reveal_tab(k);
                }
            }
            // A tooltip or a how-to card up: Esc puts it away first.
            ESC if self.tip_up() => self.quiet_tip(),
            ESC if self.howto_up() => self.quiet_howto(),
            // A reset's question: Esc answers no.
            ESC if self.confirm.is_some() => self.click_reset(Hit::ResetNo),
            // The first-run flow: Esc skips it (what was chosen stays).
            ESC if self.welcome.is_some() => self.welcome_move(Hit::WelcomeSkip),
            // Like the title bar's X (what is waiting is saved first).
            ESC => {
                self.post_leave_now();
                return true;
            }
            // Held keys repeat: act on the first press only.
            ENTER | SPACE if repeat => return true,
            // Enter with nothing focused goes on through the flow.
            ENTER if self.welcome.is_some() && self.focus.is_none() => self.welcome_move(Hit::WelcomeNext),
            ENTER | SPACE => match self.focus {
                Some(Hit::Slider(..) | Hit::Search | Hit::Choice(..)) | None => return false,
                Some(h) => self.click(h, 0.0),
            },
            LEFT | RIGHT | HOME | END | UP | DOWN => match self.focus {
                Some(Hit::Slider(ri, ..)) if vk != UP && vk != DOWN => {
                    if let Some(Ctl::Slider { get, set, min, max, step, .. }) = self.row_ctl(ri) {
                        let (get, set, min, max, step) = (*get, *set, *min, *max, *step);
                        let v = match vk {
                            HOME => min,
                            END => max,
                            RIGHT => model::snap(get(&self.cfg) + step, min, max, step),
                            _ => model::snap(get(&self.cfg) - step, min, max, step),
                        };
                        if v != get(&self.cfg) {
                            set(&mut self.cfg, v);
                            self.changed(ri);
                        }
                    }
                }
                Some(Hit::Choice(ri, oi)) if vk != UP && vk != DOWN => {
                    if let Some(Ctl::Choice { opts, set, .. }) = self.row_ctl(ri) {
                        let (n, set) = (opts.len(), *set);
                        let ni = match vk {
                            HOME => 0,
                            END => n - 1,
                            RIGHT => (oi + 1).min(n - 1),
                            _ => oi.saturating_sub(1),
                        };
                        if ni != oi {
                            set(&mut self.cfg, ni);
                            self.focus = Some(Hit::Choice(ri, ni));
                            self.changed(ri);
                        }
                    }
                }
                // The accent's colors: the next or previous one (never on
                // to "Custom…": that one opens a dialog, by Enter / Space).
                Some(Hit::Swatch(ri, oi)) if vk != UP && vk != DOWN => {
                    if let Some((opts, get, _)) = self.row_ctl(ri).and_then(|c| c.options()) {
                        let cur = get(&self.cfg).min(opts.len().saturating_sub(1));
                        let from = if oi == crate::config::ACCENT_CUSTOM { cur } else { oi };
                        if let Some(ni) = rows::swatch_step(from, opts.len(), vk == RIGHT || vk == END, vk == HOME || vk == END) {
                            self.focus = Some(Hit::Swatch(ri, ni));
                            self.pick_swatch(ri, ni);
                            return true;
                        }
                    }
                }
                Some(Hit::Menu(ri)) if vk != UP && vk != DOWN => {
                    if let Some((opts, get, _)) = self.row_ctl(ri).and_then(|c| c.options()) {
                        let (n, cur) = (opts.len(), get(&self.cfg));
                        let ni = match vk {
                            HOME => 0,
                            END => n - 1,
                            RIGHT => (cur + 1).min(n - 1),
                            _ => cur.saturating_sub(1),
                        };
                        self.pick_menu(ri, ni);
                    }
                }
                // Step 1's two cards, like a choice.
                Some(Hit::WelcomeCard(k)) if vk == LEFT || vk == RIGHT => {
                    let nk = if vk == RIGHT { 1 } else { 0 };
                    if nk != k {
                        self.focus = Some(Hit::WelcomeCard(nk));
                        self.pick_taskbar(nk);
                    }
                }
                // The flow's theme cards: the next or previous theme.
                Some(Hit::Theme(i)) if self.welcome.is_some() && (vk == LEFT || vk == RIGHT) => {
                    let n = self.themes.len();
                    let ni = if vk == RIGHT { (i + 1).min(n.saturating_sub(1)) } else { i.saturating_sub(1) };
                    if ni != i {
                        self.focus = Some(Hit::Theme(ni));
                        self.click_extra(Hit::Theme(ni));
                    }
                }
                // The sidebar: the next or previous tab.
                Some(Hit::Tab(k)) if vk == UP || vk == DOWN => {
                    let n = model::Tab::ALL.len();
                    let nk = if vk == DOWN { (k + 1).min(n - 1) } else { k.saturating_sub(1) };
                    self.click(Hit::Tab(nk), 0.0);
                    self.focus = Some(Hit::Tab(nk));
                    self.reveal_tab(nk);
                }
                // The switch under the title: the tab's next or previous page.
                Some(Hit::Section(i)) if vk == LEFT || vk == RIGHT => {
                    let pages = self.tab_pages();
                    if let Some(k) = pages.iter().position(|p| *p == i) {
                        let nk = if vk == RIGHT { (k + 1).min(pages.len() - 1) } else { k.saturating_sub(1) };
                        self.click(Hit::Section(pages[nk]), 0.0);
                        self.focus = Some(Hit::Section(pages[nk]));
                    }
                }
                _ if vk == UP || vk == DOWN => self.wheel(if vk == UP { 48.0 } else { -48.0 }),
                _ => return false,
            },
            _ => return false,
        }
        self.redraw();
        true
    }

    /// The wheel at `x` (window DIPs) over the sidebar's list scrolls it.
    /// False: not over it (or it all fits).
    fn side_wheel(&mut self, x: f32, delta: f32) -> bool {
        let dx = self.side_dx();
        if self.welcome.is_some() || self.searching() || x < dx || x >= dx + SIDEBAR || self.list_open() {
            return false;
        }
        let max = self.side_scroll_max();
        let s = (self.side_scroll - delta).clamp(0.0, max);
        if s != self.side_scroll {
            self.side_scroll = s;
            self.redraw();
        }
        true
    }

    fn wheel(&mut self, delta: f32) {
        // A long open list scrolls; a short one hangs from its button and
        // would scroll away: it closes.
        if let Some((ri, _)) = self.menu {
            let n = self.row_ctl(ri).and_then(|c| c.options()).map(|(opts, ..)| opts.len()).unwrap_or(0);
            let shown = self.list_shown.get();
            if n > shown {
                let step = (delta / 48.0 * 3.0).round() as isize;
                self.menu_first = (self.menu_first as isize - step).clamp(0, (n - shown) as isize) as usize;
                self.redraw();
                return;
            }
        }
        if let Some(m) = self.apps_menu.as_mut() {
            let shown = self.list_shown.get();
            if m.items.len() > shown {
                let step = (delta / 48.0 * 3.0).round() as isize;
                m.first = (m.first as isize - step).clamp(0, (m.items.len() - shown) as isize) as usize;
                self.redraw();
                return;
            }
        }
        if self.menu.take().is_some() | self.apps_menu.take().is_some() {
            self.redraw();
        }
        let max = (self.content_h - self.view_h() + 16.0).max(0.0);
        let s = (self.scroll - delta).clamp(0.0, max);
        if s != self.scroll {
            self.scroll = s;
            self.page_moved();
            self.redraw();
        }
    }

    /// Closing saves what is still waiting, now (no question: every change
    /// is kept, as in Windows' and macOS' settings). Closing the first-run
    /// flow is skipping it. False: a save failed (close_now keeps trying).
    fn close(&mut self) -> bool {
        if self.welcome.is_some() {
            self.end_welcome(false, true);
        }
        let saved = self.flush_now();
        self.remember_size();
        saved
    }

    /// The window's last size (neither minimized nor maximized) goes into
    /// the config when it changed: the next window opens at it.
    fn remember_size(&mut self) {
        let Some((w, h)) = NORMAL_SIZE.with(|s| s.get()) else { return };
        let size = Some([w.round() as u32, h.round() as u32]);
        let opened = OPENED.with(|s| s.get());
        if size == Some([opened.0.round() as u32, opened.1.round() as u32]) {
            return;
        }
        if self.cfg.settings_window == size {
            return;
        }
        let mut c = self.cfg.clone();
        c.settings_window = size;
        match self.client.commit(&c, &["settings_window".to_string()]) {
            Ok(_) => self.cfg.settings_window = size,
            Err(e) => crate::log!("settings: could not remember the window size: {e}"),
        }
    }

    /// The window was resized: everything is laid out again in the new
    /// size (rows wrap, the page scrolls no further than its end).
    fn resized(&mut self) {
        self.menu = None;
        self.apps_menu = None;
        pins::cancel_scan();
        self.page_moved();
        self.side_scroll = self.side_scroll.min(self.side_scroll_max());
        self.redraw();
        let max = (self.content_h - self.view_h()).max(0.0);
        if self.scroll > max {
            self.scroll = max;
            self.redraw();
        }
    }
}

/// The sub-page switch's label widths and the padding around each, in
/// `room` DIPs: the labels' own widths (`text`) with padding 28 down to 12
/// when they fit; else padding 12 and the longest labels cut to a shared
/// width (ellipsized when drawn) so that labels and padding fill `room`
/// exactly and never more.
pub(super) fn fit_subtabs(text: &[f32], room: f32) -> (Vec<f32>, f32) {
    let n = text.len().max(1) as f32;
    let sum: f32 = text.iter().sum();
    let pad = ((room - sum) / n).clamp(12.0, 28.0);
    let avail = (room - pad * n).max(0.0);
    if sum <= avail {
        return (text.to_vec(), pad);
    }
    // The shortest names keep their width while the rest share what is left.
    let mut sorted = text.to_vec();
    sorted.sort_by(f32::total_cmp);
    let (mut left, mut cap) = (avail, 0.0);
    for (i, w) in sorted.iter().enumerate() {
        let k = (sorted.len() - i) as f32;
        if *w * k <= left {
            left -= w;
            cap = *w;
        } else {
            cap = left / k;
            break;
        }
    }
    (text.iter().map(|w| w.min(cap)).collect(), pad)
}

/// How far a spring released `t` ago has gone toward its target (0 at
/// the start, about 1 at the end; it may pass 1 a little on the way),
/// exactly 1 from `ms` on. Pure: tested.
pub(super) fn spring_done(s: crate::motion::Spring, t: std::time::Duration, ms: u64) -> f32 {
    if t >= std::time::Duration::from_millis(ms) {
        1.0
    } else {
        1.0 - s.left(t.as_secs_f32())
    }
}

/// How long a page takes to come in (ms) in `mode`.
pub(super) fn page_ms(mode: crate::motion::Mode) -> u64 {
    match mode {
        crate::motion::Mode::Full => PAGE_MS,
        crate::motion::Mode::Reduced => crate::motion::REDUCED_MS as u64,
    }
}

/// A page `t` after it began coming in: (how far below its place it still
/// is, its opacity). It rises on Spring C and fades in on the kit's ease-out
/// over the first 180 ms; with reduced motion it only fades, in
/// REDUCED_MS. Pure: tested.
pub(super) fn page_frame(t: std::time::Duration, mode: crate::motion::Mode) -> (f32, f32) {
    let ms = t.as_secs_f32() * 1000.0;
    if mode == crate::motion::Mode::Reduced {
        return (0.0, crate::motion::bezier(crate::motion::EASE_OUT, ms / crate::motion::REDUCED_MS as f32));
    }
    let done = spring_done(crate::motion::SPRING_C, t, PAGE_MS);
    let fade = (ms / crate::motion::PANEL_FADE_MS as f32).clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - fade).powi(3);
    ((PAGE_RISE * (1.0 - done)).max(0.0), ease)
}

/// How far the sidebar's list (`list_h` tall) can scroll in a window
/// `win_h` tall. Pure: tested.
pub(super) fn side_scroll_max(list_h: f32, win_h: f32) -> f32 {
    (list_h - (win_h - 6.0 - search_box::LIST_TOP) + 8.0).max(0.0)
}

/// The scroll that shows the span `top`..`top + h` of a list in `room`
/// (scrolled `scroll` now): unchanged when it is in sight, else just far
/// enough. Pure: tested.
pub(super) fn reveal_span(scroll: f32, top: f32, h: f32, room: f32) -> f32 {
    if top < scroll {
        top - 4.0
    } else if top + h > scroll + room {
        top + h - room + 8.0
    } else {
        scroll
    }
}

/// Whether the keyboard stops at control `h` (`picked`: its row's option
/// chosen, for a choice or the accent's colors): a choice at its pick (the
/// arrows move it), the accent at its color and "Custom…", every "i";
/// never a list's items or what closes it. Pure: tested.
fn focusable(h: Hit, picked: Option<usize>) -> bool {
    match h {
        Hit::Choice(_, oi) => picked == Some(oi),
        Hit::Swatch(_, oi) => picked.is_some_and(|cur| rows::swatch_focusable(oi, cur)),
        Hit::MenuItem(..) | Hit::MenuDismiss | Hit::AppItem(..) => false,
        _ => true,
    }
}

/// Same control (slider geometry aside).
fn same(a: Hit, b: Hit) -> bool {
    match (a, b) {
        (Hit::Slider(x, ..), Hit::Slider(y, ..)) => x == y,
        _ => a == b,
    }
}

/// `r` cut to `clip`, or None if nothing is left.
fn clip_to(r: D2D_RECT_F, clip: D2D_RECT_F) -> Option<D2D_RECT_F> {
    let c = D2D_RECT_F { left: r.left.max(clip.left), top: r.top.max(clip.top), right: r.right.min(clip.right), bottom: r.bottom.min(clip.bottom) };
    (c.left < c.right && c.top < c.bottom).then_some(c)
}

fn dip(lp: LPARAM, scale: f32) -> (f32, f32) {
    let x = (lp.0 & 0xFFFF) as i16 as f32 / scale;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
    (x, y)
}

/// Closes the window now: what is still waiting is saved first (close());
/// if that fails, the window hides and keeps trying (apply.rs) instead.
unsafe fn close_now(hwnd: HWND) {
    // Already leaving (a second WM_CLOSE): the retries go on.
    if with_app(|a| a.exit_tries.is_some()).unwrap_or(false) {
        return;
    }
    let saved = with_app(|a| a.close()).unwrap_or(true);
    if !saved {
        with_app(|a| a.leave_after_save());
        return;
    }
    let _ = DestroyWindow(hwnd);
    // Saved or not, drop copied icons nothing points at.
    extra::clean_icons();
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CREATE {
        crate::companion::chat::session::subscribe(hwnd);
        if !cfg!(test) {crate::companion::chat::cli::refresh(hwnd); SetTimer(Some(hwnd), companion_ai::DETECT_TIMER, 15*60*1000,None);}
    }
    match msg {
        WM_MOUSEMOVE => {
            with_app(|a| {
                let (x, y) = dip(lp, a.scale);
                // A real move (not one to the same place): the pointer's
                // "i" comes first now, even on the same one (tooltip.rs).
                let took = a.pointer_at(((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32));
                if a.pin_drag.is_some() {
                    a.pin_drag_move(y);
                    return;
                }
                if let Some(Hit::Slider(ri, x0, tw)) = a.dragging {
                    a.set_slider(ri, x0, tw, x);
                    return;
                }
                let h = a.hit(x, y);
                // A widget row under the pointer: its how-to card (told
                // when the pointer leaves the window, so it goes too).
                let row = a.howto_row_at(x, y);
                if row != a.howto_hover {
                    if row.is_some() {
                        let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    a.howto_hover = row;
                    if h == a.hover {
                        a.redraw();
                    }
                }
                if h != a.hover {
                    // On an "i": told when the pointer leaves the window, so
                    // its tip does not stay behind.
                    if let Some(Hit::Info(_)) = h {
                        let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    a.hover = h;
                    a.redraw();
                } else if took {
                    // The same target, another tip (the keyboard's or an
                    // asked one gave way).
                    a.redraw();
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            // The pointer left the window: nothing is under it now.
            with_app(|a| {
                a.last_pt = None;
                if a.howto_hover.take().is_some() {
                    a.redraw();
                }
                if a.hover.is_some() && a.dragging.is_none() && a.pin_drag.is_none() {
                    a.hover = None;
                    a.redraw();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            with_app(|a| {
                let (x, y) = dip(lp, a.scale);
                a.pointer_down();
                a.pressed = a.hit(x, y);
                match a.hit(x, y) {
                    Some(h) => {
                        if a.focus == Some(Hit::Search) && h != Hit::Search && h != Hit::SearchClear {
                            a.cancel_composition();
                        }
                        a.focus = Some(h);
                        if let Hit::Slider(..) = h {
                            a.dragging = Some(h);
                            SetCapture(hwnd);
                        }
                        // A pin row: pressed to drag it (or just to pick it).
                        if let Hit::PinRow(i) = h {
                            a.pin_drag_start(i, y);
                            SetCapture(hwnd);
                        }
                        a.note.clear();
                        a.click(h, x);
                    }
                    // Outside the window (the open list holds the mouse).
                    None => a.close_menu(),
                }
            });
            LRESULT(0)
        }
        WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            with_app(|a| {
                a.pointer_down();
                a.close_menu();
            });
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // Switched away (Alt+Tab, another app clicked): the list closes,
            // a shortcut being recorded stays as it was.
            // Not in front (or minimized): a how-to card stops (howto.rs).
            let away = wp.0 & 0xFFFF == WA_INACTIVE as usize || (wp.0 >> 16) & 0xFFFF != 0;
            with_app(|a| a.set_howto_away(away));
            if wp.0 & 0xFFFF == WA_INACTIVE as usize {
                with_app(|a| {
                    if a.recording.is_some() {
                        a.stop_recording();
                        a.redraw();
                    }
                    a.close_menu();
                });
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SHOWWINDOW if wp.0 == 0 => {
            with_app(|a| a.set_howto_away(true));
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_GETMINMAXINFO => {
            // Never smaller than MIN_W x MIN_H DIPs (rows and the sidebar fit).
            let mmi = &mut *(lp.0 as *mut MINMAXINFO);
            let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
            let (w, h) = outer_size(MIN_W, MIN_H, scale);
            mmi.ptMinTrackSize = windows::Win32::Foundation::POINT { x: w, y: h };
            LRESULT(0)
        }
        WM_SIZE => {
            // The new client size (DIP) for every page's layout; the last
            // ordinary one is what is remembered.
            let (cw, ch) = ((lp.0 & 0xFFFF) as f32, ((lp.0 >> 16) & 0xFFFF) as f32);
            let scale = with_app(|a| a.scale).unwrap_or_else(|| GetDpiForWindow(hwnd).max(96) as f32 / 96.0);
            // Minimized: a how-to card stops; back: it may play again (the
            // window is in front once it is restored).
            with_app(|a| a.set_howto_away(wp.0 as u32 == SIZE_MINIMIZED || GetForegroundWindow() != hwnd));
            if wp.0 as u32 != SIZE_MINIMIZED && cw > 0.0 && ch > 0.0 {
                let size = ((cw / scale).max(MIN_W), (ch / scale).max(MIN_H));
                let changed = SIZE.with(|s| s.replace(size)) != size;
                if wp.0 as u32 == SIZE_RESTORED {
                    NORMAL_SIZE.with(|s| s.set(Some(size)));
                }
                if changed {
                    with_app(|a| a.resized());
                }
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        // Recording a shortcut: every key goes to the recorder (Alt and
        // F10 too, which would open the window menu).
        WM_KEYDOWN | WM_SYSKEYDOWN if with_app(|a| a.recording.is_some()).unwrap_or(false) => {
            with_app(|a| a.record_key(wp.0 as u16));
            LRESULT(0)
        }
        WM_KEYUP | WM_SYSKEYUP | WM_CHAR | WM_SYSCHAR if with_app(|a| a.record_tail()).unwrap_or(false) => LRESULT(0),
        WM_LBUTTONUP | WM_CAPTURECHANGED => {
            if msg == WM_CAPTURECHANGED {
                // Someone else took the mouse: the list closes too.
                with_app(|a| a.menu = None);
            }
            let was = with_app(|a| {
                a.pressed = None;
                let pin = a.pin_drag.is_some();
                a.pin_drag_end();
                a.dragging.take().is_some() || pin
            })
            .unwrap_or(false);
            if was && msg == WM_LBUTTONUP {
                let _ = ReleaseCapture();
            }
            with_app(|a| a.redraw());
            LRESULT(0)
        }
        WM_ASK_LP_RESET => {
            let ok = MessageBoxW(
                Some(hwnd),
                &windows::core::HSTRING::from(t("settings.launchpad.reset.confirm")),
                w!("NopBar"),
                MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
            );
            if ok == IDOK {
                with_app(|a| a.reset_launchpad());
            }
            LRESULT(0)
        }
        WM_PICK_ACCENT => {
            // The dialog's own message loop runs with the app free: Windows'
            // look changing meanwhile reaches WM_SETTINGCHANGE below.
            if let Some(start) = with_app(|a| a.accent_start()) {
                let picked = dialogs::pick_color(hwnd, start);
                with_app(|a| a.accent_picked(wp.0, picked));
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
            let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            let repeat = (lp.0 >> 30) & 1 == 1;
            let done = with_app(|a| a.key(wp.0 as u16, shift, ctrl, repeat)).unwrap_or(false);
            if done { LRESULT(0) } else { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_CHAR => {
            let taken = with_app(|a| a.search_char(wp.0 as u16) || a.cal_char(wp.0 as u16)).unwrap_or(false);
            if taken { LRESULT(0) } else { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_IME_SETCONTEXT => {
            // The search box draws the composition itself; candidates stay.
            let lp = LPARAM(lp.0 & !(windows::Win32::UI::Input::Ime::ISC_SHOWUICOMPOSITIONWINDOW as isize));
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_IME_STARTCOMPOSITION => {
            with_app(|a| a.place_ime());
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_IME_COMPOSITION => {
            let taken = with_app(|a| a.on_composition(lp) || a.cal_composition(lp)).unwrap_or(false);
            if taken { LRESULT(0) } else { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_IME_ENDCOMPOSITION => {
            with_app(|a| {
                a.end_composition();
                if !a.cal.comp.is_empty() {
                    a.cal.comp.clear();
                    a.redraw();
                }
            });
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        calendar::WM_CAL_TESTED => {
            with_app(|a| a.cal_tested());
            LRESULT(0)
        }
        pins::WM_RUNNING_APPS => {
            with_app(|a| a.running_apps_ready());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == companion_ai::DETECT_TIMER => {crate::companion::chat::cli::refresh(hwnd);LRESULT(0)}
        WM_TIMER if wp.0 == companion_ai::SPINNER_TIMER => {
            with_app(|a| {
                use crate::companion::chat::{cli, state::Provider};
                if crate::companion::chat::install::progressing() || a.tab() == model::Tab::CompanionAi && [Provider::ClaudeCode, Provider::Antigravity].iter().any(|p| cli::cached(*p).status == cli::Status::Checking) {
                    a.redraw();
                } else { let _ = KillTimer(Some(hwnd), companion_ai::SPINNER_TIMER); }
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == ANIM_TIMER => {
            with_app(|a| a.anim_tick());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == rows::FLASH_TIMER => {
            with_app(|a| a.flash_tick());
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if crate::companion::chat::install::snapshot().is_some(){
                let delta=((wp.0 >> 16) as i16) as f32;
                crate::companion::chat::install::scroll(-delta/120.*48.);
                with_app(|a|a.redraw());return LRESULT(0);
            }
            let d = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0 * 48.0;
            // Where the pointer is: over the search results they scroll.
            let mut p = windows::Win32::Foundation::POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p);
            with_app(|a| {
                let x = p.x as f32 / a.scale;
                if !a.results_wheel(x, d) && !a.side_wheel(x, d) {
                    a.wheel(d);
                }
            });
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED => {
            // Windows' light / dark, accent, transparency or high contrast
            // changed: the page follows (only when it really did).
            with_app(|a| {
                let sys = crate::syslook::current();
                if sys != a.look_for.1 {
                    a.refresh_look(sys);
                    a.redraw();
                }
            });
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DISPLAYCHANGE => {
            // Displays came, went or moved: the map follows.
            with_app(|a| {
                a.refresh_screens();
                a.redraw();
            });
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DPICHANGED => {
            let scale = ((wp.0 & 0xFFFF) as f32) / 96.0;
            let r = &*(lp.0 as *const RECT);
            // The new scale first: the resize below lays out in it.
            with_app(|a| a.scale = scale);
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            with_app(|a| a.redraw());
            LRESULT(0)
        }
        WM_GPU_LOST => {
            gpu_recover(hwnd, true);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == GPU_RETRY_TIMER => {
            let _ = KillTimer(Some(hwnd), GPU_RETRY_TIMER);
            gpu_recover(hwnd, false);
            LRESULT(0)
        }
        crate::companion::chat::settings::UPDATED | crate::companion::chat::session::CHANGED => {
            with_app(|a| {
                for (claude,on,result) in crate::companion::chat::settings::take_mcp_results() {
                    match result {
                        Ok(()) => {
                            a.cfg.set_agent_notifications(claude, on);
                            let key = match claude { 0 => "chat.claude_mcp", 1 => "chat.codex_mcp", _ => "chat.antigravity_mcp" };
                            a.touch(key);
                            a.animate_switch(key);
                            a.send_preview();
                        }
                        Err(e) => a.note=e,
                    }
                }
                if let Some(model) =
                    crate::companion::chat::settings::default_model(&a.cfg.chat.model)
                {
                    a.cfg.chat.model = model;
                    a.touch("chat.model");
                    a.send_preview();
                }
                a.install_updated();
                a.redraw();
            });
            LRESULT(0)
        }
        update_card::WM_UPDATE_DONE => {
            with_app(|a| a.update_done());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == update_card::UPD_POLL_TIMER => {
            with_app(|a| a.update_poll());
            LRESULT(0)
        }
        extra::WM_ICON_READY => {
            with_app(|a| {
                a.take_icons();
                a.redraw();
            });
            LRESULT(0)
        }
        m if m != 0 && m == WM_CONFIG_SAVED.load(std::sync::atomic::Ordering::SeqCst) => {
            with_app(|a| a.pins_saved_elsewhere());
            LRESULT(0)
        }
        m if m != 0 && m == WM_SHOW_WELCOME.load(std::sync::atomic::Ordering::SeqCst) => {
            // Not ready yet (no App): 0, the sender asks again.
            let shown = with_app(|a| a.show_welcome()).is_some();
            LRESULT(shown as isize)
        }
        m if m != 0 && m == WM_SHOW_PAGE.load(std::sync::atomic::Ordering::SeqCst) => {
            // Not ready yet (no App): 0, the sender asks again. The page is
            // shown from a posted message: "report" opens Explorer and mail,
            // which must not run inside another process's SendMessage.
            let ready = with_app(|_| ()).is_some();
            if ready {
                let _ = PostMessageW(Some(hwnd), WM_SHOW_PAGE_NOW, wp, LPARAM(0));
            }
            LRESULT(ready as isize)
        }
        WM_LAUNCHERS_FOUND => {
            with_app(|a| {
                a.sections = model::sections();
                // An open list may have had other options.
                a.menu = None;
                a.redraw();
            });
            LRESULT(0)
        }
        WM_SHOW_PAGE_NOW => {
            with_app(|a| a.show_page(wp.0));
            LRESULT(0)
        }
        WM_CLOSE => {
            // X, Alt+F4, the taskbar's Close: no question (close() saves).
            close_now(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == apply::SAVE_TIMER => {
            with_app(|a| {
                a.save();
            });
            LRESULT(0)
        }
        // Closed with a save that failed: one more try (gone once saved).
        WM_TIMER if wp.0 == apply::CLOSE_TIMER => {
            if with_app(|a| a.close_retry()).unwrap_or(true) {
                let _ = DestroyWindow(hwnd);
                extra::clean_icons();
            }
            LRESULT(0)
        }
        // Signing out or shutting down: everything waiting is saved now
        // (journaled if that fails); never held up.
        WM_QUERYENDSESSION => {
            with_app(|a| a.flush_for_exit());
            LRESULT(1)
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                with_app(|a| a.flush_for_exit());
            }
            LRESULT(0)
        }
        WM_LEAVE_NOW => {
            close_now(hwnd);
            LRESULT(0)
        }
        WM_GETOBJECT => {
            // Screen readers: the tree of the page shown (access.rs).
            match crate::a11y::on_getobject(hwnd, wp, lp, || {
                with_app(|a| a.publish_a11y());
            }) {
                Some(r) => r,
                None => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
        m if m == crate::a11y::wm_action() => {
            for (key, act) in crate::a11y::take(hwnd) {
                with_app(|a| a.a11y_act(&key, act));
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            crate::companion::chat::install::close();
            access::forget(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod page_tests {

    /// The settings window takes the dock's finish (owner, 2026-11-12):
    /// Glass is glass while the PC allows it; transparency off, high
    /// contrast or the blur switched off make it Flat, as the dock's.
    #[test]
    fn the_settings_window_takes_the_finish() {
        use crate::theme::Finish;
        let mut cfg = crate::config::Config::default();
        for f in [Finish::Flat, Finish::Glass] {
            cfg.theme.finish = if f == Finish::Glass { "glass" } else { "flat" }.into();
            for light in [false, true] {
                let mut raw = crate::theme::Theme::graphite();
                raw.finish = f;
                let sys = crate::syslook::SysLook { light, transparency: true, ..Default::default() };
                let t = super::settings_look(raw.clone(), &sys, &cfg);
                assert_eq!(t.glass(), f == Finish::Glass && !crate::gfx::software_in_force(), "{f:?}");
                let t = super::settings_look(raw.clone(), &crate::syslook::SysLook { transparency: false, ..sys }, &cfg);
                assert!(!t.glass(), "{f:?}: transparency off");
                let mut off = cfg.clone();
                off.effects.blur = false;
                assert!(!super::settings_look(raw.clone(), &sys, &off).glass(), "{f:?}: the blur off");
                // The low spec switch and software rendering: Flat, as the dock.
                let mut low = cfg.clone();
                low.effects.low_spec = true;
                assert!(!super::settings_look(raw.clone(), &sys, &low).glass(), "{f:?}: low spec");
                let mut soft = cfg.clone();
                soft.effects.render = "software".into();
                assert!(!super::settings_look(raw, &sys, &soft).glass(), "{f:?}: software");
            }
        }
    }

    #[test]
    fn the_focus_ring_follows_its_control_s_corners() {
        use super::{focus_radius, rows, Hit};
        let (ctl, card) = (4.0, 8.0);
        assert_eq!(focus_radius(Hit::Toggle(0), (40.0, 20.0), ctl, card), 10.0, "a switch: a capsule");
        assert_eq!(focus_radius(Hit::Swatch(0, 1), (24.0, 24.0), ctl, card), 12.0, "a color: a circle");
        assert_eq!(focus_radius(Hit::Info(3), (20.0, 20.0), ctl, card), 10.0);
        assert_eq!(focus_radius(Hit::Theme(0), (120.0, 75.0), ctl, card), 8.0, "a picture: a card");
        assert_eq!(focus_radius(Hit::Button(0), (96.0, 32.0), ctl, card), 4.0, "a button: a control");
        assert_eq!(focus_radius(Hit::Button(0), (6.0, 6.0), ctl, card), 3.0, "never past a capsule");
        // One ring: 2 DIP wide, 2 out.
        assert_eq!((rows::tok::RING_W, rows::tok::RING_GAP), (2.0, 2.0));
    }

    /// An "i"'s ring goes round its 12 DIP glyph, centered on it, not
    /// round its 24 DIP target (owner, 0.1.46: a ring twice the glyph's
    /// size running into the label); a control's goes RING_GAP out.
    #[test]
    fn an_i_s_ring_hugs_its_glyph() {
        use super::{focus_ring_rect, rows, Hit};
        use rows::tok;
        let hit = rows::info_hit(100.5, 40.25);
        let (r, rad) = focus_ring_rect(Hit::Info(0), hit, 4.0, 8.0);
        let glyph = rows::info_glyph(hit);
        assert_eq!(((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0), (100.5, 40.25), "centered on the glyph");
        let outer = r.right + tok::RING_W / 2.0 - glyph.right;
        assert!(outer <= tok::INFO_RING_GAP + tok::RING_W + 0.01, "{outer} past the glyph");
        assert_eq!(rad, (r.right - r.left) / 2.0, "a circle");
        // The label stops INFO_GAP before the glyph: the ring stays clear
        // of it (2 DIP at least).
        assert!(tok::INFO_GAP - outer >= 2.0, "ring {outer} into the label's gap {}", tok::INFO_GAP);
        let ctl = super::cards::rect(10.0, 10.0, 190.0, 38.0);
        let (r, _) = focus_ring_rect(Hit::Shortcut(0), ctl, 4.0, 8.0);
        assert_eq!(r.left, 10.0 - tok::RING_GAP - tok::RING_W / 2.0);
    }
    #[test]
    fn glass_cards_are_a_sheet_of_white_over_the_acrylic() {
        use super::card_layer;
        let (l, d) = (card_layer(true), card_layer(false));
        // White, the acrylic showing through (the mockup's .45 / .07).
        assert_eq!((l.r, l.g, l.b), (1.0, 1.0, 1.0));
        assert!(l.a > 0.4 && l.a < 0.7, "{}", l.a);
        assert_eq!((d.r, d.g, d.b), (1.0, 1.0, 1.0));
        assert!(d.a > 0.04 && d.a <= 0.08, "{}", d.a);
    }

    #[test]
    fn the_keyboard_stops_at_every_info_and_at_picks_only() {
        use super::{focusable, Hit};
        use crate::config::ACCENT_CUSTOM;
        // Every "i" (its row's description), whatever the row holds.
        assert!(focusable(Hit::Info(4), None));
        assert!(focusable(Hit::Info(4), Some(2)));
        // A choice at its pick only; the accent at its color and Custom….
        assert!(focusable(Hit::Choice(1, 2), Some(2)));
        assert!(!focusable(Hit::Choice(1, 0), Some(2)));
        assert!(focusable(Hit::Swatch(1, 2), Some(2)));
        assert!(!focusable(Hit::Swatch(1, 5), Some(2)));
        assert!(focusable(Hit::Swatch(1, ACCENT_CUSTOM), Some(2)));
        assert!(!focusable(Hit::Swatch(1, 2), None));
        // Never a list's items or what closes it; the rest always.
        assert!(!focusable(Hit::MenuItem(1, 0), Some(0)) && !focusable(Hit::MenuDismiss, None));
        assert!(focusable(Hit::Toggle(3), None) && focusable(Hit::Menu(3), Some(1)));
    }

    #[test]
    fn a_page_only_fades_in_quickly_with_reduced_motion() {
        use super::{page_frame, page_ms, PAGE_MS};
        use crate::motion::{Mode, REDUCED_MS};
        use std::time::Duration;
        let ms = |m: u64| Duration::from_millis(m);
        // Full: it rises and fades in; done by PAGE_MS.
        let (rise, a) = page_frame(ms(40), Mode::Full);
        assert!(rise > 0.0 && a > 0.0 && a < 1.0);
        assert_eq!(page_frame(ms(PAGE_MS), Mode::Full), (0.0, 1.0));
        // Reduced (Windows' animation effects off, whatever the preset):
        // no rise, a fade over REDUCED_MS, then still.
        let (rise, a) = page_frame(ms(20), Mode::Reduced);
        assert!(rise == 0.0 && a > 0.0 && a < 1.0);
        assert_eq!(page_frame(ms(REDUCED_MS as u64), Mode::Reduced), (0.0, 1.0));
        assert_eq!(page_ms(Mode::Reduced), REDUCED_MS as u64);
        assert_eq!(page_ms(Mode::Full), PAGE_MS);
    }

    #[test]
    fn a_search_preview_keeps_what_is_being_edited() {
        use super::{page_change, Nav};
        // Typing in the search moves the page shown: nothing ends (an undo
        // stays).
        assert!(!page_change(3, 7, Nav::Preview));
        // Another page opened: the undo ends; the same page again keeps it.
        assert!(page_change(3, 7, Nav::Open));
        assert!(!page_change(3, 3, Nav::Open));
    }

    #[test]
    fn sub_page_switch_fits_any_width() {
        use super::fit_subtabs;
        let total = |(w, pad): (Vec<f32>, f32)| w.iter().sum::<f32>() + pad * w.len() as f32;
        // Short names: their own widths, roomy padding, inside the page.
        let (w, pad) = fit_subtabs(&[60.0, 70.0], 460.0);
        assert_eq!((w, pad), (vec![60.0, 70.0], 28.0));
        // Window management in long languages at 720 DIP (~460 to spare):
        // French ~534, Spanish ~586 of names and padding before.
        for names in [vec![170.0, 95.0, 120.0, 105.0], vec![190.0, 110.0, 140.0, 98.0], vec![400.0, 400.0, 400.0, 400.0]] {
            for room in [460.0, 520.0, 300.0] {
                let (w, pad) = fit_subtabs(&names, room);
                assert_eq!(pad, 12.0, "the least padding first");
                assert!((total((w.clone(), pad)) - room).abs() < 0.01, "{names:?} in {room}: {w:?}");
                assert!(w.iter().zip(&names).all(|(a, b)| a <= b), "never wider than the name");
            }
        }
        // Only the long ones are cut; the short one stays whole.
        let (w, _) = fit_subtabs(&[60.0, 300.0, 300.0], 460.0);
        assert_eq!(w[0], 60.0);
        assert!((w[1] - w[2]).abs() < 0.01 && w[1] < 300.0);
        // Nothing to draw in: nothing past the page.
        let (w, pad) = fit_subtabs(&[100.0, 100.0], 10.0);
        assert!(w.iter().all(|x| *x == 0.0) && pad == 12.0);
        // Right to left the rectangles are mirrored inside the same span
        // (rows::Mirror): a switch that fits left to right fits there too.
        let m = super::rows::Mirror { on: true, left: 228.0, right: 692.0 };
        let r = m.r(windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: 228.0, top: 0.0, right: 228.0 + 460.0, bottom: 1.0 });
        assert!(r.left >= 228.0 - 0.01 && r.right <= 692.0 + 0.01, "{r:?}");
    }

    #[test]
    fn the_window_opens_at_its_last_size_within_reason() {
        let work = (1600.0, 900.0);
        assert_eq!(super::opening_size(None, work), (super::W0, super::H0), "first time: the default");
        assert_eq!(super::opening_size(Some([1000, 700]), work), (1000.0, 700.0));
        assert_eq!(super::opening_size(Some([300, 200]), work), (super::MIN_W, super::MIN_H), "never under the minimum");
        assert_eq!(super::opening_size(Some([4000, 3000]), work), work, "never past the screen");
        // A tiny screen: the minimum still.
        assert_eq!(super::opening_size(Some([900, 700]), (640.0, 480.0)), (super::MIN_W, super::MIN_H));
    }

    #[test]
    fn page_names() {
        assert_eq!(super::page_code(Some("about")), 1);
        assert_eq!(super::page_code(Some("report")), 2);
        assert_eq!(super::page_code(Some("input")), 3);
        assert_eq!(super::page_code(Some("topbar")), super::PAGE_TOPBAR);
        // Older links to the workspaces open Window management.
        for name in ["workspaces", "workspaces-save", "workspaces-work", "workspaces-rename-3", "workspaces-rename-999"] {
            assert_eq!(super::plain_page(super::page_code(Some(name))), Some("settings.tab.windows"), "{name}");
        }
        // The top bar's right-click menu: its own pages, each a page of
        // the sidebar (not the timer's).
        let secs = super::model::sections();
        for (name, title) in [
            ("general", "settings.section.general"),
            ("dock", "settings.section.dock"),
            ("bar", "settings.section.topbar"),
            ("calendar", "settings.group.clock"),
            ("updates", "settings.section.about"),
            ("appearance", "settings.section.appearance"),
            ("shortcuts", "settings.section.shortcuts"),
        ] {
            let code = super::page_code(Some(name));
            assert_eq!(super::plain_page(code), Some(title), "{name}");
            assert!(code != super::PAGE_TOPBAR, "{name}");
            assert!(secs.iter().any(|s| s.title == title), "{title}");
        }
        assert_eq!(super::plain_page(super::PAGE_TOPBAR), None);
        assert_eq!(super::page_code(Some("nope")), 0);
        assert_eq!(super::page_code(None), 0);
    }

    /// Every `--page` name still opens where it should after the settings
    /// structure pass (no name dropped), and every name a caller in this
    /// build passes to launch_page is one of them.
    #[test]
    fn every_page_link_lands() {
        use super::model::{Kind, Tab};
        let secs = super::model::sections();
        let land = |name: &str| -> (Tab, &'static str, Option<&'static str>) {
            let code = super::page_code(Some(name));
            assert_ne!(code, 0, "{name}: not a page name");
            if code == super::PAGE_TOPBAR {
                let (si, ri) = super::model::find_row(&secs, super::TIMER_ROW).expect("the timer's row");
                return (secs[si].tab, secs[si].title, Some(secs[si].rows[ri].key));
            }
            let si = super::page_target(&secs, code).unwrap_or_else(|| panic!("{name}: lands nowhere"));
            (secs[si].tab, secs[si].title, None)
        };
        let want: [(&str, Tab, &str, Option<&str>); 16] = [
            ("companion", Tab::Companion, "companion.title", None),
            ("about", Tab::About, "settings.section.about", None),
            ("report", Tab::About, "settings.section.about", None),
            ("input", Tab::Keyboard, "settings.section.input", None),
            ("topbar", Tab::TopBar, "settings.group.clock", Some(super::TIMER_ROW)),
            ("general", Tab::General, "settings.section.general", None),
            ("dock", Tab::Dock, "settings.section.dock", None),
            ("bar", Tab::TopBar, "settings.section.topbar", None),
            ("calendar", Tab::TopBar, "settings.group.clock", None),
            ("updates", Tab::About, "settings.section.about", None),
            ("appearance", Tab::Appearance, "settings.section.appearance", None),
            ("shortcuts", Tab::Keyboard, "settings.section.shortcuts", None),
            ("workspaces", Tab::Windows, "settings.tab.windows", None),
            ("workspaces-save", Tab::Windows, "settings.tab.windows", None),
            ("workspaces-work", Tab::Windows, "settings.tab.windows", None),
            ("workspaces-rename-0", Tab::Windows, "settings.tab.windows", None),
        ];
        assert_eq!(want.map(|w| w.0), super::PAGE_NAMES, "every name, in PAGE_NAMES' order");
        for (name, tab, title, row) in want {
            assert_eq!(land(name), (tab, title, row), "{name}");
        }
        // The pages these open are what they hold.
        let about = secs.iter().find(|s| s.title == "settings.section.about").unwrap();
        assert!(super::model::page_keys(about).contains(&"updates.auto_check"), "updates live on About");
        assert_eq!(secs.iter().find(|s| s.title == "settings.tab.windows").map(|s| s.kind), Some(Kind::Windows));
        // Every literal name this build passes to launch_page is listed
        // (a renamed page cannot leave a caller behind).
        let callers = [include_str!("../dock/menu.rs"), include_str!("../topbar/panels.rs"), include_str!("../topbar/mod.rs"), include_str!("../main.rs"), include_str!("mod.rs")];
        let mut seen = 0;
        for src in callers {
            for part in src.split("launch_page(\"").skip(1) {
                let name = &part[..part.find('"').unwrap()];
                seen += 1;
                assert!(super::PAGE_NAMES.contains(&name), "launch_page(\"{name}\") is not a page name");
            }
        }
        // The dock's menu (2), the top bar's panels (2).
        assert!(seen >= 4, "the callers were found ({seen})");
    }
}
