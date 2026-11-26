//! The top bar: start button, current app name, app menus (titles for now;
//! menus open in M4b), and on the right network, volume, battery and clock.
//!
//! Lives on the dock's UI thread and shares its Gfx (one compositor and D3D
//! device per process). Reserves the top edge as an AppBar, or the bottom
//! edge with topbar.position "bottom" (then everything hanging off the bar,
//! menus, panels, the island's card and tips, opens upward: `at_bottom`).
//! Idle cost: one one-shot timer per minute for the clock (per second with
//! topbar.clock_seconds, only while someone can see the bar); everything
//! else is events.

pub(crate) mod about;
pub mod control;
mod access;
mod companion;
pub use companion::{companion_obstacles, companion_anchor, companion_changed, companion_agenda, companion_tip, companion_slot_at, companion_tooltip_changed};
pub use companion::{companion_news_anchor, companion_news_bounce};
pub(crate) mod baseline;
mod capsule;
pub(crate) mod caption;
mod desktops;
mod form;
mod hidden;
mod input;
pub mod island;
mod look;
pub mod menus;
pub mod panels;
pub mod stats;
pub(crate) mod shutdown;
pub mod status;
mod system;

/// The drawn tooltip (radius tip, E1, the ramp's Footnote) the bar's tips
/// use, shared with the dock's window previews and the settings window.
pub(crate) use desktops::{hide_tip, show_tip_at, show_tip_wrapped, wrapped_tip_size, tip_shadows, TIP_DELAY_MS};


use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use windows::core::{w, Interface, Result, BOOL};
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionStretch, CompositionSurfaceBrush, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY,
    SYSTEM_POWER_STATUS,
};
use windows::Win32::System::SystemServices::{GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetDoubleClickTime, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Shell::{ABN_POSCHANGED, ABE_BOTTOM, ABE_TOP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Vector2, Vector3};

use crate::config::{Config, TopbarCfg};
use crate::placement::BarSpace;
use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::taskbar;
use crate::theme::Theme;
use status::Net;

const CLOCK_TIMER: usize = 1;
/// One-shot: run a picked menu action once the target app has the focus.
const ACTION_TIMER: usize = 2;
/// A native menu was read on the worker: lParam = Box<(u64, Built)>.
const WM_MENU_READY: u32 = WM_APP + 40;
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;
/// The corners of the fill behind a pressed or open bar cell (the
/// Windows top bar: a soft rounded rectangle, not a capsule).
const MARK_RADIUS: f32 = 4.0;

/// Pure: the fill behind a bar cell on a bar `bar_h` tall (the
/// Windows top bar): its inset from the bar's top and bottom
/// (BAR_INSET), its height (the bar's less 4: 22 on the 26 bar) and its
/// corners (MARK_RADIUS; never more than a capsule's).
fn hover_pill(bar_h: f32) -> (f32, f32, f32) {
    let inset = crate::theme::metrics::size::BAR_INSET;
    let h = (bar_h - 2.0 * inset).max(1.0);
    (inset, h, MARK_RADIUS.min(crate::theme::capsule(h)))
}

/// Space kept between the app menus and the expanded drawer (DIP).
const DRAWER_GAP: f32 = crate::theme::metrics::space::S3;
/// One-shot: fold the expanded tray back behind its chevron.
const TRAY_TIMER: usize = 3;
/// The expanded tray folds back after this long without the pointer moving
/// on the bar (a click or a drag restarts it too).
const TRAY_COLLAPSE_MS: u32 = 10_000;
/// One-shot: the pointer rested on the desktop dots long enough for the
/// name tip.
const TIP_TIMER: usize = 4;
/// Clear's wallpaper read is tried again (look::retry_after).
const WALL_TIMER: usize = 45;
/// Outer margin at both ends (DIP; the Windows top bar: 8).
const EDGE_PAD: f32 = 8.0;
/// Left side: the padding either side of a cell's content (the logo, the
/// menus: 9), and the app name's before it (6: it sits close to the logo).
const CELL_PAD: f32 = 9.0;
const APP_LEAD: f32 = 6.0;
/// Right side: the status icons stand STATUS_GAP apart (half of it either
/// side of each: one even grid; the 15 DIP gap between 16 DIP glyphs), a
/// cell at least ICON_CELL wide (a 16 glyph's).
const STATUS_GAP: f32 = 15.0;
const ICON_CELL: f32 = crate::theme::metrics::size::ICON_S + STATUS_GAP;
/// The clock: text alone at the bar's end, 10 before it and 6 after.
const CLOCK_LEAD: f32 = 10.0;
const CLOCK_TAIL: f32 = 6.0;
/// Glyphs on the bar are the kit's row size (ICON_S, 16); marks (the
/// chevron, the battery's bolt) the mark size (ICON_XS, 12); the logo a
/// glyph's (16: at 15 it read faint beside them).
const START_PX: f32 = crate::theme::metrics::size::ICON_S;
const GLYPH_SIZE: f32 = crate::theme::metrics::size::ICON_S;
/// Glyph icons are drawn ink-centered in a square of this size (DIP), the
/// same box as the tray icons, so they all share one center line.
const GLYPH_BOX: f32 = crate::theme::metrics::size::ICON_S;
/// The tray chevron is a smaller glyph in the same box.
const CHEVRON_SIZE: f32 = crate::theme::metrics::size::ICON_XS;
/// Tray icons are 18 DIP: the taskbar's 16 read small on the bar beside
/// its glyphs (an app's 16 px icon has its own margin inside).
const TRAY_DIP: f32 = 18.0;
/// New tray icons are shown until this many are; later ones go behind the
/// chevron (the user can drag them across).
const TRAY_AUTO_MAX: usize = 6;
/// Pointer travel (DIP) before a press on a tray icon becomes a drag.
const DRAG_THRESHOLD: f32 = crate::theme::metrics::space::S2;
/// Battery drawing (DIPs, whole ones): a 24 x 12 body (a small mark's
/// corners: battery_radius) 1 in from the top, a 2 x 4 terminal 1 after it.
const BATTERY_W: f32 = 28.0;
/// Even, like the 16-DIP glyph box: both center on the same pixel row (an
/// odd 13 sat half a pixel lower than the glyphs beside it).
const BATTERY_H: f32 = 14.0;
/// The body's right edge is status::BATTERY_BODY: the level's width
/// (status::battery_look) is the body's own.
const BATTERY_BODY: (f32, f32, f32, f32) = (0.0, 1.0, status::BATTERY_BODY, 13.0);
const BATTERY_NUB: (f32, f32, f32, f32) = (25.0, 5.0, 27.0, 9.0);
/// Where the battery's fill ends (px from the surface's left, a whole
/// pixel) for a `level` DIP wide (status::battery_look) at `scale`: the
/// body's right edge when full. Pure: tested.
fn battery_edge(level: f32, scale: f32) -> f32 {
    ((BATTERY_BODY.0 + level) * scale).round()
}
/// The battery's number (the ramp's Caption, semibold, tabular: never Bold)
/// and the bolt that takes its place while charging (9: at the mark size,
/// 12, it filled the 12 high body to its edges).
const BATTERY_TEXT: f32 = crate::theme::ramp::CAPTION.size;
const BATTERY_BOLT: f32 = 9.0;

/// px the top bar reserves on the primary monitor (0 = none). Read by the
/// dock to keep side docks below it.
/// AppBars may be registered (startup's deferred step has run): bars
/// created later register right away.
static TRAY_FOCUS_BARS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn can_focus_notification_area() -> bool { TRAY_FOCUS_BARS.load(Ordering::Acquire) != 0 }

static APPBARS_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Bar-wide status, so a bar created later starts up to date.
static LAST_NET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);
static LAST_VOL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);
/// Posted to a bar whose update was skipped while the bars were busy.
const WM_BAR_RETRY: u32 = WM_APP + 41;

/// What each bar keeps the dock out of on its monitor: (monitor rect,
/// space; placement::BarSpace).
static RESERVED: std::sync::Mutex<Vec<(RECT, BarSpace)>> = std::sync::Mutex::new(Vec::new());
/// The bars sit on the bottom edge (topbar.position): what hangs off them
/// opens upward. False while there are no bars.
static AT_BOTTOM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Bumped whenever a bar is created or destroyed: a menu opened by an
/// older bar must not touch a newer one.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    /// One bar per monitor; the primary monitor's bar also hosts the tray.
    static BARS: RefCell<Vec<TopBar>> = const { RefCell::new(Vec::new()) };
    /// The app in front, for bars created later.
    static LAST_APP: RefCell<Option<menus::ActiveApp>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cell {
    Start,
    App,
    Menu(usize),
    /// Index into the tray icon snapshot.
    Tray(usize),
    /// The chevron left of the shown tray icons: slides the hidden ones
    /// out to its left (and back).
    Chevron,
    /// In the tray's place when the mirror is off or cannot be trusted
    /// (crate::tray::needs_button): opens Windows' own notification area.
    TrayButton,
    Net,
    Volume,
    Battery,
    Companion,
    /// Search button, left of the Quick settings (primary only).
    Search,
    /// Quick settings button (primary monitor only, like the tray).
    Control,
    Clock,
    /// Virtual desktop dots: the leftmost cell of the right side (left of
    /// the tray drawer when it is out).
    Desktops,
    /// The windows hidden to the tray (hidden.rs): one cell however many
    /// there are, right of the apps' tray icons.
    Hidden,
    /// The input indicator (input.rs): 가 / A.
    Input,
}

/// Pure: the padding before and after cell `c`'s content, in reading
/// order (DIP; the Windows top bar).
fn pads(c: Cell) -> (f32, f32) {
    match c {
        Cell::App => (APP_LEAD, CELL_PAD),
        Cell::Clock => (CLOCK_LEAD, CLOCK_TAIL),
        c if c.right() => (STATUS_GAP / 2.0, STATUS_GAP / 2.0),
        _ => (CELL_PAD, CELL_PAD),
    }
}

/// Pure: how wide cell `c` is with content `w` DIP wide (0: not shown):
/// its content and pads; a status icon's cell at least ICON_CELL.
fn cell_width(c: Cell, w: f32) -> f32 {
    if w <= 0.0 {
        return 0.0;
    }
    let (a, b) = pads(c);
    let full = w + a + b;
    if c.right() && c != Cell::Clock {
        full.max(ICON_CELL)
    } else {
        full
    }
}

/// Pure: where content `cw` DIP wide starts in cell `c` at `x`, `w` wide
/// (left to right on screen): centered, moved by half the difference of
/// its pads (towards the shorter one; mirrored right to left).
fn content_x(c: Cell, x: f32, w: f32, cw: f32, rtl: bool) -> f32 {
    let (a, b) = pads(c);
    let skew = (a - b) / 2.0;
    x + (w - cw) / 2.0 + if rtl { -skew } else { skew }
}

impl Cell {
    fn right(self) -> bool {
        matches!(
            self,
            Cell::Tray(_)
                | Cell::Chevron
                | Cell::TrayButton
                | Cell::Net
                | Cell::Volume
                | Cell::Battery
                | Cell::Companion
                | Cell::Search
                | Cell::Control
                | Cell::Clock
                | Cell::Desktops
                | Cell::Hidden
                | Cell::Input
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Style {
    Text,
    /// The clock: text with tabular figures as wide as its steady
    /// template (status::steady_template), so ticking moves nothing.
    Clock,
    Glyph,
    /// A smaller glyph in the same box (the tray chevron).
    SmallGlyph,
}

struct Slot {
    cell: Cell,
    visual: SpriteVisual,
    /// What the visual currently shows ("" = nothing).
    key: String,
    content: (f32, f32),
    x: f32,
    w: f32,
    /// Offset and opacity last given to the visual (set only on change:
    /// tray visuals animate every set).
    at: (f32, f32),
    alpha: f32,
    /// Its content is shown in another color than it was drawn in (high
    /// contrast's HighlightText on a pressed or open cell): that color and
    /// the brush it was drawn with, to put back.
    inked: Option<(crate::theme::Rgba, windows::UI::Composition::CompositionBrush)>,
}

/// A tray icon being dragged: across the chevron, or to another place on
/// its side.
struct Drag {
    /// The dragged icon's id (tray_ids), re-resolved on every use: the
    /// snapshot and the slots may be rebuilt under a drag.
    id: String,
    /// Pointer x minus the icon's left edge when the drag began (DIP).
    grab: f32,
    /// Pointer x now (DIP).
    x: f32,
    /// Where it would land if dropped now: the side, and its place among
    /// that side's other icons.
    hidden: bool,
    slot: usize,
}

struct TopBar {
    hwnd: HWND,
    /// Its monitor (HMONITOR as isize).
    hmon: isize,
    /// On the primary monitor: tray icons and status listeners live here.
    primary: bool,
    gfx: Rc<Gfx>,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    backdrop: SpriteVisual,
    mica: SpriteVisual,
    tint: SpriteVisual,
    /// The line along the bar's inner edge.
    edge: SpriteVisual,
    /// The capsules behind the cells: hover, pressed, open (capsule.rs).
    caps: capsule::Capsules,
    /// Clear's guard plates, one per cell (look::CellLook::guard): over
    /// the ground, under the capsules.
    guards: Guards,
    /// The cell whose menu or panel is up (or being read): it keeps its
    /// pressed fill until that surface starts to close (6.2 "open").
    anchor: Option<Cell>,
    slots: Vec<Slot>,
    surfaces: HashMap<(Style, String), (CompositionSurfaceBrush, f32, f32)>,
    cfg: TopbarCfg,
    companion_enabled: bool,
    companion_situations: bool,
    theme: Theme,
    scale: f32,
    width: f32,
    height: f32,
    monitor: RECT,
    /// The bar's window rect (screen px): the monitor's top edge, or below
    /// / beside a taskbar NopBar keeps (placement::topbar_rect).
    at: RECT,
    appbar: bool,
    app: String,
    /// The app in front and its menus.
    active: menus::ActiveApp,
    source: menus::Source,
    titles: Vec<String>,
    /// Bar title open right now (0 = app menu, k = titles[k - 1]).
    open: Option<usize>,
    /// The menu being shown (its actions).
    built: Option<menus::Built>,
    /// A native menu read finished for this title.
    ready: Option<(usize, menus::Built)>,
    /// A native menu read is out: (token, title, target window).
    waiting: Option<(u64, usize, isize)>,
    token: u64,
    /// Picked action with the app it was picked for, and how many times we
    /// have waited for that app to take the focus.
    after: Option<(menus::Action, menus::ActiveApp, u8)>,
    clock: String,
    /// The widest digit of the bar's font, by (family, text px): what the
    /// clock's width is kept for.
    clock_digit: Option<((String, u32), char)>,
    /// The room the clock is drawn in (status::steady_room), by its text's
    /// template, font and size: measured again only when that changes.
    clock_room: Option<(String, String)>,
    /// Its width so far under the time format, font and scale in force: it
    /// never narrows in a session.
    clock_width: status::SteadyWidth,
    net: Option<Net>,
    volume: Option<(u8, bool)>,
    battery: Option<(u8, bool)>,
    hovered: Option<usize>,
    pressed: Option<usize>,
    tracking: bool,
    /// Tray icons on show (refreshed on WM_TRAY_CHANGED).
    tray: Vec<crate::tray::Entry>,
    /// Last tray click, for double-click detection: (owner, uid, time ms).
    tray_focus_available: bool,
    tray_focus: Option<(String, HWND)>,
    tray_hover: Option<crate::tray::Entry>,
    last_tray_click: Option<(u32, u32, u32)>,
    /// Stable id of each tray icon (crate::tray::stable_id), same order.
    tray_ids: Vec<String>,
    /// Owner window -> exe path, for the ids (successful lookups only).
    tray_owners: HashMap<u32, String>,
    /// Tray surfaces by picture generation (kept across slot rebuilds).
    tray_brushes: HashMap<u64, CompositionSurfaceBrush>,
    /// The hidden tray icons are out (left of the chevron).
    expanded: bool,
    /// The notification area button stands in for the tray icons.
    tray_button: bool,
    /// Pointer x (DIP) at the last press, for the drag threshold, and the
    /// id of the tray icon pressed (checked again before a drag starts).
    press_x: f32,
    press_id: Option<String>,
    drag: Option<Drag>,
    /// Choices for icons without a stable id: this session only, never
    /// saved (id -> hidden).
    session_choices: HashMap<String, bool>,
    /// When each tray id was first seen this session (the order of icons
    /// the user has not placed: a re-added icon keeps its place).
    tray_seen: HashMap<String, u64>,
    /// The order the user dragged into this session, icons without a
    /// stable id included (the config keeps the rest); None = the config's.
    session_order: Option<Vec<String>>,
    /// The drawer shows `room` of its `total` hidden icons (what fits left
    /// of the chevron before the app menus); the wheel scrolls it by
    /// `shift` (0 = the ones nearest the chevron).
    drawer_room: usize,
    drawer_total: usize,
    drawer_shift: usize,
    /// Battery notifications, unregistered on destroy.
    power: Vec<HPOWERNOTIFY>,
    /// A fullscreen app is in front on its monitor: the bar lies behind it
    /// (not topmost) and rests (input poll, island, UIA).
    hidden_full: bool,
    /// Windows' virtual desktops (crate::vdesk), for the dots.
    desktops: crate::vdesk::Desktops,
    /// The desktop whose name tip is shown or waited for, and whether the
    /// tip is up (then it follows the pointer from dot to dot at once).
    tip: Option<desktops::TipTarget>,
    tip_live: bool,
    /// Windows hidden to the tray shown on this bar (hidden.rs), and its
    /// cell's tip: waited for, or shown (true).
    hidden: Vec<crate::traywin::Entry>,
    hidden_tip: Option<(usize, bool)>,
    /// Wheel delta over empty bar space not yet a whole notch.
    wheel_acc: i32,
    /// The Live Island (primary bar, when on): island/mod.rs.
    island: Option<island::Island>,
    /// What the PC allows the bar's style (look::Env), the wallpaper
    /// under the bar (clear) and how many measurements of it were taken
    /// (look::LooksIn), the measurement out for it (its key and token)
    /// and how many times a failed one was tried again, the ground drawn
    /// from all that (look::ground), and each cell's own look on a clear
    /// bar (look::cell_look; None: the ground's), read again only when
    /// what it is read from changes.
    env: look::Env,
    wall: look::Wall,
    wall_ver: u64,
    wall_asked: Option<(look::WallKey, u64)>,
    wall_tries: usize,
    glass_light: Option<bool>,
    ground: look::Ground,
    cell_looks: look::CellLooks,
}

/// Clear's guard plates (look::CellLook::guard): a rounded plate under a
/// cell, the open fill's shape, shown at once.
struct Guards {
    layer: ContainerVisual,
    plates: Vec<GuardPlate>,
}

/// One guard plate, its brush (recolored, never made again) and what it
/// shows now (look::plate_edit sets only what changes).
struct GuardPlate {
    v: SpriteVisual,
    g: windows::UI::Composition::CompositionRoundedRectangleGeometry,
    brush: Option<windows::UI::Composition::CompositionColorBrush>,
    shown: Option<look::Plate>,
}

impl Guards {
    fn new(c: &windows::UI::Composition::Compositor, layer: &windows::UI::Composition::VisualCollection, above: &impl Interface) -> Result<Guards> {
        let v = c.CreateContainerVisual()?;
        layer.InsertAbove(&v, &above.cast::<windows::UI::Composition::Visual>()?)?;
        Ok(Guards { layer: v, plates: Vec::new() })
    }

    /// Plate `i` at `spot` (x, y, w, h, corner DIP) in `color`, or none.
    fn show(&mut self, c: &windows::UI::Composition::Compositor, i: usize, at: Option<(capsule::Spot, crate::theme::Rgba)>) {
        while self.plates.len() <= i && at.is_some() {
            let made = (|| -> Result<_> {
                let v = c.CreateSpriteVisual()?;
                let g = c.CreateRoundedRectangleGeometry()?;
                v.SetClip(&c.CreateGeometricClipWithGeometry(&g)?)?;
                v.SetOpacity(0.0)?;
                self.layer.Children()?.InsertAtTop(&v)?;
                Ok(GuardPlate { v, g, brush: None, shown: None })
            })();
            match made {
                Ok(p) => self.plates.push(p),
                Err(_) => return,
            }
        }
        let Some(p) = self.plates.get_mut(i) else { return };
        let edit = look::plate_edit(p.shown, at);
        if let Some(((x, y, w, h, r), col)) = at {
            if edit.place {
                let size = Vector2 { X: w.max(0.0), Y: h.max(0.0) };
                let _ = p.v.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
                let _ = p.v.SetSize(size);
                let _ = p.g.SetSize(size);
                let _ = p.g.SetCornerRadius(Vector2 { X: r, Y: r });
            }
            if edit.color {
                let want = color(col.a, col.r, col.g, col.b);
                match &p.brush {
                    Some(b) => {
                        let _ = b.SetColor(want);
                    }
                    None => {
                        if let Ok(b) = c.CreateColorBrushWithColor(want) {
                            let _ = p.v.SetBrush(&b);
                            p.brush = Some(b);
                        }
                    }
                }
            }
        }
        if let Some(on) = edit.show {
            let _ = p.v.SetOpacity(if on { 1.0 } else { 0.0 });
        }
        // (No brush could be made: tried again next time.)
        p.shown = at.filter(|_| p.brush.is_some());
    }

    /// No plates past `n` cells.
    fn keep(&mut self, n: usize) {
        for p in self.plates.iter_mut().skip(n) {
            if p.shown.take().is_some() {
                let _ = p.v.SetOpacity(0.0);
            }
        }
    }
}

/// The bar that owns window `h`.
fn with_bar_of<R>(h: HWND, f: impl FnOnce(&mut TopBar) -> R) -> Option<R> {
    BARS.with(|b| b.try_borrow_mut().ok().and_then(|mut g| g.iter_mut().find(|x| x.hwnd == h).map(f)))
}

/// Runs `f` on every bar. False if the bars are busy (re-entered).
fn for_each_bar(mut f: impl FnMut(&mut TopBar)) -> bool {
    BARS.with(|b| {
        b.try_borrow_mut()
            .map(|mut g| {
                for x in g.iter_mut() {
                    f(x)
                }
            })
            .is_ok()
    })
}

/// Where a bar `h_px` tall goes on monitor `hmon` (rect `m`): clear of a
/// taskbar NopBar keeps; the monitor's top (`bottom`: bottom) edge while
/// it hides it.
fn bar_rect(hmon: isize, m: RECT, h_px: i32, bottom: bool) -> RECT {
    crate::placement::topbar_rect(m, &taskbar::bars_on(hmon), taskbar::hiding_now(), h_px, bottom)
}

fn reserve_bar(hwnd: HWND, monitor: RECT, r: RECT, cfg: &TopbarCfg) {
    let h_px = r.bottom - r.top;
    let reserve = if cfg.reserve_work_area { h_px } else { 0 };
    let bottom = cfg.at_bottom();
    let (edge, beyond) = if bottom { (ABE_BOTTOM, monitor.bottom - r.bottom) } else { (ABE_TOP, r.top - monitor.top) };
    taskbar::appbar_reserve_reach(hwnd, monitor, reserve, edge, beyond + reserve);
    let space = if bottom {
        BarSpace { top: 0, bottom: beyond + h_px, bottom_free: if reserve > 0 { 0 } else { h_px } }
    } else {
        BarSpace { top: if reserve > 0 { beyond + reserve } else { 0 }, ..BarSpace::default() }
    };
    set_reserved(monitor, space);
}

/// The taskbar moved, resized, or was hidden / given back: bars whose
/// place under it changes are placed again.
pub fn follow_taskbar() {
    for_each_bar(|b| unsafe {
        let h_px = b.at.bottom - b.at.top;
        if crate::monitors::info(b.hmon).is_some_and(|i| bar_rect(b.hmon, i.rect, h_px, b.cfg.at_bottom()) != b.at) {
            crate::log!("topbar: the taskbar moved; placing the bar clear of it");
            b.place();
            b.layout();
        }
    });
}

/// Px from the monitor's top (or bottom) that a bar keeps the dock out of
/// (its own strip plus a taskbar beyond it).
fn set_reserved(mon: RECT, space: BarSpace) {
    if let Ok(mut r) = RESERVED.lock() {
        r.retain(|(m, _)| *m != mon);
        if space != BarSpace::default() {
            r.push((mon, space));
        }
    }
}

fn reserved_snapshot() -> Vec<(RECT, BarSpace)> {
    RESERVED.lock().map(|r| r.clone()).unwrap_or_default()
}

// ---- public API (UI thread) ---------------------------------------------------

/// What the top bar keeps the dock out of on monitor `mon` (nothing when
/// it has no bar). Read by the dock: side docks keep below (or above) it,
/// a bottom dock sits right above a bar at the bottom.
pub fn space_for(mon: RECT) -> BarSpace {
    RESERVED.lock().ok().and_then(|r| r.iter().find(|(m, _)| *m == mon).map(|(_, s)| *s)).unwrap_or_default()
}

/// The bars sit on the bottom edge: menus, panels, cards and tips that hang
/// off them open upward.
pub fn at_bottom() -> bool {
    AT_BOTTOM.load(Ordering::SeqCst)
}

/// Makes the set of bars match the monitors (one each), if enabled.
pub fn create(gfx: Rc<Gfx>, cfg: &Config, theme: &Theme) {
    if !cfg.topbar.enabled {
        return;
    }
    AT_BOTTOM.store(cfg.topbar.at_bottom(), Ordering::SeqCst);
    // The system menu's facts about this PC, read off the UI thread.
    system::warm();
    // topbar.tray first, on every path (the first start, a config change,
    // new displays): no spy may start before it is applied. tray::start
    // below takes it too.
    crate::tray::set_enabled(cfg.topbar.tray);
    crate::tray::set_show_hotplug(cfg.topbar.tray_hotplug);
    let monitors = crate::monitors::all();
    let old_primary = BARS.with(|b| b.borrow().iter().find(|bar| bar.primary).map(|bar| bar.hmon));
    let new_primary = monitors.iter().find(|m| m.primary).map(|m| m.handle);
    // Bars of monitors that are gone.
    let gone: Vec<TopBar> = BARS.with(|b| {
        let mut g = b.borrow_mut();
        let (keep, drop): (Vec<TopBar>, Vec<TopBar>) =
            g.drain(..).partition(|x| monitors.iter().any(|m| m.handle == x.hmon));
        *g = keep;
        drop
    });
    if !gone.is_empty() {
        // What a gone bar may have left open (its desktop tip, its panels,
        // placed on a monitor that may be gone too) closes with it.
        desktops::hide_tip();
        control::cancel();
        stats::cancel();
        about::cancel();
    }
    if old_primary.is_some() && old_primary != new_primary {
        // The main bar's helpers stop posting to its window (a closed
        // window's handle can be reused by another one); the new main bar
        // below takes them back.
        status::detach();
        crate::tray::detach();
        crate::removable::detach();
        crate::vdesk::detach();
        crate::ime::detach();
    }
    for mut bar in gone {
        unsafe { bar.close() };
    }
    // Changing the primary display keeps both windows and their AppBars.
    for_each_bar(|bar| {
        let primary = Some(bar.hmon) == new_primary;
        if primary == bar.primary {
            return;
        }
        bar.primary = primary;
        if bar.full() {
            bar.refresh_tray();
        } else {
            bar.set_expanded(false);
            bar.tray.clear();
            bar.tray_ids.clear();
            bar.tray_button = false;
        }
        let _ = bar.build_slots();
        bar.update();
    });
    let mut created = false;
    for m in &monitors {
        if BARS.with(|b| b.borrow().iter().any(|x| x.hmon == m.handle)) {
            continue;
        }
        match unsafe { TopBar::new(gfx.clone(), cfg, theme, m.handle, m.primary) } {
            Ok(mut bar) => {
                // Start with what the other bars already show.
                if let Some(app) = LAST_APP.with(|a| a.borrow().clone()) {
                    bar.app = app.name.clone();
                    let (source, titles) = menus::menus_for(&app);
                    bar.source = source;
                    bar.titles = titles;
                    bar.active = app;
                    let _ = bar.build_slots();
                }
                let net = LAST_NET.load(Ordering::SeqCst);
                if net != usize::MAX {
                    bar.net = Some(Net::from_code(net));
                }
                let vol = LAST_VOL.load(Ordering::SeqCst);
                if vol != usize::MAX {
                    bar.volume = status::unpack_volume(vol);
                }
                // Another display's bar with the whole set: the tray too
                // (the main bar's comes with the tray spy's notice).
                if !bar.primary {
                    bar.refresh_tray();
                }
                bar.update();
                let hwnd = bar.hwnd;
                BARS.with(|b| b.borrow_mut().push(bar));
                created = true;
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            }
            Err(e) => crate::log!("topbar: cannot create: {e}"),
        }
    }
    if old_primary != new_primary {
        let owner = BARS.with(|b| b.borrow().iter().find(|b| b.primary).map(|b| (b.hwnd, b.tray_px())));
        if let Some((hwnd, px)) = owner {
            status::start(hwnd);
            crate::tray::start(hwnd, px, cfg.topbar.tray);
            crate::removable::start(hwnd);
            crate::vdesk::start(hwnd);
            crate::ime::attach(hwnd);
        }
    }
    if created || old_primary != new_primary {
        GENERATION.fetch_add(1, Ordering::SeqCst);
        crate::log!("topbar: {} bar(s)", BARS.with(|b| b.borrow().len()));
    }
    refresh_fullscreen();
}

/// Registers the AppBars (deferred until after the first frame, like the
/// dock's taskbar work: these are synchronous explorer round trips).
pub fn apply_appbar() {
    APPBARS_READY.store(true, Ordering::SeqCst);
    for_each_bar(|b| unsafe { b.register_appbar() });
}

/// The shared mica cache was invalidated by the dock or its session monitor.
pub fn mica_changed() {
    BARS.with(|bars| {
        if let Ok(bars) = bars.try_borrow() {
            for bar in bars.iter() {
                unsafe {
                    let _ = PostMessageW(Some(bar.hwnd), crate::desklayer::backdrop::WM_BACKDROP, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

/// Config, theme or monitors changed. Returns true if what any bar reserves
/// changed (the dock must re-place itself).
pub fn apply_config(gfx: &Rc<Gfx>, cfg: &Config, theme: &Theme) -> bool {
    let before = reserved_snapshot();
    if !cfg.topbar.enabled {
        if BARS.with(|b| !b.borrow().is_empty()) {
            destroy();
        }
    } else {
        if cfg.topbar.at_bottom() != at_bottom() && BARS.with(|b| !b.borrow().is_empty()) {
            // The bars change edges: what hangs off them closes (it opened
            // the other way, from where the bar was).
            crate::popup::cancel();
            control::cancel();
            stats::cancel();
            about::cancel();
            desktops::hide_tip();
        }
        // New bars register themselves (APPBARS_READY).
        create(gfx.clone(), cfg, theme);
        for_each_bar(|b| b.reconfigure(cfg, theme));
        // An open Quick settings follows light/dark (its own dark mode
        // tile among the causes), high contrast and the animation setting.
        control::retheme(theme);
        stats::retheme(theme);
        about::retheme(theme);
    }
    crate::pieces::retheme(theme.non_shell());
    crate::notes::retheme(theme.non_shell());
    crate::search::retheme(theme);
    crate::launchpad::retheme(theme);
    before != reserved_snapshot()
}

/// The UI language changed: menu titles and the clock are text in it.
pub fn language_changed() {
    for_each_bar(|b| {
        let (source, titles) = menus::menus_for(&b.active);
        b.source = source;
        b.titles = titles;
        let _ = b.build_slots();
        b.tick_clock();
        // Default desktop names are in the language; the dots mirror in RTL.
        b.refresh_tip();
    });
}

/// The app in front changed (or its windows did). Every bar shows it.
pub fn set_app(app: menus::ActiveApp) {
    if crate::popup::is_tracking() {
        // A menu is open: keep its target until it closes.
        return;
    }
    LAST_APP.with(|a| *a.borrow_mut() = Some(app.clone()));
    // "Only on the display I'm using": the island goes where this window is.
    let bars: Vec<(isize, bool)> = BARS.with(|b| b.try_borrow().map(|g| g.iter().map(|x| (x.hmon, x.primary)).collect()).unwrap_or_default());
    let moved = island::set_island_monitor(island::island_monitor(window_monitor(app.hwnd), &bars));
    for_each_bar(|b| {
        if moved {
            b.island_refresh();
        }
        if let Some((_, _, target)) = b.waiting {
            if target != app.hwnd {
                // The user moved on before the native menu arrived.
                b.waiting = None;
                b.highlight_open(None);
            }
        }
        let refresh_menus = b.active.hwnd != app.hwnd || b.active.exe != app.exe || b.active.desktop != app.desktop;
        b.app = app.name.clone();
        b.active = app.clone();
        if refresh_menus {
            let (source, titles) = menus::menus_for(&b.active);
            b.source = source;
            if titles != b.titles {
                b.titles = titles;
                let _ = b.build_slots();
            }
        }
        b.update();
    });
}

pub(crate) fn last_app_window() -> HWND {
    let hwnd = LAST_APP.with(|app| app.try_borrow().ok().and_then(|app| app.as_ref().map(|app| app.hwnd)));
    HWND(hwnd.unwrap_or_default() as *mut _)
}

/// The window in front may have moved to another display (dock/mod.rs:
/// its location events, debounced): with "Only on the display I'm
/// using" the island goes there.
pub fn foreground_moved() {
    let fg = unsafe { GetForegroundWindow() };
    let bars: Vec<(isize, bool)> = BARS.with(|b| b.try_borrow().map(|g| g.iter().map(|x| (x.hmon, x.primary)).collect()).unwrap_or_default());
    if bars.is_empty() || BARS.with(|b| b.try_borrow().map(|g| g.iter().any(|x| x.hwnd == fg)).unwrap_or(true)) {
        return;
    }
    if island::set_island_monitor(island::island_monitor(window_monitor(fg.0 as isize), &bars)) {
        for_each_bar(|b| b.island_refresh());
    }
}

/// The monitor window `h` is on (None: no window, or on none).
fn window_monitor(h: isize) -> Option<isize> {
    use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONULL};
    if h == 0 {
        return None;
    }
    let m = unsafe { MonitorFromWindow(HWND(h as *mut _), MONITOR_DEFAULTTONULL) };
    (!m.is_invalid()).then_some(m.0 as isize)
}

/// Sink each bar behind a fullscreen app that covers its monitor (as the
/// Windows taskbar does) and bring it back on top once the app goes. It is
/// never hidden: a wrong guess leaves it under one window, not gone.
/// False if the bars were busy (the caller checks again later).
pub fn refresh_fullscreen() -> bool {
    let mut changed = false;
    let checked = for_each_bar(|b| unsafe {
        let full = crate::dock::fullscreen_on_monitor(b.hmon, b.monitor);
        if full == b.hidden_full {
            // Left under a normal window although topmost: back on top
            // (as the dock does, crate::dock::sunk).
            if !full && crate::dock::sunk(b.hwnd) {
                b.restack();
            }
            return;
        }
        b.hidden_full = full;
        changed = true;
        if full {
            b.drop_tip();
        }
        b.restack();
        // The input poll rests while its bar is covered.
        b.input_arm();
        // So does the island (and its music bars).
        b.island_refresh();
        // And the clock's seconds (back on the second when uncovered).
        b.tick_clock();
    });
    if changed {
        crate::companion::refresh();
    }
    checked
}

/// Someone can see the bars again, or no one can (the session locked,
/// asleep or back, the display off or on; dock/session.rs): the clock
/// ticks by the second or rests on the minute. Busy bars catch up on their
/// next minute tick.
pub fn session_idle_changed() {
    for_each_bar(|b| b.tick_clock());
    // An open calendar's agenda rests too, and catches up when seen again.
    if let Some(bar) = OPEN_CALENDAR.with(|o| o.borrow().as_ref().map(|c| c.bar)) {
        if crate::dock::session_idle() {
            arm_agenda(bar, None);
        } else {
            agenda_changed();
        }
    }
}

/// Explorer restarted, or a logoff was cancelled: the shell forgot our
/// AppBars. Register them again.
pub fn reregister() {
    // A cancelled logoff: shutdowns can be scheduled again (the one
    // pending then stays dropped).
    self::shutdown::reopen();
    for_each_bar(|b| unsafe {
        b.appbar = false;
        b.register_appbar();
    });
    // Explorer may have rewritten the desktop keys.
    crate::vdesk::poke();
}

/// After sleep, unlock or a reconnect (audit A09, from dock/session.rs):
/// the top edges are reserved again, the clock re-armed (a timer set before
/// sleeping fires late), battery read, the audio/network listener and the
/// tray spy refreshed. False if the bars were busy (the caller retries).
pub fn resync() -> bool {
    let done = for_each_bar(|b| {
        b.tick_clock();
        b.read_battery();
        b.island_resync();
    });
    if done {
        reregister();
        status::resync();
        crate::tray::resync();
    }
    done
}

/// The GPU device was replaced (dock/gpu.rs): every surface lost its
/// pixels, so each bar draws all of its cells and tray icons again. False
/// if the bars were busy (the caller tries again shortly).
pub fn redraw_all() -> bool {
    desktops::hide_tip();
    for_each_bar(|b| {
        b.drop_tip();
        b.follow_device();
        b.surfaces.clear();
        b.tray_brushes.clear();
        for s in &mut b.slots {
            s.key.clear();
        }
        b.update();
        b.island_redraw();
    })
}

/// Give the top edges back (exit, logoff). A scheduled shutdown goes
/// first: NopBar quitting drops it, and no timer may run it meanwhile.
pub fn shutdown() {
    self::shutdown::stop();
    for_each_bar(|b| unsafe { b.remove_appbar() });
}

pub fn destroy() {
    self::shutdown::stop();
    GENERATION.fetch_add(1, Ordering::SeqCst);
    crate::popup::cancel();
    control::cancel();
    stats::cancel();
    about::cancel();
    menus::clear_ready();
    let bars: Vec<TopBar> = BARS.with(|b| b.borrow_mut().drain(..).collect());
    status::detach();
    crate::tray::detach();
    crate::removable::detach();
    crate::vdesk::detach();
    crate::ime::detach();
    desktops::hide_tip();
    for mut bar in bars {
        unsafe { bar.close() };
    }
    if let Ok(mut r) = RESERVED.lock() {
        r.clear();
    }
    AT_BOTTOM.store(false, Ordering::SeqCst);
    crate::log!("topbar: removed");
}

// ---- the bar --------------------------------------------------------------

fn color(a: u8, r: u8, g: u8, b: u8) -> Color {
    Color { A: a, R: r, G: g, B: b }
}

/// Pure: the bar draws its tint alone, opaque (no backdrop): only when the
/// resolved theme makes it solid (Windows' transparency off, high
/// contrast, a solid theme). The blur effect is not looked at: Flat's bar
/// is as it was (0.1.37), and Glass without the blur is Flat already.
fn bar_solid(theme: &Theme) -> bool {
    theme.topbar.material == "solid"
}

/// What the bar itself is drawn from.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ground {
    /// Material M0's tint: opaque when the bar is solid (bar_solid).
    tint: crate::theme::Rgba,
    /// The blur shows under the tint (Glass).
    backdrop: bool,
    /// A line along the bar's inner edge: (color, DIP). High contrast
    /// only (12.3: a surface's boundary there; the bar's along the one
    /// edge that is not the screen's); otherwise the bar is borderless: a
    /// surface on the screen's edge has neither rim nor shadow (2.6).
    edge: Option<(crate::theme::Rgba, f32)>,
}

/// Pure: the bar's ground in `theme` (design system 2.1, material M0:
/// `#F5F5F5` / `#1E1E1E` at .74 over the blur with Glass; the same tint
/// opaque on Flat, `#F0F0F0` / `#1E1E1E`).
fn bar_ground(theme: &Theme) -> Ground {
    let solid = bar_solid(theme);
    let t = theme.material(crate::theme::Material::Bar);
    Ground {
        tint: crate::theme::Rgba { a: if solid { 255 } else { t.a }, ..t },
        backdrop: !solid,
        edge: theme.outline_of(crate::theme::Role::Raised, crate::theme::State::Rest),
    }
}

/// Pure: where the bar's edge line (`w` DIP thick) lies down a bar `bar_h`
/// tall: along its bottom, or its top for a bar at the screen's bottom
/// (`at_bottom`): the edge towards the desktop.
fn edge_y(bar_h: f32, w: f32, at_bottom: bool) -> f32 {
    if at_bottom {
        0.0
    } else {
        (bar_h - w).max(0.0)
    }
}

/// What the battery is drawn from: its level and colors (status::
/// battery_look), its number (None while charging: the bolt), its body's
/// corners and the scale.
struct BatteryArt {
    label: Option<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32, f32)>,
    level: f32,
    fill: D2D1_COLOR_F,
    on_fill: D2D1_COLOR_F,
    ink: D2D1_COLOR_F,
    radius: f32,
    percent: u8,
    charging: bool,
    scale: f32,
}

/// Pure: the battery body's corners: a small mark's (the theme's inner
/// radius, 4: design system 1.2), not a control's (8 would make the 12 DIP
/// body a capsule).
fn battery_radius(theme: &Theme) -> f32 {
    theme.radii().inner
}

/// The battery at `percent` in the bar's `ink` at `scale`.
fn battery_art(gfx: &Gfx, theme: &Theme, ink: D2D1_COLOR_F, percent: u8, charging: bool, scale: f32) -> BatteryArt {
    let hc = theme.hc.filter(|_| theme.high_contrast);
    let (level, fill_kind) = status::battery_look(percent, charging, hc.is_some());
    let opaque = |c: crate::theme::Rgba| D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: 1.0 };
    let (fill, on_fill) = match fill_kind {
        // Dark on a light ink (dark bar), white on a dark ink (light bar).
        status::BatteryFill::Normal => {
            let light = 0.2126 * ink.r + 0.7152 * ink.g + 0.0722 * ink.b > 0.5;
            let v = if light { 0.0 } else { 1.0 };
            (D2D1_COLOR_F { a: 1.0, ..ink }, D2D1_COLOR_F { r: v, g: v, b: v, a: 1.0 })
        }
        status::BatteryFill::Low => (D2D1_COLOR_F { r: 1.0, g: 0.27, b: 0.23, a: 1.0 }, D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }),
        status::BatteryFill::HighContrast => (D2D1_COLOR_F { a: 1.0, ..ink }, hc.map(|h| opaque(h.window)).unwrap_or(ink)),
    };
    // The number, or the bolt (crate::glyphs) while charging.
    let label = if charging { None } else { gfx.text_layout_tabular(&format!("{percent}"), &theme.font.family, BATTERY_TEXT * scale, DWRITE_FONT_WEIGHT_SEMI_BOLD).ok() };
    BatteryArt { label, level, fill, on_fill, ink, radius: battery_radius(theme), percent, charging, scale }
}

/// Where battery `a` drawn from (`ox`, `oy`) px puts its number (its ink
/// centered on the body, whole pixels) and the fill's edge (px): never
/// through the number (final audit 8: "72" had its "2" half in each
/// color), out of its ink (or the bolt's) to the nearer side.
fn battery_places(a: &BatteryArt, ox: f32, oy: f32) -> (Option<windows_numerics::Vector2>, f32) {
    let s = a.scale;
    let (bl, bt, br, bb) = BATTERY_BODY;
    let (mx, my) = ((bl + br) / 2.0, (bt + bb) / 2.0);
    let label_at = a.label.as_ref().map(|(l, _, _)| {
        let at = ink_origin(l, ox + mx * s, oy + my * s);
        windows_numerics::Vector2 { X: at.X.round(), Y: at.Y.round() }
    });
    let span = match (&a.label, label_at) {
        (Some((l, _, _)), Some(at)) => Some(ink_span(l, at.X)),
        _ if a.charging => Some((ox + (mx - BATTERY_BOLT / 2.0) * s, ox + (mx + BATTERY_BOLT / 2.0) * s)),
        _ => None,
    };
    let edge = ox + battery_edge(a.level, s);
    let edge = if a.level > 0.0 && a.percent < 100 { span.map_or(edge, |(x0, x1)| clear_of_ink(edge, x0, x1)) } else { edge };
    (label_at, edge)
}

/// Draws battery `a` with its top left at (`ox`, `oy`) px (BATTERY_W x
/// BATTERY_H DIP).
unsafe fn paint_battery(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, ox: f32, oy: f32, a: &BatteryArt) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
    use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT};
    let (s, ink, fill, on_fill) = (a.scale, a.ink, a.fill, a.on_fill);
    let r = |l: f32, t: f32, rr: f32, b: f32| D2D_RECT_F { left: ox + l * s, top: oy + t * s, right: ox + rr * s, bottom: oy + b * s };
    // A dim track, the fill up to the charge (its edge on a whole pixel),
    // the terminal lit only when full.
    let (bl, bt, br, bb) = BATTERY_BODY;
    let rad = a.radius * s;
    let body_r = D2D1_ROUNDED_RECT { rect: r(bl, bt, br, bb), radiusX: rad, radiusY: rad };
    let track = dc.CreateSolidColorBrush(&D2D1_COLOR_F { a: 0.38, ..ink }, None)?;
    dc.FillRoundedRectangle(&body_r, &track);
    let (mx, my) = ((bl + br) / 2.0, (bt + bb) / 2.0);
    let (label_at, edge) = battery_places(a, ox, oy);
    let filled = D2D_RECT_F { left: ox, top: oy, right: edge, bottom: oy + BATTERY_H * s };
    let empty = D2D_RECT_F { left: edge, top: oy, right: ox + BATTERY_W * s, bottom: oy + BATTERY_H * s };
    let body = dc.CreateSolidColorBrush(&fill, None)?;
    if a.level > 0.0 {
        dc.PushAxisAlignedClip(&filled, D2D1_ANTIALIAS_MODE_ALIASED);
        dc.FillRoundedRectangle(&body_r, &body);
        dc.PopAxisAlignedClip();
    }
    let nub_c = if a.percent >= 100 { D2D1_COLOR_F { a: fill.a * 0.7, ..fill } } else { D2D1_COLOR_F { a: 0.38, ..ink } };
    let nub = dc.CreateSolidColorBrush(&nub_c, None)?;
    let (nl, nt, nr, nb) = BATTERY_NUB;
    let nub_rad = (nr - nl) / 2.0 * s;
    dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r(nl, nt, nr, nb), radiusX: nub_rad, radiusY: nub_rad }, &nub);
    if let (Some((l, _, _)), Some(at)) = (&a.label, label_at) {
        // Center the ink, not the line box. Over the fill in the dark
        // (on-fill) color, over the track in the bar's ink.
        let on = dc.CreateSolidColorBrush(&on_fill, None)?;
        let off = dc.CreateSolidColorBrush(&D2D1_COLOR_F { a: 1.0, ..ink }, None)?;
        for (clip, brush) in [(filled, &on), (empty, &off)] {
            dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            dc.DrawTextLayout(at, l, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            dc.PopAxisAlignedClip();
        }
    }
    if a.charging {
        // The bolt (a mark, ICON_XS), in the same two colors as the number.
        for (clip, c) in [(filled, on_fill), (empty, D2D1_COLOR_F { a: 1.0, ..ink })] {
            dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            crate::glyphs::draw(dc, Glyph::Bolt, ox + mx * s, oy + my * s, BATTERY_BOLT * s, c)?;
            dc.PopAxisAlignedClip();
        }
    }
    Ok(())
}

/// A glyph cell's picture: glyph `g` at about `em` px fitted in the `side`
/// px square from (`ox`, `oy`) (glyphs::fitted) in `ink`. The bar's glyph
/// cells and the audit's pictures of them.
pub(super) unsafe fn paint_glyph_cell(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, g: Glyph, ox: f32, oy: f32, side: f32, em: f32, ink: D2D1_COLOR_F) -> Result<()> {
    crate::gfx::draw_glyph_frame(dc, g, &crate::glyphs::fitted(g, ox, oy, side, em), ink)
}

/// Pure: the right side's status cells, in reading order. The main bar
/// (and every bar with `same_on_all_displays`): the chosen status icons,
/// Search, then the Quick settings button. Other bars have no Quick
/// settings: Wi-Fi, sound and the chosen battery.
fn status_cells(full: bool, wifi: bool, volume: bool, battery: bool) -> Vec<Cell> {
    let mut cells = Vec::new();
    if wifi || !full {
        cells.push(Cell::Net);
    }
    if volume || !full {
        cells.push(Cell::Volume);
    }
    if battery {
        cells.push(Cell::Battery);
    }
    if full {
        cells.extend([Cell::Search, Cell::Control]);
    }
    cells
}

/// The bar's picture for the promo film (tests only). Declared down
/// here, with the tests: the tests that read this file's code read it up
/// to its first `#[cfg(test)]`.
#[cfg(test)]
mod promo;
#[cfg(test)]
pub(crate) use companion::paint_home as paint_companion_home;

/// Snapshot adapter for the production tooltip, without creating a window.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn paint_tip_offscreen(gfx: &Gfx, theme: &Theme, dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, scale: f32, text: &str, x: f32, y: f32) -> Result<()> {
    if let Some(a) = desktops::tip_art(gfx, theme, scale, text, Some(200. * scale)) {
        desktops::paint_tip(dc, x - a.b.margin as f32, y - a.b.margin as f32, &a)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) use promo::font_compare;

/// The bar's cells in their states for the design audit (tests only).
#[cfg(test)]
#[path = "bar_harmony.rs"]
mod bar_harmony;
#[cfg(test)]
mod site_renders;

#[cfg(test)]
mod look_tests {
    use super::*;
    use crate::syslook::SysLook;
    use crate::theme::Finish;

    #[test]
    fn flats_bar_is_opaque_with_or_without_the_blur() {
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let flat = Theme::graphite();
            // Flat (harmony v2.1): material M0 opaque, no backdrop, with
            // the blur effect or without it.
            for blur in [true, false] {
                assert!(bar_solid(&flat.resolve_fx(&sys, blur)), "flat, blur {blur}, light {light}");
            }
            // Glass: see-through over the blur; without the blur exactly
            // Flat's bar.
            let mut glass = Theme::graphite();
            glass.finish = Finish::Glass;
            assert!(!bar_solid(&glass.resolve_fx(&sys, true)));
            let fallen = glass.resolve_fx(&sys, false);
            assert!(bar_solid(&fallen));
            assert_eq!((fallen.topbar_tint(), &fallen.topbar), (flat.resolve_fx(&sys, false).topbar_tint(), &flat.resolve_fx(&sys, false).topbar));
            // Transparency off: opaque, either finish.
            let off = SysLook { light, transparency: false, ..Default::default() };
            assert!(bar_solid(&flat.resolve_fx(&off, true)) && bar_solid(&glass.resolve_fx(&off, true)));
        }
    }

    #[test]
    fn glass_bar_is_a_clean_frost() {
        let mut glass = Theme::graphite();
        glass.finish = Finish::Glass;
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let t = glass.resolve_fx(&sys, true);
            // See-through: the blur and the tint alone (the bar has no
            // other visual), a little thinner than the panels.
            assert!(t.glass() && !bar_solid(&t), "light {light}");
            assert!(t.topbar_tint().a < t.popup_tint().a && t.topbar_tint().a < 255);
            assert_eq!(t.topbar.material, "acrylic");
            // Opaque again with transparency off or high contrast.
            assert!(bar_solid(&glass.resolve_fx(&SysLook { light, transparency: false, ..Default::default() }, true)));
            let hc = crate::syslook::HighContrast { window: crate::theme::Rgba { r: 0, g: 0, b: 0, a: 255 }, text: crate::theme::Rgba { r: 255, g: 255, b: 255, a: 255 }, highlight: crate::theme::Rgba { r: 255, g: 255, b: 0, a: 255 }, highlight_text: crate::theme::Rgba { r: 0, g: 0, b: 0, a: 255 }, gray_text: crate::syslook::GRAY_TEXT };
            assert!(bar_solid(&glass.resolve_fx(&SysLook { light, high_contrast: Some(hc), ..Default::default() }, true)));
        }
    }
}

/// Where to draw `layout` so its ink (not its line box) is centered on
/// (cx, cy), px.
/// The span (px) the ink of `layout` drawn from x `at_x` takes across.
fn ink_span(layout: &windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, at_x: f32) -> (f32, f32) {
    let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = layout.GetMetrics(&mut m);
    }
    let o = unsafe { layout.GetOverhangMetrics() }.unwrap_or_default();
    (at_x - o.left, at_x + m.layoutWidth + o.right)
}

/// Pure: the battery fill's edge (px, whole) kept out of the number's ink
/// `x0`..`x1` (px): where it would fall within it (or a pixel of it), it
/// goes just past the nearer side, so the number stands all in one color.
fn clear_of_ink(edge: f32, x0: f32, x1: f32) -> f32 {
    if edge <= x0 - 1.0 || edge >= x1 + 1.0 {
        return edge;
    }
    if edge - x0 < x1 - edge {
        (x0 - 1.0).floor()
    } else {
        (x1 + 1.0).ceil()
    }
}

pub(crate) fn ink_origin(layout: &windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, cx: f32, cy: f32) -> windows_numerics::Vector2 {
    let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = layout.GetMetrics(&mut m);
    }
    let o = unsafe { layout.GetOverhangMetrics() }.unwrap_or_default();
    let (ink_l, ink_t) = (-o.left, -o.top);
    let (ink_w, ink_h) = (m.layoutWidth + o.right - ink_l, m.layoutHeight + o.bottom - ink_t);
    windows_numerics::Vector2 { X: cx - ink_w / 2.0 - ink_l, Y: cy - ink_h / 2.0 - ink_t }
}

impl TopBar {
    unsafe fn new(gfx: Rc<Gfx>, cfg: &Config, theme: &Theme, hmon: isize, primary: bool) -> Result<Self> {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("NopBarTopBar");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("Top bar"),
            WS_POPUP,
            0,
            0,
            100,
            28,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        let early = if APPBARS_READY.load(Ordering::SeqCst) {
            crate::monitors::info(hmon).map(|info| {
                let h_px = (cfg.topbar.height.clamp(20.0, 48.0) * info.scale()).round() as i32;
                let at = bar_rect(hmon, info.rect, h_px, cfg.topbar.at_bottom());
                let _ = SetWindowPos(hwnd, None, at.left, at.top, at.right - at.left, at.bottom - at.top, SWP_NOACTIVATE | SWP_NOZORDER);
                taskbar::appbar_register(hwnd);
                reserve_bar(hwnd, info.rect, at, &cfg.topbar);
                (info.rect, at)
            })
        } else {
            None
        };
        // Anything below failing must not leave a stray window behind.
        let r = Self::init(hwnd, gfx, cfg, theme, hmon, primary).map(|mut bar| {
            if let Some((mon, at)) = early {
                bar.appbar = true;
                if (mon, at) != (bar.monitor, bar.at) {
                    set_reserved(mon, BarSpace::default());
                    reserve_bar(hwnd, bar.monitor, bar.at, &cfg.topbar);
                }
            }
            bar
        });
        if r.is_err() {
            if let Some((mon, _)) = early {
                taskbar::appbar_remove(hwnd);
                set_reserved(mon, BarSpace::default());
            }
            let _ = DestroyWindow(hwnd);
        }
        r
    }

    unsafe fn init(hwnd: HWND, gfx: Rc<Gfx>, cfg: &Config, theme: &Theme, hmon: isize, primary: bool) -> Result<Self> {
        let enable = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_HOSTBACKDROPBRUSH,
            &enable as *const _ as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
        let c = gfx.compositor.clone();
        let interop: ICompositorDesktopInterop = c.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
        let root = c.CreateContainerVisual()?;
        target.SetRoot(&root)?;
        let backdrop = c.CreateSpriteVisual()?;
        // The blur through the glass's graph (crate::glass; apply_theme
        // keeps it in step with the theme).
        backdrop.SetBrush(&crate::glass::backdrop(&gfx, theme, crate::theme::Material::Bar)?)?;
        backdrop.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        let mica = c.CreateSpriteVisual()?;
        mica.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        mica.SetOpacity(0.0)?;
        let tint = c.CreateSpriteVisual()?;
        tint.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        let edge = c.CreateSpriteVisual()?;
        edge.SetOpacity(0.0)?;
        root.Children()?.InsertAtTop(&backdrop)?;
        root.Children()?.InsertAtTop(&mica)?;
        root.Children()?.InsertAtTop(&tint)?;
        root.Children()?.InsertAtTop(&edge)?;
        // The cells' capsules: over the bar's ground, under the cells.
        let caps = capsule::Capsules::new(&c, &root.Children()?, &edge)?;
        // (Right over the edge: under the capsules made above it.)
        let guards = Guards::new(&c, &root.Children()?, &edge)?;

        // Battery: event-driven via WM_POWERBROADCAST.
        let power: Vec<HPOWERNOTIFY> = [&GUID_BATTERY_PERCENTAGE_REMAINING, &GUID_ACDC_POWER_SOURCE]
            .into_iter()
            .filter_map(|g| {
                RegisterPowerSettingNotification(windows::Win32::Foundation::HANDLE(hwnd.0), g, REGISTER_NOTIFICATION_FLAGS(0)).ok()
            })
            .collect();

        let mut bar = Self {
            hwnd,
            hmon,
            primary,
            hidden_full: false,
            gfx,
            _target: target,
            root,
            backdrop,
            mica,
            tint,
            edge,
            caps,
            guards,
            anchor: None,
            slots: Vec::new(),
            surfaces: HashMap::new(),
            cfg: cfg.topbar.clone(),
            companion_enabled: cfg.companion.enabled,
            companion_situations: cfg.companion.enabled&&cfg.companion.situations,
            theme: theme.clone(),
            scale: 1.0,
            width: 0.0,
            height: 0.0,
            monitor: RECT::default(),
            at: RECT::default(),
            appbar: false,
            app: String::new(),
            active: menus::ActiveApp::default(),
            source: menus::Source::None,
            titles: Vec::new(),
            open: None,
            built: None,
            ready: None,
            waiting: None,
            token: 0,
            after: None,
            clock: String::new(),
            clock_digit: None,
            clock_room: None,
            clock_width: status::SteadyWidth::default(),
            net: None,
            volume: None,
            battery: None,
            hovered: None,
            pressed: None,
            tracking: false,
            power,
            tray: Vec::new(),
            tray_focus_available: false,
            tray_focus: None,
            tray_hover: None,
            last_tray_click: None,
            tray_ids: Vec::new(),
            tray_owners: HashMap::new(),
            tray_brushes: HashMap::new(),
            expanded: false,
            tray_button: false,
            press_x: 0.0,
            press_id: None,
            drag: None,
            session_choices: HashMap::new(),
            tray_seen: HashMap::new(),
            session_order: None,
            drawer_room: 0,
            drawer_total: 0,
            drawer_shift: 0,
            desktops: crate::vdesk::latest(),
            tip: None,
            tip_live: false,
            hidden: Vec::new(),
            hidden_tip: None,
            wheel_acc: 0,
            island: None,
            env: look::Env::now(cfg),
            wall: look::Wall::Pending,
            wall_ver: 0,
            wall_asked: None,
            wall_tries: 0,
            glass_light: None,
            ground: look::ground(theme, look::Style::of(cfg.topbar.style_for(theme.finish.is_glass())), look::Env::now(cfg), &look::Wall::Pending),
            cell_looks: look::CellLooks::default(),
        };
        bar.place();
        bar.apply_theme();
        if let Err(e) = bar.build_slots() {
            for h in bar.power.drain(..) {
                let _ = UnregisterPowerSettingNotification(h);
            }
            return Err(e);
        }
        bar.read_battery();
        bar.tick_clock();
        bar.island_setup();
        Ok(bar)
    }

    /// The bar's style as chosen (topbar.style; "auto": the finish
    /// in force, TopbarCfg::style_for: a Glass the PC turns Flat, with the
    /// blur off or software rendering, is opaque like the dock).
    fn style(&self) -> look::Style {
        look::Style::of(self.cfg.style_for(self.theme.finish.is_glass()))
    }

    /// The ground from the theme, the style, what the PC allows and the
    /// wallpaper under the bar (look::ground); drawn again only if it
    /// changed.
    fn apply_ground(&mut self) {
        let g = look::ground(&self.theme, self.style(), self.env, &self.wall);
        if g != self.ground {
            self.apply_theme();
            self.update();
            self.island_redraw();
        }
    }

    /// The device may be the software one now, or the hardware one again
    /// (a GPU recovery): the frost bar falls back with the glass
    /// (look::Env::blurs). The theme follows by itself where the look
    /// changed (apply_config); a Flat finish's frost bar does not wait
    /// for that.
    fn follow_device(&mut self) {
        let env = look::Env { software: crate::gfx::software_in_force(), ..self.env };
        if env != self.env {
            self.env = env;
            if look::ground(&self.theme, self.style(), env, &self.wall) != self.ground {
                self.apply_theme();
            }
        }
    }

    fn apply_theme(&mut self) {
        let c = &self.gfx.compositor;
        // The style (look::ground). Recipe is material M0 (bar_ground):
        // the tint over the blur with a glass, opaque on Flat. Clear has no
        // plate, Frost its own one over the blur, Solid an opaque band.
        // Glass uses the cached mica strip. Transparency off and high
        // contrast use the solid band.
        let ground = look::ground(&self.theme, self.style(), self.env, &self.wall);
        self.ground = ground;
        let recipe = ground.backdrop && ground.style == look::Style::Recipe;
        let glass = ground.style == look::Style::Glass;
        let mica = ground.mica;
        // Recipe over the blur: the glass's graph retuned and the glass's
        // tint for the brush it has (the Luminosity layer folded in where
        // that brush does not draw it). Frost: the same graph at its own
        // saturation alone, whatever the glass, its tint as it is.
        let t = if recipe {
            crate::glass::refresh_backdrop(&self.gfx, &self.backdrop, &self.theme, crate::theme::Material::Bar)
        } else {
            if ground.backdrop && ground.style == look::Style::Frost {
                crate::glass::refresh_fixed(&self.gfx, &self.backdrop, look::FROST_SAT);
            }
            if glass && !mica {
                crate::glass::refresh_fixed(&self.gfx, &self.backdrop, crate::theme::AdaptiveGlass::SATURATION);
            }
            ground.tint
        };
        // Denser over a wallpaper the ink needs more of (look::deepen_for).
        let t = ground.laid(t);
        // Flat v2's opaque band falls from top to bottom (ground.fall).
        if let Ok(b) = crate::glass::fall_brush(c, ground.fall, t) {
            let _ = self.tint.SetBrush(&b);
        }
        let _ = self.backdrop.SetOpacity(if ground.backdrop && !mica { 1.0 } else { 0.0 });
        let _ = self.mica.SetOpacity(if mica { 1.0 } else { 0.0 });
        // The glass's top light and grain over the tint (the bar has no
        // rim: it touches the screen's edge); Flat v2's grain on its band.
        if let Ok(tint) = self.tint.cast::<windows::UI::Composition::Visual>() {
            crate::glass::place_top_light(&self.gfx, &self.theme, crate::theme::Material::Bar, &self.root, &tint, recipe || ground.grain > 0.0);
        }
        match ground.edge {
            Some((e, _)) => {
                if let Ok(b) = c.CreateColorBrushWithColor(color(e.a, e.r, e.g, e.b)) {
                    let _ = self.edge.SetBrush(&b);
                }
                let _ = self.edge.SetOpacity(1.0);
            }
            None => {
                let _ = self.edge.SetOpacity(0.0);
            }
        }
        self.place_edge();
        self.halo_slots();
        // Clear and Glass read their ink from the wallpaper.
        self.refresh_wall();
        // What is lit is painted again in the new colors.
        self.caps.restyle();
        self.surfaces.clear();
        // Our own tray icon is drawn in the bar's text color (tray_surface).
        self.tray_brushes.clear();
        for s in &mut self.slots {
            s.key.clear();
        }
    }

    /// Clear's halo (look::Halo) under every cell's content but the tray
    /// icons (drawn in their own colors, NopBar's gnome too: a shadow
    /// under them read as a smudge); none otherwise. The shadow takes the
    /// shape of what the cell shows, whatever its brush (gfx::halo_shadow).
    fn halo_slots(&self) {
        let c = &self.gfx.compositor;
        let mine = |s: &Slot| !matches!(s.cell, Cell::Tray(_));
        for (i, s) in self.slots.iter().enumerate() {
            let halo = self.cell_looks.get(i).map(|l| l.halo).or(self.ground.halo);
            let made = match halo.filter(|_| mine(s)) {
                Some(h) => crate::gfx::halo_shadow(c, h).and_then(|d| s.visual.SetShadow(&d)),
                None => s.visual.SetShadow(None::<&windows::UI::Composition::CompositionShadow>),
            };
            if let Err(e) = made {
                crate::log!("topbar: halo: {e}");
            }
        }
    }

    /// Clear's ink, and how dense a frost or recipe plate is
    /// (look::deepen_for), follow the wallpaper under the bar: measured
    /// again on a helper (look::ask) when the monitor, the bar's own
    /// rectangle on it, the displays' box or the wallpaper changed. Nothing
    /// for the solid band.
    fn refresh_wall(&mut self) {
        let style = look::in_force(self.style(), &self.theme, self.env);
        if !look::reads_wall(style) || self.monitor == RECT::default() {
            return;
        }
        let k = look::key(self.monitor, self.at, about::displays_box(), style == look::Style::Glass);
        match self.wall_asked {
            Some((a, _)) if a == k => return,
            Some(_) | None => {
                // Something changed: a failed read starts over.
                self.wall_tries = 0;
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), WALL_TIMER);
                }
            }
        }
        self.wall_asked = Some((k, look::ask(self.hwnd, k)));
    }

    /// A read that failed is tried again (WALL_TIMER): the same key.
    fn retry_wall(&mut self) {
        let Some((k, _)) = self.wall_asked else { return };
        if look::reads_wall(look::in_force(self.style(), &self.theme, self.env)) {
            self.wall_asked = Some((k, look::ask(self.hwnd, k)));
        }
    }

    /// A measurement came back (WM_WALL): the ground and the cells take
    /// it; a failed read is tried again a few times (look::retry_after).
    fn wall_measured(&mut self, token: u64) {
        let Some(read) = look::taken(token) else { return };
        if !self.wall_asked.is_some_and(|(_, t)| t == token) {
            return;
        }
        let wall = match read {
            look::WallRead::Standard(wall) => wall,
            look::WallRead::Mica { luminance, strip } => match self.set_mica(&strip) {
                Ok(()) => {
                    let glass = crate::theme::AdaptiveGlass::new(luminance, self.glass_light);
                    self.glass_light = Some(glass.light);
                    look::Wall::Glass(glass)
                }
                Err(e) => {
                    crate::log!("topbar: mica strip upload failed ({e})");
                    look::Wall::Unknown { retry: true }
                }
            }
        };
        if let Some(ms) = look::retry_after(&wall, self.wall_tries) {
            self.wall_tries += 1;
            unsafe {
                SetTimer(Some(self.hwnd), WALL_TIMER, ms, None);
            }
        }
        self.wall = wall;
        self.wall_ver += 1;
        self.apply_ground();
        self.layout_hover();
    }

    fn set_mica(&self, strip: &crate::icons::Icon) -> Result<()> {
        let bitmap = self.gfx.surface_from_bgra(strip.width, strip.height, &strip.pixels)?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&bitmap)?;
        brush.SetStretch(windows::UI::Composition::CompositionStretch::Fill)?;
        self.mica.SetBrush(&brush)
    }

    /// What each cell's own look on a clear bar is read from
    /// (look::looks_of: look::cell_look over the wallpaper's columns under
    /// it): no look off clear, while the wallpaper is not known, and for
    /// other apps' tray icons (their own colors). Cells whose content
    /// cannot be shown in another color than it was drawn in keep the
    /// bar's ink (a guard if need be).
    fn looks_in(&self) -> look::LooksIn {
        let bar_light = self.ground.ink == look::INK_ON_LIGHT;
        let cells = self
            .slots
            .iter()
            .map(|s| {
                let own = match s.cell {
                    Cell::Tray(k) => self.tray.get(k).is_some_and(|e| crate::dock::is_own_tray_icon(e.owner)),
                    _ => true,
                };
                (s.w > 0.0 && own).then(|| (s.x, s.w, (!one_color(s.cell)).then_some(bar_light)))
            })
            .collect();
        look::LooksIn { wall: self.wall_ver, style: self.ground.style, ink: self.ground.ink, width: self.width, cells }
    }

    /// The line along the bar's inner edge.
    fn place_edge(&self) {
        let Some((_, w)) = self.ground.edge else { return };
        let (h, w) = if self.ground.style == look::Style::Glass {
            ((self.at.bottom - self.at.top) as f32 / self.scale, w / self.scale)
        } else {
            (self.height, w)
        };
        let y = edge_y(h, w, self.cfg.at_bottom());
        let _ = self.edge.SetOffset(Vector3 { X: 0.0, Y: y, Z: 0.0 });
        let _ = self.edge.SetSize(Vector2 { X: self.width, Y: w });
    }

    fn reconfigure(&mut self, cfg: &Config, theme: &Theme) {
        // Top or bottom edge: placed again like a new height.
        let companion_changed = self.companion_enabled != cfg.companion.enabled;
        self.companion_enabled = cfg.companion.enabled;
        self.companion_situations=cfg.companion.enabled&&cfg.companion.situations;
        let moved = cfg.topbar.at_bottom() != self.cfg.at_bottom();
        let height_changed = cfg.topbar.height != self.cfg.height || cfg.topbar.reserve_work_area != self.cfg.reserve_work_area || moved;
        // Which bars have the whole set (tray, desktops, buttons).
        let everywhere_changed = cfg.topbar.everywhere() != self.cfg.everywhere();
        let status_changed = (cfg.topbar.show_wifi, cfg.topbar.show_volume, cfg.topbar.show_battery) != (self.cfg.show_wifi, self.cfg.show_volume, self.cfg.show_battery);
        let menus_changed = cfg.topbar.show_app_menus != self.cfg.show_app_menus || everywhere_changed || status_changed;
        let env = look::Env::now(cfg);
        let theme_changed = *theme != self.theme || cfg.topbar.style != self.cfg.style || env != self.env;
        self.env = env;
        let seconds_changed = cfg.topbar.clock_seconds != self.cfg.clock_seconds;
        // The saved order changed elsewhere (a reset): it wins over ours.
        if cfg.topbar.tray_order != self.cfg.tray_order {
            self.session_order = None;
        }
        self.cfg = cfg.topbar.clone();
        self.theme = theme.clone();
        if theme_changed {
            self.apply_theme();
        }
        if moved {
            // The tip and the island's card hang the other way now.
            self.drop_tip();
            self.island_bar_moved();
        }
        if height_changed {
            unsafe { self.place() };
            self.surfaces.clear();
            for s in &mut self.slots {
                s.key.clear();
            }
        }
        if everywhere_changed && !self.primary {
            // The tray comes to this bar (or leaves it); build_slots below
            // makes the cells.
            if self.full() {
                self.tray_button = crate::tray::needs_button(crate::tray::health());
                self.tray = if self.tray_button { Vec::new() } else { companion::other_icons(crate::tray::snapshot(), crate::dock::is_own_tray_icon) };
                self.refresh_tray_ids();
            } else {
                self.set_expanded(false);
                self.tray.clear();
                self.tray_ids.clear();
                self.tray_button = false;
            }
        }
        if menus_changed || companion_changed {
            let _ = self.build_slots();
        } else {
            // Windows' animation setting may have changed.
            self.attach_slides();
        }
        if seconds_changed {
            // The new text and the timer's new pace (tick_clock updates).
            self.tick_clock();
        }
        self.update();
        self.refresh_tip();
        if theme_changed || height_changed {
            self.island_redraw();
        }
        // On or off, its activities, its motion.
        self.island_setup();
    }

    /// The bar's place in the z-order: above everything, or right behind
    /// the fullscreen app in front of its monitor. (A topmost window put
    /// after a non-topmost one stops being topmost, in one step: no frame
    /// with the bar over the game.)
    unsafe fn restack(&self) {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        // (The dock's rule too: crate::dock::z_after.)
        let _ = SetWindowPos(self.hwnd, Some(crate::dock::z_after(self.hidden_full, self.hwnd)), 0, 0, 0, 0, flags);
    }

    unsafe fn place(&mut self) {
        // Its own monitor and that monitor's DPI (the window may still sit
        // on another one while it is being moved).
        let Some(info) = crate::monitors::info(self.hmon) else { return };
        self.scale = info.scale();
        if self.monitor != info.rect && self.monitor != RECT::default() {
            set_reserved(self.monitor, BarSpace::default());
        }
        self.monitor = info.rect;
        let m = info.rect;
        self.height = self.cfg.height.clamp(20.0, 48.0);
        let h_px = (self.height * self.scale).round() as i32;
        // Below a taskbar kept on the top edge (auto-hide or not; above
        // one on the bottom edge for a bar at the bottom), and beside one
        // on the left or right (its Start button stays usable), never over
        // it.
        let bottom = self.cfg.at_bottom();
        self.at = bar_rect(self.hmon, m, h_px, bottom);
        let r = self.at;
        self.width = (r.right - r.left) as f32 / self.scale;
        if self.appbar {
            reserve_bar(self.hwnd, self.monitor, r, &self.cfg);
        }
        let _ = SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE | SWP_NOZORDER);
        // Topmost, or still behind a fullscreen app.
        self.restack();
        let _ = self.root.SetScale(Vector3 { X: self.scale, Y: self.scale, Z: 1.0 });
        // Children size themselves relative to this.
        let _ = self.root.SetSize(Vector2 { X: self.width, Y: self.height });
        self.place_edge();
        self.refresh_wall();
    }

    unsafe fn register_appbar(&mut self) {
        if !self.appbar {
            taskbar::appbar_register(self.hwnd);
            self.appbar = true;
        }
        self.place();
        self.layout();
    }

    unsafe fn remove_appbar(&mut self) {
        if self.appbar {
            taskbar::appbar_remove(self.hwnd);
            self.appbar = false;
        }
        set_reserved(self.monitor, BarSpace::default());
    }

    /// Remove this bar for good (its monitor is gone, or bars are off).
    unsafe fn close(&mut self) {
        self.note_tray_focus(false);
        self.tray_motion(None);
        // A Start request it left waiting for the modifiers goes with it.
        crate::keys::cancel_start(Some(self.hwnd.0 as isize));
        self.island_close();
        self.remove_appbar();
        for h in self.power.drain(..) {
            let _ = UnregisterPowerSettingNotification(h);
        }
        let _ = DestroyWindow(self.hwnd);
    }

    fn build_slots(&mut self) -> Result<()> {
        let layer = self.root.Children()?;
        for s in self.slots.drain(..) {
            let _ = layer.Remove(&s.visual);
        }
        let mut cells = vec![Cell::Start, Cell::App];
        if self.cfg.show_app_menus {
            cells.extend((0..self.titles.len()).map(Cell::Menu));
        }
        if self.full() {
            // Shown or not by desktops_look().
            cells.push(Cell::Desktops);
        }
        // Every tray icon has a slot; layout() decides which side of the
        // chevron each sits on and whether the hidden ones are out.
        cells.extend((0..self.tray.len()).map(Cell::Tray));
        if !self.tray.is_empty() {
            cells.push(Cell::Chevron);
        }
        if self.full() && self.tray_button {
            cells.push(Cell::TrayButton);
        }
        self.hidden = self.hidden_list();
        if self.full() {
            // Shown or not by input_key().
            cells.push(Cell::Input);
        }
        cells.extend(status_cells(self.full(), self.cfg.show_wifi, self.cfg.show_volume, self.cfg.show_battery));
        // Windows sent to the tray (hidden.rs): right next to the clock, the
        // last cells laid out from the right edge, so the cell is always
        // on screen (optional cells make room first: overflow()).
        if !self.hidden.is_empty() {
            cells.push(Cell::Hidden);
        }
        cells.push(Cell::Companion);
        cells.push(Cell::Clock);
        for cell in cells {
            let visual = self.gfx.compositor.CreateSpriteVisual()?;
            layer.InsertAtTop(&visual)?;
            let _ = visual.SetOpacity(0.0);
            self.slots.push(Slot { cell, visual, key: String::new(), content: (0.0, 0.0), x: 0.0, w: 0.0, at: (f32::NAN, f32::NAN), alpha: 0.0, inked: None });
        }
        self.halo_slots();
        self.hovered = None;
        // Slot indices change: a press on an old slot must not carry over
        // (a drag keeps going: it is held by the icon's id).
        self.pressed = None;
        self.press_id = None;
        self.update();
        // After the first layout, so new icons do not slide in from 0,0.
        self.attach_slides();
        Ok(())
    }

    /// Tray icons, the chevron and the desktop dots glide when they move
    /// (expand, collapse, reflow while dragging): their place changes, so
    /// GLIDE (design system 7.4), and they come and go with the pointer's
    /// fades (layout: slides()). With reduced motion nothing slides: only
    /// the short fade is left (motion::glide_offset, motion::pointer_ms).
    fn attach_slides(&self) {
        let c = &self.gfx.compositor;
        let dragged = self.drag_k().map(Cell::Tray);
        for s in &self.slots {
            if Some(s.cell) == dragged {
                // The icon under the pointer follows it exactly: nothing
                // implicit on it.
                let _ = crate::dock::implicit_visual(c, &s.visual, &[]);
            } else if slides(s.cell) {
                let _ = crate::motion::glide_offset(c, &s.visual, true);
            }
        }
    }

    /// The vector glyph a cell shows now (None: text, drawn otherwise, or
    /// hidden).
    fn cell_glyph(&self, cell: Cell) -> Option<Glyph> {
        match cell {
            // Quick settings: three sliders; a dot with removable storage
            // connected (topbar.removable_dot).
            Cell::Control => Some(if self.cfg.removable_dot && crate::removable::present() { Glyph::SlidersDot } else { Glyph::Sliders }),
            Cell::Search => Some(Glyph::Search),
            // Left: the hidden icons slide out to the left; right once they
            // are out (folds them back). Right to left the drawer opens to
            // the right: mirrored.
            Cell::Chevron => Some(if self.expanded != crate::i18n::rtl() { Glyph::ChevronRight } else { Glyph::ChevronLeft }),
            // Up, like the taskbar's "Show hidden icons".
            Cell::TrayButton => Some(Glyph::ChevronUp),
            Cell::Net => self.net.map(status::net_glyph),
            Cell::Volume => self.volume.map(|(p, m)| status::volume_glyph(p, m)),
            _ => None,
        }
    }

    /// What a cell shows now: (style, text). Empty text = hidden. Glyph
    /// cells: the glyph's name (their cache key; drawn by glyph_surface).
    fn content(&self, cell: Cell) -> (Style, String) {
        let glyph = |style: Style| (style, self.cell_glyph(cell).map(|g| format!("{g:?}")).unwrap_or_default());
        match cell {
            Cell::Start | Cell::Tray(_) => (Style::Glyph, String::new()),
            Cell::Control | Cell::Search | Cell::Net | Cell::Volume => glyph(Style::Glyph),
            Cell::Chevron | Cell::TrayButton => glyph(Style::SmallGlyph),
            Cell::App => (Style::Text, self.app.clone()),
            Cell::Menu(i) => (Style::Text, self.titles.get(i).cloned().unwrap_or_default()),
            // Drawn (battery_surface); the text is only its cache key.
            Cell::Battery => (Style::Glyph, self.battery.map(|(p, ch)| format!("{p}|{ch}")).unwrap_or_default()),
            Cell::Companion => (
                Style::Glyph,
                companion::content_key(self.companion_enabled),
            ),
            Cell::Clock => (Style::Clock, self.clock.clone()),
            // Dots are drawn (dots_surface); the text is their cache key.
            Cell::Desktops => match self.desktops_look() {
                desktops::Look::Hidden => (Style::Glyph, String::new()),
                desktops::Look::Dots { n, current, rtl } => (Style::Glyph, format!("\u{0}desktops|{n}|{current:?}|{rtl}")),
                desktops::Look::Count(text) => (Style::Text, text),
            },
            // Drawn (hidden.rs); the text is the cache key.
            Cell::Hidden => (Style::Glyph, self.hidden_key()),
            // Drawn (input.rs); the text is its cache key.
            Cell::Input => (Style::Glyph, self.input_key()),
        }
    }

    /// The bar font's vertical metrics (topbar::baseline), Segoe UI's if it
    /// cannot be read.
    fn font_metrics(&self) -> baseline::Metrics {
        self.gfx
            .font_metrics(&self.theme.font.family, DWRITE_FONT_WEIGHT_NORMAL)
            .map(|m| baseline::Metrics::of(m.designUnitsPerEm, m.descent, m.capHeight))
            .unwrap_or(baseline::Metrics::SEGOE)
    }

    /// The line the bar's text cells stand on: a box as tall as the bar,
    /// one baseline that centers text `size` DIP tall optically in it.
    fn bar_line(&self, size: f32) -> crate::gfx::TextLine {
        baseline::line(self.height * self.scale, &self.font_metrics(), size * self.scale * crate::fonts::scale())
    }

    /// The bar's text size (the theme's font size: the ramp's Body, 13).
    pub(super) fn font_size(&self) -> f32 {
        self.theme.font.size.clamp(10.0, 18.0)
    }

    /// The bar's one text line (px): a box as tall as the bar and the
    /// baseline every text on it stands on, whatever its size (the plain
    /// text cells, and the drawn one: the hidden icons' count, which is
    /// drawn as tall as the bar for it).
    pub(super) fn line_px(&self) -> crate::gfx::TextLine {
        self.bar_line(self.font_size())
    }

    /// Where a layout's top goes in a surface as tall as the bar (line_px)
    /// so its first line stands on the bar's baseline (`height` is the
    /// layout's).
    pub(super) fn on_bar_line(&self, l: &windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, height: f32) -> f32 {
        baseline::origin_y(self.line_px().baseline, crate::gfx::first_baseline(l, height))
    }

    fn text_color(&self) -> D2D1_COLOR_F {
        let c = self.ground.ink;
        D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
    }

    fn surface(&mut self, style: Style, text: &str) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        let k = (style, text.to_string());
        if let Some(s) = self.surfaces.get(&k) {
            return Some(s.clone());
        }
        if self.surfaces.len() > 48 {
            // Old clock strings mostly; cheap to redraw.
            self.surfaces.clear();
        }
        let font_size = self.font_size();
        // App name and other text share the regular weight.
        let weight = DWRITE_FONT_WEIGHT_NORMAL;
        // Every text cell is as tall as the bar, its text on the bar's one
        // baseline (the app name, the menus, the clock, the desktop count
        // line up whatever script or fallback font each one shows).
        let line = self.bar_line(font_size);
        let (surface, w, h) = if style == Style::Clock {
            // One clock surface at a time (it changes every second or
            // minute). Its width is that of the widest time the format
            // gives (two-digit hours, either side of noon) and never less
            // than before under the same format, font and scale.
            self.surfaces.retain(|(st, _), _| *st != Style::Clock);
            let digit = self.clock_digit(font_size);
            let room = self.clock_room(text, font_size, digit);
            let key = format!(
                "{}|{}|{}|{}",
                crate::i18n::current(),
                crate::i18n::date::time_pattern(self.cfg.clock_seconds),
                self.theme.font.family,
                // The text size counts too (crate::fonts::scale).
                (font_size * self.scale * self.theme.font.scale * 100.0) as u32
            );
            let floor = self.clock_width.floor(&key);
            let got = self.gfx.text_steady(text, &room, &self.theme.font.family, font_size, weight, self.text_color(), self.scale, floor, Some(line)).ok()?;
            self.clock_width.fit(&key, got.1);
            got
        } else {
            self.gfx.text_on(text, &self.theme.font.family, font_size, weight, self.text_color(), self.scale, Some(line)).ok()?
        };
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        self.surfaces.insert(k, (brush.clone(), w, h));
        Some((brush, w, h))
    }

    /// The room the clock showing `text` is drawn in: the widest of it and
    /// the same day's two-digit hours (status::clock_rooms), digits made
    /// `digit`. Measured again only when the text's template, the font or
    /// its size changes (once a minute at most, mostly once a day).
    fn clock_room(&mut self, text: &str, size: f32, digit: char) -> String {
        let px = size * self.scale;
        let family = self.theme.font.family.clone();
        let key = format!("{}|{family}|{}|{}", status::steady_template(text, digit), (px * 100.0) as u32, self.cfg.clock_seconds);
        if let Some((k, room)) = &self.clock_room {
            if *k == key {
                return room.clone();
            }
        }
        let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        // The samples first: a tie goes to them, so the room is the same
        // whichever time of the day it is.
        let mut texts = status::clock_rooms(&now, self.cfg.clock_seconds);
        texts.push(text.to_string());
        let gfx = self.gfx.clone();
        let room = status::steady_room(&texts, digit, |t| gfx.text_layout_tabular(t, &family, px, DWRITE_FONT_WEIGHT_NORMAL).map(|l| l.1).unwrap_or(0.0));
        self.clock_room = Some((key, room.clone()));
        room
    }

    /// The bar font's widest digit at `size` DIP (measured once per font
    /// and scale: ten runs of each digit, with tabular figures, so a font
    /// without them still gets its widest).
    fn clock_digit(&mut self, size: f32) -> char {
        let family = self.theme.font.family.clone();
        let px = size * self.scale;
        let key = (family, (px * 100.0) as u32);
        if let Some((k, c)) = &self.clock_digit {
            if *k == key {
                return *c;
            }
        }
        let widths: Vec<f32> = (b'0'..=b'9')
            .map(|d| {
                let run = char::from(d).to_string().repeat(10);
                self.gfx.text_layout_tabular(&run, &key.0, px, DWRITE_FONT_WEIGHT_NORMAL).map(|l| l.1).unwrap_or(0.0)
            })
            .collect();
        let c = status::widest_digit(&widths);
        self.clock_digit = Some((key, c));
        c
    }

    /// A glyph centered in a GLYPH_BOX square, so every glyph icon (and
    /// the tray icons, same box) shares one center line. Cached under the
    /// cell's content key. (brush, content w, h: DIP.)
    fn glyph_surface(&mut self, style: Style, g: Glyph) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        let k = (style, format!("{g:?}"));
        if let Some(s) = self.surfaces.get(&k) {
            return Some(s.clone());
        }
        let s = self.scale;
        let side = (GLYPH_BOX * s).round().max(1.0);
        let size = if style == Style::SmallGlyph { CHEVRON_SIZE } else { GLYPH_SIZE } * s;
        let color = self.text_color();
        let surface = self.gfx.draw(side as u32, side as u32, |dc, ox, oy| unsafe { paint_glyph_cell(dc, g, ox, oy, side, size, color) }).ok()?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        self.surfaces.insert(k, (brush.clone(), side / s, side / s));
        Some((brush, side / s, side / s))
    }

    /// Tray icon k as a surface (its pixels come from the tray thread,
    /// already normalized to the slot size). Cached by picture generation.
    fn tray_surface(&mut self, k: usize) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        let e = self.tray.get(k)?;
        if let Some(b) = self.tray_brushes.get(&e.gen) {
            return Some((b.clone(), TRAY_DIP, TRAY_DIP));
        }
        let px = self.tray_px();
        let pixels = e.pixels.as_ref()?;
        let gen = e.gen;
        // The tray thread renders at the main bar's size; a bar on a
        // display with another scale resamples its own copy (it showed no
        // icons at all: every picture was "the wrong size").
        let n = ((pixels.len() / 4) as f64).sqrt().round() as u32;
        let resized;
        let pixels: &[u8] = if n == px {
            pixels
        } else if n > 0 && (n * n * 4) as usize == pixels.len() {
            resized = crate::tray::fit::normalize(pixels, n, px);
            &resized
        } else {
            return None;
        };
        let surface = self.gfx.surface_from_bgra(px, px, pixels).ok()?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        self.tray_brushes.insert(gen, brush.clone());
        Some((brush, TRAY_DIP, TRAY_DIP))
    }

    /// The fallback battery, flat: the body filled up to the
    /// charge over a dim track, the percent centered on its ink (equal
    /// margins all round), dark over the fill and in the bar's ink over the
    /// track; red at 20 % or less. While charging the bolt takes the number's place inside
    /// the body (the body is too small for three digits and a bolt that both
    /// stay readable; the percent is in the battery panel), so the icon
    /// keeps one width and nothing beside it moves on plug-in.
    fn battery_surface(&mut self, percent: u8, charging: bool) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        let k = (Style::Glyph, format!("\u{0}battery|{percent}|{charging}"));
        if let Some(s) = self.surfaces.get(&k) {
            return Some(s.clone());
        }
        let s = self.scale;
        let wd = BATTERY_W;
        let art = battery_art(&self.gfx, &self.theme, self.text_color(), percent, charging, s);
        let (w_px, h_px) = ((wd * s).ceil() as u32, (BATTERY_H * s).ceil() as u32);
        let surface = self.gfx.draw(w_px, h_px, |dc, ox, oy| unsafe { paint_battery(dc, ox, oy, &art) }).ok()?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        self.surfaces.insert(k, (brush.clone(), wd, BATTERY_H));
        Some((brush, wd, BATTERY_H))
    }

    fn tray_px(&self) -> u32 {
        (TRAY_DIP * self.scale).round() as u32
    }

    /// This bar shows everything the main bar does: it is the main bar,
    /// or "Same on every display" is on.
    pub(super) fn full(&self) -> bool {
        self.primary || self.cfg.everywhere()
    }

    /// The stable ids of `self.tray` (see refresh_tray).
    fn refresh_tray_ids(&mut self) {
        let tray = std::mem::take(&mut self.tray);
        self.tray_ids = self.ids_for(&tray);
        self.tray = tray;
    }

    fn ids_for(&mut self, fresh: &[crate::tray::Entry]) -> Vec<String> {
        self.tray_owners.retain(|o, _| fresh.iter().any(|e| e.owner == *o));
        let mut ids = Vec::with_capacity(fresh.len());
        for e in fresh {
            let stable = if e.guid.is_some() {
                crate::tray::stable_id(e.guid, None, e.uid)
            } else {
                if !self.tray_owners.contains_key(&e.owner) {
                    if let Some(exe) = crate::tray::owner_exe(e.owner) {
                        self.tray_owners.insert(e.owner, exe);
                    }
                }
                crate::tray::stable_id(None, self.tray_owners.get(&e.owner).map(String::as_str), e.uid)
            };
            ids.push(stable.unwrap_or_else(|| crate::tray::session_id(e.owner, e.uid)));
        }
        ids
    }

    fn refresh_tray(&mut self) {
        if !self.full() {
            return;
        }
        let fresh = companion::other_icons(crate::tray::snapshot(), crate::dock::is_own_tray_icon);
        // The mirror is off, or its icons cannot be trusted to be all
        // there (explorer keeps the front, or does not answer): one button
        // to Windows' own notification area instead of a tray that looks
        // complete but is not.
        let button = crate::tray::needs_button(crate::tray::health());
        let fresh = if button { Vec::new() } else { fresh };
        let button_changed = button != self.tray_button;
        self.tray_button = button;
        // Ids: the GUID, else the owner's exe (looked up once per owner;
        // a failed lookup is not kept, so it is tried again next time).
        // Icons with neither get a session-only id and are never saved.
        let ids = self.ids_for(&fresh);
        for id in &ids {
            let n = self.tray_seen.len() as u64;
            self.tray_seen.entry(id.clone()).or_insert(n);
        }
        // Rebuild the slots only when their number changes (one per icon
        // plus the chevron); otherwise just refresh contents.
        let same_set = fresh.len() == self.tray.len();
        if self.tray_hover.as_ref().is_some_and(|old| !fresh.iter().any(|e| (e.owner, e.uid) == (old.owner, old.uid))) {
            self.tray_motion(None);
            self.drop_tip();
        }
        if self.drag.as_ref().is_some_and(|d| !ids.contains(&d.id)) {
            // The icon under the pointer is gone.
            self.end_drag(false);
        }
        self.tray_brushes.retain(|g, _| fresh.iter().any(|e| e.gen == *g));
        self.tray = fresh;
        self.tray_ids = ids;
        if self.tray.is_empty() {
            self.set_expanded(false);
        }
        if !same_set || button_changed {
            let _ = self.build_slots();
        } else {
            self.update();
        }
    }

    /// Per tray icon: behind the chevron (true) or shown. The user's choices
    /// from the config, new icons placed automatically; the icon being
    /// dragged sits where it would land.
    fn tray_split(&self) -> Vec<bool> {
        let mut out = self.saved_split();
        if let (Some(k), Some(d)) = (self.drag_k(), &self.drag) {
            if let Some(h) = out.get_mut(k) {
                *h = d.hidden;
            }
        }
        out
    }

    /// tray_split without the drag: the config's choices plus this
    /// session's choices for icons that cannot be saved.
    fn saved_split(&self) -> Vec<bool> {
        let mut hidden = self.cfg.tray_hidden.clone();
        let mut visible = self.cfg.tray_visible.clone();
        for (id, &h) in &self.session_choices {
            if h { hidden.push(id.clone()) } else { visible.push(id.clone()) }
        }
        crate::tray::split(&self.tray_ids, &hidden, &visible, TRAY_AUTO_MAX)
    }

    /// The user's tray order (this session's, else the saved one).
    fn tray_order(&self) -> &[String] {
        self.session_order.as_deref().unwrap_or(&self.cfg.tray_order)
    }

    /// Tray icons (indexes into `tray`) left to right in reading order; the
    /// one being dragged where it would land.
    fn tray_rank(&self) -> Vec<usize> {
        let base = crate::tray::rank(&self.tray_ids, self.tray_order(), &self.tray_seen);
        let Some(d) = self.drag.as_ref().filter(|_| self.drag_k().is_some()) else { return base };
        let ranked: Vec<String> = base.iter().map(|&k| self.tray_ids[k].clone()).collect();
        let moved = self.reordered(&ranked, d);
        moved.iter().filter_map(|id| self.tray_ids.iter().position(|x| x == id)).collect()
    }

    /// `ranked` with the dragged icon moved where `d` says.
    fn reordered(&self, ranked: &[String], d: &Drag) -> Vec<String> {
        let split = self.saved_split();
        let side_of = |id: &str| self.tray_ids.iter().position(|x| x == id).and_then(|k| split.get(k).copied()).unwrap_or(false);
        crate::tray::reorder(ranked, side_of, &d.id, d.hidden, d.slot)
    }

    /// Where the dragged icon is in the snapshot now.
    fn drag_k(&self) -> Option<usize> {
        let d = self.drag.as_ref()?;
        self.tray_ids.iter().position(|x| *x == d.id)
    }

    /// Slides the hidden tray icons out (left of the chevron) or back.
    fn set_expanded(&mut self, on: bool) {
        if on == self.expanded {
            return;
        }
        self.expanded = on;
        if on {
            self.poke_tray_timer();
        } else {
            if self.drag.is_some() {
                self.end_drag(false);
            }
            self.drawer_shift = 0;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TRAY_TIMER);
            }
        }
        self.update();
    }

    /// The user did something with the expanded tray: restart its timer.
    fn poke_tray_timer(&self) {
        if self.expanded {
            unsafe {
                SetTimer(Some(self.hwnd), TRAY_TIMER, TRAY_COLLAPSE_MS, None);
            }
        }
    }

    /// A press on slot `pressed` moved to `x`: past the threshold, a tray
    /// icon of the expanded tray starts to drag.
    fn maybe_start_drag(&mut self, x: f32) {
        if self.drag.is_some() || !self.expanded || (x - self.press_x).abs() < DRAG_THRESHOLD {
            return;
        }
        let Some(i) = self.pressed else { return };
        // The pressed slot must still be the icon that was pressed.
        let Some(Cell::Tray(k)) = self.slots.get(i).map(|s| s.cell) else { return };
        let Some(id) = self.tray_ids.get(k).cloned() else { return };
        if self.press_id.as_ref() != Some(&id) {
            self.pressed = None;
            self.press_id = None;
            return;
        }
        let split = self.tray_split();
        let hidden = split.get(k).copied().unwrap_or(false);
        // Its place now among the other icons on its side.
        let slot = self.tray_rank().iter().take_while(|&&j| j != k).filter(|&&j| split.get(j).copied() == Some(hidden)).count();
        let s = &self.slots[i];
        let left = s.x + (s.w - s.content.0) / 2.0;
        let visual = s.visual.clone();
        self.drag = Some(Drag { id, grab: self.press_x - left, x, hidden, slot });
        self.pressed = None;
        self.press_id = None;
        self.hovered = None;
        unsafe {
            SetCapture(self.hwnd);
        }
        // On top of its neighbors, following the pointer without easing.
        if let Ok(layer) = self.root.Children() {
            let _ = layer.Remove(&visual);
            let _ = layer.InsertAtTop(&visual);
        }
        self.attach_slides();
        self.poke_tray_timer();
        self.drag_to(x);
    }

    /// The dragged icon follows the pointer; crossing the chevron's middle
    /// moves it to that side (the others make room).
    fn drag_to(&mut self, x: f32) {
        if self.drag.is_some() && escape_down() {
            self.end_drag(false);
            return;
        }
        let Some(chev) = self.slots.iter().find(|s| s.cell == Cell::Chevron).map(|s| s.x + s.w / 2.0) else { return };
        let rtl = crate::i18n::rtl();
        let Some(k) = self.drag_k() else { return };
        let split = self.saved_split();
        let half = self.slots.iter().find(|s| s.cell == Cell::Tray(k)).map(|s| s.content.0 / 2.0).unwrap_or(0.0);
        let Some(d) = self.drag.as_mut() else { return };
        d.x = x;
        // The chevron shifts one cell when the icon changes sides, which
        // takes it further from the pointer: no flip-flopping. Right to
        // left the hidden side is the chevron's right.
        d.hidden = if rtl { x > chev } else { x < chev };
        // Its place on that side: how many of the side's other (shown)
        // icons are before the dragged icon's middle. They make room as it
        // passes, moving away from it, so this does not flip-flop either.
        let hidden = d.hidden;
        let mid = x - d.grab + half;
        let before = self
            .slots
            .iter()
            .filter(|s| s.w > 0.0 && matches!(s.cell, Cell::Tray(j) if j != k && split.get(j).copied() == Some(hidden)))
            .filter(|s| {
                let c = s.x + s.w / 2.0;
                if rtl { c > mid } else { c < mid }
            })
            .count();
        // Hidden icons scrolled out of the drawer on its start come first.
        let folded_before = if hidden { self.drawer_shift.min(self.drawer_total) } else { 0 };
        if let Some(d) = self.drag.as_mut() {
            d.slot = before + folded_before;
        }
        self.layout();
    }

    /// Drops (commit) or cancels the drag. A drop on the other side of the
    /// chevron is saved for that icon's app (through the dock, which owns
    /// the config); icons without a stable id keep it for this session.
    fn end_drag(&mut self, commit: bool) {
        let k = self.drag_k();
        let Some(d) = self.drag.take() else { return };
        if commit && !escape_down() && k.is_some() {
            let base = crate::tray::rank(&self.tray_ids, self.tray_order(), &self.tray_seen);
            let ranked: Vec<String> = base.iter().map(|&j| self.tray_ids[j].clone()).collect();
            let moved = self.reordered(&ranked, &d);
            if moved != ranked {
                // The whole order as it is now: icons the user never
                // placed get a place too, so nothing shuffles later. Ids
                // saved before for icons not running now keep theirs.
                let mut keep: Vec<String> = moved.iter().filter(|id| !crate::tray::is_session_id(id)).cloned().collect();
                for old in self.tray_order() {
                    if !keep.contains(old) && !crate::tray::is_session_id(old) {
                        keep.push(old.clone());
                    }
                }
                self.cfg.tray_order = keep.clone();
                self.session_order = Some(moved);
                crate::dock::save_tray_order(keep);
            }
        }
        if commit && !escape_down() {
            let before = k.and_then(|k| self.saved_split().get(k).copied());
            if before.is_some_and(|b| b != d.hidden) {
                let id = d.id.clone();
                if crate::tray::is_session_id(&id) {
                    crate::log!("tray: icon has no stable id; choice kept for this session only");
                    self.session_choices.insert(id, d.hidden);
                } else {
                    // Here at once; the saved config comes back the same.
                    self.cfg.tray_hidden.retain(|x| *x != id);
                    self.cfg.tray_visible.retain(|x| *x != id);
                    if d.hidden {
                        self.cfg.tray_hidden.push(id.clone());
                    } else {
                        self.cfg.tray_visible.push(id.clone());
                    }
                    crate::dock::save_tray_choice(id, d.hidden);
                }
            }
        }
        unsafe {
            if GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
        }
        // Eases from where it was dropped into its place.
        self.attach_slides();
        self.layout();
        self.poke_tray_timer();
    }

    fn start_surface(&mut self) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        let k = (Style::Glyph, "\u{0}start".to_string());
        if let Some(s) = self.surfaces.get(&k) {
            return Some(s.clone());
        }
        // The Windows Start mark (four tiles), because the button is the
        // real Start button: in the bar's text color (dark or light with
        // the bar, no accent), drawn sharp at this DPI.
        let px = (START_PX * self.scale).round();
        let c = self.ground.ink;
        let rgba = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, c.a as f32 / 255.0);
        let full = px as u32;
        let surface = self.gfx.draw(full, full, |dc, ox, oy| unsafe { crate::gfx::paint_start(dc, ox, oy, px, rgba) }).ok()?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        self.surfaces.insert(k, (brush.clone(), START_PX, START_PX));
        Some((brush, START_PX, START_PX))
    }

    /// Refresh changed cell contents, then lay out.
    fn update(&mut self) {
        for i in 0..self.slots.len() {
            let cell = self.slots[i].cell;
            let (style, text) = self.content(cell);
            let key = match cell {
                Cell::Start => "\u{0}start".to_string(),
                Cell::Tray(k) => self.tray.get(k).map(|e| format!("\u{0}tray|{}|{}|{}", e.owner, e.uid, e.gen)).unwrap_or_default(),
                _ => format!("{style:?}|{text}"),
            };
            if key == self.slots[i].key {
                continue;
            }
            // Drawn straight into the slot (hidden.rs).
            if cell == Cell::Hidden {
                let size = self.hidden_content(i);
                let slot = &mut self.slots[i];
                slot.key = key;
                slot.content = size;
                slot.inked = None;
                continue;
            }
            let got = if cell == Cell::Start {
                self.start_surface()
            } else if cell == Cell::Companion {
                self.companion_surface()
            } else if cell == Cell::Battery {
                self.battery.and_then(|(p, ch)| self.battery_surface(p, ch))
            } else if let Cell::Tray(k) = cell {
                self.tray_surface(k)
            } else if cell == Cell::Input && !text.is_empty() {
                self.input_surface()
            } else if let (Cell::Desktops, desktops::Look::Dots { n, current, rtl }) = (cell, self.desktops_look()) {
                self.dots_surface(n, current, rtl)
            } else if text.is_empty() {
                None
            } else if let Some(g) = self.cell_glyph(cell) {
                self.glyph_surface(style, g)
            } else {
                self.surface(style, &text)
            };
            let slot = &mut self.slots[i];
            slot.key = key;
            match got {
                // Opacity and offset are layout()'s (folded tray icons).
                Some((brush, w, h)) => {
                    let _ = slot.visual.SetBrush(&brush);
                    let _ = slot.visual.SetSize(Vector2 { X: w, Y: h });
                    slot.content = (w, h);
                    // (A new picture: layout_hover inks it again if lit.)
                    slot.inked = None;
                }
                None => {
                    slot.content = (0.0, 0.0);
                }
            }
        }
        self.layout();
    }

    fn layout(&mut self) {
        let h = self.height;
        let scale = self.scale;
        let mut x = EDGE_PAD;
        for s in self.slots.iter_mut().filter(|s| !s.cell.right()) {
            s.x = x;
            s.w = cell_width(s.cell, s.content.0);
            x += s.w;
        }
        // Right side, left to right: hidden tray icons (out only while
        // expanded, and only as many as fit before the app menus), the
        // chevron, the shown tray icons, then the status cells and the
        // clock in slot order. One grid: every icon's cell is its content
        // and STATUS_GAP, at least ICON_CELL (cell_width).
        let mut left_end = x + DRAWER_GAP;
        let split = self.tray_split();
        let dragged = self.drag_k();
        let find = |c: Cell| self.slots.iter().position(|s| s.cell == c);
        let chevron = find(Cell::Chevron);
        let mut order: Vec<usize> = Vec::new();
        order.extend(chevron);
        let rank = self.tray_rank();
        order.extend(rank.iter().copied().filter(|&k| !split[k]).filter_map(|k| find(Cell::Tray(k))));
        order.extend(
            self.slots
                .iter()
                .enumerate()
                .filter(|(_, s)| s.cell.right() && !matches!(s.cell, Cell::Tray(_) | Cell::Chevron | Cell::Desktops))
                .map(|(i, _)| i),
        );
        let desks = find(Cell::Desktops);
        let hidden: Vec<usize> = rank.iter().copied().filter(|&k| split[k]).filter_map(|k| find(Cell::Tray(k))).collect();
        let cell_w = |s: &Slot| cell_width(s.cell, s.content.0);
        // Too wide for the room the left side leaves: optional cells go
        // first (the opt-in buttons), so the
        // clock, the tray cell and the Quick settings always fit.
        // Still too narrow with every optional cell gone: the left side
        // (the app's menus from the last, then its name, then the logo)
        // gives way; required right cells that still do not fit are not
        // placed at all (narrow_plan): no cell off the bar or overlapping.
        let widths: Vec<f32> = order.iter().map(|&i| cell_w(&self.slots[i])).collect();
        let ranks: Vec<Option<u8>> = order.iter().map(|&i| drop_rank(self.slots[i].cell)).collect();
        let desks_w = find(Cell::Desktops).map(|i| cell_w(&self.slots[i])).unwrap_or(0.0);
        let lefts: Vec<usize> = (0..self.slots.len()).filter(|&i| !self.slots[i].cell.right()).collect();
        let left_w: Vec<f32> = lefts.iter().map(|&i| self.slots[i].w).collect();
        let plan = narrow_plan(&left_w, &widths, &ranks, desks_w, self.width);
        let (kept, dropped) = (&plan.kept, &plan.dropped);
        // Dots that do not fit are not placed.
        let desks_w = if plan.desks { desks_w } else { 0.0 };
        if kept.iter().any(|k| !k) {
            let mut lx = EDGE_PAD;
            for (k, &i) in lefts.iter().enumerate() {
                let s = &mut self.slots[i];
                if !kept[k] {
                    s.w = 0.0;
                }
                s.x = lx;
                lx += s.w;
            }
            left_end = lx + DRAWER_GAP;
        }
        let mut x = self.width - EDGE_PAD;
        // Folded icons wait under the chevron (they slide out from there).
        let mut fold_x = x;
        for (k, &i) in order.iter().enumerate().rev() {
            let w = if dropped[k] { 0.0 } else { cell_w(&self.slots[i]) };
            x -= w;
            let s = &mut self.slots[i];
            s.w = w;
            s.x = x;
            if Some(i) == chevron {
                fold_x = x + (w - ICON_CELL) / 2.0;
            }
        }
        // The drawer: the hidden icons that fit between the menus and the
        // chevron; the wheel scrolls through the rest.
        let room = if self.expanded { drawer_room(left_end + desks_w, x, ICON_CELL) } else { 0 };
        let (start, end, shift) = drawer_window(hidden.len(), room, self.drawer_shift);
        self.drawer_room = room;
        self.drawer_total = hidden.len();
        self.drawer_shift = shift;
        let out: Vec<usize> = hidden[start..end].to_vec();
        let mut folded = vec![false; self.slots.len()];
        for &i in &hidden {
            if !out.contains(&i) {
                folded[i] = true;
                let s = &mut self.slots[i];
                s.x = fold_x;
                s.w = 0.0;
            }
        }
        for &i in out.iter().rev() {
            let w = cell_w(&self.slots[i]);
            x -= w;
            let s = &mut self.slots[i];
            s.w = w;
            s.x = x;
        }
        // The desktop dots: left of everything on the right side.
        if let Some(i) = desks {
            x -= desks_w;
            let s = &mut self.slots[i];
            s.w = desks_w;
            s.x = x;
        }
        // Right to left the bar is mirrored: app name and menus on the
        // right, the tray, status cells and clock on the left (like
        // Windows' right-to-left taskbar). Folded icons wait under the
        // chevron in an ICON_CELL box.
        if crate::i18n::rtl() {
            let width = self.width;
            for (i, s) in self.slots.iter_mut().enumerate() {
                let w = if folded[i] { ICON_CELL } else { s.w };
                s.x = crate::i18n::mirror::span(true, s.x, w, 0.0, width);
            }
        }
        let drag = self.drag.as_ref().zip(dragged).map(|(d, k)| (Cell::Tray(k), d.x - d.grab));
        let c = self.gfx.compositor.clone();
        let mode = crate::motion::now();
        let rtl = crate::i18n::rtl();
        for (i, s) in self.slots.iter_mut().enumerate() {
            let is_dragged = drag.is_some_and(|(c, _)| c == s.cell);
            let mut left = content_x(s.cell, s.x, s.w.max(if folded[i] { ICON_CELL } else { 0.0 }), s.content.0, rtl);
            if let Some((_, at)) = drag.filter(|_| is_dragged) {
                left = at;
            }
            // Never placed yet (a slot just built): it is simply there.
            let fresh = s.at.0.is_nan();
            // Centered on whole device pixels, across and down (text
            // cells: as tall as the bar, at its top; topbar-text review
            // P2: x was a whole DIP, 1.25 px at 125 %).
            let at = (baseline::snap_px(left, scale), baseline::cell_top(h, s.content.1, scale));
            if at != s.at {
                s.at = at;
                let _ = s.visual.SetOffset(Vector3 { X: at.0, Y: at.1, Z: 0.0 });
            }
            let alpha = if s.content.0 > 0.0 && ((!folded[i] && s.w > 0.0) || is_dragged) { 1.0 } else { 0.0 };
            if alpha != s.alpha {
                s.alpha = alpha;
                // What slides comes over FADE_IN_MS and goes over
                // FADE_OUT_MS (7.4: in and out); the rest at once.
                let on = alpha > 0.0;
                let ms = if slides(s.cell) && !fresh { crate::motion::pointer_ms(mode, on) } else { 0 };
                let curve = if on { crate::motion::EASE_OUT } else { crate::motion::EASE_IN };
                if crate::motion::fade_to(&c, &s.visual, alpha, ms, curve).is_err() {
                    let _ = s.visual.SetOpacity(alpha);
                }
            }
        }
        self.layout_hover();
        self.island_layout();
        let available = !self.hidden_full && !self.tray_stops().is_empty();
        self.note_tray_focus(available);
        self.publish_a11y();
        self.publish_tray_cells();
    }

    /// Tells the tray where this bar shows each app's icon (the icons out,
    /// not folded): apps asking where their icon is get its cell.
    fn publish_tray_cells(&self) {
        let cells = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.w > 0.0 && s.alpha > 0.0)
            .filter_map(|(i, s)| match s.cell {
                Cell::Tray(k) => self.tray.get(k).map(|e| {
                    let r = self.slot_rect(i);
                    (e.owner, e.uid, e.guid, (r.left, r.top, r.right, r.bottom))
                }),
                _ => None,
            })
            .collect();
        crate::tray::set_shown(self.hwnd.0 as isize, cells);
    }

    /// The cells that are lit now (capsule::lit): the one whose surface is
    /// up, and the one under the pointer.
    fn lit(&self) -> capsule::Lit {
        let open = self.anchor.and_then(|c| self.slot_of(c));
        capsule::lit(self.tray_focus.as_ref().and_then(|(k, _)| self.slot_by_key(k)).or(self.hovered), self.pressed, open)
    }

    /// Shows the cells' states (capsule.rs: hover, pressed, open) and, in
    /// high contrast, the lit cells' content in the color their fill asks.
    fn layout_hover(&mut self) {
        let lit = self.lit();
        let (inset, ph, rad) = hover_pill(self.height);
        let spots: Vec<Option<capsule::Spot>> = self.slots.iter().map(|s| (s.w > 0.0).then_some((s.x, inset, s.w, ph, rad))).collect();
        let c = self.gfx.compositor.clone();
        // Clear: each cell's own ink, halo, open fill and guard (read
        // again only when the wallpaper, the ground or the cells' places
        // changed; a hover or a press does not).
        let input = self.looks_in();
        if self.cell_looks.refresh(&self.wall, input) {
            self.halo_slots();
            self.caps.restyle();
        }
        for (i, spot) in spots.iter().enumerate() {
            let guard = self.cell_looks.get(i).and_then(|l| l.guard);
            self.guards.show(&c, i, spot.zip(guard));
        }
        self.guards.keep(spots.len());
        let mark = self.ground.mark;
        let cell_marks: Vec<Option<crate::theme::Rgba>> = self.cell_looks.looks.iter().map(|l| l.map(|l| l.mark)).collect();
        self.caps.show(&c, &self.theme, mark, &|i| cell_marks.get(i).copied().flatten(), lit, self.scale, &|i| spots.get(i).copied().flatten());
        let mode = crate::motion::now();
        let inks: Vec<Option<crate::theme::Rgba>> = spots
            .iter()
            .zip(&self.slots)
            .enumerate()
            .map(|(i, (spot, slot))| {
                let state = if lit.open == Some(i) { crate::theme::State::Open } else { lit.under.filter(|u| u.0 == i).map(|u| u.1).unwrap_or(crate::theme::State::Rest) };
                let cell = self.cell_looks.get(i);
                let mark = cell.map(|l| l.mark).or(mark);
                // A clear cell in another ink than the bar's: its content
                // shown in it (look::mask_ink: the same strength).
                let own = cell.map(|l| look::mask_ink(l.ink, self.ground.ink)).filter(|_| cell.is_some_and(|l| l.ink != self.ground.ink));
                // Another app's one-color tray glyph drawn for the system
                // theme (white on a dark taskbar) that would vanish on this
                // bar's ground: shown in the bar's ink, its shape kept.
                let glyph = match slot.cell {
                    Cell::Tray(k) => self
                        .tray
                        .get(k)
                        .filter(|e| !crate::dock::is_own_tray_icon(e.owner))
                        .and_then(|e| e.pixels.as_deref())
                        .and_then(|px| glyph_lightness(px))
                        .filter(|light| *light == (self.ground.ink != look::INK_ON_LIGHT))
                        .map(|_| self.ground.ink),
                    _ => None,
                };
                spot.and_then(|sp| capsule::look(&self.theme, mark, state, sp.2, sp.3, mode).ink).or(own).filter(|_| one_color(slot.cell)).or(glyph)
            })
            .collect();
        for (i, ink) in inks.into_iter().enumerate() {
            self.ink_slot(i, ink);
        }
    }

    /// Shows slot `i`'s content in `ink` instead of the color it was
    /// drawn in (None: as drawn). The content is its own picture's shape
    /// filled with the color (a mask), so nothing is drawn again.
    fn ink_slot(&mut self, i: usize, ink: Option<crate::theme::Rgba>) {
        let c = &self.gfx.compositor;
        let s = &mut self.slots[i];
        if s.inked.as_ref().map(|k| k.0) == ink {
            return;
        }
        if let Some((_, plain)) = s.inked.take() {
            let _ = s.visual.SetBrush(&plain);
        }
        let Some(ink) = ink else { return };
        let made = (|| -> Result<windows::UI::Composition::CompositionBrush> {
            let plain = s.visual.Brush()?;
            let mask = c.CreateMaskBrush()?;
            mask.SetSource(&c.CreateColorBrushWithColor(color(ink.a, ink.r, ink.g, ink.b))?)?;
            mask.SetMask(&plain)?;
            s.visual.SetBrush(&mask)?;
            Ok(plain)
        })();
        if let Ok(plain) = made {
            s.inked = Some((ink, plain));
        }
    }

    /// Cell `cell`'s surface is up (None: it started to close, or none
    /// is): the cell keeps its pressed fill meanwhile (6.2 "open"). The
    /// fill comes on, and moves from cell to cell while scrubbing, at
    /// once; it goes over FADE_OUT_MS. Tips go when a surface opens (7.4).
    fn set_anchor(&mut self, cell: Option<Cell>) {
        if cell.is_some() {
            self.drop_tip();
            self.hidden_hover(None);
            // The pointer is the surface's now (a menu takes the mouse):
            // the bar hears from it again with its next move.
            self.tray_motion(None);
            self.hovered = None;
            self.pressed = None;
            self.tracking = false;
        }
        if self.anchor == cell {
            return;
        }
        self.anchor = cell;
        self.layout_hover();
    }

    /// The slot under bar x. The right side's cells win where anything
    /// overlaps them (layout keeps them apart; this is the backstop).
    fn hit(&self, x: f32) -> Option<usize> {
        let at = |s: &Slot| s.w > 0.0 && x >= s.x && x < s.x + s.w;
        (0..self.slots.len()).find(|&i| self.slots[i].cell.right() && at(&self.slots[i])).or_else(|| self.slots.iter().position(at))
    }

    /// Shows the time now and re-arms the clock's one-shot timer for its
    /// next change: the next second while seconds are on and someone can
    /// see the bar, else the next minute. A bar behind a fullscreen app, a
    /// locked or sleeping session or a dark display rests on the minute
    /// timer; refresh_fullscreen, session_idle_changed and resync call this
    /// again when that ends, so it re-aligns to the second at once.
    fn tick_clock(&mut self) {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let fast = status::ticks_every_second(self.cfg.clock_seconds, self.hidden_full, crate::dock::session_idle());
        self.clock = status::clock_text(&t, self.cfg.clock_seconds);
        unsafe {
            SetTimer(Some(self.hwnd), CLOCK_TIMER, status::ms_to_next_tick(fast, t.wSecond, t.wMilliseconds), None);
        }
        self.update();
    }

    fn read_battery(&mut self) {
        let mut s = SYSTEM_POWER_STATUS::default();
        self.battery = if unsafe { GetSystemPowerStatus(&mut s) }.is_ok() && s.BatteryFlag != 128 && s.BatteryLifePercent <= 100 {
            Some((s.BatteryLifePercent, s.ACLineStatus == 1))
        } else {
            None
        };
        self.update();
        self.island_battery();
    }

    /// If slot i is a tray icon: what to send and where (screen px, just
    /// below the bar so menus open downward from the icon; just above a
    /// bar at the bottom, where the apps' menus flip upward as they do on
    /// the taskbar).
    fn tray_target(&mut self, i: usize, right: bool) -> Option<(u32, u32, crate::tray::Click, POINT)> {
        let Cell::Tray(k) = self.slots.get(i)?.cell else { return None };
        let e = self.tray.get(k)?;
        let r = self.slot_rect(i);
        let at = tray_click_point(r, self.at, self.scale, self.cfg.at_bottom(), work_area(mid_of(&r)));
        let kind = if right {
            crate::tray::Click::Right
        } else {
            let now = unsafe { GetTickCount() };
            let dbl = unsafe { GetDoubleClickTime() };
            match self.last_tray_click {
                Some((o, u, t)) if o == e.owner && u == e.uid && now.wrapping_sub(t) <= dbl => {
                    self.last_tray_click = None;
                    crate::tray::Click::Double
                }
                _ => {
                    self.last_tray_click = Some((e.owner, e.uid, now));
                    crate::tray::Click::Left
                }
            }
        };
        Some((e.owner, e.uid, kind, at))
    }

    /// A click on slot `i`. Status items open our own panels (returned, the
    /// caller shows them outside the bar's borrow).
    fn click(&mut self, i: usize) -> Option<panels::Panel> {
        match self.slots.get(i)?.cell {
            Cell::Start => None,
            Cell::Clock => {
                let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
                Some(panels::Panel::Calendar(t.wYear as i32, t.wMonth as u32, 0))
            }
            Cell::Net => Some(panels::Panel::Network),
            Cell::Volume => Some(panels::Panel::Volume),
            Cell::Battery => Some(panels::Panel::Battery),
            Cell::Tray(_) | Cell::Chevron | Cell::TrayButton | Cell::Control | Cell::Search | Cell::App | Cell::Menu(_) | Cell::Desktops | Cell::Hidden | Cell::Input | Cell::Companion => None,
        }
    }

    /// Everything to show `panel` under slot `i` (over it at the bottom).
    fn panel_request(&self, i: usize, panel: panels::Panel) -> (panels::Built, POINT, Rc<Gfx>, Theme, f32) {
        let r = self.slot_rect(i);
        let mut at = self.menu_at(&r);
        // The calendar opens where the Quick settings does: its far corner
        // POPUP_GAP_DIP in from the work area's right end (right to left: left
        // end), cal_align's corner.
        if matches!(panel, panels::Panel::Calendar(..)) {
            if let Some(work) = work_area(POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }) {
                let gap = crate::placement::popup_gap(self.scale);
                at.x = if crate::i18n::rtl() { work.left + gap } else { work.right - gap };
            }
        }
        // The calendar's agenda leaves room for its clock row as the popup
        // measures it (same font, text size and display scale), in the
        // room the popup keeps to under the bar's gap (popup::level_room).
        let clock_h = || crate::popup::measured_clock_text_h(&self.gfx, &self.theme.font.family, self.scale);
        let built = panels::build_measured(panel, self.volume, hang_room_dip(at, self.cfg.at_bottom(), self.scale), &clock_h);
        (built, at, self.gfx.clone(), self.theme.clone(), self.scale)
    }

    /// Where a menu or panel hangs from bar cell `r` (screen px):
    /// placement::POPUP_GAP_DIP under the bar, or over it while the bar is at
    /// the bottom (menu_align picks the matching corner), the same gap as
    /// the Quick settings's and every other panel's (placement::hang_edge).
    fn menu_at(&self, r: &RECT) -> POINT {
        POINT { x: menu_x(r), y: hang_y(*r, self.at, self.scale, self.cfg.at_bottom(), work_area(mid_of(r))) }
    }

    fn slot_of(&self, cell: Cell) -> Option<usize> {
        self.slots.iter().position(|s| s.cell == cell && s.w > 0.0)
    }

    // ---- menus -------------------------------------------------------------

    /// Slot index of bar title `t` (0 = app name, k = Menu(k - 1)).
    fn title_slot(&self, t: usize) -> Option<usize> {
        let want = if t == 0 { Cell::App } else { Cell::Menu(t - 1) };
        self.slots.iter().position(|s| s.cell == want && s.w > 0.0)
    }

    fn title_of_slot(&self, i: usize) -> Option<usize> {
        match self.slots.get(i)?.cell {
            Cell::App => Some(0),
            Cell::Menu(k) => Some(k + 1),
            _ => None,
        }
    }

    fn title_count(&self) -> usize {
        1 + if self.cfg.show_app_menus { self.titles.len() } else { 0 }
    }

    fn slot_rect(&self, i: usize) -> RECT {
        let Some(s) = self.slots.get(i) else { return RECT::default() };
        let m = self.at;
        RECT {
            left: m.left + (s.x * self.scale) as i32,
            top: m.top,
            right: m.left + ((s.x + s.w) * self.scale) as i32,
            bottom: m.top + (self.height * self.scale).round() as i32,
        }
    }

    fn window_titles(&self) -> Vec<(isize, String)> {
        self.active
            .windows
            .iter()
            .map(|&h| {
                let mut buf = [0u16; 256];
                let n = unsafe { GetWindowTextW(HWND(h as *mut _), &mut buf) }.max(0) as usize;
                (h, String::from_utf16_lossy(&buf[..n]))
            })
            .filter(|(_, t)| !t.is_empty())
            .collect()
    }

    /// Everything needed to show title `t`, or None (nothing to show, or a
    /// native menu is being read and will open when it arrives).
    fn prepare(&mut self, t: usize) -> Option<Prepared> {
        if crate::popup::is_tracking() {
            return None;
        }
        let slot = self.title_slot(t)?;
        let ready = self.ready.take().filter(|(rt, _)| *rt == t).map(|(_, b)| b);
        // Opening anything forgets an older native read.
        self.waiting = None;
        let built = if t == 0 {
            menus::app_menu(&self.active)
        } else if let Some(b) = ready {
            b
        } else {
            match menus::build(&self.source, &self.titles, t - 1, &self.active, &self.window_titles()) {
                Some(b) => b,
                None => {
                    if let menus::Source::Native(positions) = &self.source {
                        if let Some(&pos) = positions.get(t - 1) {
                            self.token += 1;
                            let started = menus::read_native_async(self.active.hwnd(), pos, self.hwnd, WM_MENU_READY, self.token);
                            if started {
                                self.waiting = Some((self.token, t, self.active.hwnd));
                                self.highlight_open(Some(t));
                                return None;
                            }
                        }
                    }
                    self.highlight_open(None);
                    return None;
                }
            }
        };
        let built = if self.active.locked && t > 0 { menus::lock(built) } else { built };
        if built.items.is_empty() {
            self.highlight_open(None);
            return None;
        }
        let r = self.slot_rect(slot);
        let titles = self.ring_titles(Ring::Title(t));
        let items = built.items.clone();
        self.built = Some(built);
        self.highlight_open(Some(t));
        Some(Prepared {
            gfx: self.gfx.clone(),
            theme: self.theme.clone(),
            items,
            at: self.menu_at(&r),
            scale: self.scale,
            titles,
        })
    }

    /// The bar's menu ring on show, in reading order: the system menu
    /// under the logo, the app's own menu, its titles. The pointer sliding
    /// along the bar and the left / right arrows go from one to the next
    /// while a menu is up (scrubbing, design system 7.4).
    fn ring(&self) -> Vec<Ring> {
        let mut ring = Vec::new();
        if self.slot_of(Cell::Start).is_some() {
            ring.push(Ring::System);
        }
        ring.extend((0..self.title_count()).filter(|&t| self.title_slot(t).is_some()).map(Ring::Title));
        ring
    }

    /// What the popup needs to know of the ring while menu `at` is up:
    /// where each of its cells is, which one is open, and the cell the
    /// menu hangs from (it grows open from the point of its edge nearest
    /// that cell: popup::open_origin).
    fn ring_titles(&self, at: Ring) -> crate::popup::Titles {
        let ring = self.ring();
        let rect = |r: Ring| self.slot_of(r.cell()).map(|i| self.slot_rect(i));
        crate::popup::Titles { rects: ring.iter().map(|&r| rect(r).unwrap_or_default()).collect(), current: ring.iter().position(|&r| r == at), anchor: rect(at) }
    }

    /// The popup closed. Returns the next menu of the ring to open, if
    /// the user went on to one (the open cell then stays lit until that
    /// menu's own cell takes over, at once).
    fn finish(&mut self, t: usize, out: crate::popup::Outcome) -> Option<Ring> {
        use crate::popup::Outcome;
        let next = ring_next(&self.ring(), Ring::Title(t), out);
        if next.is_some() {
            self.open = None;
            self.publish_a11y();
        } else {
            self.highlight_open(None);
        }
        let built = self.built.take();
        let target = self.active.hwnd();
        match out {
            Outcome::Picked(id) => {
                if let Some(a) = built.as_ref().and_then(|b| b.action(id)).cloned() {
                    self.after = Some((a, self.active.clone(), 0));
                    unsafe {
                        let _ = SetForegroundWindow(target);
                        // Runs once the app really has the focus (checked).
                        SetTimer(Some(self.hwnd), ACTION_TIMER, 40, None);
                    }
                }
                None
            }
            Outcome::Cancelled => {
                unsafe {
                    let _ = SetForegroundWindow(target);
                }
                None
            }
            // The user went elsewhere: leave the focus where it is.
            Outcome::Lost | Outcome::Dragged(_) => None,
            Outcome::Switch(_) | Outcome::Neighbor(_) => next,
        }
    }

    fn run_after(&mut self) {
        let Some((a, app, tries)) = self.after.take() else { return };
        let app = &app;
        let target = app.hwnd();
        unsafe {
            // Everything below acts on the app the item was picked for.
            if !matches!(a, menus::Action::NewWindow | menus::Action::OpenLocation) && !IsWindow(Some(target)).as_bool() {
                crate::log!("menu: target window is gone; action dropped");
                return;
            }
            let in_front = || GetAncestor(GetForegroundWindow(), GA_ROOT) == target;
            match a {
                menus::Action::Keys(seq) => {
                    if !in_front() {
                        if tries < 5 {
                            let _ = SetForegroundWindow(target);
                            self.after = Some((menus::Action::Keys(seq), app.clone(), tries + 1));
                            SetTimer(Some(self.hwnd), ACTION_TIMER, 60, None);
                        } else {
                            crate::log!("menu: {} never took the focus; shortcut dropped", app.name);
                        }
                        return;
                    }
                    for chord in &seq {
                        // Stop if the focus moved or Windows refused input.
                        if !in_front() || !crate::keys::press_chord(chord) {
                            crate::log!("menu: shortcut for {} interrupted", app.name);
                            break;
                        }
                    }
                }
                menus::Action::Command(id) => {
                    let _ = PostMessageW(Some(target), WM_COMMAND, WPARAM(id as usize), LPARAM(0));
                }
                menus::Action::Minimize => {
                    let _ = PostMessageW(Some(target), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0));
                }
                menus::Action::Zoom => {
                    let cmd = if IsZoomed(target).as_bool() { SC_RESTORE } else { SC_MAXIMIZE };
                    let _ = PostMessageW(Some(target), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0));
                }
                menus::Action::CloseWindow => {
                    let _ = PostMessageW(Some(target), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                menus::Action::NewWindow => {
                    crate::launch::open(&crate::launch::Target::new(&app.path, "", crate::launch::Source::TopBar));
                }
                menus::Action::OpenLocation => {
                    if let Some(exe) = &app.exe {
                        let args = windows::core::HSTRING::from(format!("/select,\"{exe}\""));
                        crate::launch::open_soon(crate::launch::Target::new("explorer.exe", "Explorer", crate::launch::Source::Dock).args(&args.to_string(), ""));
                    }
                }
                menus::Action::HideApp => {
                    for &h in &app.windows {
                        let _ = PostMessageW(Some(HWND(h as *mut _)), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0));
                    }
                }
                menus::Action::QuitApp => {
                    for &h in &app.windows {
                        let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
                menus::Action::Focus(h) => {
                    // Restore + front without waiting on the app (A02).
                    crate::dock::bring_to_front(HWND(h as *mut _));
                }
            }
        }
    }

    /// Title `t`'s menu is up, or being read (None: none is): its cell is
    /// the open one (set_anchor).
    fn highlight_open(&mut self, t: Option<usize>) {
        self.open = t;
        match t {
            Some(t) => self.set_anchor(Some(Ring::Title(t).cell())),
            // (Another cell's panel may be up: only a title's fill goes.)
            None if matches!(self.anchor, Some(Cell::App | Cell::Menu(_))) => self.set_anchor(None),
            None => {}
        }
        // A menu title opened or closed (ExpandCollapse state).
        self.publish_a11y();
    }

    fn note_tray_focus(&mut self, available: bool) {
        if std::mem::replace(&mut self.tray_focus_available, available) != available {
            if available { TRAY_FOCUS_BARS.fetch_add(1, Ordering::Release); }
            else { TRAY_FOCUS_BARS.fetch_sub(1, Ordering::Release); }
        }
    }

    /// Where Win+B and the arrows go, left to right: the tray icons (else
    /// the chevron), then the status cells after them, as Windows' Win+B
    /// walks from the notification area on to quick settings and the clock.
    /// With no tray icon shown, Win+B still lands in the bar, never on the
    /// hidden taskbar.
    fn tray_stops(&self) -> Vec<usize> {
        let shown = |s: &Slot| s.w > 0.0 && s.alpha > 0.0;
        let mut stops: Vec<_> = self.slots.iter().enumerate().filter(|(_, s)| shown(s) && matches!(s.cell, Cell::Tray(_) | Cell::TrayButton)).map(|(i, _)| i).collect();
        if stops.is_empty() { stops.extend(self.slot_of(Cell::Chevron)); }
        stops.extend(self.slots.iter().enumerate().filter(|(_, s)| shown(s) && status_stop(s.cell)).map(|(i, _)| i));
        stops.sort_by(|&a, &b| self.slots[a].x.total_cmp(&self.slots[b].x));
        stops
    }

    fn tray_at(&self, x: f32) -> Option<&crate::tray::Entry> {
        let Cell::Tray(k) = self.slots[self.hit(x)?].cell else { return None };
        self.tray.get(k)
    }

    fn tray_motion(&mut self, x: Option<f32>) {
        let next = x.filter(|_| crate::taskbar::hidden_by_us()).and_then(|x| self.tray_at(x));
        let mut at = POINT::default();
        unsafe { let _ = GetCursorPos(&mut at); }
        if self.tray_hover.as_ref().map(|e| (e.owner, e.uid, e.version, e.callback)) != next.map(|e| (e.owner, e.uid, e.version, e.callback)) {
            if let Some(old) = self.tray_hover.as_ref().filter(|e| e.version >= 4) {
                old.pointer_message(WM_USER + 7, at); // NIN_POPUPCLOSE
            }
            if let Some(new) = next.filter(|e| e.version >= 4) {
                new.pointer_message(WM_USER + 6, at); // NIN_POPUPOPEN
            }
            self.tray_hover = next.cloned();
        }
        if let Some(e) = &self.tray_hover {
            e.pointer_message(WM_MOUSEMOVE, at);
        }
    }

    fn on_move(&mut self, x: f32) {
        self.tray_motion(Some(x));
        if !self.tracking {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking = true;
        }
        let h = self.hit(x);
        if h != self.hovered {
            self.hovered = h;
            self.layout_hover();
        }
        self.track_tip(Some(x));
        self.hidden_hover(Some(x));
    }
}

/// An owned snapshot for showing a popup outside the bar's borrow.
struct Prepared {
    gfx: Rc<Gfx>,
    theme: Theme,
    items: Vec<crate::popup::Item>,
    at: POINT,
    scale: f32,
    titles: crate::popup::Titles,
}

/// One of the menus of a bar's ring (TopBar::ring): the system menu under
/// the logo, or bar title `t`'s (0: the app's own menu).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ring {
    System,
    Title(usize),
}

impl Ring {
    /// The cell it hangs from.
    fn cell(self) -> Cell {
        match self {
            Ring::System => Cell::Start,
            Ring::Title(0) => Cell::App,
            Ring::Title(t) => Cell::Menu(t - 1),
        }
    }
}

/// Pure: the menu of `ring` the user went on to when menu `at` closed
/// with `out`: the one whose cell the pointer slid onto (Switch: its place
/// in the ring), or the previous / next one, round the ring (Neighbor: the
/// left and right arrows). None: the menu closed for good.
fn ring_next(ring: &[Ring], at: Ring, out: crate::popup::Outcome) -> Option<Ring> {
    use crate::popup::Outcome;
    match out {
        Outcome::Switch(k) => ring.get(k).copied(),
        Outcome::Neighbor(d) => {
            let here = ring.iter().position(|&r| r == at)? as i32;
            ring.get((here + d).rem_euclid(ring.len() as i32) as usize).copied()
        }
        _ => None,
    }
}

/// Opens bar title `t` and follows the user across the bar's menus until
/// one closes for good (open_ring).
fn open_menus(bar: HWND, t: usize) {
    open_ring(bar, Ring::Title(t));
}

/// Opens menu `at` of bar `bar`'s ring and follows the user round it (the
/// pointer sliding along the bar, the left and right arrows) until a menu
/// closes for good. The first one opens as menus do; each one after it
/// takes over from the one before (scrubbing, design system 7.4: the open
/// fill moves at once, the two menus cross at full size: popup's hand-over).
/// Runs outside any with_bar borrow (popup::track pumps messages,
/// including ours).
fn open_ring(bar: HWND, mut at: Ring) {
    loop {
        let next = match at {
            Ring::Title(t) => menu_step(bar, t),
            Ring::System => with_bar_of(bar, |b| b.slot_of(Cell::Start)).flatten().and_then(|i| panel_step(bar, i, panels::Panel::System)),
        };
        match next {
            Some(n) => at = n,
            None => break,
        }
    }
    // Nothing of the ring is up any more: its open fill goes (unless a
    // native menu is still being read: its title stays lit until it comes).
    with_bar_of(bar, |b| {
        if b.waiting.is_none() && !crate::popup::is_tracking() {
            b.highlight_open(None);
            if b.anchor == Some(Cell::Start) {
                b.set_anchor(None);
            }
        }
    });
}

/// Shows bar title `t`'s menu until it closes; the ring's next menu if
/// the user went on to one.
fn menu_step(bar: HWND, t: usize) -> Option<Ring> {
    let generation = GENERATION.load(Ordering::SeqCst);
    let p = with_bar_of(bar, |b| b.prepare(t)).flatten()?;
    let out = crate::popup::track(crate::popup::Request {
        gfx: &p.gfx,
        theme: &p.theme,
        items: p.items,
        at: p.at,
        align: menu_align(),
        scale: p.scale,
        titles: p.titles,
        on_slide: None,
        drag_ids: 0..0,
        on_space: None,
        on_pick: None,
        hangs: true,
    });
    if GENERATION.load(Ordering::SeqCst) != generation {
        // The bar that opened this menu is gone.
        return None;
    }
    with_bar_of(bar, |b| b.finish(t, out)).flatten()
}

/// Runs `f`, which shows what cell `cell` of bar `bar` opens and returns
/// when that surface starts to close (it pumps messages: outside the bars'
/// borrow), with the cell lit as open meanwhile (6.2).
fn while_open<R>(bar: HWND, cell: Cell, f: impl FnOnce() -> R) -> R {
    with_bar_of(bar, |b| b.set_anchor(Some(cell)));
    let out = f();
    with_bar_of(bar, |b| {
        if b.anchor == Some(cell) {
            b.set_anchor(None);
        }
    });
    out
}

/// Quick settings under slot i. The panel pumps messages.
fn open_control(bar: HWND, i: usize) {
    let Some((gfx, theme, button, scale, volume)) = with_bar_of(bar, |b| (b.gfx.clone(), b.theme.clone(), b.slot_rect(i), b.scale, b.volume)) else {
        return;
    };
    while_open(bar, Cell::Control, || control::show(control::Request { gfx: &gfx, theme: &theme, button, scale, volume }));
}

/// "About this PC" centered on bar `bar`'s monitor (outside the bars'
/// borrow; it pumps messages): the system menu's first row.
fn open_about(bar: HWND) {
    let Some((gfx, theme, hmon, scale)) = with_bar_of(bar, |b| (b.gfx.clone(), b.theme.clone(), b.hmon, b.scale)) else { return };
    about::show(&gfx, &theme, hmon, scale);
}

/// Search on this bar's monitor (outside the bars' borrow; it pumps
/// messages).
fn open_search(bar: HWND) {
    let Some((gfx, theme, hmon)) = with_bar_of(bar, |b| (b.gfx.clone(), b.theme.clone(), b.hmon)) else { return };
    // Its button stays lit while it is up (6.2 "open").
    while_open(bar, Cell::Search, || crate::search::show(crate::search::Request { gfx: &gfx, theme: &theme, monitor: Some(hmon) }));
}

/// The bar a shortcut or hot corner opens `cell`'s panel from: the one on
/// the pointer's display when it has that button ("Same on every
/// display"), else the main bar. (bar, slot); None when no bar on show has
/// it, or the bars are busy.
fn bar_with(cell: Cell) -> Option<(HWND, usize)> {
    let here = crate::monitors::under_cursor().map(|m| m.handle);
    BARS.with(|b| {
        let g = b.try_borrow().ok()?;
        let shown = |x: &&TopBar| !x.hidden_full && x.slot_of(cell).is_some();
        let pick = g.iter().filter(shown).find(|x| Some(x.hmon) == here).or_else(|| g.iter().filter(shown).find(|x| x.primary))?;
        Some((pick.hwnd, pick.slot_of(cell)?))
    })
}

pub(crate) fn focus_notification_area() {
    let here = crate::monitors::under_cursor().map(|m| m.handle);
    let target = BARS.with(|bars| {
        let bars = bars.try_borrow().ok()?;
        // The bar with the notification area first (the primary's, like
        // Windows), then the one under the pointer.
        let tray = |b: &TopBar| b.slots.iter().any(|s| s.w > 0.0 && matches!(s.cell, Cell::Tray(_) | Cell::TrayButton | Cell::Chevron));
        bars.iter().filter(|b| !b.hidden_full && !b.tray_stops().is_empty())
            .max_by_key(|b| (tray(b), Some(b.hmon) == here, b.primary)).map(|b| b.hwnd)
    });
    let Some(hwnd) = target else {
        crate::log!("topbar: Win+B: no bar with a place to focus");
        return;
    };
    unsafe {
        let prev = GetForegroundWindow();
        let _ = SetForegroundWindow(hwnd);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
        if GetForegroundWindow() != hwnd {
            crate::log!("topbar: Win+B: could not take the keyboard focus");
            return;
        }
        crate::log!("topbar: Win+B: keyboard focus in the bar");
        with_bar_of(hwnd, |b| {
            if b.tray_stops().first().is_some_and(|&i| b.slots[i].cell == Cell::Chevron) {
                b.set_expanded(true);
            }
            if let Some(&i) = b.tray_stops().first() {
                let previous = b.tray_focus.as_ref().map_or(prev, |(_, h)| *h);
                b.tray_focus = Some((b.slot_key(i), previous));
                b.layout_hover();
                b.publish_a11y();
            }
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarKeyAction { Default, ReleaseFocus, HandleKey }

fn bar_key_action(message: u32, value: usize, focused: bool) -> BarKeyAction {
    match message {
        WM_CLOSE => BarKeyAction::ReleaseFocus,
        WM_SYSCOMMAND if value & 0xfff0 == SC_CLOSE as usize => BarKeyAction::ReleaseFocus,
        WM_SYSKEYDOWN if focused && value == 0x73 => BarKeyAction::ReleaseFocus, // Alt+F4
        WM_KEYDOWN | WM_SYSKEYDOWN if focused && value == 0x1b => BarKeyAction::ReleaseFocus,
        WM_KEYDOWN | WM_SYSKEYDOWN if focused => BarKeyAction::HandleKey,
        _ => BarKeyAction::Default,
    }
}

fn release_tray_focus(hwnd: HWND) {
    let previous = with_bar_of(hwnd, |b| {
        let (_, previous) = b.tray_focus.take()?;
        if b.expanded { b.poke_tray_timer(); }
        b.layout_hover();
        b.publish_a11y();
        Some(previous)
    }).flatten();
    if let Some(previous) = previous {
        unsafe {
            if GetForegroundWindow() == hwnd &&
                (previous == hwnd || previous.is_invalid() || !SetForegroundWindow(previous).as_bool()) {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(None);
            }
        }
    }
}

/// The bar's status cells Win+B's arrows walk on to after the tray icons.
fn status_stop(c: Cell) -> bool {
    matches!(c, Cell::Hidden | Cell::Input | Cell::Net | Cell::Volume | Cell::Battery | Cell::Search | Cell::Control | Cell::Companion | Cell::Clock)
}

fn tray_key(hwnd: HWND, vk: usize) {
    let Some((key, previous)) = with_bar_of(hwnd, |b| b.tray_focus.clone()).flatten() else { return };
    match vk {
        0x25 | 0x27 => { with_bar_of(hwnd, |b| {
            let stops = b.tray_stops();
            if stops.is_empty() { return; }
            let pos = stops.iter().position(|&i| b.slot_key(i) == key).unwrap_or(0);
            let n = stops.len();
            let next = if vk == 0x25 { (pos+n-1)%n } else { (pos+1)%n };
            b.tray_focus = Some((b.slot_key(stops[next]), previous));
            b.layout_hover();
            b.publish_a11y();
        }); }
        0x0d | 0x20 => {
            let target = with_bar_of(hwnd, |b| {
                let i = b.slot_by_key(&key)?;
                let Cell::Tray(k) = b.slots[i].cell else { return None };
                let e = b.tray.get(k)?;
                Some((e.owner, e.uid, mid_of(&b.slot_rect(i))))
            }).flatten();
            if let Some((owner, uid, at)) = target {
                crate::tray::click(owner, uid, crate::tray::Click::Keyboard, at);
            } else {
                access::act(hwnd, &key, crate::a11y::Act::Invoke);
            }
        }
        _ => {}
    }
}

fn mid_of(r: &RECT) -> POINT {
    POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }
}

/// Pure: the edge (screen px) a menu or panel hanging off bar cell `r` on
/// bar `bar` starts at, at `scale`: the popup gap under the bar (over it,
/// `up`, a bar at the bottom), across the bar's whole height (a pill
/// inside it hangs from the bar too), counted from what `work` leaves
/// (placement::hang_edge); off the bar itself without a work area.
fn hang_y(r: RECT, bar: RECT, scale: f32, up: bool, work: Option<RECT>) -> i32 {
    let gap = crate::placement::popup_gap(scale);
    let cell = RECT { top: bar.top, bottom: bar.bottom, ..r };
    match work {
        Some(work) => crate::placement::hang_edge(cell, gap, up, work),
        None if up => cell.top - gap,
        None => cell.bottom + gap,
    }
}

/// Pure: the point a click on tray cell `r` hands the icon's app, where
/// its menu or flyout anchors: the cell's middle, on the edge our own
/// menus hang from (hang_y: the popup gap off the bar).
fn tray_click_point(r: RECT, bar: RECT, scale: f32, up: bool, work: Option<RECT>) -> POINT {
    POINT { x: (r.left + r.right) / 2, y: hang_y(r, bar, scale, up, work) }
}

/// Pure: a click with anchor point `at` came from bar cell `cell` (screen
/// px), whose clicks tray_click_point puts at `point` (the cell's middle,
/// the popup gap off the bar): `at` is across the cell and between the
/// bar and that point.
fn cell_anchors(cell: RECT, point: POINT, at: POINT) -> bool {
    at.x >= cell.left && at.x < cell.right && at.y >= cell.top.min(point.y) && at.y <= cell.bottom.max(point.y)
}

/// NopBar's own notification icon as a bar shows it, for a click on it
/// whose anchor point is `at`: that bar's look, the icon's cell (screen
/// px), the bar's scale and whether it sits at the bottom. None when no
/// bar shows our icon there (it was clicked in Windows' own tray).
pub fn own_tray_cell(at: POINT) -> Option<(Rc<Gfx>, Theme, RECT, f32, bool)> {
    BARS.with(|b| {
        let g = b.try_borrow().ok()?;
        g.iter().filter(|x| !x.hidden_full).find_map(|x| {
            x.slots.iter().enumerate().find_map(|(i, s)| match s.cell {
                Cell::Companion => {
                    let r = x.slot_rect(i);
                    let up = x.cfg.at_bottom();
                    let point = tray_click_point(r, x.at, x.scale, up, work_area(mid_of(&r)));
                    cell_anchors(r, point, at).then(|| (x.gfx.clone(), x.theme.clone(), r, x.scale, up))
                }
                _ => None,
            })
        })
    })
}

/// Opens the Quick settings under the primary bar's button (a hot corner).
/// False if there is no such button (top bar off, or hidden for a
/// fullscreen app) or the bars are busy.
pub fn open_control_center() -> bool {
    let target = bar_with(Cell::Control);
    match target {
        Some((bar, i)) => {
            open_control(bar, i);
            true
        }
        None => false,
    }
}

/// A click at bar x (px from the bar's left edge) on the desktop cell:
/// switch to the dot's desktop, or with the "3/12" text open the list.
fn desktops_click(bar: HWND, i: usize, x_px: f32) {
    let pick = with_bar_of(bar, |b| {
        b.drop_tip();
        match b.desktops_look() {
            desktops::Look::Dots { .. } => b.desktop_at(x_px / b.scale).map(Some),
            desktops::Look::Count(_) => Some(None),
            desktops::Look::Hidden => None,
        }
    })
    .flatten();
    match pick {
        Some(Some(d)) => {
            let s = crate::vdesk::go_to(d);
            if s != crate::keys::Sent::Ok {
                crate::log!("desktops: switch to {}: not sent ({s:?})", d + 1);
            }
        }
        Some(None) => open_panel(bar, i, panels::Panel::Desktops),
        None => {}
    }
}

/// The work area of the monitor around screen point `at` (px).
fn work_area(at: POINT) -> Option<RECT> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let mon = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(mon, &mut mi).as_bool().then_some(mi.rcWork)
    }
}

/// The room (DIP) a menu or panel hanging from `at` (menu_at: the bar's
/// gap in it; `up` for a bar at the bottom) has in its work area: what
/// the popup lays it out in (popup::level_room), so the calendar's agenda
/// is chosen for the same height.
fn hang_room_dip(at: POINT, up: bool, scale: f32) -> f32 {
    match work_area(at) {
        Some(work) => crate::placement::room_from(at.y, up, work) as f32 / scale.max(0.1),
        None => f32::INFINITY,
    }
}

/// The calendar panel while it is open: where it hangs and what its rows
/// do, so a fetch that brings new events redraws it in place
/// (agenda_changed).
struct OpenCalendar {
    bar: HWND,
    slot: usize,
    panel: panels::Panel,
    built: Rc<RefCell<panels::Built>>,
}

thread_local! {
    static OPEN_CALENDAR: RefCell<Option<OpenCalendar>> = const { RefCell::new(None) };
    /// The panel open_panel has up: which bar, cell and kind of panel.
    static OPEN_PANEL: std::cell::Cell<Option<PanelOwner>> = const { std::cell::Cell::new(None) };
}

/// Who a panel from open_panel belongs to: its bar, its cell, its kind.
type PanelOwner = (isize, Cell, std::mem::Discriminant<panels::Panel>);

/// Pure: asking for the panel `asked` while `open` is up is a second
/// press on the same button for the same panel (a screen reader's Invoke
/// twice): it closes it, as a second click does. Another panel or another
/// cell's is no such toggle.
fn repeats(open: Option<PanelOwner>, asked: Option<PanelOwner>) -> bool {
    asked.is_some() && open == asked
}

/// Timer on a bar window while it shows the calendar: a row's event starts
/// or ends, or the day turns (Built::refresh_at).
const AGENDA_TIMER: usize = 31;

/// Sets bar `bar`'s agenda timer for `at` (UTC seconds), or none while
/// nobody can see the screen (session_idle_changed catches up).
fn arm_agenda(bar: HWND, at: Option<i64>) {
    unsafe {
        let _ = KillTimer(Some(bar), AGENDA_TIMER);
        let Some(at) = at else { return };
        if crate::dock::session_idle() {
            return;
        }
        let ms = ((at - crate::agenda::unix_now() as i64) * 1000 + 500).clamp(1000, 3_600_000);
        SetTimer(Some(bar), AGENDA_TIMER, ms as u32, None);
    }
}

/// The agenda changed (crate::agenda posted WM_AGENDA, or its time came):
/// an open calendar shows the new rows in place. Its picks mean the new
/// rows only once they are on screen; if they cannot be drawn, the old rows
/// and their picks stay together.
fn agenda_changed() {
    let Some((bar, slot, panel, shared)) = OPEN_CALENDAR.with(|o| o.borrow().as_ref().map(|c| (c.bar, c.slot, c.panel, c.built.clone()))) else { return };
    let Some(built) = with_bar_of(bar, |b| {
        let cell = b.slots.get(slot)?.cell;
        let i = b.slot_of(cell)?;
        Some(b.panel_request(i, panel).0)
    })
    .flatten() else {
        return;
    };
    arm_agenda(bar, built.refresh_at);
    if *shared.borrow() == built {
        return;
    }
    if crate::popup::replace(built.items.clone()) {
        *shared.borrow_mut() = built;
    }
}

/// How often an open panel showing the progress ring asks whether what
/// it waits for has settled (ms).
const WAIT_POLL_MS: u32 = 100;

/// Panel `panel` under slot `i` of bar `bar`, built again as it is now.
fn panel_again(bar: HWND, i: usize, panel: panels::Panel) -> Option<panels::Built> {
    with_bar_of(bar, |b| {
        let cell = b.slots.get(i)?.cell;
        let i = b.slot_of(cell)?;
        Some(b.panel_request(i, panel).0)
    })
    .flatten()
}

/// An open panel shows the progress ring (design system 11.3: the Wi-Fi
/// list did not come within 300 ms): once that settles the panel is built
/// again and shown in place (the list, or the failure with its button),
/// its picks meaning the new rows. `alive` goes false when the panel
/// closes.
fn wait_poll(bar: HWND, i: usize, panel: panels::Panel, shared: Rc<RefCell<panels::Built>>, alive: Rc<std::cell::Cell<bool>>) {
    let _ = crate::motion::after(
        WAIT_POLL_MS,
        Box::new(move || {
            if !alive.get() || !shared.borrow().waiting {
                return;
            }
            if !panels::wifi_settled() {
                return wait_poll(bar, i, panel, shared, alive);
            }
            let Some(nb) = panel_again(bar, i, panel) else { return };
            if crate::popup::replace(nb.items.clone()) {
                let again = nb.waiting;
                *shared.borrow_mut() = nb;
                if again {
                    wait_poll(bar, i, panel, shared, alive);
                }
            }
        }),
    );
}

/// Pure: the calendar a calendar's row `action` shows next (its month
/// buttons, a day picked), in the same open popup; None for anything else.
fn calendar_step(action: Option<&panels::Action>) -> Option<panels::Panel> {
    match action {
        Some(&panels::Action::Month(y, m)) => Some(panels::Panel::Calendar(y, m, 0)),
        Some(&panels::Action::Day(y, m, d)) => Some(panels::Panel::Calendar(y, m, d)),
        _ => None,
    }
}

/// Shows a panel under slot `i` of bar `bar` (outside the bars' borrow;
/// the popup pumps messages). Calendar month buttons and days change it
/// in place (its day grid cross-fades: popup's OnPick); should that fail,
/// they reopen it. The system menu is one of the bar's menus (the ring):
/// from it the user goes on to the app's (open_ring).
fn open_panel(bar: HWND, i: usize, panel: panels::Panel) {
    if let Some(next) = panel_step(bar, i, panel) {
        open_ring(bar, next);
    }
}

/// open_panel until the panel closes. Its cell is lit as open meanwhile
/// (6.2). Returns the ring's next menu if the user went on to one from the
/// system menu (the pointer slid onto a title, or an arrow key).
fn panel_step(bar: HWND, i: usize, mut panel: panels::Panel) -> Option<Ring> {
    // Another menu wins over a Start request still waiting for the keys.
    crate::keys::cancel_start(None);
    loop {
        let owner = with_bar_of(bar, |b| b.slots.get(i).map(|s| (bar.0 as isize, s.cell, std::mem::discriminant(&panel)))).flatten();
        if repeats(OPEN_PANEL.with(|o| o.get()), owner) && crate::popup::is_tracking() {
            crate::popup::cancel();
            return None;
        }
        let ring = matches!(panel, panels::Panel::System);
        let ((built, at, gfx, theme, scale), cell, titles) = with_bar_of(bar, |b| {
            let cell = b.slots.get(i)?.cell;
            let i = b.slot_of(cell)?;
            // The system menu takes its place in the ring; any other
            // panel only says which cell it hangs from (its growth's
            // origin: popup::open_origin).
            let titles = if ring { b.ring_titles(Ring::System) } else { crate::popup::Titles { anchor: Some(b.slot_rect(i)), ..Default::default() } };
            Some((b.panel_request(i, panel), cell, titles))
        })
        .flatten()?;
        let calendar = matches!(panel, panels::Panel::Calendar(..));
        let built = Rc::new(RefCell::new(built));
        if calendar {
            arm_agenda(bar, built.borrow().refresh_at);
            OPEN_CALENDAR.with(|o| *o.borrow_mut() = Some(OpenCalendar { bar, slot: i, panel, built: built.clone() }));
            crate::agenda::panel_open(bar.0 as isize);
        }
        let (items, slider) = {
            let b = built.borrow();
            (b.items.clone(), b.volume_slider)
        };
        let on_slide: Option<Box<dyn Fn(f32)>> = if slider { Some(Box::new(status::set_volume)) } else { None };
        // Another month or day: the same popup shows it (no close and
        // reopen); its picks then mean the new rows.
        // A list that failed: its button asks again, in the same popup.
        let alive = Rc::new(std::cell::Cell::new(true));
        if built.borrow().waiting {
            wait_poll(bar, i, panel, built.clone(), alive.clone());
        }
        let reload: crate::popup::OnPick = {
            let (shared, alive) = (built.clone(), alive.clone());
            Box::new(move |id: u32, swap: &mut dyn FnMut(Vec<crate::popup::Item>) -> bool| {
                if shared.borrow().action(id) != Some(&panels::Action::Reload) {
                    return false;
                }
                let Some(nb) = panel_again(bar, i, panel) else { return false };
                if !swap(nb.items.clone()) {
                    return false;
                }
                let waiting = nb.waiting;
                *shared.borrow_mut() = nb;
                if waiting {
                    wait_poll(bar, i, panel, shared.clone(), alive.clone());
                }
                true
            })
        };
        let on_pick: Option<crate::popup::OnPick> = if !calendar { Some(reload) } else { Some({
            let shared = built.clone();
            Box::new(move |id: u32, swap: &mut dyn FnMut(Vec<crate::popup::Item>) -> bool| {
                let Some(next) = calendar_step(shared.borrow().action(id)) else { return false };
                let Some(nb) = with_bar_of(bar, |b| {
                    let cell = b.slots.get(i)?.cell;
                    let i = b.slot_of(cell)?;
                    Some(b.panel_request(i, next).0)
                })
                .flatten() else {
                    return false;
                };
                if !swap(nb.items.clone()) {
                    return false;
                }
                arm_agenda(bar, nb.refresh_at);
                OPEN_CALENDAR.with(|o| {
                    if let Some(c) = o.borrow_mut().as_mut() {
                        c.panel = next;
                    }
                });
                *shared.borrow_mut() = nb;
                true
            }) as crate::popup::OnPick
        }) };
        let before = OPEN_PANEL.with(|o| o.replace(owner));
        with_bar_of(bar, |b| b.set_anchor(Some(cell)));
        let out = crate::popup::track(crate::popup::Request {
            gfx: &gfx,
            theme: &theme,
            items,
            at,
            align: if calendar { cal_align() } else { menu_align() },
            scale,
            titles,
            on_slide,
            drag_ids: 0..0,
            on_space: None,
            on_pick,
            hangs: true,
        });
        OPEN_PANEL.with(|o| o.set(before));
        alive.set(false);
        if calendar {
            OPEN_CALENDAR.with(|o| o.borrow_mut().take());
            crate::agenda::panel_open(0);
            arm_agenda(bar, None);
        }
        // From the system menu on to another of the bar's menus: its
        // cell stays lit until that menu's takes over (at once).
        let next = if ring { with_bar_of(bar, |b| ring_next(&b.ring(), Ring::System, out)).flatten() } else { None };
        if next.is_some() {
            return next;
        }
        let picked = match out {
            crate::popup::Outcome::Picked(id) => built.borrow().action(id).cloned(),
            _ => None,
        };
        let again = calendar_step(picked.as_ref());
        if again.is_none() {
            // It started to close: the cell's open fill goes.
            with_bar_of(bar, |b| {
                if b.anchor == Some(cell) {
                    b.set_anchor(None);
                }
            });
        }
        if let Some(next) = again {
            // (Not shown in place: opened again.)
            panel = next;
            continue;
        }
        if let Some(a) = picked {
            panels::run(&a, bar);
        }
        return None;
    }
}

/// What a right-click on the bar does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RightClick {
    /// The cell's own menu, as before: the desktops' list, the keyboard
    /// layouts, the system menu on the logo, a tray icon's app menu.
    Own,
    /// Nothing: the app name and its menu titles (the app's menus, which
    /// open on a left press).
    Nothing,
    /// The bar's own menu (panels::Panel::Bar): settings.
    BarMenu,
}

/// Pure: what a right-click on `cell` does (None: an empty part of the
/// bar, the island included). Every cell without a menu of its own opens
/// the bar's menu, the clock and the status icons too.
fn right_click(cell: Option<Cell>) -> RightClick {
    match cell {
        // The windows sent to the tray: their own Show / Close menu.
        Some(Cell::Desktops | Cell::Input | Cell::Start | Cell::Tray(_) | Cell::Hidden) => RightClick::Own,
        Some(Cell::App | Cell::Menu(_)) => RightClick::Nothing,
        Some(
            Cell::Chevron
            | Cell::TrayButton
            | Cell::Net
            | Cell::Volume
            | Cell::Battery
            | Cell::Companion
            | Cell::Search
            | Cell::Control
            | Cell::Clock,
        )
        | None => RightClick::BarMenu,
    }
}

/// The bar's own menu (right_click): Settings at the Top bar, Dock or
/// General page, and Task Manager. Hangs from `at` (screen px; menu_at's
/// point under the pointer, or under a cell for a screen reader): downward
/// from a bar at the top, upward from one at the bottom (menu_align), the
/// popup gap from the bar like every menu off it.
fn open_bar_menu(bar: HWND, at: POINT) {
    crate::keys::cancel_start(None);
    let Some((gfx, theme, scale)) = with_bar_of(bar, |b| {
        // Tips go when a menu opens (7.4).
        b.drop_tip();
        b.hidden_hover(None);
        (b.gfx.clone(), b.theme.clone(), b.scale)
    }) else {
        return;
    };
    let built = panels::build(panels::Panel::Bar, None, f32::INFINITY);
    let out = crate::popup::track(crate::popup::Request {
        gfx: &gfx,
        theme: &theme,
        items: built.items.clone(),
        at,
        align: menu_align(),
        scale,
        titles: Default::default(),
        on_slide: None,
        drag_ids: 0..0,
        on_space: None,
        on_pick: None,
        hangs: true,
    });
    if let crate::popup::Outcome::Picked(id) = out {
        if let Some(a) = built.action(id) {
            panels::run(a, bar);
        }
    }
}

/// A click on slot `i` (released where it was pressed). `x_px`: where on
/// the bar (px from its left edge), for the desktop dots; None (a screen
/// reader's Invoke) opens their list instead.
fn click_slot(bar: HWND, i: usize, x_px: Option<f32>) {
    match with_bar_of(bar, |b| b.slots.get(i).map(|s| s.cell)).flatten() {
        Some(Cell::Companion) => {
            companion::click(bar, i, false, x_px.is_none());
            return;
        }
        Some(Cell::Chevron) => {
            with_bar_of(bar, |b| b.set_expanded(!b.expanded));
            return;
        }
        Some(Cell::TrayButton) => {
            crate::tray::open_overflow();
            return;
        }
        Some(Cell::Control) => {
            open_control(bar, i);
            return;
        }
        Some(Cell::Search) => {
            open_search(bar);
            return;
        }
        Some(Cell::Hidden) => {
            hidden::click(bar, i);
            return;
        }
        // 한/영 (native / Latin) in the window in front; without an IME
        // to switch, the layouts.
        Some(Cell::Input) => {
            if crate::ime::can_toggle() {
                crate::ime::toggle();
            } else {
                open_panel(bar, i, panels::Panel::Input);
            }
            return;
        }
        Some(Cell::Desktops) => {
            match x_px {
                Some(x) => desktops_click(bar, i, x),
                None => open_panel(bar, i, panels::Panel::Desktops),
            }
            return;
        }
        Some(Cell::Start) => {
            crate::keys::start_menu(bar.0 as isize);
            return;
        }
        _ => {}
    }
    // Tray icons: the click goes to the app (outside the borrow: the app
    // may answer synchronously).
    match with_bar_of(bar, |b| b.tray_target(i, false)).flatten() {
        Some((owner, uid, kind, at)) => crate::tray::click(owner, uid, kind, at),
        None => {
            let panel = with_bar_of(bar, |b| b.click(i)).flatten();
            if let Some(p) = panel {
                open_panel(bar, i, p);
            }
        }
    }
}

/// Pure: the cells that glide to a new place and fade in and out as the
/// tray folds, reflows or is dragged in: the tray's icons with their
/// chevron, and the desktop dots beside them.
fn slides(c: Cell) -> bool {
    matches!(c, Cell::Tray(_) | Cell::Chevron | Cell::Desktops)
}

/// Pure: the cell's content is drawn in one color (the bar's text color):
/// text, a glyph, the dots, the input box. Such a cell can be shown in
/// another color as it is (high contrast's HighlightText on a lit cell).
/// Not an app's own picture (a tray icon, a hidden window's icon) nor the
/// battery (its fill and its number are two colors).
fn one_color(c: Cell) -> bool {
    !matches!(c, Cell::Tray(_) | Cell::Hidden | Cell::Battery | Cell::Companion)
}

/// Pure: whether a premultiplied BGRA picture is a one-color glyph (no
/// hue in its solid pixels) and which: Some(true) light (white-ish),
/// Some(false) dark, None for a colored or mid-grey picture.
fn glyph_lightness(px: &[u8]) -> Option<bool> {
    let (mut solid, mut hued, mut sum) = (0u32, 0u32, 0f32);
    for p in px.chunks_exact(4) {
        let a = p[3] as u32;
        if a < 96 {
            continue;
        }
        let c = |v: u8| (v as u32 * 255 / a).min(255);
        let (b, g, r) = (c(p[0]), c(p[1]), c(p[2]));
        solid += 1;
        if r.max(g).max(b) - r.min(g).min(b) > 40 {
            hued += 1;
        }
        sum += (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
    }
    if solid < 12 || hued * 20 > solid {
        return None;
    }
    let mean = sum / solid as f32;
    if mean >= 0.75 {
        Some(true)
    } else if mean <= 0.25 {
        Some(false)
    } else {
        None
    }
}

/// Which optional right-side cells give way first when the bar is too
/// narrow (lower goes first); None: never (the clock, the tray cell, the
/// Quick settings, status icons, tray icons: they have the drawer).
fn drop_rank(c: Cell) -> Option<u8> {
    match c {
        Cell::Input => Some(0),
        Cell::Search => Some(1),
        _ => None,
    }
}

/// Pure: which of the cells (`widths`, with their drop `ranks`) are left
/// out so their total fits in `room`: lowest rank first, one at a time,
/// until it fits or nothing optional is left.
fn overflow(widths: &[f32], ranks: &[Option<u8>], room: f32) -> Vec<bool> {
    let mut out = vec![false; widths.len()];
    let mut total: f32 = widths.iter().sum();
    let mut order: Vec<usize> = (0..widths.len()).filter(|&k| ranks[k].is_some() && widths[k] > 0.0).collect();
    order.sort_by_key(|&k| ranks[k]);
    for k in order {
        if total <= room {
            break;
        }
        out[k] = true;
        total -= widths[k];
    }
    out
}

/// The narrow bar's plan (narrow_plan).
#[derive(Clone, Debug, PartialEq)]
struct Plan {
    /// Left side cells kept (logo, app name, menus).
    kept: Vec<bool>,
    /// Right side cells left out (optional ones, and required ones that do
    /// not fit at all).
    dropped: Vec<bool>,
    /// The desktop dots are placed.
    desks: bool,
}

/// Pure: the narrow bar's plan. `left`: the left side's widths (logo,
/// app name, menus); `right`: the right side's (with their drop `ranks`),
/// laid out from the right edge; `desks`: the desktop dots, left of them.
/// 1. Optional right cells go first (overflow).
/// 2. The rest is laid out from the right edge; a cell that does not fit
///    between the edges is not placed (no negative or overlapping rect),
///    and neither are the dots if they do not fit left of them.
/// 3. The left side keeps what fits before them, from its start: the last
///    menus go first, then the app name, then the logo too.
fn narrow_plan(left: &[f32], right: &[f32], ranks: &[Option<u8>], desks: f32, width: f32) -> Plan {
    let left_end = EDGE_PAD + left.iter().sum::<f32>() + DRAWER_GAP;
    let mut dropped = overflow(right, ranks, width - EDGE_PAD - left_end - desks);
    let mut x = width - EDGE_PAD;
    for k in (0..right.len()).rev() {
        if dropped[k] || right[k] <= 0.0 {
            continue;
        }
        if x - right[k] < EDGE_PAD {
            dropped[k] = true;
            continue;
        }
        x -= right[k];
    }
    let desks_fit = desks <= 0.0 || x - desks >= EDGE_PAD;
    if desks > 0.0 && desks_fit {
        x -= desks;
    }
    let limit = x - DRAWER_GAP;
    let mut kept = vec![true; left.len()];
    let mut lx = EDGE_PAD;
    let mut cut = false;
    for (k, &w) in left.iter().enumerate() {
        if cut || (w > 0.0 && lx + w > limit) {
            cut = true;
            kept[k] = false;
            continue;
        }
        lx += w;
    }
    Plan { kept, dropped, desks: desks_fit }
}

/// What a wheel notch on the bar does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wheel {
    /// The volume up (true) or down.
    Volume(bool),
    /// The next (true) or previous virtual desktop.
    Desktop(bool),
    /// This many desktops on (positive) or back: whole notches on empty
    /// bar space.
    Desktops(i32),
    Nothing,
}

/// Pure: the wheel (`delta`, with `acc` not yet a whole notch) over cell
/// `over` (`empty`: no cell and not the island). The volume and the dots
/// take every message as before; empty space switches desktops only with
/// topbar.wheel_desktops on, one per whole notch (touchpads add up).
/// Returns the action and the delta left over.
fn wheel_action(over: Option<Cell>, empty: bool, desktops_on_empty: bool, acc: i32, delta: i32) -> (Wheel, i32) {
    let up = delta > 0;
    match over {
        Some(Cell::Volume) => (Wheel::Volume(up), 0),
        Some(Cell::Desktops) => (Wheel::Desktop(!up), 0),
        None if empty && desktops_on_empty => {
            // Up is the previous desktop: one per whole notch.
            let (steps, rest) = crate::vdesk::notches(acc, delta);
            if steps == 0 { (Wheel::Nothing, rest) } else { (Wheel::Desktops(-steps), rest) }
        }
        _ => (Wheel::Nothing, 0),
    }
}

/// Windows hidden to the tray changed (crate::traywin): every bar's
/// entries follow.
pub fn traywin_changed() {
    if !for_each_bar(|b| b.refresh_hidden()) {
        // Busy (re-entered): the next change, or the dock's next sync, catches up.
        crate::log!("topbar: bars busy; tray entries follow with the next change");
    }
}

/// Menus and panels hang from the bar cell's reading-start corner: its
/// left, or (right to left) its right edge.
fn menu_x(r: &RECT) -> i32 {
    if crate::i18n::rtl() { r.right } else { r.left }
}

/// The corner of a menu or panel at menu_at's point: its top (bottom, for a
/// bar at the bottom: it opens upward) reading-start corner.
fn menu_align() -> crate::popup::Align {
    crate::popup::Align::hanging(at_bottom(), crate::i18n::rtl())
}

/// The calendar's corner at its point (panel_request): its far top corner
/// (bottom, for a bar at the bottom), so it lines up with the Control
/// Center at the work area's end.
fn cal_align() -> crate::popup::Align {
    crate::popup::Align::hanging(at_bottom(), !crate::i18n::rtl())
}

fn dip_x(lp: LPARAM, scale: f32) -> f32 {
    (lp.0 & 0xFFFF) as i16 as f32 / scale
}

/// How many drawer cells fit between the app menus' end (plus the gap)
/// and the chevron's left edge.
fn drawer_room(left_end: f32, chevron_left: f32, cell: f32) -> usize {
    ((chevron_left - left_end) / cell).floor().max(0.0) as usize
}

/// Which of `n` hidden icons (left to right) the drawer shows when `room`
/// fit: [start, end), the ones nearest the chevron unless scrolled by
/// `shift`; the shift comes back clamped.
fn drawer_window(n: usize, room: usize, shift: usize) -> (usize, usize, usize) {
    let take = room.min(n);
    let shift = shift.min(n - take);
    let end = n - shift;
    (end - take, end, shift)
}

/// A drop at (x, y) DIP counts only on the bar itself.
fn drop_inside(x: f32, y: f32, width: f32, height: f32) -> bool {
    x >= 0.0 && x < width && y >= 0.0 && y < height
}

fn dip_y(lp: LPARAM, scale: f32) -> f32 {
    ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / scale
}

/// Esc is held (cancels a tray drag; the bar never has the keyboard focus,
/// so it is read from the key state).
fn escape_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
    unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000 != 0 }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Its CPU counts to the top bar (desklayer::load: the stats panel's split).
    let _span = crate::desklayer::load::span(crate::desklayer::load::Part::TopBar);
    match msg {
        WM_MOUSEMOVE => {
            let held = wp.0 & 0x0001 != 0; // MK_LBUTTON
            with_bar_of(hwnd, |b| {
                let x = dip_x(lp, b.scale);
                if b.drag.is_some() {
                    b.drag_to(x);
                    return;
                }
                if held && b.pressed.is_some() {
                    b.maybe_start_drag(x);
                    if b.drag.is_some() {
                        return;
                    }
                }
                b.poke_tray_timer();
                b.on_move(x);
                b.island_move(x);
            });
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_SYSCOMMAND | WM_CLOSE => {
            let focused = with_bar_of(hwnd, |b| b.tray_focus.is_some()).unwrap_or(false);
            match bar_key_action(msg, wp.0, focused) {
                BarKeyAction::Default => return DefWindowProcW(hwnd, msg, wp, lp),
                BarKeyAction::ReleaseFocus => release_tray_focus(hwnd),
                BarKeyAction::HandleKey => tray_key(hwnd, wp.0),
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            with_bar_of(hwnd, |b| {
                b.tray_focus = None;
                if b.expanded { b.poke_tray_timer(); }
                b.layout_hover();
                b.publish_a11y();
            });
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_MBUTTONDOWN | WM_MBUTTONUP if crate::taskbar::hidden_by_us() => {
            with_bar_of(hwnd, |b| {
                if let Some(e) = b.tray_at(dip_x(lp, b.scale)) {
                    let mut at = POINT::default();
                    let _ = GetCursorPos(&mut at);
                    e.pointer_message(msg, at);
                }
            });
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // Someone else took the mouse mid-drag: put the icon back.
            if lp.0 as *mut core::ffi::c_void != hwnd.0 {
                with_bar_of(hwnd, |b| b.end_drag(false));
            }
            LRESULT(0)
        }
        WM_CANCELMODE => {
            // The system cancels modes (a dialog, a lock): nothing held.
            with_bar_of(hwnd, |b| {
                b.end_drag(false);
                b.pressed = None;
                b.press_id = None;
                b.layout_hover();
            });
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER if wp.0 == AGENDA_TIMER => {
            let _ = KillTimer(Some(hwnd), AGENDA_TIMER);
            agenda_changed();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TRAY_TIMER => {
            let _ = KillTimer(Some(hwnd), TRAY_TIMER);
            with_bar_of(hwnd, |b| {
                if b.drag.is_some() {
                    b.poke_tray_timer();
                } else if b.tray_focus.is_none() {
                    b.set_expanded(false);
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE_MSG => {
            with_bar_of(hwnd, |b| {
                b.tray_motion(None);
                b.tracking = false;
                b.hovered = None;
                b.pressed = None;
                b.drop_tip();
                b.hidden_hover(None);
                b.layout_hover();
                b.island_leave();
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // A press is the user's next command: a Start request still
            // waiting for their Shift goes (access::calls_off_start).
            access::command(None, crate::a11y::Act::Invoke);
            // The island takes its own presses.
            if with_bar_of(hwnd, |b| b.island_down(dip_x(lp, b.scale))).unwrap_or(false) {
                return LRESULT(0);
            }
            // Menu titles open on press, like a menu bar.
            let title = with_bar_of(hwnd, |b| {
                b.drop_tip();
                // Anything else on the bar folds a card that came on hover.
                b.island_event(island::model::Event::Dismiss);
                let i = b.hit(dip_x(lp, b.scale));
                match i.and_then(|i| b.title_of_slot(i)) {
                    Some(t) => Some(t),
                    None => {
                        b.pressed = i;
                        b.press_x = dip_x(lp, b.scale);
                        b.press_id = match i.and_then(|i| b.slots.get(i)).map(|s| s.cell) {
                            Some(Cell::Tray(k)) => b.tray_ids.get(k).cloned(),
                            _ => None,
                        };
                        b.poke_tray_timer();
                        b.layout_hover();
                        None
                    }
                }
            })
            .flatten();
            if let Some(t) = title {
                open_menus(hwnd, t);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == ACTION_TIMER => {
            let _ = KillTimer(Some(hwnd), ACTION_TIMER);
            with_bar_of(hwnd, |b| b.run_after());
            LRESULT(0)
        }
        crate::agenda::WM_AGENDA => {
            // New events while the calendar is open: drawn in place.
            agenda_changed();
            LRESULT(0)
        }
        WM_MENU_READY => {
            let token = wp.0 as u64;
            let built = menus::take_ready(token);
            let t = with_bar_of(hwnd, |b| match (b.waiting, built) {
                // Still wanted, nothing else open, and its app still in front.
                (Some((tok, t, target)), Some(built))
                    if tok == token
                        && !crate::popup::is_tracking()
                        && b.active.hwnd == target
                        && GetAncestor(GetForegroundWindow(), GA_ROOT).0 as isize == target =>
                {
                    b.waiting = None;
                    b.ready = Some((t, built));
                    Some(t)
                }
                (Some((tok, ..)), _) if tok == token => {
                    b.waiting = None;
                    None
                }
                _ => None,
            })
            .flatten();
            match t {
                Some(t) => open_menus(hwnd, t),
                None => {
                    with_bar_of(hwnd, |b| {
                        if b.waiting.is_none() && !crate::popup::is_tracking() {
                            b.highlight_open(None);
                        }
                    });
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // A drag ends with its drop, not a click.
            let dropped = with_bar_of(hwnd, |b| {
                let on = b.drag.is_some();
                if on {
                    // Only a drop on the bar counts; released anywhere else
                    // (pulled off the bar to cancel) puts it back.
                    let (x, y) = (dip_x(lp, b.scale), dip_y(lp, b.scale));
                    let inside = drop_inside(x, y, b.width, b.height);
                    if inside {
                        b.drag_to(x);
                    }
                    b.end_drag(inside);
                }
                on
            })
            .unwrap_or(false);
            if dropped {
                return LRESULT(0);
            }
            if with_bar_of(hwnd, |b| b.island_up(dip_x(lp, b.scale))).unwrap_or(false) {
                return LRESULT(0);
            }
            let target = with_bar_of(hwnd, |b| {
                let at = b.hit(dip_x(lp, b.scale));
                let p = b.pressed.take();
                b.layout_hover();
                if p.is_some() && p == at { p } else { None }
            })
            .flatten();
            if let Some(i) = target {
                click_slot(hwnd, i, Some(dip_x(lp, 1.0)));
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // Anything but Start's own menu again calls off a waiting
            // Start request (access::calls_off_start).
            let on = with_bar_of(hwnd, |b| b.hit(dip_x(lp, b.scale)).map(|i| b.slots[i].cell)).flatten();
            access::command(on, crate::a11y::Act::Menu);
            let hid = with_bar_of(hwnd, |b| b.hit(dip_x(lp, b.scale)).filter(|&i| b.slots[i].cell == Cell::Hidden)).flatten();
            if let Some(i) = hid {
                hidden::menu(hwnd, i);
                return LRESULT(0);
            }
            let Some(hit) = with_bar_of(hwnd, |b| {
                b.drop_tip();
                b.hit(dip_x(lp, b.scale)).map(|i| (i, b.slots[i].cell))
            }) else {
                return LRESULT(0);
            };
            match (hit, right_click(hit.map(|h| h.1))) {
                (Some((i, Cell::Companion)), _) => companion::click(hwnd, i, true, false),
                (Some((i, Cell::Desktops)), _) => open_panel(hwnd, i, panels::Panel::Desktops),
                (Some((i, Cell::Input)), _) => open_panel(hwnd, i, panels::Panel::Input),
                // Same context action as the dock's Windows button.
                (Some((_, Cell::Start)), _) => crate::keys::win_x(),
                // A tray icon: its app's own menu.
                (Some((i, Cell::Tray(_))), _) => {
                    if let Some((owner, uid, kind, at)) = with_bar_of(hwnd, |b| b.tray_target(i, true)).flatten() {
                        crate::tray::click(owner, uid, kind, at);
                    }
                }
                (_, RightClick::BarMenu) => {
                    // A card that came on hover folds first.
                    with_bar_of(hwnd, |b| b.island_event(island::model::Event::Dismiss));
                    // At the pointer along the bar, hanging off the bar
                    // like every menu (the popup gap from its edge).
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    if let Some(at) = with_bar_of(hwnd, |b| b.menu_at(&RECT { left: pt.x, right: pt.x, ..b.at })) {
                        open_bar_menu(hwnd, at);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DEVICECHANGE => {
            // Broadcast to top-level windows: drive letters came or went
            // (a volume's), for the removable storage list.
            let kind = if matches!(wp.0, 0x8000 | 0x8004) && lp.0 != 0 { (*(lp.0 as *const DEV_BROADCAST_HDR)).dbch_devicetype.0 } else { 0 };
            crate::removable::device_broadcast(wp.0, kind);
            LRESULT(1)
        }
        crate::removable::WM_REMOVABLE => {
            // Removable storage came, went or changed: the Quick settings
            // button's dot (every bar with one) and an open Quick settings.
            if !for_each_bar(|b| b.update()) {
                let _ = PostMessageW(Some(hwnd), crate::removable::WM_REMOVABLE, WPARAM(0), LPARAM(0));
            }
            control::removable_changed();
            LRESULT(0)
        }
        crate::tray::WM_TRAY_CHANGED => {
            // Every bar with the whole set shows the tray (one notice, to
            // the main bar).
            if !for_each_bar(|b| b.refresh_tray()) {
                let _ = PostMessageW(Some(hwnd), crate::tray::WM_TRAY_CHANGED, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Over the expanded tray: scroll the drawer when not all of its
            // icons fit.
            let scrolled = with_bar_of(hwnd, |b| {
                if !b.expanded || b.drawer_total <= b.drawer_room {
                    return false;
                }
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let x = (pt.x - b.at.left) as f32 / b.scale;
                let over = b.hit(x).is_some_and(|i| matches!(b.slots[i].cell, Cell::Tray(_) | Cell::Chevron));
                if !over {
                    return false;
                }
                let up = ((wp.0 >> 16) as i16) > 0;
                let max = b.drawer_total - b.drawer_room;
                b.drawer_shift = if up { (b.drawer_shift + 1).min(max) } else { b.drawer_shift.saturating_sub(1) };
                b.poke_tray_timer();
                b.layout();
                true
            })
            .unwrap_or(false);
            if scrolled {
                access::command(None, crate::a11y::Act::Invoke);
                return LRESULT(0);
            }
            let delta = (wp.0 >> 16) as i16;
            let act = with_bar_of(hwnd, |b| {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let x = (pt.x - b.at.left) as f32 / b.scale;
                let over = b.hit(x).map(|i| b.slots[i].cell);
                let empty = over.is_none() && !b.island_hit(x);
                let (a, rest) = wheel_action(over, empty, b.cfg.wheel_desktops, b.wheel_acc, delta as i32);
                b.wheel_acc = rest;
                a
            })
            .unwrap_or(Wheel::Nothing);
            if act != Wheel::Nothing {
                // The volume or the desktop changed: a command too.
                access::command(None, crate::a11y::Act::Invoke);
            }
            match act {
                Wheel::Volume(up) => status::volume_step(up),
                // Up = the previous desktop, down = the next (no wrap).
                Wheel::Desktop(next) => {
                    let s = crate::vdesk::step(next);
                    if s != crate::keys::Sent::Ok {
                        crate::log!("desktops: wheel: not sent ({s:?})");
                    }
                }
                Wheel::Desktops(n) => {
                    let s = crate::vdesk::wheel(n);
                    if s != crate::keys::Sent::Ok {
                        crate::log!("desktops: wheel: not sent ({s:?})");
                    }
                }
                Wheel::Nothing => {}
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_GETOBJECT => {
            let answer = crate::a11y::on_getobject(hwnd, wp, lp, || {
                with_bar_of(hwnd, |b| b.publish_a11y());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == crate::a11y::wm_action() => {
            for (key, act) in crate::a11y::take(hwnd) {
                access::act(hwnd, &key, act);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            crate::a11y::forget(hwnd);
            crate::tray::set_shown(hwnd.0 as isize, Vec::new());
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        island::WM_ISLAND_MEDIA | island::WM_ISLAND_PRIVACY | island::WM_ISLAND_ACT => {
            let Some(mirror) = with_bar_of(hwnd, |b| {
                b.island_message(msg, wp.0, lp.0);
                b.island_mirror()
            }) else {
                // Bars busy (re-entered): once they are free.
                let _ = PostMessageW(Some(hwnd), msg, wp, lp);
                return LRESULT(0);
            };
            // The main bar's listeners and feeds: the other displays'
            // islands show the same (and new ones start from it).
            let feed = msg != island::WM_ISLAND_ACT || wp.0 == island::ACT_REFRESH;
            if let (Some(m), true) = (mirror, feed) {
                for_each_bar(|b| b.island_follow(&m));
                island::keep_mirror(Some(m));
            }
            LRESULT(0)
        }
        WM_TIMER if matches!(wp.0, island::HOVER_TIMER | island::LEAVE_TIMER | island::NOTICE_TIMER | island::TICK_TIMER | island::PAUSE_TIMER) => {
            with_bar_of(hwnd, |b| b.island_timer(wp.0));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIP_TIMER => {
            let _ = KillTimer(Some(hwnd), TIP_TIMER);
            with_bar_of(hwnd, |b| b.tip_due());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == input::POLL_TIMER => {
            with_bar_of(hwnd, |b| b.input_tick());
            LRESULT(0)
        }
        crate::ime::WM_IME_STATUS => {
            if !for_each_bar(|b| {
                b.update();
                b.input_arm();
            }) {
                // Bars busy (re-entered): once they are free.
                let _ = PostMessageW(Some(hwnd), crate::ime::WM_IME_STATUS, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == hidden::TIP_TIMER => {
            let _ = KillTimer(Some(hwnd), hidden::TIP_TIMER);
            with_bar_of(hwnd, |b| b.hidden_tip_due());
            LRESULT(0)
        }
        crate::vdesk::WM_DESKTOPS => {
            let d = crate::vdesk::latest();
            let done = for_each_bar(|b| {
                if b.desktops != d {
                    b.desktops = d.clone();
                    b.update();
                    b.refresh_tip();
                }
            });
            if !done {
                // Bars busy (re-entered): once they are free.
                let _ = PostMessageW(Some(hwnd), crate::vdesk::WM_DESKTOPS, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        companion::WM_AGENDA => {
            let target=with_bar_of(hwnd,|b|b.slot_of(Cell::Clock)).flatten();
            if let Some(i)=target {let (y,m,d)=crate::agenda::companion_day(lp.0 as i64);open_panel(hwnd,i,panels::Panel::Calendar(y,m,d));}
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CLOCK_TIMER => {
            let _ = KillTimer(Some(hwnd), CLOCK_TIMER);
            with_bar_of(hwnd, |b| b.tick_clock());
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            with_bar_of(hwnd, |b| b.read_battery());
            // Awake again: a scheduled shutdown whose time went by in sleep
            // is dropped, not run now (shutdown.rs).
            shutdown::on_power(wp.0);
            // Back from sleep: Keep Awake's request is made again.
            control::on_power(wp.0);
            crate::agents::on_power(wp.0);
            LRESULT(1)
        }
        status::WM_STATUS_NET => {
            LAST_NET.store(wp.0, Ordering::SeqCst);
            for_each_bar(|b| {
                b.net = Some(Net::from_code(wp.0));
                b.update();
            });
            control::net_changed();
            LRESULT(0)
        }
        status::WM_STATUS_VOL => {
            LAST_VOL.store(wp.0, Ordering::SeqCst);
            for_each_bar(|b| {
                b.volume = status::unpack_volume(wp.0);
                b.update();
            });
            control::set_volume(status::unpack_volume(wp.0));
            LRESULT(0)
        }
        taskbar::WM_APPBAR => {
            match wp.0 as u32 {
                ABN_POSCHANGED => {
                    if with_bar_of(hwnd, |b| b.place()).is_none() {
                        // Bars busy (inside another bar's AppBar call): later.
                        let _ = PostMessageW(Some(hwnd), WM_BAR_RETRY, WPARAM(0), LPARAM(0));
                    }
                }
                // Fullscreen apps: the dock decides for both bars.
                _ => crate::dock::fullscreen_notice(),
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            // Time zone / locale changes; a new wallpaper (clear's ink is
            // read from it again).
            let wallpaper = wp.0 == SPI_SETDESKWALLPAPER.0 as usize;
            with_bar_of(hwnd, |b| {
                b.tick_clock();
                if wallpaper {
                    look::wallpaper_changed();
                    b.refresh_wall();
                }
            });
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        look::WM_WALL => {
            with_bar_of(hwnd, |b| b.wall_measured(lp.0 as u64));
            LRESULT(0)
        }
        crate::desklayer::backdrop::WM_BACKDROP => {
            with_bar_of(hwnd, |b| b.refresh_wall());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == WALL_TIMER => {
            let _ = KillTimer(Some(hwnd), WALL_TIMER);
            with_bar_of(hwnd, |b| b.retry_wall());
            LRESULT(0)
        }
        WM_TIMECHANGE => {
            with_bar_of(hwnd, |b| {
                b.tick_clock();
                // The focus timer's deadlines are wall-clock: its ring
                // starts again from what is left now.
                b.island_resync();
            });
            LRESULT(0)
        }
        WM_BAR_RETRY | WM_DISPLAYCHANGE | WM_DPICHANGED => {
            if msg == WM_DISPLAYCHANGE {
                // Another display moved or changed size: a wallpaper
                // spanned across them shows another part on this one
                // (clear's ink is read again; the displays' box is in the
                // key too).
                look::wallpaper_changed();
            }
            // Title rects and scale are about to change under an open menu.
            crate::popup::cancel();
            control::cancel();
            stats::cancel();
            about::cancel();
            crate::search::cancel();
            let done = with_bar_of(hwnd, |b| {
                b.place();
                if b.primary {
                    crate::tray::start(b.hwnd, b.tray_px(), b.cfg.tray);
                }
                b.surfaces.clear();
                for s in &mut b.slots {
                    s.key.clear();
                }
                b.update();
                // New scale or monitor: the island is drawn again.
                b.island_redraw();
            });
            if done.is_none() {
                let _ = PostMessageW(Some(hwnd), WM_BAR_RETRY, WPARAM(0), LPARAM(0));
            }
            if msg == WM_BAR_RETRY {
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// NopBar's tray picture (assets/brand/tray: crate::dock::own_tray_png, the
/// app icon in color, or with `line` its outline,
/// crate::dock::own_tray_line_png) in a `px` square, premultiplied BGRA,
/// filling the box every tray icon gets (crate::tray::fit: `px` less an
/// inset on each side), by how big each hand-tuned size's picture is (its
/// box; the color picture's, the outline being the same shape): one that
/// fills the box within 2 px is placed 1:1, centered (sharp at 100, 125,
/// 150, 200 and 250 %); else the smallest bigger one is shrunk into it like
/// any tray icon (crate::tray::fit::normalize); with none that big, the
/// largest is placed 1:1 (never blown up). Sizes that cannot be read are
/// skipped; None if none can.
fn own_mark(px: u32, line: bool) -> Option<Vec<u8>> {
    use crate::tray::fit;
    let room = (px as f32 - 2.0 * fit::inset_px(px)).max(1.0) as u32;
    // (side, picture in this mode, the color picture's box, its longer side)
    let sizes: Vec<_> = crate::dock::own_tray_sides()
        .into_iter()
        .filter_map(|side| {
            let color = crate::icons::decode_bytes_fit(crate::dock::own_tray_png(side).1, 1024)?;
            let b = fit::opaque_bbox(&color.pixels, color.width, color.height, fit::ALPHA_THRESHOLD)?;
            let icon = if line { crate::icons::decode_bytes_fit(crate::dock::own_tray_line_png(side).1, 1024)? } else { color };
            (icon.width == side as u32 && icon.height == side as u32).then_some((side as u32, icon.pixels, b, (b.2 - b.0).max(b.3 - b.1)))
        })
        .collect();
    let exact = sizes.iter().rev().find(|s| s.3 <= room && room - s.3 <= 2);
    let bigger = sizes.iter().find(|s| s.3 >= room);
    match (exact, bigger, sizes.last()) {
        (Some((n, pixels, b, _)), _, _) => Some(centered_1to1(pixels, *n, px, *b)),
        (None, Some((n, pixels, _, _)), _) => Some(fit::normalize(pixels, *n, px)),
        (None, None, Some((n, pixels, b, _))) => Some(centered_1to1(pixels, *n, px, *b)),
        (None, None, None) => None,
    }
}

/// Pure: `src` (an `n` px square, premultiplied BGRA) copied unscaled into
/// an `out` px square with its box `b` (x0, y0, x1, y1, end exclusive)
/// centered (left / up by half a pixel when it cannot be exact); what
/// falls outside is cut.
fn centered_1to1(src: &[u8], n: u32, out: u32, b: (u32, u32, u32, u32)) -> Vec<u8> {
    let dx = (out as i32 - (b.2 - b.0) as i32).div_euclid(2) - b.0 as i32;
    let dy = (out as i32 - (b.3 - b.1) as i32).div_euclid(2) - b.1 as i32;
    let mut px = vec![0u8; (out * out * 4) as usize];
    for y in 0..n as i32 {
        for x in 0..n as i32 {
            let (tx, ty) = (x + dx, y + dy);
            if tx < 0 || ty < 0 || tx >= out as i32 || ty >= out as i32 {
                continue;
            }
            let s = ((y * n as i32 + x) * 4) as usize;
            let d = ((ty * out as i32 + tx) * 4) as usize;
            px[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    px
}

/// Pure: premultiplied BGRA pixels of a one-color picture redrawn in
/// color `c` (its alpha ignored): each pixel's coverage (alpha) kept.
fn tint_premultiplied(pixels: &[u8], c: crate::theme::Rgba) -> Vec<u8> {
    let pre = |v: u8, a: u8| ((v as u32 * a as u32 + 127) / 255) as u8;
    pixels.chunks_exact(4).flat_map(|p| [pre(c.b, p[3]), pre(c.g, p[3]), pre(c.r, p[3]), p[3]]).collect()
}

#[cfg(test)]
mod tray_point_tests {
    use super::*;

    #[test]
    fn a_tray_click_anchors_the_popup_gap_off_the_bar() {
        for (s, gap) in [(1.0f32, 6), (1.25, 8), (1.5, 9), (2.0, 12)] {
            let h = (28.0 * s).round() as i32;
            // Bar on top, reserving its strip: the cell's middle, the gap
            // under the bar, where the bar's own menus hang.
            let bar = RECT { left: 0, top: 0, right: 2560, bottom: h };
            let cell = RECT { left: 2400, top: 0, right: 2400 + (30.0 * s) as i32, bottom: h };
            let work = RECT { left: 0, top: h, right: 2560, bottom: 1440 };
            let p = tray_click_point(cell, bar, s, false, Some(work));
            assert_eq!((p.x, p.y), ((cell.left + cell.right) / 2, h + gap), "top {s}");
            assert_eq!(p.y, hang_y(cell, bar, s, false, Some(work)), "the menus' edge");
            // Bar at the bottom: the gap over it.
            let bar_b = RECT { left: 0, top: 1440 - h, right: 2560, bottom: 1440 };
            let cell_b = RECT { top: bar_b.top, bottom: bar_b.bottom, ..cell };
            let work_b = RECT { left: 0, top: 0, right: 2560, bottom: 1440 - h };
            assert_eq!(tray_click_point(cell_b, bar_b, s, true, Some(work_b)).y, 1440 - h - gap, "bottom {s}");
            // No work area known: off the bar itself.
            assert_eq!(tray_click_point(cell, bar, s, false, None).y, h + gap);
            assert_eq!(tray_click_point(cell_b, bar_b, s, true, None).y, 1440 - h - gap);
        }
    }

    #[test]
    fn a_click_on_our_tray_cell_is_found_by_its_point() {
        for s in [1.0f32, 1.25, 1.5, 2.0] {
            let h = (28.0 * s).round() as i32;
            let bar = RECT { left: 0, top: 0, right: 1920, bottom: h };
            let cell = RECT { left: 1700, top: 0, right: 1730, bottom: h };
            let work = RECT { left: 0, top: h, right: 1920, bottom: 1080 };
            // Top bar: the point is the gap under the cell; bottom bar: over it.
            let top = tray_click_point(cell, bar, s, false, Some(work));
            assert!(cell_anchors(cell, top, top), "top {s}");
            let bar_b = RECT { left: 0, top: 1080 - h, right: 1920, bottom: 1080 };
            let cell_b = RECT { top: bar_b.top, bottom: bar_b.bottom, ..cell };
            let work_b = RECT { left: 0, top: 0, right: 1920, bottom: 1080 - h };
            let bottom = tray_click_point(cell_b, bar_b, s, true, Some(work_b));
            assert!(cell_anchors(cell_b, bottom, bottom), "bottom {s}");
            // The next cell over, or Windows' tray far below: not this cell.
            let next = RECT { left: 1730, top: 0, right: 1760, bottom: h };
            assert!(!cell_anchors(next, tray_click_point(next, bar, s, false, Some(work)), top));
            assert!(!cell_anchors(cell, top, POINT { x: 1730, y: 10 }), "right edge belongs to the next cell");
            assert!(!cell_anchors(cell, top, POINT { x: 1715, y: 1060 }));
            assert!(!cell_anchors(cell, top, POINT { x: 1715, y: top.y + 1 }), "past the gap");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn win_b_walks_on_to_the_status_cells_like_windows() {
        use super::{status_stop, Cell};
        for c in [Cell::Net, Cell::Volume, Cell::Battery, Cell::Control, Cell::Clock, Cell::Companion, Cell::Input, Cell::Hidden, Cell::Search] {
            assert!(status_stop(c), "{c:?}: right of the tray, reachable with the arrows");
        }
        for c in [Cell::Start, Cell::App, Cell::Menu(0), Cell::Desktops, Cell::Tray(0), Cell::Chevron, Cell::TrayButton] {
            assert!(!status_stop(c), "{c:?}: not a status cell (the tray cells are taken first)");
        }
    }

    #[test]
    fn tray_keyboard_close_releases_focus_and_owner_can_still_destroy() {
        use super::{bar_key_action, BarKeyAction::*, WM_CLOSE, WM_DESTROY, WM_NCDESTROY, WM_KEYDOWN, WM_SYSKEYDOWN, WM_SYSCOMMAND, SC_CLOSE, SC_MINIMIZE};
        for focused in [true, false] {
            assert_eq!(bar_key_action(WM_CLOSE, 0, focused), ReleaseFocus);
            assert_eq!(bar_key_action(WM_SYSCOMMAND, SC_CLOSE as usize | 3, focused), ReleaseFocus);
            for msg in [WM_DESTROY, WM_NCDESTROY] { assert_eq!(bar_key_action(msg, 0, focused), Default); }
            assert_eq!(bar_key_action(WM_SYSCOMMAND, SC_MINIMIZE as usize, focused), Default);
        }
        assert_eq!(bar_key_action(WM_SYSKEYDOWN, 0x73, true), ReleaseFocus);
        assert_eq!(bar_key_action(WM_KEYDOWN, 0x1b, true), ReleaseFocus);
        assert_eq!(bar_key_action(WM_SYSKEYDOWN, 0x28, true), HandleKey);
        assert_eq!(bar_key_action(WM_SYSKEYDOWN, 0x28, false), Default);
    }

    use super::*;

    #[test]
    fn status_cells_follow_the_switches() {
        use Cell::*;
        assert_eq!(status_cells(true, false, false, true), vec![Battery, Search, Control]);
        assert_eq!(status_cells(true, true, true, true), vec![Net, Volume, Battery, Search, Control]);
        assert_eq!(status_cells(true, false, false, false), vec![Search, Control]);
        assert_eq!(status_cells(false, false, false, true), vec![Net, Volume, Battery]);
        assert_eq!(status_cells(false, false, false, false), vec![Net, Volume]);
    }

    #[test]
    fn the_bar_text_is_body_13_and_app_name_is_regular() {
        // The ramp's Body (design B2: 12 -> 13) for Graphite's bar.
        assert_eq!(crate::theme::Theme::graphite().font.size, crate::theme::ramp::BODY.size);
        let src = include_str!("mod.rs");
        let body = &src[..src.rfind(concat!("mod ", "tests {")).unwrap_or(src.len())];
        assert!(body.contains("Cell::App => (Style::Text, self.app.clone())"));
        assert!(!body.contains("DWRITE_FONT_WEIGHT_BOLD"));
        for (name, src) in [("input.rs", include_str!("input.rs")), ("hidden.rs", include_str!("hidden.rs")), ("island/mod.rs", include_str!("island/mod.rs")), ("island/card.rs", include_str!("island/card.rs"))] {
            let body = &src[..src.rfind(concat!("mod ", "tests {")).unwrap_or(src.len())];
            assert!(!body.contains("DWRITE_FONT_WEIGHT_BOLD"), "{name}");
        }
        // The small texts are the ramp's (no literal 10 DIP text).
        assert_eq!((BATTERY_TEXT, input::LABEL_SIZE), (crate::theme::ramp::CAPTION.size, crate::theme::ramp::CAPTION.size));
    }

    #[test]
    fn every_text_on_the_bar_stands_on_its_one_baseline() {
        // The topbar-text review's P2: the drawn cells (the hidden count
        // among them) built their own baseline per size and box. They
        // are drawn as tall as the bar now and stand on line_px's baseline;
        // the island's pill has one line for all of its text.
        for (name, src) in [("hidden.rs", include_str!("hidden.rs")), ("island/mod.rs", include_str!("island/mod.rs"))] {
            let body = &src[..src.rfind(concat!("mod ", "tests {")).unwrap_or(src.len())];
            assert!(!body.contains(".text_y("), "{name} still centers its own text");
            assert!(!body.contains("pill_line(TEXT_SIZE)") && !body.contains("pill_line(size)"), "{name}: one pill line");
        }
        let src = include_str!("hidden.rs");
        assert!(src.contains("self.on_bar_line(") && src.contains("self.line_px().box_h"), "hidden.rs");
        // A surface as tall as the bar sits at the bar's top at every
        // scale, so a baseline drawn in it is the bar's baseline.
        for scale in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            for bar in [20.0f32, 24.0, 28.0, 32.0, 36.0, 48.0] {
                let box_px = (bar * scale).round();
                assert_eq!(baseline::cell_top(bar, box_px / scale, scale), 0.0, "{bar} at {scale}");
            }
        }
    }

    #[test]
    fn cells_land_on_device_pixels_across_too() {
        // The topbar-text review's P2: a cell's x was a whole DIP (11 DIP
        // is 13.75 px at 125 %). It is a whole pixel at every scale now.
        for scale in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            for x in [11.0f32, 12.3, 57.5, 300.0, 1234.4] {
                let v = baseline::snap_px(x, scale) * scale;
                assert!((v - v.round()).abs() < 1e-3 && (v / scale - x).abs() <= 0.5 / scale + 1e-4, "{x} at {scale}");
            }
        }
        // The source places cells with it.
        let src = include_str!("mod.rs");
        assert!(src.contains("baseline::snap_px(left, scale)"));
        assert!(!src.contains(concat!("(left.", "round(), baseline::cell_top")));
    }

    #[test]
    fn the_cells_fill_is_a_soft_rectangle_inside_the_bar() {
        // The Windows top bar: on the 26 bar 2 in, 22 tall, corners 4
        // (not a capsule); the island's pill is a hair slimmer (20).
        assert_eq!(hover_pill(26.0), (2.0, 22.0, 4.0));
        assert_eq!(hover_pill(32.0), (2.0, 28.0, 4.0));
        assert_eq!(island::model::pill_height(26.0), 20.0);
        for bar in [20.0f32, 22.0, 24.0, 26.0, 28.0, 32.0, 36.0, 40.0, 48.0] {
            let (inset, h, r) = hover_pill(bar);
            assert_eq!(h + 2.0 * inset, bar, "centered in the bar");
            assert_eq!(h, bar - 4.0);
            assert_eq!(r, MARK_RADIUS.min(crate::theme::capsule(h)));
            assert_eq!(inset, crate::theme::metrics::size::BAR_INSET);
            assert!(island::model::pill_height(bar) <= h, "the pill never taller than a cell's fill");
        }
        // No inset of its own is left.
        let src = include_str!("mod.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        assert!(body.contains("let inset = crate::theme::metrics::size::BAR_INSET;"));
    }

    #[test]
    fn the_bar_is_material_m0_without_light() {
        use crate::syslook::{HighContrast, SysLook};
        use crate::theme::{Finish, Rgba};
        let grey = |v: u8, a: u8| Rgba { r: v, g: v, b: v, a };
        for (light, flat, glass) in [(false, 0x1D, 0x1E), (true, 0xE6, 0xF5)] {
            let sys = SysLook { light, ..Default::default() };
            // Flat (v2): its mean grey opaque, no backdrop (the band falls
            // from 1E to 1C, E7 to E4: look::Ground::fall).
            let t = Theme::graphite().resolve_fx(&sys, true);
            assert_eq!(bar_ground(&t), Ground { tint: grey(flat, 255), backdrop: false, edge: None }, "flat, light {light}");
            // A glass: the same step of grey at its glass's bar opacity
            // over the blur.
            for f in [Finish::Glass] {
                let mut g = Theme::graphite();
                g.finish = f;
                let t = g.resolve_fx(&sys, true);
                let a = t.coat().unwrap().alpha[crate::theme::Material::Bar as usize];
                assert!((0.25..0.65).contains(&a), "{f:?}");
                assert_eq!(bar_ground(&t), Ground { tint: grey(glass, (a * 255.0).round() as u8), backdrop: true, edge: None }, "{f:?}, light {light}");
            }
        }
        // High contrast: Window, opaque, and a 1 DIP WindowText line along
        // the edge towards the desktop only (12.3).
        let (white, black) = (grey(255, 255), grey(0, 255));
        let hc = HighContrast { window: black, text: white, highlight: Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 }, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT };
        let t = Theme::graphite().resolve_fx(&SysLook { high_contrast: Some(hc), ..Default::default() }, true);
        assert_eq!(bar_ground(&t), Ground { tint: black, backdrop: false, edge: Some((white, 1.0)) });
        assert_eq!((edge_y(32.0, 1.0, false), edge_y(32.0, 1.0, true)), (31.0, 0.0));
    }

    #[test]
    fn the_menus_of_a_bar_are_a_ring() {
        use crate::popup::Outcome;
        let ring = [Ring::System, Ring::Title(0), Ring::Title(1), Ring::Title(2)];
        // Each hangs from its own cell.
        assert_eq!(ring.map(Ring::cell), [Cell::Start, Cell::App, Cell::Menu(0), Cell::Menu(1)]);
        // The pointer slid onto another cell of the ring: that menu.
        assert_eq!(ring_next(&ring, Ring::Title(0), Outcome::Switch(2)), Some(Ring::Title(1)));
        assert_eq!(ring_next(&ring, Ring::Title(1), Outcome::Switch(0)), Some(Ring::System));
        assert_eq!(ring_next(&ring, Ring::System, Outcome::Switch(9)), None);
        // The arrows: the next and the previous, round the ring.
        assert_eq!(ring_next(&ring, Ring::System, Outcome::Neighbor(1)), Some(Ring::Title(0)));
        assert_eq!(ring_next(&ring, Ring::Title(2), Outcome::Neighbor(1)), Some(Ring::System));
        assert_eq!(ring_next(&ring, Ring::System, Outcome::Neighbor(-1)), Some(Ring::Title(2)));
        assert_eq!(ring_next(&ring, Ring::Title(1), Outcome::Neighbor(-1)), Some(Ring::Title(0)));
        // A bar too narrow for its logo: the titles alone.
        assert_eq!(ring_next(&ring[1..], Ring::Title(0), Outcome::Neighbor(-1)), Some(Ring::Title(2)));
        // Anything else closes for good.
        for out in [Outcome::Picked(3), Outcome::Cancelled, Outcome::Lost, Outcome::Dragged(1)] {
            assert_eq!(ring_next(&ring, Ring::Title(0), out), None, "{out:?}");
        }
        // Every hand-over is one the popup crosses (scrubbing), and going
        // on to another cell is what capsule::scrub calls a scrub.
        assert!(crate::popup::swaps(Outcome::Switch(1)) && crate::popup::swaps(Outcome::Neighbor(1)));
        assert!(capsule::scrub(Some(Ring::System.cell()), Ring::Title(0).cell()));
    }

    #[test]
    fn what_slides_and_what_can_be_shown_in_another_color() {
        // The tray and the dots beside it glide and fade; nothing else.
        for c in [Cell::Tray(0), Cell::Chevron, Cell::Desktops] {
            assert!(slides(c), "{c:?}");
        }
        for c in [Cell::Start, Cell::App, Cell::Menu(0), Cell::Clock, Cell::Battery, Cell::Control, Cell::Input, Cell::Hidden] {
            assert!(!slides(c), "{c:?}");
        }
        // One-color content takes high contrast's HighlightText as it is;
        // an app's own picture and the two-color battery stay as drawn.
        for c in [Cell::Start, Cell::App, Cell::Menu(2), Cell::Clock, Cell::Search, Cell::Control, Cell::Net, Cell::Volume, Cell::Desktops, Cell::Input, Cell::Chevron, Cell::TrayButton] {
            assert!(one_color(c), "{c:?}");
        }
        for c in [Cell::Tray(1), Cell::Hidden, Cell::Battery] {
            assert!(!one_color(c), "{c:?}");
        }
        // Other apps' tray pictures: a white or black glyph is one color
        // (shown in the bar's ink where it would vanish); a colored logo,
        // a mid grey or a near-empty picture is not.
        let pic = |bgra: [u8; 4], n: usize| [bgra, [0, 0, 0, 0]].iter().flat_map(|p| p.repeat(n)).collect::<Vec<u8>>();
        assert_eq!(glyph_lightness(&pic([240, 240, 240, 255], 200)), Some(true));
        assert_eq!(glyph_lightness(&pic([120, 120, 120, 128], 200)), Some(true), "premultiplied half-alpha white");
        assert_eq!(glyph_lightness(&pic([20, 20, 22, 255], 200)), Some(false));
        assert_eq!(glyph_lightness(&pic([40, 60, 220, 255], 200)), None, "red logo");
        assert_eq!(glyph_lightness(&pic([128, 128, 128, 255], 200)), None, "mid grey");
        assert_eq!(glyph_lightness(&pic([255, 255, 255, 255], 5)), None, "too few solid pixels");
        let mut mixed = pic([240, 240, 240, 255], 200);
        mixed.extend([40u8, 60, 220, 255].repeat(30));
        assert_eq!(glyph_lightness(&mixed), None, "white with a colored badge");
        // The bar's motion asks the kit: no length or curve of its own
        // (the whole file up to its tests at the end).
        let src = include_str!("mod.rs").replace('\r', "");
        let body = src.split("#[cfg(test)]\nmod tray_point_tests").next().unwrap();
        for banned in ["SLIDE_MS", "fade_visual(", "(\"Offset\",", "(\"Opacity\",", ", 120)", "_MS: i64 ="] {
            assert!(!body.contains(banned), "{banned}");
        }
    }

    #[test]
    fn cells_and_the_battery_sit_on_the_grid() {
        use crate::theme::metrics::space;
        // The Windows top bar (the mock-up's .bar.mac): ends 8, a
        // menu's sides 9, the app's name 6 after the logo, status icons
        // 15 apart, the clock 10 before and 6 after its text.
        assert_eq!((EDGE_PAD, CELL_PAD, APP_LEAD, STATUS_GAP, CLOCK_LEAD, CLOCK_TAIL), (8.0, 9.0, 6.0, 15.0, 10.0, 6.0));
        assert_eq!((DRAWER_GAP, DRAG_THRESHOLD), (space::S3, space::S2));
        // Glyphs and the logo one 16 box, tray icons 18; an icon cell is a
        // glyph and its gap.
        assert_eq!((ICON_CELL, GLYPH_BOX, GLYPH_SIZE, TRAY_DIP, START_PX), (31.0, 16.0, 16.0, 18.0, 16.0));
        // Widths: a menu its text and 9 a side; the app's name 6 and 9;
        // status icons 31 apart whatever their pads; the clock 10 + 6.
        assert_eq!(cell_width(Cell::Menu(0), 26.0), 44.0);
        assert_eq!(cell_width(Cell::App, 40.0), 55.0);
        assert_eq!(cell_width(Cell::Start, START_PX), 34.0);
        assert_eq!(cell_width(Cell::Tray(0), TRAY_DIP), 33.0);
        assert_eq!((cell_width(Cell::Search, 16.0), cell_width(Cell::Tray(0), 16.0), cell_width(Cell::Chevron, 16.0)), (31.0, 31.0, 31.0));
        assert_eq!(cell_width(Cell::Battery, BATTERY_W), BATTERY_W + STATUS_GAP);
        assert_eq!(cell_width(Cell::Clock, 120.0), 136.0);
        assert_eq!(cell_width(Cell::Clock, 0.0), 0.0, "nothing shown: no cell");
        // The content sits off center by half its pads' difference: the
        // app's name 1.5 towards the logo, the clock 2 towards its end;
        // mirrored right to left.
        assert_eq!(content_x(Cell::App, 0.0, 55.0, 40.0, false), 6.0);
        assert_eq!(content_x(Cell::Clock, 0.0, 136.0, 120.0, false), 10.0);
        assert_eq!(content_x(Cell::Clock, 0.0, 136.0, 120.0, true), 6.0);
        assert_eq!(content_x(Cell::Search, 0.0, 31.0, 16.0, false), 7.5);
        // Marks (the chevron, the battery's bolt) are the kit's 12; no 9,
        // 10, 13, 14 or 18 DIP glyph is left (design system 5).
        use crate::theme::metrics::size;
        assert_eq!((GLYPH_SIZE, CHEVRON_SIZE, BATTERY_BOLT), (size::ICON_S, size::ICON_XS, 9.0));
        // The battery's body: a small mark's corners, not a capsule.
        assert_eq!(battery_radius(&Theme::graphite()), 4.0);
        // The battery in whole DIPs (it was 0.5 / 24.3 / 4.8 ...).
        let whole = |v: f32| v.fract() == 0.0;
        let (bl, bt, br, bb) = BATTERY_BODY;
        let (nl, nt, nr, nb) = BATTERY_NUB;
        assert!([BATTERY_W, BATTERY_H, bl, bt, br, bb, nl, nt, nr, nb].iter().all(|v| whole(*v)));
        assert!(nr <= BATTERY_W && bb <= BATTERY_H && nl > br);
        assert_eq!(((bt + bb) / 2.0, (nt + nb) / 2.0), (BATTERY_H / 2.0, BATTERY_H / 2.0), "body and terminal centered");
        // Final audit 8: the edge never cuts the number; past its ink
        // to the nearer side, elsewhere where the level says.
        assert_eq!(clear_of_ink(20.0, 30.0, 40.0), 20.0);
        assert_eq!(clear_of_ink(50.0, 30.0, 40.0), 50.0);
        assert_eq!(clear_of_ink(32.0, 30.0, 40.0), 29.0);
        assert_eq!(clear_of_ink(38.0, 30.0, 40.0), 41.0);
        assert_eq!(clear_of_ink(39.6, 30.2, 40.4), 42.0);
        // Design B2 review P3 #2: the level fills the drawn body - full at
        // 100 % (its right edge), half at 50 % - at every scale.
        assert_eq!(br - bl, status::BATTERY_BODY, "one body width");
        for s in [1.0f32, 1.25, 1.5, 2.0, 2.5] {
            let at = |p: u8| battery_edge(status::battery_look(p, false, false).0, s);
            assert_eq!(at(100), (br * s).round(), "scale {s}: 100 % reaches the end");
            assert_eq!(at(50), (((bl + br) / 2.0) * s).round(), "scale {s}: 50 % the middle");
            assert_eq!(at(0), (bl * s).round(), "empty");
        }
    }

    /// Final audit 8: the battery's number stands in one color whatever
    /// the level: the fill's edge (as drawn) is never inside its ink, and
    /// the number's ink is centered on the body within a pixel.
    #[test]
    fn the_battery_number_is_never_cut_by_the_fill() {
        let stage = crate::gfx::promo_kit::stage();
        let t = Theme::graphite().resolve(&crate::syslook::SysLook { light: true, ..Default::default() });
        let ink = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.86 };
        for s in [1.0f32, 1.25, 1.5, 2.0] {
            for p in 1..100u8 {
                let a = battery_art(&stage.gfx, &t, ink, p, false, s);
                let (at, edge) = battery_places(&a, 0.0, 0.0);
                let (l, _, _) = a.label.as_ref().expect("a number");
                let (x0, x1) = ink_span(l, at.unwrap().X);
                assert!(edge <= x0 || edge >= x1, "{p} % at {s}: edge {edge} in the number {x0}..{x1}");
                assert_eq!(edge.fract(), 0.0, "whole pixels");
                // Within a pixel of where the level says (or just past the
                // number).
                let want = battery_edge(a.level, s);
                assert!((edge - want).abs() <= 1.0 || edge == (x0 - 1.0).floor() || edge == (x1 + 1.0).ceil(), "{p} % at {s}");
                let (bl, _, br, _) = BATTERY_BODY;
                assert!((((x0 + x1) / 2.0) - (bl + br) / 2.0 * s).abs() <= 1.0, "{p} % at {s}: centered");
            }
        }
    }

    /// Invoke on the Start button twice (a screen reader) closes the
    /// system menu, as a second click does; other cells and panels do not.
    #[test]
    fn a_second_invoke_on_the_same_button_closes_its_panel() {
        use panels::Panel;
        let d = std::mem::discriminant;
        let start = Some((7, Cell::Start, d(&Panel::System)));
        assert!(repeats(start, start));
        assert!(!repeats(None, start), "nothing open: it opens");
        assert!(!repeats(start, None), "no panel asked for");
        assert!(!repeats(start, Some((8, Cell::Start, d(&Panel::System)))), "another bar's Start");
        assert!(!repeats(start, Some((7, Cell::Desktops, d(&Panel::Desktops)))), "another cell");
        // The calendar stays the calendar whatever month it shows.
        let cal = |m| Some((7, Cell::Clock, d(&Panel::Calendar(2026, m, 0))));
        assert!(repeats(cal(11), cal(12)));
    }

    #[test]
    fn calendar_months_and_days_change_the_open_popup() {
        use panels::{Action, Panel};
        // Month buttons and days: the next calendar, shown in place (the
        // popup stays; popup::OnPick).
        assert_eq!(calendar_step(Some(&Action::Month(2026, 12))), Some(Panel::Calendar(2026, 12, 0)));
        assert_eq!(calendar_step(Some(&Action::Day(2026, 11, 7))), Some(Panel::Calendar(2026, 11, 7)));
        // Anything else closes the popup and runs.
        assert_eq!(calendar_step(Some(&Action::Notifications)), None);
        assert_eq!(calendar_step(None), None);
        // Every month button and day of a built calendar steps in place
        // (found by what they do: labels follow the language, which other
        // tests change meanwhile).
        let c = panels::build(Panel::Calendar(2026, 11, 0), None, f32::INFINITY);
        // The month header's ‹ › (and title) and the grid's days.
        let ids = c.items.iter().flat_map(|i| i.nav.iter().flat_map(|n| [n.prev, n.next, n.today]).chain(i.cell_ids.iter().copied()).chain([i.id]));
        let steps: Vec<Panel> = ids.filter_map(|id| calendar_step(c.action(id))).collect();
        assert!(steps.contains(&Panel::Calendar(2026, 12, 0)) && steps.contains(&Panel::Calendar(2026, 10, 0)), "{steps:?}");
    }

    #[test]
    fn our_tray_mark_takes_the_bars_text_color() {
        let c = |r, g, b| crate::theme::Rgba { r, g, b, a: 255 };
        // The black outline (high contrast) on a dark bar: white, its soft
        // edge kept (premultiplied).
        let black = [0, 0, 0, 255, 0, 0, 0, 128, 0, 0, 0, 0];
        assert_eq!(tint_premultiplied(&black, c(255, 255, 255)), vec![255, 255, 255, 255, 128, 128, 128, 128, 0, 0, 0, 0]);
        // A white picture on a light bar: the bar's ink.
        let white = [255, 255, 255, 255];
        assert_eq!(tint_premultiplied(&white, c(0x27, 0x31, 0x3B)), vec![0x3B, 0x31, 0x27, 255]);
    }

    #[test]
    fn a_picture_placed_one_to_one_keeps_its_pixels_centered() {
        // A 4 px square holding a 2 x 1 box at (1, 2): into 6 px, the box
        // lands at (2, 2) (centered), each pixel unchanged.
        let mut src = vec![0u8; 4 * 4 * 4];
        for (x, y, v) in [(1usize, 2usize, 10u8), (2, 2, 20)] {
            src[(y * 4 + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
        }
        let out = centered_1to1(&src, 4, 6, (1, 2, 3, 3));
        let at = |x: usize, y: usize| out[(y * 6 + x) * 4..][..4].to_vec();
        assert_eq!((at(2, 2), at(3, 2)), (vec![10, 10, 10, 255], vec![20, 20, 20, 255]));
        assert_eq!(out.as_chunks::<4>().0.iter().filter(|p| p[3] != 0).count(), 2);
        // Bigger than the square: cut, never out of bounds.
        assert_eq!(centered_1to1(&src, 4, 2, (0, 0, 4, 4)).len(), 2 * 2 * 4);
    }

    #[test]
    fn our_bar_picture_is_sharp_at_the_common_scales() {
        // At 100, 125, 150, 200 and 250 % the bar's tray box (TRAY_DIP less
        // the inset) matches a hand-tuned size: its pixels, unscaled, every
        // byte, its box centered in the square (within half a pixel).
        use crate::tray::fit::{opaque_bbox, ALPHA_THRESHOLD};
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
        let centered = |b: (u32, u32, u32, u32), px: u32, what: &str| {
            assert!((b.0 as i32 - (px - b.2) as i32).abs() <= 1, "{what}: across {b:?} in {px}");
            assert!((b.1 as i32 - (px - b.3) as i32).abs() <= 1, "{what}: down {b:?} in {px}");
        };
        for (scale, side) in [(1.0f32, 16u32), (1.25, 20), (1.5, 24), (2.0, 32), (2.5, 40)] {
            let px = (TRAY_DIP * scale).round() as u32;
            for line in [false, true] {
                let what = format!("{scale} line {line}");
                let got = own_mark(px, line).expect("decodes");
                let png = if line { crate::dock::own_tray_line_png(side as i32) } else { crate::dock::own_tray_png(side as i32) };
                let src = crate::icons::decode_bytes_fit(png.1, 1024).unwrap();
                assert_eq!((src.width, src.height), (side, side));
                let color = crate::icons::decode_bytes_fit(crate::dock::own_tray_png(side as i32).1, 1024).unwrap();
                let sb = opaque_bbox(&color.pixels, side, side, ALPHA_THRESHOLD).unwrap();
                // The color picture's box moved by (dx, dy): the source's
                // pixels there, byte for byte, nothing anywhere else.
                let dx = (px as i32 - (sb.2 - sb.0) as i32).div_euclid(2) - sb.0 as i32;
                let dy = (px as i32 - (sb.3 - sb.1) as i32).div_euclid(2) - sb.1 as i32;
                for y in 0..px as i32 {
                    for x in 0..px as i32 {
                        let (sx, sy) = (x - dx, y - dy);
                        let want: &[u8] = if (0..side as i32).contains(&sx) && (0..side as i32).contains(&sy) {
                            &src.pixels[((sy * side as i32 + sx) * 4) as usize..][..4]
                        } else {
                            &[0, 0, 0, 0]
                        };
                        assert_eq!(&got[((y * px as i32 + x) * 4) as usize..][..4], want, "{what}: at {x}, {y}");
                    }
                }
                if !line {
                    centered(opaque_bbox(&got, px, px, ALPHA_THRESHOLD).unwrap(), px, &what);
                }
            }
        }
        // In between (175 %): shrunk to fill the box, centered.
        let px = (TRAY_DIP * 1.75).round() as u32;
        let room = px - 2 * crate::tray::fit::inset_px(px) as u32;
        let b = opaque_bbox(&own_mark(px, false).expect("decodes"), px, px, ALPHA_THRESHOLD).unwrap();
        assert!((room - 1..=room).contains(&(b.2 - b.0).max(b.3 - b.1)), "175: {b:?} in {room}");
        centered(b, px, "175");
    }

    #[test]
    fn the_wheel_on_empty_space_switches_desktops_only_when_asked() {
        // Off (the default): empty space does nothing.
        assert_eq!(wheel_action(None, true, false, 0, 120), (Wheel::Nothing, 0));
        // On: one desktop per whole notch; up is the previous one.
        assert_eq!(wheel_action(None, true, true, 0, 120), (Wheel::Desktops(-1), 0));
        assert_eq!(wheel_action(None, true, true, 0, -120), (Wheel::Desktops(1), 0));
        // Several notches in one message: as many desktops.
        assert_eq!(wheel_action(None, true, true, 0, -240), (Wheel::Desktops(2), 0));
        assert_eq!(wheel_action(None, true, true, 0, 360), (Wheel::Desktops(-3), 0));
        assert_eq!(wheel_action(None, true, true, 30, 250), (Wheel::Desktops(-2), 40));
        // Touchpad deltas add up.
        assert_eq!(wheel_action(None, true, true, 0, 40), (Wheel::Nothing, 40));
        assert_eq!(wheel_action(None, true, true, 80, 40), (Wheel::Desktops(-1), 0));
        // The island is not empty space; other cells keep theirs.
        assert_eq!(wheel_action(None, false, true, 0, 120), (Wheel::Nothing, 0));
        assert_eq!(wheel_action(Some(Cell::Clock), false, true, 0, 120), (Wheel::Nothing, 0));
        assert_eq!(wheel_action(Some(Cell::Volume), false, true, 0, -120), (Wheel::Volume(false), 0));
        assert_eq!(wheel_action(Some(Cell::Desktops), false, false, 0, 120), (Wheel::Desktop(false), 0));
    }

    #[test]
    fn a_very_narrow_bar_cuts_its_menus_before_the_required_cells() {
        // 632 DIP: logo 32, app name 80, menus 120 + 120 + 140; right: a
        // monitor 140 (optional), Control 32, the tray cell 32, clock 60.
        let left = [32.0, 80.0, 120.0, 120.0, 140.0];
        let right = [140.0, 32.0, 32.0, 60.0];
        let ranks = [Some(0), None, None, None];
        let Plan { kept, dropped, .. } = narrow_plan(&left, &right, &ranks, 0.0, 632.0);
        assert_eq!(dropped, vec![true, false, false, false], "the input indicator goes first");
        assert_eq!(kept, vec![true, true, true, true, false], "then the last menu");
        // Nothing on the left reaches the right side's cells.
        let left_end = EDGE_PAD + left.iter().zip(&kept).filter(|(_, k)| **k).map(|(w, _)| w).sum::<f32>();
        let right_start = 632.0 - EDGE_PAD - (32.0 + 32.0 + 60.0);
        assert!(left_end + DRAWER_GAP <= right_start, "{left_end} {right_start}");
        // Tiny: every menu and the name go, the logo and the required cells stay.
        let Plan { kept, dropped, .. } = narrow_plan(&left, &right, &ranks, 0.0, 200.0);
        assert_eq!(kept, vec![true, false, false, false, false]);
        assert!(!dropped[1] && !dropped[2] && !dropped[3]);
        // Wide: nothing cut.
        let Plan { kept, dropped, desks } = narrow_plan(&left, &right, &ranks, 30.0, 2000.0);
        assert!(kept.iter().all(|k| *k) && dropped.iter().all(|d| !d) && desks);
    }

    /// The rects a plan places (right side from the right edge, then the
    /// left side from the left edge), for the checks below.
    fn placed(left: &[f32], right: &[f32], plan: &Plan, width: f32) -> Vec<(f32, f32)> {
        let mut out = Vec::new();
        let mut x = width - EDGE_PAD;
        for k in (0..right.len()).rev() {
            if !plan.dropped[k] && right[k] > 0.0 {
                x -= right[k];
                out.push((x, x + right[k]));
            }
        }
        let mut lx = EDGE_PAD;
        for (k, &w) in left.iter().enumerate() {
            if plan.kept[k] && w > 0.0 {
                out.push((lx, lx + w));
                lx += w;
            }
        }
        out
    }

    fn on_bar_and_apart(rects: &[(f32, f32)], width: f32) {
        for (i, a) in rects.iter().enumerate() {
            assert!(a.0 >= EDGE_PAD && a.1 <= width - EDGE_PAD, "{a:?} off the bar ({width})");
            for b in &rects[i + 1..] {
                assert!(a.1 <= b.0 || b.1 <= a.0, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn required_cells_that_cannot_fit_are_not_placed_and_the_logo_goes_first() {
        // A long clock (140) with the tray cell and Quick settings (32 each).
        let left = [32.0, 80.0];
        let right = [32.0, 32.0, 140.0];
        let ranks = [None, None, None];
        // 200 DIP: the clock and the tray cell fit, Quick settings does
        // not, and the logo gives way.
        let p = narrow_plan(&left, &right, &ranks, 0.0, 200.0);
        assert_eq!(p.dropped, vec![true, false, false]);
        assert_eq!(p.kept, vec![false, false], "the logo goes too");
        on_bar_and_apart(&placed(&left, &right, &p, 200.0), 200.0);
        // 100 DIP: the clock cannot fit at all; the tray cell and Control
        // Center take the room.
        let p = narrow_plan(&left, &right, &ranks, 0.0, 100.0);
        assert_eq!(p.dropped, vec![false, false, true]);
        on_bar_and_apart(&placed(&left, &right, &p, 100.0), 100.0);
        // 30 DIP: nothing fits; nothing is placed (no negative rect).
        let p = narrow_plan(&left, &right, &ranks, 20.0, 30.0);
        assert!(p.dropped.iter().all(|d| *d) && p.kept.iter().all(|k| !k) && !p.desks);
        assert!(placed(&left, &right, &p, 30.0).is_empty());
    }

    #[test]
    fn optional_cells_make_room_so_the_tray_cell_always_fits() {
        // [input 120, search 30, hidden 30, control 30, clock 60].
        let w = [120.0, 30.0, 30.0, 30.0, 60.0];
        let r = [Some(0), Some(1), None, None, None];
        assert_eq!(overflow(&w, &r, 400.0), vec![false; 5], "room for all");
        assert_eq!(overflow(&w, &r, 200.0), vec![true, false, false, false, false], "the monitor goes first");
        assert_eq!(overflow(&w, &r, 125.0), vec![true, true, false, false, false], "then search");
        // Never the tray cell or the clock, however narrow.
        let d = overflow(&w, &r, 10.0);
        assert!(!d[2] && !d[3] && !d[4]);
        // Hidden cells (width 0) are not counted.
        assert_eq!(overflow(&[0.0, 30.0], &[Some(0), None], 30.0), vec![false, false]);
    }

    #[test]
    fn drawer_never_reaches_the_menus() {
        // Menus end at 300, gap 8, chevron at 420: (420 - 308) / 28 = 4.
        assert_eq!(drawer_room(308.0, 420.0, 28.0), 4);
        // No space (or menus past the chevron): nothing comes out.
        assert_eq!(drawer_room(400.0, 420.0, 28.0), 0);
        assert_eq!(drawer_room(500.0, 420.0, 28.0), 0);
        // 6 hidden, 4 fit: the 4 nearest the chevron.
        assert_eq!(drawer_window(6, 4, 0), (2, 6, 0));
        // Scrolled by the wheel, clamped at the far end.
        assert_eq!(drawer_window(6, 4, 1), (1, 5, 1));
        assert_eq!(drawer_window(6, 4, 9), (0, 4, 2));
        // Everything fits: no scrolling.
        assert_eq!(drawer_window(3, 10, 5), (0, 3, 0));
        assert_eq!(drawer_window(0, 0, 0), (0, 0, 0));
    }

    #[test]
    fn right_clicks_keep_their_own_menus_and_the_rest_opens_the_bars() {
        // Their own menus, as before.
        for c in [Cell::Desktops, Cell::Input, Cell::Start, Cell::Tray(0), Cell::Tray(7)] {
            assert_eq!(right_click(Some(c)), RightClick::Own, "{c:?}");
        }
        // The app's menu titles stay the app's.
        for c in [Cell::App, Cell::Menu(0), Cell::Menu(3)] {
            assert_eq!(right_click(Some(c)), RightClick::Nothing, "{c:?}");
        }
        // Everything else, and the empty bar (the island too): settings.
        assert_eq!(right_click(None), RightClick::BarMenu);
        for c in [Cell::Clock, Cell::Net, Cell::Volume, Cell::Battery, Cell::Control, Cell::Search, Cell::Chevron, Cell::TrayButton] {
            assert_eq!(right_click(Some(c)), RightClick::BarMenu, "{c:?}");
        }
    }

    #[test]
    fn drops_count_only_on_the_bar() {
        assert!(drop_inside(10.0, 5.0, 1920.0, 32.0));
        assert!(!drop_inside(10.0, 40.0, 1920.0, 32.0));
        assert!(!drop_inside(-1.0, 5.0, 1920.0, 32.0));
        assert!(!drop_inside(1920.0, 5.0, 1920.0, 32.0));
        assert!(!drop_inside(10.0, -3.0, 1920.0, 32.0));
    }
}
