//! The Launchpad overlay: one full-screen window per open, drawn with
//! Windows.UI.Composition. The backdrop (blurred desktop + tint, or solid),
//! then visuals for each app cell (an icon sprite and a name sprite), pages
//! side by side in a strip that slides, and small D2D surfaces for the
//! search field, the page dots and the quiet states. What the pointer or the
//! keyboard is on is lit by small shapes (view/select.rs); nothing
//! full-screen is ever redrawn. Menus are the shared ones (crate::popup),
//! shown from the overlay's loop (run_menu).
//!
//! Cells exist only for the page on screen and its neighbours (made as the
//! pages are reached); icons arrive from the loader thread and replace a
//! placeholder as they come.
//!
//! Opening and closing fade (no zoom, no icon motion: the plate's 220 in
//! and 180 out, straight) and page turns glide on the GLIDE spring, with
//! Composition animations started once (motion.rs; the values are
//! crate::motion's). Closing
//! ends Launchpad at once; its window is retired like every fading popup's
//! (crate::motion::retire_faded): click-through for every app while it
//! fades, gone when the closing animation's batch completes (or after its
//! fallback), so a click or a new Launchpad never waits for the fade.
//! Without motion (effects off, Windows' animations off, high contrast) it
//! opens and closes at once.
//!
//! The pages follow Launchpad's own arrangement (layout_store.rs; search
//! results stay by name): the Home pages, then the App Library's page
//! (library.rs). Edit mode, dragging, folders and the Library's category
//! panels (view/arrange.rs), the Library page, the clean-up suggestions
//! and the Home / Library menu actions (view/applib.rs) and the screen
//! reader's tree (view/access.rs) are in child modules.
//! Icons are drawn at exactly their device-pixel size and placed on whole
//! device pixels (shown 1:1: never filtered by the brush).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use windows::core::{w, IInspectable, Interface, Result, BOOL, HSTRING};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionBatchCompletedEventArgs, CompositionBatchTypes, CompositionStretch, CompositionSurfaceBrush, Compositor, ContainerVisual, ScalarKeyFrameAnimation,
    SpriteVisual,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_HIT_TEST_METRICS, DWRITE_LINE_METRICS, DWRITE_READING_DIRECTION_RIGHT_TO_LEFT, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Accessibility::NotifyWinEvent;
use windows::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmNotifyIME, ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow, CANDIDATEFORM, CFS_EXCLUDE,
    CFS_POINT, COMPOSITIONFORM, CPS_CANCEL, GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR, HIMC, IME_COMPOSITION_STRING, ISC_SHOWUICOMPOSITIONWINDOW,
    NI_COMPOSITIONSTR,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VIRTUAL_KEY, VK_APPS, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F10,
    VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_PROCESSKEY, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use super::edit::{self, Pos};
use super::grid::{self, Arrow, Grid, Rect};
use super::layout_store::{self, Pages};
use super::motion;
use super::{folders, list, loader};
use crate::config::LpItem;
use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::i18n::{t, tf};
use crate::search::edit::Field;
use crate::search::sources::App;
use crate::theme::metrics::{size, space};
use crate::theme::{Rgba, Role, State, Theme};

/// The app list was read again (search's reader thread).
const WM_LP_APPS: u32 = WM_APP + 1;
/// After the first frame: the neighbouring pages' cells.
const WM_LP_MORE: u32 = WM_APP + 3;
/// The closing animation is done (its batch completed): the retired
/// window goes (crate::motion::faded).
const WM_LP_CLOSED: u32 = WM_APP + 4;
const CARET_TIMER: usize = 1;
/// The overlay's styles when it opens: layered from the start (like the
/// dock), never click-through or unactivatable (closing adds those to the
/// window it retires; every open makes a new window).
const OPEN_EX: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_NOREDIRECTIONBITMAP.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0 | WS_EX_LAYERED.0);
/// A press that moves this far sideways becomes a swipe.
const DRAG_START: f32 = 8.0;
/// Wheel notches closer than this turn one page only.
const WHEEL_GAP_MS: u128 = 260;
/// Edit mode (arrange.rs): a press held still starts it; a dragged icon
/// resting on another makes a folder; resting at a side edge turns the page.
const LONG_PRESS_TIMER: usize = 3;
const HOLD_TIMER: usize = 4;
const EDGE_TIMER: usize = 5;
/// A dragged icon stayed with the same place long enough: the others make
/// room (edit::SETTLE_MS).
const SETTLE_TIMER: usize = 6;
/// The app list is still being read: the wait before the ring shows, then
/// before its line does (update_msg).
const WAIT_TIMER: usize = 7;
/// A wait longer than this shows the ring's line (design system 11.2).
const WAIT_LINE_MS: u128 = 5000;

mod access;
mod applib;
mod arrange;
mod organize;
mod select;

// Glyphs (crate::glyphs).
const G_SEARCH: Glyph = Glyph::Search;

/// The open overlay's window (0 = none), for the app list's "refreshed".
static OPEN_HWND: AtomicIsize = AtomicIsize::new(0);

// ---- state -----------------------------------------------------------------------

/// Run once the overlay is gone (the app gets the foreground).
#[derive(Clone, Debug, PartialEq)]
enum After {
    Open(String),
    Admin(String),
    Reveal(String),
}

/// Run between messages, outside the overlay's borrow (it reaches into the
/// dock, which may call back here).
enum Deferred {
    Pin { name: String, path: String, on: bool },
    /// Edit mode ended with the pages changed.
    SaveLayout(Vec<Vec<LpItem>>),
    /// Home or the Library changed (a menu action, the clean-up, the
    /// migration, an app seen): Launchpad's whole section.
    SaveLp(crate::config::LaunchpadCfg),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Field,
    Grid,
}

#[derive(Clone, Debug, PartialEq)]
enum MenuAct {
    Open,
    Uninstall,
    Admin,
    Reveal(String),
    Pin { name: String, path: String, on: bool },
    /// Start edit mode (arrange the pages).
    Arrange,
    /// Open the folder at this listing index.
    OpenFolder(usize),
    /// Home and the Library (applib.rs), on the menu's app.
    AddHome,
    RemoveHome,
    Hide,
    Unhide,
    /// "Move to category…": the categories' menu; then one of them.
    MoveTo,
    SetCat(super::library::Cat),
    /// A Library tile's menu: open its category (tile index).
    OpenCategory(usize),
    /// "Tidy Home" (organize.rs).
    Tidy,
    /// Edit mode, on the picked items (organize.rs): group them into a
    /// folder; Home and the Library for all their apps.
    Group,
    ManyRemoveHome,
    ManyHide,
    ManyMoveTo,
    ManySetCat(super::library::Cat),
}

/// A menu asked for: shown by the overlay's loop, outside its borrow
/// (run_menu: crate::popup's nested loop dispatches to the overlay). Its
/// rows' ids are 1 + their index in `acts`; `app` is what every row acts
/// on (a snapshot: even if the list changes meanwhile).
struct Menu {
    items: Vec<crate::popup::Item>,
    acts: Vec<MenuAct>,
    app: App,
    /// Where (DIP in the overlay).
    at: (f32, f32),
}

/// What run_menu needs of the overlay, taken out of its borrow.
struct MenuRun {
    menu: Menu,
    gfx: Rc<Gfx>,
    theme: Theme,
    scale: f32,
    /// `Menu::at` in screen px.
    at: POINT,
    rtl: bool,
}

/// A row that opens the categories: each one's label, what it does and
/// whether it is the one in force.
type CatRows = Vec<(String, MenuAct, bool)>;

fn app_commands(packaged: bool, target: Option<String>, hidden_taskbar: bool) -> Vec<Option<(String, MenuAct)>> {
    let mut rows = vec![Some((t("launchpad.menu.open").into(), MenuAct::Open))];
    if !packaged { rows.push(Some((t("dock.menu.run_admin").into(), MenuAct::Admin))); }
    if let Some(path) = target { rows.push(Some((t("dock.menu.open_location").into(), MenuAct::Reveal(path)))); }
    if hidden_taskbar { rows.push(Some((t("launchpad.menu.uninstall").into(), MenuAct::Uninstall))); }
    rows.push(None);
    rows
}

/// Pure: a menu's rows as the shared menu's items and what each id does
/// (id = 1 + its index in the result's second part). A row that opens the
/// categories ("Move to category…") becomes a submenu of `cats`' rows: its
/// own ellipsis goes (the submenu's arrow says it).
fn menu_items(rows: Vec<Option<(String, MenuAct)>>, cats: &dyn Fn(&MenuAct) -> Option<CatRows>) -> (Vec<crate::popup::Item>, Vec<MenuAct>) {
    use crate::popup::Item;
    let mut acts: Vec<MenuAct> = Vec::new();
    let mut items = Vec::new();
    for row in rows {
        let Some((label, act)) = row else {
            items.push(Item::sep());
            continue;
        };
        let sub = cats(&act);
        acts.push(act);
        let id = acts.len() as u32;
        match sub {
            None => items.push(Item::new(id, label)),
            Some(list) => {
                let kids: Vec<Item> = list
                    .into_iter()
                    .map(|(l, a, on)| {
                        acts.push(a);
                        Item::new(acts.len() as u32, l).checked(on)
                    })
                    .collect();
                items.push(Item::new(id, label.trim_end_matches('\u{2026}').trim_end().to_string()).submenu(kids));
            }
        }
    }
    (items, acts)
}

/// The search field's text: body (the ramp's).
const FIELD_TEXT: f32 = crate::theme::ramp::BODY.size;
/// From the field's ends to what is in it (the glass, the text).
const FIELD_PAD: f32 = space::S4;
/// The room round the field for the keyboard's ring.
const FIELD_RING_M: f32 = size::FOCUS_GAP + size::FOCUS_W;
/// The line of the quiet block that says the app list could not be read:
/// short, with its button under it (the long sentence of
/// "launchpad.load_failed" told to click the text itself).
const FAILED_LINE: &str = "topbar.panel.network.list_failed";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Field,
    /// A slot of the grid on screen with an app in it.
    App(usize),
    Dot(usize),
    /// Edit mode's Done button.
    Done,
    /// In the open folder's panel: an app (its index in the folder), the
    /// name, the rest of the panel; outside the panel.
    FolderApp(usize),
    FolderName,
    FolderPanel,
    FolderOutside,
    /// A Library tile (its index) and the part of it.
    LibTile(usize, super::library::Part),
    /// The "try again" button (the list could not be read): where it is
    /// over what would be empty.
    Retry,
    Empty,
}

struct Press {
    x: f32,
    y: f32,
    hit: Hit,
}

struct Drag {
    /// Strip offset when it started.
    base: f32,
    /// Strip offset now.
    at: f32,
    start_x: f32,
    last_x: f32,
    last_t: Instant,
    /// DIP/s, smoothed.
    speed: f32,
}

/// The cells of one page (or of the search results, or the open folder).
struct View {
    container: ContainerVisual,
    /// (icon, name) per slot.
    cells: Vec<(SpriteVisual, SpriteVisual)>,
    /// What each cell shows (Tile::key: an app's launch target, a folder's
    /// own key).
    bound: Vec<Option<String>>,
}

/// What a cell shows: an app or a folder (its apps' launch targets, the
/// first ones drawn on its tile).
#[derive(Clone, Debug, PartialEq)]
struct Tile {
    key: String,
    name: String,
    folder: Option<Vec<String>>,
    /// New since Launchpad last saw it: a dot by its name.
    new: bool,
    /// Hidden (shown by search only): dimmed.
    dim: bool,
}

/// Which cells a binding fills.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Cells {
    Page(usize),
    Results,
    Folder,
}

struct Pad {
    hwnd: HWND,
    gfx: Rc<Gfx>,
    theme: Theme,
    s: f32,
    /// Monitor size (DIP).
    w: f32,
    h: f32,
    /// Work area inside the monitor (DIP, monitor-relative).
    work: Rect,
    grid: Grid,
    rtl: bool,
    dw: IDWriteFactory,
    /// (Holds the visual tree: root > backdrop, tint, content > grid box,
    /// field, dots, ...)
    _target: DesktopWindowTarget,
    backdrop: SpriteVisual,
    tint: SpriteVisual,
    /// Holds the strip and the results: scales in edit mode (around the
    /// grid's centre).
    grid_box: ContainerVisual,
    strip: ContainerVisual,
    /// Where the strip is (or is sliding to).
    strip_x: f32,
    /// The next slide ends a swipe: (where the strip is, release speed
    /// DIP/s, positive to the right).
    fling: Option<(f32, f32)>,
    pages: HashMap<usize, View>,
    results: View,
    /// What lights the cell under the pointer or the keyboard (select.rs).
    glow: select::Glow,
    /// The view the lit shapes are in: None = results, Some(p) = page p.
    sel_in: Option<Option<usize>>,
    /// The keyboard (or a screen reader) moved the selection last: it is
    /// shown by the focus ring, at once; else by the pointer's ground.
    by_key: bool,
    /// Typing chose the selection (the first result: Enter opens it): it
    /// shows by the pointer's ground, never the ring (a letter is not a
    /// key that moves the focus: crate::input).
    typed: bool,
    /// A screen reader's focus shows the ring (crate::input::reader_focus):
    /// it showed already, or Windows shows keyboard cues always.
    reader_ring: bool,
    /// The keyboard put the focus in the field (Tab, the up arrow): its
    /// ring shows until the pointer takes over.
    field_ring: bool,
    field_v: SpriteVisual,
    dots_v: SpriteVisual,
    msg_v: SpriteVisual,
    /// The progress ring while the app list is read (it turns by a
    /// Composition animation: nothing is redrawn).
    spin_v: SpriteVisual,
    /// Since when the app list has been awaited (the ring shows after
    /// TIP_DELAY_MS, its line after WAIT_LINE_MS).
    wait_since: Option<Instant>,
    /// The ring as shown (None: not shown).
    spin_at: Option<Ring>,
    /// Where the strip is on its spring (page turns: GLIDE, carried on).
    strip_run: crate::motion::SpringRun,
    /// The list from search (to notice a refresh) and sorted by name.
    raw: Arc<Vec<App>>,
    apps: Vec<App>,
    /// Search results: indexes into `apps`, best first.
    found: Vec<usize>,
    searching: bool,
    /// The app list could not be read: where the message is (a click there
    /// reads it again).
    msg_retry: Option<Rect>,
    /// What the failure the button answers says (a key: the button's
    /// help text for a screen reader).
    retry_why: &'static str,
    /// The pointer is on nothing that lights (select.rs): what it lit is
    /// off; the keyboard's ring still shows.
    pointer_out: bool,
    page: usize,
    /// The last page change went back (towards the first page): a window
    /// of one or two page dots leads on that way (Regions::dots_row).
    dots_back: bool,
    focus: Focus,
    /// Selected app: index in `apps` (browsing) or in `found` (searching).
    sel: Option<usize>,
    field: Field,
    comp: Vec<char>,
    comp_cursor: usize,
    high: Option<u16>,
    caret_on: bool,
    /// Field text scroll (DIP) and the caret's x in the text.
    shift: f32,
    caret_x: f32,
    /// Icon edge in px.
    px: u32,
    icon_brushes: HashMap<String, CompositionSurfaceBrush>,
    label_brushes: HashMap<String, CompositionSurfaceBrush>,
    placeholder: Option<CompositionSurfaceBrush>,
    asked: HashSet<String>,
    press: Option<Press>,
    drag: Option<Drag>,
    /// The icon pressed and the opacity it rests at.
    pressed_cell: Option<(SpriteVisual, f32)>,
    wheel: i32,
    last_flip: Option<Instant>,
    last_mouse: (i32, i32),
    /// A menu asked for and not shown yet (the loop takes it: run_menu).
    menu: Option<Menu>,
    /// A menu is up (crate::popup has the foreground and the capture):
    /// losing the activation to it does not close Launchpad.
    menu_open: bool,
    /// The dock's pins (for "Keep in Dock" / "Remove from Dock").
    pinned: Vec<String>,
    done: bool,
    /// Animating away: click-through, input is ignored, the window goes
    /// when it ends.
    closing: bool,
    /// The closing animation started: the window is retired when the loop
    /// ends (crate::motion::retire_faded), gone within this many ms.
    fade_ms: Option<u32>,
    /// The window in front before we opened (it gets the focus back when
    /// we close without opening anything).
    prev_fg: HWND,
    after: Option<After>,
    deferred: Vec<Deferred>,
    /// The next WM_CHAR(' ') belongs to a Space already handled on key down.
    eat_space: bool,
    /// Building what the first frame shows (neighbours wait).
    first_frame: bool,
    /// The pages as arranged (layout_store::arrange over `apps`), fitted
    /// to this grid. Searching ignores it (results are by name).
    layout: Pages,
    /// App id -> index into `apps`.
    by_id: HashMap<String, usize>,
    /// Edit mode, dragging, the open folder (arrange.rs).
    arr: arrange::Arr,
    /// The App Library, hidden apps, the clean-up (applib.rs).
    lib: applib::Lib,
    /// Several icons at once, Tidy Home and its undo (organize.rs).
    org: organize::Organize,
    /// What was last said aloud (the window's name; the UIA root's too).
    spoken: String,
    /// The app list snapshot shown (search::apps_snapshot): its number and
    /// whether it was complete. Arranging goes by this, never by the
    /// global state (which may be a newer reading not shown yet).
    apps_gen: u64,
    apps_complete: bool,
    /// A screen reader put its focus on one of edit mode's buttons (Done,
    /// Move, the folder's Close): shown as the tree's focus until the
    /// focus moves elsewhere.
    ax_focus: Option<String>,
}

thread_local! {
    static PAD: RefCell<Option<Pad>> = const { RefCell::new(None) };
}

fn with_pad<R>(f: impl FnOnce(&mut Pad) -> R) -> Option<R> {
    PAD.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn is_open() -> bool {
    PAD.with(|c| c.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

/// Launchpad is animating away (still open, input ignored).
pub fn is_closing() -> bool {
    PAD.with(|c| c.try_borrow().map(|g| g.as_ref().is_some_and(|p| p.closing)).unwrap_or(false))
}

/// Closes it with the closing animation (instantly when it does not
/// animate).
pub fn close() {
    let h = with_pad(|p| {
        p.finish(None);
        p.hwnd
    });
    if let Some(h) = h {
        unsafe {
            let _ = PostMessageW(Some(h), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

/// Closes it at once (no animation).
pub fn cancel() {
    let h = with_pad(|p| {
        p.done = true;
        p.hwnd
    });
    if let Some(h) = h {
        unsafe {
            let _ = PostMessageW(Some(h), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn retheme(theme: &Theme) {
    with_pad(|p| {
        if p.theme != *theme {
            p.theme = theme.clone();
            // A new font or text size: the names' box measured again.
            p.regrid();
            p.restyle();
        }
    });
}

pub fn apps_refreshed() {
    let h = OPEN_HWND.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_LP_APPS, WPARAM(0), LPARAM(0));
        }
    }
}

// ---- helpers -----------------------------------------------------------------------

fn rgba(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

fn with_alpha(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a, ..c }
}

fn color(c: D2D1_COLOR_F) -> Color {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color { A: b(c.a), R: b(c.r), G: b(c.g), B: b(c.b) }
}

fn d2r(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.l, top: r.t, right: r.r, bottom: r.b }
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

/// `v` ms of real time.
fn real(v: i64) -> TimeSpan {
    TimeSpan { Duration: v.max(1) * 10_000 }
}

/// An opacity animation to `to` (from `from`, else from where it is now)
/// as `plan` says (crate::motion::fade_plan's answer: ms of real time and
/// the curve).
fn fade(c: &Compositor, from: Option<f32>, to: f32, plan: (i64, crate::motion::Curve)) -> Result<ScalarKeyFrameAnimation> {
    let a = c.CreateScalarKeyFrameAnimation()?;
    if let Some(f) = from {
        a.InsertKeyFrame(0.0, f)?;
    }
    a.InsertKeyFrameWithEasingFunction(1.0, to, &crate::motion::easing(c, plan.1)?)?;
    a.SetDuration(real(plan.0))?;
    Ok(a)
}

/// The middle of the app grid (DIP, in the overlay; `work` the work area).
fn grid_centre(g: &Grid, work: &Rect) -> (f32, f32) {
    (work.l + g.left + g.cell_w * g.cols as f32 / 2.0, work.t + g.top + g.cell_h * g.rows as f32 / 2.0)
}

/// Can `h` take the focus back (still there, shown, not minimized, not us)?
unsafe fn can_take_focus(h: HWND) -> bool {
    !h.is_invalid() && IsWindow(Some(h)).as_bool() && IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool()
}

/// Takes the foreground (the input that opened us may have gone to
/// another app: a hook, a hotkey). Like the dock's snap::activate: no
/// AttachThreadInput (sharing the input queue of the app in front makes us
/// wait on it, and a hung one would freeze Launchpad and the dock with
/// it), only our own window raised (our thread: nothing waits) and asked
/// for the foreground again. If Windows still refuses, false.
unsafe fn take_foreground(h: HWND) -> bool {
    if SetForegroundWindow(h).as_bool() && GetForegroundWindow() == h {
        return true;
    }
    let _ = SetWindowPos(h, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER);
    let _ = SetForegroundWindow(h);
    GetForegroundWindow() == h
}

// ---- open / close ------------------------------------------------------------------

pub fn show(gfx: &Rc<Gfx>, theme: &Theme, hmon: Option<isize>) {
    let _companion_pause = crate::companion::pause_cover();
    if is_open() || crate::search::is_open() || crate::popup::is_tracking() || crate::topbar::control::is_open() {
        return;
    }
    let t0 = Instant::now();
    unsafe {
        register_class();
        let pad = match Pad::new(gfx, theme, hmon) {
            Ok(p) => p,
            Err(e) => {
                crate::log!("launchpad: cannot open: {e}");
                return;
            }
        };
        let hwnd = pad.hwnd;
        PAD.with(|c| *c.borrow_mut() = Some(pad));
        OPEN_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        // The dock's pins, read now (the dock is not borrowed here).
        let pinned = crate::dock::pinned_paths();
        // Its own arrangement (empty: by name) and Library, read now too.
        let lp = crate::dock::launchpad_cfg();
        with_pad(|p| {
            p.pinned = pinned;
            p.arr.saved = lp.layout.clone();
            p.lib.cfg = lp;
            p.start();
        });
        // A fresh look at the installed apps (the list on screen updates
        // when it is back).
        crate::search::refresh_apps();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let fg = take_foreground(hwnd);
        let _ = SetFocus(Some(hwnd));
        let blink = GetCaretBlinkTime();
        if blink != 0 && blink != u32::MAX {
            SetTimer(Some(hwnd), CARET_TIMER, blink, None);
        }
        let n = with_pad(|p| p.apps.len()).unwrap_or(0);
        crate::log!("launchpad: open in {} ms ({n} apps)", t0.elapsed().as_millis());
        if !fg {
            // Without the focus no key reaches it and nothing would close it.
            crate::log!("launchpad: could not take the foreground; not opening");
        } else {
            let mut msg = MSG::default();
            loop {
                if with_pad(|p| p.done).unwrap_or(true) {
                    break;
                }
                let r = GetMessageW(&mut msg, None, 0, 0);
                if r.0 == 0 || r.0 == -1 {
                    PostQuitMessage(msg.wParam.0 as i32);
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                for d in with_pad(|p| std::mem::take(&mut p.deferred)).unwrap_or_default() {
                    run_deferred(d);
                }
                // A menu asked for: shown here, outside the overlay's borrow.
                if let Some(m) = with_pad(|p| p.take_menu()).flatten() {
                    run_menu(m);
                }
            }
        }
        for t in [CARET_TIMER, LONG_PRESS_TIMER, HOLD_TIMER, EDGE_TIMER, SETTLE_TIMER, WAIT_TIMER] {
            let _ = KillTimer(Some(hwnd), t);
        }
        OPEN_HWND.store(0, Ordering::SeqCst);
        let mut after = None;
        let mut save = None;
        let mut left = Vec::new();
        if let Some(mut p) = PAD.with(|c| c.borrow_mut().take()) {
            after = p.after.take();
            save = p.layout_to_save();
            left = std::mem::take(&mut p.deferred);
            match p.fade_ms {
                // Fading away: Launchpad is closed now, its window goes
                // when the fade ends (what was chosen runs then).
                Some(ms) => p.retire(ms, after.take()),
                None => p.close(),
            }
        }
        // (Asked for by the last message: saved now.)
        for d in left {
            run_deferred(d);
        }
        // Arranged while open: saved by the dock (outside our borrow).
        if let Some(l) = save {
            crate::dock::set_launchpad_layout(l);
        }
        if let Some(a) = after {
            run_after(a);
        }
    }
}

/// Shows a menu of the overlay's (the shared one: crate::popup, with its
/// own loop; the overlay is not borrowed meanwhile) at the pointer, like
/// any menu opened there, and tells the overlay how it closed.
fn run_menu(m: MenuRun) {
    use crate::popup::{Align, Request};
    let MenuRun { menu, gfx, theme, scale, at, rtl } = m;
    let before = crate::motion::retired_windows();
    let out = crate::popup::track(Request {
        gfx: &gfx,
        theme: &theme,
        items: menu.items,
        at,
        align: Align::hanging(false, rtl),
        scale,
        titles: Default::default(),
        on_slide: None,
        drag_ids: 0..0,
        on_space: None,
        on_pick: None,
        hangs: false,
    });
    // The menu's own windows, fading out now.
    let fading: Vec<HWND> = crate::motion::retired_windows().into_iter().filter(|h| !before.contains(h)).collect();
    with_pad(|p| p.menu_closed(out, menu.acts, menu.app, &fading));
}

/// Pure: after its menu closed, does Launchpad stay? It does when it has
/// the foreground itself (`mine`), nothing has (`none`) or a window of
/// NopBar's has (`ours`: the menu's, fading). Another app in front: no,
/// the activation went elsewhere while the menu was up.
fn keeps_after_menu(mine: bool, none: bool, ours: bool) -> bool {
    mine || none || ours
}

/// What was chosen, run outside the overlay's borrow.
fn run_after(a: After) {
    match a {
        After::Open(target) => {
            crate::launch::open(&crate::launch::Target::new(&target, &name_of(&target), crate::launch::Source::Launchpad));
        }
        After::Admin(target) => {
            crate::launch::open(&crate::launch::Target::new(&target, &name_of(&target), crate::launch::Source::Launchpad).verb("runas"));
        }
        After::Reveal(path) => {
            let args = HSTRING::from(format!("/select,\"{path}\""));
            crate::launch::open_soon(crate::launch::Target::new("explorer.exe", "Explorer", crate::launch::Source::Dock).args(&args.to_string(), ""));
        }
    }
}

/// Pure: the empty screen's text key (None: apps are shown). Searching
/// while the list is still loading says "loading", not "no results".
fn empty_text(no_apps: bool, searching: bool, nothing_found: bool, state: crate::search::AppsState) -> Option<&'static str> {
    use crate::search::AppsState;
    if !no_apps && !(searching && nothing_found) {
        return None;
    }
    if no_apps {
        return Some(match state {
            AppsState::Failed | AppsState::Partial => "launchpad.load_failed",
            AppsState::Empty => "launchpad.no_apps",
            AppsState::Loading | AppsState::Loaded => "launchpad.loading",
        });
    }
    Some("launchpad.no_results")
}

/// Pure: where a quiet block's parts sit in its QUIET_MIN_H, for a line
/// `line` tall: (the top of the glyph and its line, the button's top).
/// The glyph (24), 8, the line and, with `button`, 12 and the button (a
/// control's height), all of it centered (11.1).
fn quiet_stack(line: f32, button: bool) -> (f32, f32) {
    let group = size::ICON_L + space::S3 + line;
    let all = group + if button { space::S4 + size::CONTROL_H } else { 0.0 };
    let top = ((size::QUIET_MIN_H - all) / 2.0).max(0.0);
    (top, top + group + space::S4)
}

/// Pure: the "try again" button of a quiet block `w` wide, its label `tw`
/// wide, its top at `top` (quiet_stack): a control's height, the label 12
/// from its ends, centered.
fn retry_rect(w: f32, tw: f32, top: f32) -> Rect {
    let bw = (tw + size::BUTTON_PAD_X * 2.0).ceil();
    let l = ((w - bw) / 2.0).round();
    Rect::new(l, top.round(), l + bw, top.round() + size::CONTROL_H)
}

/// A quiet block `w` wide and QUIET_MIN_H tall from the top left (design
/// system 11.1): the shared one (a 24 glyph, 8, one line of body text)
/// and, with `button`, that button 12 under the line (quiet_stack): a
/// raised thing on the plate (its fill and its contact shadow; an outline
/// in high contrast). What the overlay shows and the audit's picture
/// draws.
#[allow(clippy::too_many_arguments)]
unsafe fn paint_quiet(gfx: &Gfx, dc: &ID2D1DeviceContext, w: f32, kind: crate::gfx::parts::Quiet, glyph: Glyph, text: &str, button: Option<&str>, theme: &Theme) -> Result<()> {
    let line = if text.is_empty() { 0.0 } else { gfx.text_style(text, &theme.font.family, crate::theme::ramp::BODY, 1.0)?.2 };
    let (top, button_top) = quiet_stack(line, button.is_some());
    // (An area as tall as the glyph and its line: they sit at its top.)
    gfx.quiet(dc, D2D_RECT_F { left: 0.0, top, right: w, bottom: top + size::ICON_L + space::S3 + line }, kind, glyph, text, theme, 1.0, 0.0)?;
    let Some(label) = button else { return Ok(()) };
    let (l, tw, th) = gfx.text_style(label, &theme.font.family, crate::theme::ramp::BODY, 1.0)?;
    let b = retry_rect(w, tw, button_top);
    let rad = theme.radii().control;
    let rr = D2D1_ROUNDED_RECT { rect: d2r(b), radiusX: rad, radiusY: rad };
    match theme.outline_of(Role::Raised, State::Rest) {
        Some((c, lw)) => stroke_rr(dc, rgba(c), &D2D1_ROUNDED_RECT { rect: d2r(Rect::new(b.l + lw / 2.0, b.t + lw / 2.0, b.r - lw / 2.0, b.b - lw / 2.0)), ..rr }, lw)?,
        None => {
            crate::gfx::parts::contact(dc, d2r(b), rad, theme.contact(), 1.0)?;
            // (Raised: Flat v2's card light round it.)
            crate::gfx::parts::card(dc, d2r(b), rad, theme, State::Rest, theme.fill_of(Role::Raised, State::Rest), 1.0, 1.0)?;
        }
    }
    draw_layout(dc, &l, (b.cx() - tw / 2.0).round(), (b.cy() - th / 2.0).round(), rgba(theme.text_of(crate::theme::Level::Primary, State::Rest)))
}

/// The field's plate `fw` x `fh` from the top left (design system 3.2,
/// 6.1): a capsule, a well (sunk: the track fill and its upper wall's
/// shade; an outline in high contrast), the glass 16 at its leading end,
/// 12 in, in `glass`; `ring`: the keyboard's focus round all of it. It
/// does not answer the pointer. What the overlay shows and the audit's
/// picture draws.
unsafe fn paint_field_plate(dc: &ID2D1DeviceContext, fw: f32, fh: f32, theme: &Theme, ring: bool, rtl: bool, glass: D2D1_COLOR_F) -> Result<()> {
    let rect = D2D_RECT_F { left: 0.0, top: 0.0, right: fw, bottom: fh };
    let rad = theme.radii().control;
    crate::gfx::parts::well(dc, rect, rad, theme, 1.0)?;
    if ring {
        crate::gfx::parts::focus_ring(dc, rect, rad, theme, 1.0)?;
    }
    let gx = FIELD_PAD + size::ICON_S / 2.0;
    crate::glyphs::draw(dc, G_SEARCH, if rtl { fw - gx } else { gx }, fh / 2.0, size::ICON_S, glass)
}

/// Launchpad's plate (design system 2.1): over the blurred desktop the
/// scrim's tint, half see-through (the desktop stays in view); a solid
/// finish (no blur: Flat as shipped) the panels' surface, opaque; the
/// Window color in high contrast.
fn plate_tint(theme: &Theme) -> D2D1_COLOR_F {
    if let Some(hc) = theme.hc.filter(|_| theme.high_contrast) {
        return rgba(hc.window);
    }
    if theme.popup.material == "solid" {
        return D2D1_COLOR_F { a: 1.0, ..rgba(theme.popup_tint()) };
    }
    rgba(theme.scrim())
}

/// The incomplete list's line: this far above the page dots' middle.
const PARTIAL_UP: f32 = space::S7;

/// Pure: what shows `waited_ms` into reading the app list (design system
/// 11.3): (the ring, its line, ms until the next stage). Nothing for
/// TIP_DELAY_MS (what ends sooner never flickers a ring), then the ring
/// alone, a line with it past WAIT_LINE_MS.
fn wait_stage(waited_ms: u128) -> (bool, bool, Option<u32>) {
    let first = crate::motion::TIP_DELAY_MS as u128;
    if waited_ms < first {
        (false, false, Some((first - waited_ms).max(1) as u32))
    } else if waited_ms < WAIT_LINE_MS {
        (true, false, Some((WAIT_LINE_MS - waited_ms).max(1) as u32))
    } else {
        (true, true, None)
    }
}

/// Pure: the line under the apps when the list is incomplete (read part
/// way): it offers to read it again. Not while searching.
fn partial_text(no_apps: bool, searching: bool, state: crate::search::AppsState) -> Option<&'static str> {
    (!no_apps && !searching && state == crate::search::AppsState::Partial).then_some("launchpad.partial")
}

/// Pure: Enter reads the list again (the failure message offers it):
/// whenever no app is shown to open, also while searching; and with an
/// incomplete list when the field is empty and nothing is chosen.
fn enter_retries(retry_shown: bool, no_apps: bool, searching: bool, chosen: bool) -> bool {
    retry_shown && (no_apps || (!searching && !chosen))
}

/// Pure: the "try again" button (`retry`, where it is) takes what would
/// be empty under it; an app or a dot there stays itself.
fn retry_hit(h: Hit, retry: Option<Rect>, x: f32, y: f32) -> Hit {
    match h {
        Hit::Empty if retry.is_some_and(|r| r.contains(x, y)) => Hit::Retry,
        h => h,
    }
}

/// Pure: a release on `up` of a press that began on `down`: what it acts
/// on. None when they differ: a press begun beside a button (its margin
/// too) never runs it, and one begun on it and let go beside it is
/// cancelled (it does not count as a click on the empty space).
fn released_on(down: Hit, up: Hit) -> Option<Hit> {
    (down == up).then_some(up)
}

/// The progress ring as drawn: its middle, and whether it turns.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ring {
    at: (f32, f32),
    turns: bool,
}

/// Pure: the ring wanted at `at` (None: none), `animate` or not, after
/// `was`. None: it stays as it is. Some(None): it goes. Some(Some(r)): it
/// is drawn again there, turning only when `r.turns` (Launchpad stopped
/// animating while it turned: it stops, a still arc; 12.2).
fn ring_plan(was: Option<Ring>, at: Option<(f32, f32)>, animate: bool) -> Option<Option<Ring>> {
    let want = at.map(|at| Ring { at, turns: animate });
    (want != was).then_some(want)
}

/// What the IME sends.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ImeMsg {
    Start,
    Composition,
    End,
}

/// Pure: the composition shown (`comp`) is dropped, as its IME is told to
/// cancel it: nothing of it stays in the field, and what the IME still
/// sends for it is ignored (`dropped`) until a composition starts again.
/// True: something was dropped (the results go by the field's text again).
fn drop_composition(comp: &mut Vec<char>, dropped: &mut bool) -> bool {
    if comp.is_empty() {
        return false;
    }
    comp.clear();
    *dropped = true;
    true
}

/// Pure: IME message `m` with the composition shown `comp` and `dropped`
/// (drop_composition). True: go on with it (Composition: read it from the
/// IME; End: a composition shown went, so filter again). A dropped
/// composition's late messages change nothing; a new one is its own.
fn ime_gate(m: ImeMsg, comp: &mut Vec<char>, dropped: &mut bool) -> bool {
    match m {
        ImeMsg::Start => {
            *dropped = false;
            false
        }
        ImeMsg::Composition if *dropped => {
            comp.clear();
            false
        }
        ImeMsg::Composition => true,
        ImeMsg::End => {
            *dropped = false;
            let had = !comp.is_empty();
            comp.clear();
            had
        }
    }
}

/// An app's name for its launch target (named in a failure notice).
fn name_of(target: &str) -> String {
    crate::search::app_list().iter().find(|a| a.target() == target).map(|a| a.name.clone()).unwrap_or_default()
}

fn run_deferred(d: Deferred) {
    match d {
        Deferred::Pin { name, path, on } => crate::dock::set_pinned(&name, &path, on),
        Deferred::SaveLayout(l) => crate::dock::set_launchpad_layout(l),
        Deferred::SaveLp(c) => crate::dock::set_launchpad_state(c),
    }
}

unsafe fn register_class() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarLaunchpad"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    // Fails harmlessly once registered.
    RegisterClassExW(&wc);
}

impl Pad {
    unsafe fn new(gfx: &Rc<Gfx>, theme: &Theme, hmon: Option<isize>) -> Result<Self> {
        let hmon = match hmon {
            Some(m) => m,
            None => {
                let fg = GetForegroundWindow();
                if fg.is_invalid() {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST).0 as isize
                } else {
                    MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST).0 as isize
                }
            }
        };
        let mon = crate::monitors::info(hmon).or_else(crate::monitors::primary).ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        let s = mon.scale();
        let r = mon.rect;
        let (w, h) = ((r.right - r.left) as f32 / s, (r.bottom - r.top) as f32 / s);
        let wk = mon.work;
        let work = Rect::new(
            (wk.left - r.left) as f32 / s,
            (wk.top - r.top) as f32 / s,
            (wk.right - r.left) as f32 / s,
            (wk.bottom - r.top) as f32 / s,
        );
        let hwnd = CreateWindowExW(
            OPEN_EX,
            w!("NopBarLaunchpad"),
            &HSTRING::from(t("launchpad.title")),
            WS_POPUP,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        let built = (|| -> Result<Self> {
            SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), 255, LWA_ALPHA)?;
            let enable = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &enable as *const _ as *const _, 4);
            let c = gfx.compositor.clone();
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let root = c.CreateContainerVisual()?;
            root.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
            root.SetSize(Vector2 { X: w, Y: h })?;
            target.SetRoot(&root)?;
            let full = Vector2 { X: w, Y: h };
            let backdrop = c.CreateSpriteVisual()?;
            backdrop.SetSize(full)?;
            let tint = c.CreateSpriteVisual()?;
            tint.SetSize(full)?;
            let content = c.CreateContainerVisual()?;
            content.SetSize(full)?;
            // The names' box: two lines as the chosen font lays them out.
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let two_lines = lines_h(&dw, &theme.font.family, grid::LABEL_SIZE * crate::fonts::scale(), 2).unwrap_or(0.0);
            let grid = grid::grid(work.w(), work.h(), two_lines);
            let grid_box = c.CreateContainerVisual()?;
            grid_box.SetSize(full)?;
            let (gx, gy) = grid_centre(&grid, &work);
            grid_box.SetCenterPoint(Vector3 { X: gx, Y: gy, Z: 0.0 })?;
            let strip = c.CreateContainerVisual()?;
            let glow = select::Glow::new(&c)?;
            let sprite = || -> Result<SpriteVisual> {
                let v = c.CreateSpriteVisual()?;
                v.SetIsVisible(false)?;
                Ok(v)
            };
            let (field_v, dots_v, msg_v, spin_v) = (sprite()?, sprite()?, sprite()?, sprite()?);
            let results = View { container: c.CreateContainerVisual()?, cells: Vec::new(), bound: Vec::new() };
            results.container.SetIsVisible(false)?;
            for v in [&backdrop, &tint] {
                root.Children()?.InsertAtTop(v)?;
            }
            root.Children()?.InsertAtTop(&content)?;
            let ch = content.Children()?;
            ch.InsertAtTop(&grid_box)?;
            let gb = grid_box.Children()?;
            gb.InsertAtTop(&strip)?;
            gb.InsertAtTop(&results.container)?;
            // Edit mode's visuals: the folder target and the open folder
            // over the grid, Done and the dragged icon over the rest, the
            // card over everything.
            let arr = arrange::Arr::new(&c)?;
            arr.insert_low(&ch, &grid_box)?;
            let lib = applib::Lib::new();
            let org = organize::Organize::new(&c)?;
            for v in [&msg_v, &spin_v, &field_v, &dots_v] {
                ch.InsertAtTop(v)?;
            }
            org.insert(&ch)?;
            arr.insert_high(&ch)?;
            let rtl = crate::i18n::rtl();
            Ok(Pad {
                hwnd,
                gfx: gfx.clone(),
                theme: theme.clone(),
                s,
                w,
                h,
                work,
                grid,
                rtl,
                dw,
                _target: target,
                backdrop,
                tint,
                grid_box,
                strip,
                strip_x: 0.0,
                fling: None,
                pages: HashMap::new(),
                results,
                glow,
                sel_in: None,
                by_key: false,
                typed: false,
                reader_ring: false,
                field_ring: false,
                field_v,
                dots_v,
                msg_v,
                spin_v,
                wait_since: None,
                spin_at: None,
                strip_run: crate::motion::SpringRun::still(crate::motion::SPRING_GLIDE, 0.0, Instant::now()),
                raw: Arc::new(Vec::new()),
                apps: Vec::new(),
                found: Vec::new(),
                searching: false,
                msg_retry: None,
                retry_why: FAILED_LINE,
                pointer_out: false,
                page: 0,
                dots_back: false,
                focus: Focus::Field,
                sel: None,
                field: Field::default(),
                comp: Vec::new(),
                comp_cursor: 0,
                high: None,
                caret_on: true,
                shift: 0.0,
                caret_x: 0.0,
                px: (grid::ICON * s).round() as u32,
                icon_brushes: HashMap::new(),
                label_brushes: HashMap::new(),
                placeholder: None,
                asked: HashSet::new(),
                press: None,
                drag: None,
                pressed_cell: None,
                wheel: 0,
                last_flip: None,
                last_mouse: (i32::MIN, i32::MIN),
                menu: None,
                menu_open: false,
                pinned: Vec::new(),
                done: false,
                closing: false,
                fade_ms: None,
                prev_fg: GetForegroundWindow(),
                after: None,
                deferred: Vec::new(),
                eat_space: false,
                first_frame: false,
                layout: vec![Vec::new()],
                by_id: HashMap::new(),
                arr,
                lib,
                org,
                spoken: String::new(),
                apps_gen: 0,
                apps_complete: false,
                ax_focus: None,
            })
        })();
        if built.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        built
    }

    /// Fills the overlay (the window is not shown yet) and starts the
    /// opening animation.
    fn start(&mut self) {
        let snap = crate::search::apps_snapshot();
        (self.apps_gen, self.apps_complete) = (snap.gen, snap.complete());
        self.raw = snap.list;
        let loc = list::locale();
        self.apps = list::sorted(&self.raw, |a, b| list::compare(&loc, a, b));
        self.arrange_apps();
        self.restyle();
        // Only the first page before the first frame; its neighbours right
        // after (WM_LP_MORE).
        self.first_frame = true;
        self.go_page(0, false);
        self.first_frame = false;
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_LP_MORE, WPARAM(0), LPARAM(0));
        }
        self.make_results();
        // (restyle drew the field and the dots.)
        self.announce(t("launchpad.title").to_string());
        // The plate and everything on it at once (7.4: no zoom, no
        // stagger). Not animating: a 60 ms fade, never a pop.
        if let Err(e) = self.play_open(motion::scrim(self.animate(), true, crate::effects::current().speed)) {
            crate::log!("launchpad: opening animation failed: {e:?}");
            self.show_all();
        }
    }

    fn close(self) {
        loader::stop();
        unsafe {
            // A composition still open must not end up in another window.
            let himc = ImmGetContext(self.hwnd);
            if !himc.is_invalid() {
                let _ = ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                let _ = ImmReleaseContext(self.hwnd, himc);
            }
            let _ = DestroyWindow(self.hwnd);
        }
        // Visuals, surfaces and brushes go with `self`.
    }

    /// Closed while its closing animation plays: the window is retired
    /// (click-through for every app, never activated) with everything that
    /// draws it, and goes when the animation's batch completes
    /// (WM_LP_CLOSED) or after `within` ms; `after` (a reveal) runs once it
    /// is gone.
    fn retire(self, within: u32, after: Option<After>) {
        loader::stop();
        let hwnd = self.hwnd;
        let then = after.map(|a| Box::new(move || run_after(a)) as Box<dyn FnOnce()>);
        crate::motion::retire_faded(hwnd, within, Box::new(self), then);
    }

    /// Closes, fading away (quickly without animations). An app opens
    /// right away, not after the fade; the rest waits until the window is
    /// gone. Asked again while closing: nothing more.
    fn finish(&mut self, after: Option<After>) {
        if self.closing || self.done {
            return;
        }
        self.closing = true;
        // A menu up, or asked for: it goes with the overlay.
        self.menu = None;
        if self.menu_open {
            crate::popup::cancel();
        }
        self.stop_input();
        // Out of the way at once: clicks and keys reach what is under it
        // while it fades. If it cannot be, it goes now.
        if !self.let_through(after.is_none()) {
            crate::log!("launchpad: cannot let clicks through; closing at once");
            self.done = true;
            self.after = after;
            return;
        }
        // An app starts now (a helper thread: a slow start never holds the
        // window); the rest once the window is gone.
        match after {
            Some(After::Open(target)) => {
                crate::launch::open_soon(crate::launch::Target::new(&target, &name_of(&target), crate::launch::Source::Launchpad));
            }
            Some(After::Admin(target)) => {
                crate::launch::open_soon(crate::launch::Target::new(&target, &name_of(&target), crate::launch::Source::Launchpad).verb("runas"));
            }
            a => self.after = a,
        }
        // (Also when an app opens: the same 180 ms.)
        let plan = motion::scrim(self.animate(), false, crate::effects::current().speed);
        if let Err(e) = self.play_close(plan) {
            crate::log!("launchpad: closing animation failed: {e:?}");
            self.done = true;
            return;
        }
        // Closed now: the loop ends and the window is retired while the
        // animation plays (gone when its batch completes, or after the
        // fallback if that never comes).
        self.fade_ms = Some(motion::close_fallback(plan.0));
        self.done = true;
    }

    /// Closing: the overlay lets clicks through for every app, the way
    /// every fading popup does (crate::motion::click_through), and is never
    /// activated again; `give_back` (nothing is being opened): the window
    /// that was in front before gets the focus back now, not when the
    /// window goes. (An app being opened takes the focus itself.) False if
    /// it could not be made click-through.
    fn let_through(&self, give_back: bool) -> bool {
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
            if !crate::motion::click_through(self.hwnd) {
                return false;
            }
            let fg = GetForegroundWindow();
            if give_back && fg == self.hwnd && can_take_focus(self.prev_fg) {
                let _ = SetForegroundWindow(self.prev_fg);
            }
        }
        true
    }

    /// Closing: what was under way with the mouse, the keyboard or the IME
    /// stops (input is ignored from here on), and so does the caret.
    fn stop_input(&mut self) {
        self.arr_cancel_long_press();
        self.arr_cancel_drag();
        self.unpress();
        self.press = None;
        self.drag = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), CARET_TIMER);
            // (WM_CAPTURECHANGED finds the overlay borrowed: no slide back.)
            let _ = ReleaseCapture();
            let himc = ImmGetContext(self.hwnd);
            if !himc.is_invalid() {
                let _ = ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                let _ = ImmReleaseContext(self.hwnd, himc);
            }
        }
    }

    // ---- motion -------------------------------------------------------------------

    /// Launchpad animates (crate::motion, its own effect, not in high
    /// contrast).
    fn animate(&self) -> bool {
        motion::allowed(crate::motion::full(), crate::effects::items().launchpad_motion, self.theme.high_contrast)
    }

    /// The field, the dots, the message and the progress ring.
    fn chrome(&self) -> [&SpriteVisual; 4] {
        [&self.field_v, &self.dots_v, &self.msg_v, &self.spin_v]
    }

    /// The animation settings changed while open: if Launchpad no longer
    /// animates, everything jumps to where it is going (a close ends now).
    fn motion_changed(&mut self) {
        // The ring turns only while Launchpad animates (both ways).
        let ring = self.spin_at.map(|r| r.at);
        self.spin(ring);
        if self.animate() {
            return;
        }
        if self.closing {
            self.done = true;
            // (A sent message: wake the loop so it sees `done`.)
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            }
            return;
        }
        self.show_all();
        let x = self.strip_x;
        self.slide_to(x, false);
        self.arr_motion_changed();
    }

    /// The opening animation failed part way: everything as it should end.
    fn show_all(&self) {
        let opacity = HSTRING::from("Opacity");
        let _ = self.grid_box.StopAnimation(&HSTRING::from("Scale"));
        let _ = self.grid_box.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 });
        let mut all: Vec<&SpriteVisual> = vec![&self.backdrop, &self.tint];
        all.extend(self.chrome());
        all.extend(self.pages.values().flat_map(|v| v.cells.iter().flat_map(|(i, l)| [i, l])));
        for v in all {
            let _ = v.StopAnimation(&opacity);
            let _ = v.SetOpacity(1.0);
        }
    }

    /// Opening: everything fades in together with the plate (no zoom, no
    /// stagger: the icons are just there), as `plan` says (motion::scrim).
    /// Keyframe animations started once: nothing runs per frame on our side.
    fn play_open(&self, plan: (i64, crate::motion::Curve)) -> Result<()> {
        let c = &self.gfx.compositor;
        let opacity = HSTRING::from("Opacity");
        let back = fade(c, Some(0.0), 1.0, plan)?;
        for v in [&self.backdrop, &self.tint] {
            v.StartAnimation(&opacity, &back)?;
        }
        // The page on screen (the pages either side are off screen).
        if let Some(v) = self.pages.get(&self.page) {
            for (icon, label) in &v.cells {
                icon.StartAnimation(&opacity, &back)?;
                label.StartAnimation(&opacity, &back)?;
            }
        }
        for v in self.chrome() {
            v.StartAnimation(&opacity, &back)?;
        }
        Ok(())
    }

    /// Closing: the plate and everything on it fade out together (no
    /// zoom), as `plan` says. Each fade starts from where it is now (a
    /// close during the opening turns back smoothly). WM_LP_CLOSED when
    /// done.
    fn play_close(&self, plan: (i64, crate::motion::Curve)) -> Result<()> {
        let c = &self.gfx.compositor;
        let opacity = HSTRING::from("Opacity");
        let batch = c.CreateScopedBatch(CompositionBatchTypes::Animation)?;
        let started = (|| -> Result<()> {
            let back = fade(c, None, 0.0, plan)?;
            for v in [&self.backdrop, &self.tint] {
                v.StartAnimation(&opacity, &back)?;
            }
            self.strip.StartAnimation(&opacity, &back)?;
            self.results.container.StartAnimation(&opacity, &back)?;
            for v in self.chrome() {
                v.StartAnimation(&opacity, &back)?;
            }
            Ok(())
        })();
        // (Ended either way: a batch left open would take in later ones.)
        batch.End()?;
        started?;
        let hwnd = self.hwnd.0 as isize;
        batch.Completed(&TypedEventHandler::<IInspectable, CompositionBatchCompletedEventArgs>::new(move |_, _| {
            // (To the retired window: it goes. If this is lost, its
            // fallback timer ends it.)
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_LP_CLOSED, WPARAM(0), LPARAM(0));
            }
            Ok(())
        }))?;
        Ok(())
    }

    /// Tells a screen reader what is selected: the window's name changes.
    fn announce(&mut self, text: String) {
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(text.as_str()));
            NotifyWinEvent(EVENT_OBJECT_NAMECHANGE, self.hwnd, OBJID_WINDOW.0, 0);
        }
        self.spoken = text;
        self.publish();
    }

    // ---- colors -------------------------------------------------------------

    fn hc(&self) -> Option<crate::syslook::HighContrast> {
        self.theme.hc.filter(|_| self.theme.high_contrast)
    }
    fn solid(&self) -> bool {
        self.theme.popup.material == "solid" || self.hc().is_some()
    }
    /// Text on the overlay: the translucent text levels (it is drawn over
    /// whatever shows through Launchpad's tint).
    fn ink(&self) -> D2D1_COLOR_F {
        rgba(self.theme.text_primary())
    }
    fn dim(&self) -> D2D1_COLOR_F {
        rgba(self.theme.text_secondary())
    }
    /// Over the blurred desktop (or the whole backdrop when solid).
    fn tint_color(&self) -> D2D1_COLOR_F {
        plate_tint(&self.theme)
    }

    /// Backdrop, tint, and everything drawn in theme colors, again.
    fn restyle(&mut self) {
        let c = self.gfx.compositor.clone();
        if self.solid() {
            let _ = self.backdrop.SetIsVisible(false);
        } else if let Ok(b) = crate::glass::scrim_backdrop(&self.gfx, &self.theme) {
            // The blur saturated as the glass's panels (no Luminosity
            // layer: the scrim is its own lightness).
            let _ = self.backdrop.SetBrush(&b);
            let _ = self.backdrop.SetIsVisible(true);
        }
        if let Ok(b) = c.CreateColorBrushWithColor(color(self.tint_color())) {
            let _ = self.tint.SetBrush(&b);
        }
        self.glow_restyle();
        // Names, the placeholder and folder tiles are drawn in theme colors.
        self.label_brushes.clear();
        self.arr.folder_brushes.clear();
        self.placeholder = None;
        let pages: Vec<usize> = self.pages.keys().copied().collect();
        for p in pages {
            self.bind_page(p);
        }
        self.lib_restyle();
        if self.searching {
            self.bind_results();
        }
        self.draw_field();
        self.draw_dots();
        // The ring in the new colors (and turning or not: high contrast).
        self.respin();
        self.update_msg();
        if self.arr.folder.is_some() {
            self.draw_panel();
            // (Its scroll edge made again: high contrast has a line.)
            self.unedge_folder();
            self.bind_folder();
        }
        if self.arr.edit {
            // The frame in the new corners and colors; its scale and any
            // drag go on as they were.
            self.draw_edit_frame();
            self.draw_done();
        }
        // (What was lit, in the new colors.)
        self.update_sel();
    }

    // ---- the listing ----------------------------------------------------------

    fn per_page(&self) -> usize {
        self.grid.per_page()
    }
    /// Pages (one more, empty, while an icon is dragged: dropping there
    /// makes it; else the App Library's after the Home pages).
    fn pages_n(&self) -> usize {
        if self.arr.dragging() {
            edit::drag_pages(&self.layout)
        } else {
            self.layout.len().max(1) + usize::from(self.lib_page().is_some())
        }
    }
    /// Things in what is shown (the pages' apps and folders, or the search
    /// results; on the Library page its tiles).
    fn listing_len(&self) -> usize {
        if self.searching {
            self.found.len()
        } else if self.on_lib() {
            self.lib.groups.len()
        } else {
            self.layout.iter().map(|p| p.len()).sum()
        }
    }
    /// Browsing, listing index `i` is page x per_page + slot (pages need
    /// not be full).
    fn pos_of(&self, i: usize) -> Pos {
        let pp = self.per_page();
        Pos::new(i / pp, i % pp)
    }
    fn flat(&self, p: Pos) -> usize {
        p.page * self.per_page() + p.slot
    }
    fn page_len(&self, p: usize) -> usize {
        self.layout.get(p).map_or(0, |v| v.len())
    }
    /// Browsing: the app or folder at listing index `i`.
    fn item_at(&self, i: usize) -> Option<&LpItem> {
        let p = self.pos_of(i);
        self.layout.get(p.page).and_then(|v| v.get(p.slot))
    }
    /// Is listing index `i` something on screen to select?
    fn valid_sel(&self, i: usize) -> bool {
        if self.searching { i < self.found.len() } else { self.item_at(i).is_some() }
    }
    /// The app at listing index `i` (None for a folder).
    fn app_of(&self, i: usize) -> Option<&App> {
        if self.searching {
            return self.found.get(i).and_then(|&k| self.apps.get(k));
        }
        match self.item_at(i) {
            Some(LpItem::App(id)) => self.app_by_id(id),
            _ => None,
        }
    }
    fn app_by_id(&self, id: &str) -> Option<&App> {
        self.by_id.get(id).and_then(|&k| self.apps.get(k))
    }
    /// Browsing: the listing index of app `id` on the pages (not in a folder).
    fn flat_of_id(&self, id: &str) -> Option<usize> {
        let pp = self.per_page();
        self.layout.iter().enumerate().find_map(|(p, v)| v.iter().position(|it| matches!(it, LpItem::App(a) if a == id)).map(|s| p * pp + s))
    }
    /// The listing index of slot `slot` on screen.
    fn index_of_slot(&self, slot: usize) -> Option<usize> {
        if slot >= self.per_page() {
            return None;
        }
        if self.searching {
            (slot < self.found.len()).then_some(slot)
        } else {
            (slot < self.page_len(self.page)).then(|| self.page * self.per_page() + slot)
        }
    }

    /// The pages again from the saved arrangement (or, once arranged while
    /// open, from the pages as they are) and the apps installed now.
    /// Home: the saved pages only (with the Library; new apps are in it),
    /// or with every app not placed after them (the Library off, or a
    /// config not migrated yet); hidden apps never. An older config is
    /// migrated here, the first time the whole list is read.
    fn arrange_apps(&mut self) {
        self.by_id = self.apps.iter().enumerate().map(|(i, a)| (a.id.clone(), i)).collect();
        let ids: Vec<String> = self.apps.iter().map(|a| a.id.clone()).collect();
        let mut base = if self.arr.dirty { layout_store::to_config(&self.layout) } else { self.arr.saved.clone() };
        let pp = self.per_page();
        if self.apps_complete && !ids.is_empty() && layout_store::needs_migration(&self.lib.cfg) {
            let from = crate::config::LaunchpadCfg { layout: base.clone(), ..self.lib.cfg.clone() };
            // A Home never arranged leaves uninstallers, web links and
            // documents in the Library (cleanup.rs); an arranged one is kept.
            let pairs: Vec<(String, String)> = self.apps.iter().map(|a| (a.id.clone(), a.name.clone())).collect();
            let reasons = super::cleanup::reasons(&pairs);
            let clutter = |id: &str| reasons.get(id).is_some_and(|r| super::cleanup::to_library(*r));
            if let Some(m) = layout_store::migrate(&from, &ids, &clutter) {
                crate::log!("launchpad: Home is the {} pages shown until now; new apps go to the App Library", m.layout.len());
                base = m.layout.clone();
                if !self.arr.dirty {
                    self.arr.saved = m.layout.clone();
                }
                self.lib.cfg = m;
                self.lp_save();
            }
        }
        let append = !self.lib.cfg.show_library || layout_store::needs_migration(&self.lib.cfg);
        let old = std::mem::take(&mut self.layout);
        self.layout = layout_store::home(&base, &ids, &self.lib.cfg.hidden, append, pp);
        // Folders keep their session ids (screen readers hold on to them).
        keep_folder_ids(&old, &mut self.layout);
        self.lib_recompute();
    }

    fn first_on_page(&self) -> Option<usize> {
        self.index_of_slot(0)
    }
    /// x of page `p` in the strip (pages run right to left in RTL).
    fn page_x(&self, p: usize) -> f32 {
        let x = p as f32 * self.w;
        if self.rtl { -x } else { x }
    }

    /// The app list was read again: a new order or new apps re-make the pages.
    /// True: the pages were made again (positions may have changed).
    fn reload_apps(&mut self) -> bool {
        let snap = crate::search::apps_snapshot();
        if snap.gen == self.apps_gen {
            return false;
        }
        // Pages arranged and not saved yet are never merged with an
        // incomplete list (the apps it lacks would lose their places).
        if !takes_snapshot(self.arr.dirty, snap.complete()) {
            crate::log!("launchpad: an incomplete app list while arranging; kept the one shown");
            return false;
        }
        let was_complete = self.apps_complete;
        (self.apps_gen, self.apps_complete) = (snap.gen, snap.complete());
        let apps = if Arc::ptr_eq(&snap.list, &self.raw) {
            self.apps.clone()
        } else {
            self.raw = snap.list;
            let loc = list::locale();
            list::sorted(&self.raw, |a, b| list::compare(&loc, a, b))
        };
        // The same apps, now read in full: the migration, the Library and
        // its menus wait for that, so the pages are made again too.
        if !remake_needed(apps != self.apps, was_complete, self.apps_complete) {
            return false;
        }
        let sel_app = self.sel.and_then(|i| self.app_of(i)).cloned();
        // An open menu names an app that may have moved or gone: close it.
        self.close_menu();
        self.apps = apps;
        // A drag under way is put back, an open folder closed: the pages
        // are made again.
        self.arr_reset_for_reload();
        self.arrange_apps();
        self.drop_cells();
        let page = self.page.min(self.pages_n() - 1);
        if self.searching {
            self.changed();
        } else {
            self.sel = sel_app.and_then(|a| self.flat_of_id(&a.id)).filter(|&i| grid::page_of(i, self.per_page()) == page);
        }
        self.go_page(page, false);
        self.update_msg();
        true
    }

    /// Every page's cells (the Library's too) go: the pages were made
    /// again; they are made anew as they are reached.
    fn drop_cells(&mut self) {
        self.detach_sel();
        if let Ok(ch) = self.strip.Children() {
            let _ = ch.RemoveAll();
        }
        self.pages.clear();
        self.lib.view = None;
    }

    /// The search results' cells for the grid now (shown while searching).
    fn make_results(&mut self) {
        let Some(v) = self.make_view(None) else { return };
        if self.sel_in == Some(None) {
            self.detach_sel();
        }
        let _ = self.grid_box.Children().and_then(|ch| ch.Remove(&self.results.container));
        self.results = v;
        let _ = self.results.container.SetIsVisible(self.searching);
        let _ = self.grid_box.Children().and_then(|ch| ch.InsertAbove(&self.results.container, &self.strip));
    }

    /// The names' box measured again in the theme's font and text size
    /// (retheme): a new box lays the grid out anew (its cells and pages,
    /// the search results, the selection, what a click hits), on the page
    /// shown and with what was selected selected again where they still
    /// are. A drag under way is put back, a press forgotten and an open
    /// folder closed (laid out on the old grid); pages arranged and not
    /// saved yet stay as they are (arrange_apps).
    fn regrid(&mut self) {
        let two_lines = lines_h(&self.dw, &self.theme.font.family, grid::LABEL_SIZE * crate::fonts::scale(), 2).unwrap_or(0.0);
        let g = grid::grid(self.work.w(), self.work.h(), two_lines);
        if g == self.grid {
            return;
        }
        // An open menu names an app that may move to another page.
        self.close_menu();
        self.arr_reset_for_reload();
        self.unpress();
        self.press = None;
        self.drag = None;
        // What is on screen, found again on the pages made for the new grid.
        let on_lib = self.on_lib();
        let browsing = !self.searching && !on_lib;
        // (The selection with the page it is on: regrid_spot goes by it
        // only on the page shown.)
        let sel_item = self.sel.filter(|_| browsing).and_then(|i| self.item_at(i).map(|it| (it.clone(), self.pos_of(i).page)));
        let first_item = self.first_on_page().filter(|_| browsing).and_then(|i| self.item_at(i)).cloned();
        self.grid = g;
        let (gx, gy) = grid_centre(&self.grid, &self.work);
        let _ = self.grid_box.SetCenterPoint(Vector3 { X: gx, Y: gy, Z: 0.0 });
        // The names' surfaces were made for the old box.
        self.label_brushes.clear();
        self.placeholder = None;
        self.arrange_apps();
        self.drop_cells();
        self.make_results();
        let page = if on_lib {
            self.lib_page().unwrap_or(self.page)
        } else {
            let sel_item = sel_item.as_ref().map(|(it, p)| (it, *p));
            let (page, sel) = regrid_spot(&self.layout, self.per_page(), sel_item, first_item.as_ref(), self.page);
            if browsing {
                self.sel = sel;
            }
            page
        };
        if self.searching {
            // As many results as a page holds now.
            self.found = list::filter(&self.apps, &self.query(), self.per_page());
            let n = self.found.len();
            self.sel = self.sel.filter(|_| n > 0).map(|i| i.min(n - 1));
            self.bind_results();
        }
        // (The page shown keeps its place: go_page only follows a change.)
        self.page = page.min(self.pages_n() - 1);
        self.go_page(self.page, false);
        self.update_msg();
    }

    // ---- cells ------------------------------------------------------------------

    /// `v` DIP on whole device pixels: a picture shown 1:1 there is sharp
    /// (anywhere else the brush filters it across two pixels).
    fn snap(&self, v: f32) -> f32 {
        snap_to(v, self.s)
    }

    /// An icon's edge in DIP as drawn: exactly `px` device pixels.
    fn icon_dip(&self) -> f32 {
        self.px as f32 / self.s
    }

    /// The size the shell is asked for: the nearest real frame size at or
    /// above `px` (loader::frame_px; it is drawn down to `px` with D2D's
    /// cubic filter, never shrunk by the brush).
    fn src_px(&self) -> u32 {
        loader::frame_px(self.px)
    }

    /// The icon of slot `slot` of grid `g` (monitor DIP): `icon_dip`
    /// square, where the grid centres it, on whole device pixels.
    fn icon_rect(&self, g: &Grid, slot: usize) -> Rect {
        icon_box(g, slot, self.rtl, &self.work, self.s, self.px)
    }

    /// A name's surface size in DIP (whole device pixels: shown 1:1).
    fn label_size(&self) -> (f32, f32) {
        let s = self.s;
        ((self.grid.label_w() * s).ceil() / s, (self.grid.label_h * s).ceil() / s)
    }

    /// The name under the icon of slot `slot` of grid `g`.
    fn label_rect(&self, g: &Grid, slot: usize) -> Rect {
        let r = g.label(slot, self.rtl).offset(self.work.l, self.work.t);
        let (w, h) = self.label_size();
        let (l, t) = (self.snap(r.cx() - w / 2.0), self.snap(r.t));
        Rect::new(l, t, l + w, t + h)
    }

    /// `n` cells placed on grid `g` in a container at `x`, empty.
    fn make_cells(&self, g: &Grid, n: usize, x: f32) -> Option<View> {
        let c = &self.gfx.compositor;
        let container = c.CreateContainerVisual().ok()?;
        container.SetOffset(Vector3 { X: x, Y: 0.0, Z: 0.0 }).ok()?;
        let ch = container.Children().ok()?;
        let mut cells = Vec::new();
        for slot in 0..n {
            let ir = self.icon_rect(g, slot);
            let lr = self.label_rect(g, slot);
            let icon = c.CreateSpriteVisual().ok()?;
            icon.SetOffset(Vector3 { X: ir.l, Y: ir.t, Z: 0.0 }).ok()?;
            icon.SetSize(Vector2 { X: ir.w(), Y: ir.h() }).ok()?;
            // (It lifts and grows around its middle.)
            icon.SetCenterPoint(Vector3 { X: ir.w() / 2.0, Y: ir.h() / 2.0, Z: 0.0 }).ok()?;
            icon.SetIsVisible(false).ok()?;
            let label = c.CreateSpriteVisual().ok()?;
            label.SetOffset(Vector3 { X: lr.l, Y: lr.t, Z: 0.0 }).ok()?;
            label.SetSize(Vector2 { X: lr.w(), Y: lr.h() }).ok()?;
            label.SetIsVisible(false).ok()?;
            ch.InsertAtTop(&icon).ok()?;
            ch.InsertAtTop(&label).ok()?;
            cells.push((icon, label));
        }
        Some(View { container, cells, bound: vec![None; n] })
    }

    /// Cells for a page (Some) or the results (None), placed, empty.
    fn make_view(&self, page: Option<usize>) -> Option<View> {
        let x = page.map(|p| self.page_x(p)).unwrap_or(0.0);
        self.make_cells(&self.grid, self.per_page(), x)
    }

    /// Page `p`'s cells exist (made and filled the first time).
    fn ensure_page(&mut self, p: usize) {
        if p >= self.pages_n() {
            return;
        }
        if self.lib_page() == Some(p) {
            self.ensure_lib();
            return;
        }
        if self.pages.contains_key(&p) {
            return;
        }
        let Some(v) = self.make_view(Some(p)) else { return };
        if self.strip.Children().and_then(|ch| ch.InsertAtTop(&v.container)).is_err() {
            return;
        }
        self.pages.insert(p, v);
        self.bind_page(p);
    }

    /// An app's tile (new: a dot by its name; hidden: dimmed).
    fn app_tile(&self, a: &App) -> Tile {
        Tile { key: a.target(), name: a.name.clone(), folder: None, new: self.lib.new.contains(&a.id), dim: self.lib.is_hidden(&a.id) }
    }

    /// The tile of an item on the pages (None: an app not installed).
    fn item_tile(&self, it: &LpItem) -> Option<Tile> {
        match it {
            LpItem::App(id) => self.app_by_id(id).map(|a| self.app_tile(a)),
            LpItem::Folder { name, apps, .. } => {
                let targets: Vec<String> = apps.iter().filter_map(|id| self.app_by_id(id)).map(|a| a.target()).collect();
                let shown = folders::shown_name(name, t("launchpad.folder.default"));
                // Named by what it shows: a new name or other apps draw anew.
                let key = format!("folder\u{1f}{shown}\u{1f}{}", folders::mini(&targets).join("\u{1f}"));
                Some(Tile { key, name: shown, folder: Some(targets), new: false, dim: false })
            }
        }
    }

    /// What page `p`'s slots show (with the gap a dragged icon makes).
    fn page_tiles(&self, p: usize) -> Vec<Option<Tile>> {
        let gap = self.arr.gap_on(p);
        let items = self.layout.get(p);
        (0..self.per_page()).map(|s| edit::item_in_slot(s, gap).and_then(|k| items.and_then(|v| v.get(k))).and_then(|it| self.item_tile(it))).collect()
    }

    fn bind_page(&mut self, p: usize) {
        if self.lib_page() == Some(p) {
            self.bind_lib();
            return;
        }
        let entries = self.page_tiles(p);
        self.bind(Cells::Page(p), entries, p == self.page);
    }

    fn bind_results(&mut self) {
        let pp = self.per_page();
        let entries: Vec<Option<Tile>> = (0..pp).map(|s| self.app_of(s).map(|a| self.app_tile(a))).collect();
        self.bind(Cells::Results, entries, true);
    }

    /// Shows `entries` in cells; icons not loaded yet get the placeholder
    /// and are asked for (`first`: ahead of the queue). In edit mode a tile
    /// that moved to another cell slides there from its old one (the
    /// others making room), and the one just dropped from where it was let
    /// go.
    fn bind(&mut self, which: Cells, entries: Vec<Option<Tile>>, first: bool) {
        let brushes: Vec<Option<(Option<CompositionSurfaceBrush>, Option<CompositionSurfaceBrush>)>> =
            entries.iter().map(|e| e.as_ref().map(|t| (self.tile_brush(t), self.tile_label(t)))).collect();
        let placeholder = self.placeholder();
        let src = self.src_px();
        let mut missing: Vec<String> = Vec::new();
        for (e, b) in entries.iter().zip(&brushes) {
            let (Some(t), Some((ib, _))) = (e, b) else { continue };
            match &t.folder {
                None if ib.is_none() => missing.push(t.key.clone()),
                Some(apps) => missing.extend(folders::mini(apps).iter().filter(|k| loader::cached(k, src).is_none()).cloned()),
                None => {}
            }
        }
        // (Animating they glide there; else they come in in place: arrange::slide.)
        let (slide, glides) = (self.arr.edit, self.animate());
        let dropped = self.arr.drop_at.clone();
        let mut landed = false;
        let c = self.gfx.compositor.clone();
        let view = match which {
            Cells::Page(p) => self.pages.get_mut(&p),
            Cells::Results => Some(&mut self.results),
            Cells::Folder => self.arr.folder_view.as_mut(),
        };
        let Some(view) = view else { return };
        let old = view.bound.clone();
        let rests: Vec<(Vector3, Vector3)> = view.cells.iter().map(|(i, l)| (i.Offset().unwrap_or_default(), l.Offset().unwrap_or_default())).collect();
        for (i, (icon, label)) in view.cells.iter().enumerate() {
            match (entries.get(i).cloned().flatten(), brushes.get(i).cloned().flatten()) {
                (Some(tile), Some((ib, lb))) => {
                    let ib = ib.or_else(|| placeholder.clone());
                    if let Some(b) = &ib {
                        let _ = icon.SetBrush(b);
                    }
                    if let Some(lb) = &lb {
                        let _ = label.SetBrush(lb);
                    }
                    // (Hidden apps, found by search: dimmed.)
                    let o = if tile.dim { super::library::DIMMED } else { 1.0 };
                    let _ = icon.SetOpacity(o);
                    let _ = label.SetOpacity(o);
                    let _ = icon.SetIsVisible(ib.is_some());
                    let _ = label.SetIsVisible(lb.is_some());
                    if slide && old[i].as_deref() != Some(tile.key.as_str()) {
                        let from = match old.iter().position(|k| k.as_deref() == Some(tile.key.as_str())) {
                            Some(j) => Some((rests[j].0, rests[j].1)),
                            None => dropped.as_ref().filter(|(k, _)| *k == tile.key).map(|(_, at)| {
                                landed = true;
                                let (dx, dy) = (at.X - rests[i].0.X, at.Y - rests[i].0.Y);
                                (*at, Vector3 { X: rests[i].1.X + dx, Y: rests[i].1.Y + dy, Z: 0.0 })
                            }),
                        };
                        if let Some((fi, fl)) = from {
                            let _ = arrange::slide(&c, icon, fi, rests[i].0, glides);
                            let _ = arrange::slide(&c, label, fl, rests[i].1, glides);
                        }
                    }
                    view.bound[i] = Some(tile.key);
                }
                _ => {
                    let _ = icon.SetIsVisible(false);
                    let _ = label.SetIsVisible(false);
                    view.bound[i] = None;
                }
            }
        }
        if landed {
            self.arr.drop_at = None;
        }
        let want: Vec<String> = missing.into_iter().filter(|k| first || !self.asked.contains(k)).collect();
        if !want.is_empty() {
            self.asked.extend(want.iter().cloned());
            loader::want(&want, src, self.hwnd, first);
        }
    }

    /// A tile's picture: an app's icon (if loaded), a folder's mini grid.
    fn tile_brush(&mut self, t: &Tile) -> Option<CompositionSurfaceBrush> {
        match &t.folder {
            None => self.icon_brush(&t.key),
            Some(apps) => self.folder_brush(&t.key, apps),
        }
    }

    /// The icon of `key` as a brush, if it has been loaded: drawn once at
    /// exactly `px` device pixels (D2D's high-quality cubic filter from the
    /// larger frame the shell gave), shown 1:1.
    fn icon_brush(&mut self, key: &str) -> Option<CompositionSurfaceBrush> {
        if let Some(b) = self.icon_brushes.get(key) {
            return Some(b.clone());
        }
        let icon = loader::cached(key, self.src_px())??;
        // (As the app draws it: no squircle cut here.)
        let surface = self.gfx.surface_fit(icon.width, icon.height, &icon.pixels, self.px, false).ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        self.icon_brushes.insert(key.to_string(), b.clone());
        Some(b)
    }

    /// A tile's name (a new app's with its dot).
    fn tile_label(&mut self, t: &Tile) -> Option<CompositionSurfaceBrush> {
        if t.new {
            self.label_brush(&format!("{}\u{1f}new", t.key), &t.name, true)
        } else {
            self.label_brush(&t.key, &t.name, false)
        }
    }

    /// An app's name: centered, at most two lines, cut with "…"; `new`: the
    /// "new" dot before its first line (applib::paint_new_dot).
    fn label_brush(&mut self, key: &str, name: &str, new: bool) -> Option<CompositionSurfaceBrush> {
        if let Some(b) = self.label_brushes.get(key) {
            return Some(b.clone());
        }
        let (lw, lh) = (self.grid.label_w(), self.grid.label_h);
        // (No room for names: none shown.)
        if lh <= 0.0 {
            return None;
        }
        let ink = self.ink();
        let (l, _, _) = self.layout_in(name, grid::LABEL_SIZE, false, lw, 2, DWRITE_TEXT_ALIGNMENT_CENTER, lh)?;
        // Where the text starts (its leading edge; the right one right to left).
        let dot = new.then(|| (Self::x_at(&l, 0), self.rtl, self.new_dot_color()));
        let s = self.s;
        let surface = self
            .gfx
            .draw((lw * s).ceil() as u32, (lh * s).ceil() as u32, |dc, ox, oy| unsafe {
                dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
                let b = dc.CreateSolidColorBrush(&ink, None)?;
                dc.DrawTextLayout(Vector2 { X: 0.0, Y: 0.0 }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                if let Some((x, rtl, c)) = dot {
                    let d = super::library::NEW_DOT;
                    let cx = if rtl { x + d } else { x - d };
                    applib::paint_new_dot(dc, cx.clamp(d / 2.0, lw - d / 2.0), grid::LABEL_SIZE * crate::fonts::scale() * 0.68, c)?;
                }
                dc.SetTransform(&Matrix3x2::identity());
                Ok(())
            })
            .ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        self.label_brushes.insert(key.to_string(), b.clone());
        Some(b)
    }

    /// Where an icon goes until it is loaded: the tile's ground alone, in
    /// the icons' own shape (the squircle, an icon's whole edge) and the
    /// raised fill at rest; high contrast: its outline, no fill (12.3).
    fn placeholder(&mut self) -> Option<CompositionSurfaceBrush> {
        if let Some(b) = &self.placeholder {
            return Some(b.clone());
        }
        let px = self.px;
        let surface = match self.theme.outline_of(Role::Raised, State::Rest) {
            Some((c, w)) => self.gfx.squircle_ring(px, (w * self.s).max(1.0), c),
            None => {
                // (The tile's ground: raised, Flat v2's card light round
                // it inside the picture, as a folder's tile.)
                let (theme, s) = (&self.theme, self.s);
                self.gfx.draw(px, px, move |dc, ox, oy| unsafe {
                    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    arrange::raised_squircle(dc, ox, oy, px as f32, theme, None, s)
                })
            }
        }
        .ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        self.placeholder = Some(b.clone());
        Some(b)
    }

    /// Icons came from the loader: put them on the cells showing them (and
    /// draw again the folders that show them).
    fn icons_arrived(&mut self) {
        let done = loader::take_done();
        let src = self.src_px();
        let mut keys: Vec<String> = done.into_iter().filter(|(_, px)| *px == src).map(|(k, _)| k).collect();
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            return;
        }
        let in_folder = |fk: &str| fk.starts_with("folder\u{1f}") && fk.split('\u{1f}').skip(2).any(|t| keys.iter().any(|k| k == t));
        // Pages with a folder showing one of them are bound again.
        let again: Vec<usize> = self.pages.iter().filter(|(_, v)| v.bound.iter().flatten().any(|k| in_folder(k))).map(|(p, _)| *p).collect();
        self.arr.folder_brushes.retain(|fk, _| !in_folder(fk));
        let shown: HashSet<String> =
            self.pages.values().chain(std::iter::once(&self.results)).chain(self.arr.folder_view.iter()).flat_map(|v| v.bound.iter().flatten().cloned()).collect();
        for k in keys.iter().filter(|k| shown.contains(*k)) {
            let Some(b) = self.icon_brush(k) else { continue };
            for v in self.pages.values().chain(std::iter::once(&self.results)).chain(self.arr.folder_view.iter()) {
                for (i, key) in v.bound.iter().enumerate() {
                    if key.as_deref() == Some(k.as_str()) {
                        let _ = v.cells[i].0.SetBrush(&b);
                    }
                }
            }
        }
        for p in again {
            self.bind_page(p);
        }
        // The Library's tiles that show one of them are drawn again.
        self.lib_icons_arrived(&keys);
    }

    // ---- pages -------------------------------------------------------------------

    /// Shows page `p` (sliding there when `animate` and animations are on);
    /// its neighbours get their cells too, for a swipe.
    fn go_page(&mut self, p: usize, animate: bool) {
        // The page count may have changed (a drag, a drop, a new page): the
        // Library's visuals where its page is now, or hidden.
        self.sync_lib_view();
        let p = p.min(self.pages_n() - 1);
        let changed = p != self.page;
        if changed {
            self.dots_back = p < self.page;
        }
        self.page = p;
        if self.pages.contains_key(&p) {
            // Its icons go ahead of the queue.
            self.bind_page(p);
        } else {
            self.ensure_page(p);
        }
        if !self.first_frame {
            self.ensure_neighbours();
        }
        self.slide_to(-self.page_x(p), animate);
        if changed && !self.searching && self.focus == Focus::Grid {
            // The selection follows to the same place on the new page (the
            // Library: its first tile, or the one chosen before).
            let slot = self.sel.map(|s| s % self.per_page()).unwrap_or(0);
            self.sel = (0..=slot).rev().find_map(|s| self.index_of_slot(s));
            if self.on_lib() {
                let n = self.lib.groups.len();
                self.lib.sel = (n > 0).then(|| self.lib.sel.unwrap_or(0).min(n - 1));
            }
        } else if changed && !self.searching {
            // The keyboard in the field: a
            // selection left on the page that went is stale (Tab back to
            // the apps picks one on the page shown: set_focus).
            self.sel = sel_kept(self.sel, p, self.per_page());
        }
        self.draw_dots();
        self.update_sel();
    }

    /// The pages either side get their cells (for a swipe), the page on
    /// screen's icons first.
    fn ensure_neighbours(&mut self) {
        let p = self.page;
        if p + 1 < self.pages_n() {
            self.ensure_page(p + 1);
        }
        if p > 0 {
            self.ensure_page(p - 1);
        }
    }

    /// The strip to `x`. A page turn glides there on the GLIDE spring from
    /// wherever the strip is at the speed it has (7.4: a turn under way or a
    /// swipe let go carries on, it never starts over). Not animating: it
    /// is there at once and the page comes in over 60 ms (12.2).
    fn slide_to(&mut self, x: f32, animate: bool) {
        use crate::motion::{SpringRun, SPRING_GLIDE};
        let fling = self.fling.take();
        let prop = HSTRING::from("Offset.X");
        let now = Instant::now();
        let c = self.gfx.compositor.clone();
        if !(animate && self.animate()) {
            let turned = animate && (x - self.strip_x).abs() > 0.5;
            self.strip_x = x;
            self.strip_run = SpringRun::still(SPRING_GLIDE, x, now);
            let _ = self.strip.StopAnimation(&prop);
            let _ = self.strip.SetOffset(Vector3 { X: x, Y: 0.0, Z: 0.0 });
            if turned {
                let plan = crate::motion::fade_plan(crate::motion::Mode::Reduced, crate::motion::REDUCED_MS, crate::motion::LINEAR, 1.0);
                let _ = fade(&c, Some(0.0), 1.0, plan).and_then(|a| self.strip.StartAnimation(&HSTRING::from("Opacity"), &a));
            }
            return;
        }
        self.strip_x = x;
        // Ending a swipe: from where the finger left it, as fast as it went.
        let run = match fling {
            Some((from, speed)) => SpringRun { spring: SPRING_GLIDE, from, velocity: speed, to: from, since: now, stretch: 1.0 },
            None => self.strip_run,
        };
        match crate::motion::spring_scalar(&c, &self.strip, "Offset.X", &run, x) {
            Ok(r) => self.strip_run = r,
            Err(_) => {
                let _ = self.strip.StopAnimation(&prop);
                let _ = self.strip.SetOffset(Vector3 { X: x, Y: 0.0, Z: 0.0 });
                self.strip_run = SpringRun::still(SPRING_GLIDE, x, now);
            }
        }
    }

    /// One page on, in the direction of the screen (`right`: the arrow /
    /// swipe points right).
    fn page_visual(&mut self, right: bool) {
        let next = right != self.rtl;
        let p = if next { self.page + 1 } else { self.page.wrapping_sub(1) };
        if p < self.pages_n() {
            self.go_page(p, true);
            self.announce_page(p);
        }
    }

    /// Says which page is on screen (the Library by its name).
    fn announce_page(&mut self, p: usize) {
        if self.lib_page() == Some(p) {
            self.announce(t("launchpad.library.title").to_string());
        } else {
            self.announce(tf("launchpad.page", &[("n", &(p + 1).to_string()), ("count", &self.pages_n().to_string())]));
        }
    }

    // ---- selection ---------------------------------------------------------------

    /// Lights the selected app (or nothing): the pointer's ground or the
    /// keyboard's ring (select.rs), whichever moved last.
    fn update_sel(&mut self) {
        // The picked icons' rings follow the page shown (organize.rs).
        self.draw_picks();
        if self.on_lib() && self.arr.folder.is_none() {
            self.lib_update_sel();
            return;
        }
        let shown = self.sel.is_some()
            && (self.focus == Focus::Grid || self.searching)
            && select::sel_look(self.by_key, self.typed, self.pointer_out).is_some()
            && self.drag.is_none()
            && !self.arr.dragging()
            && self.arr.folder.is_none()
            && self.listing_len() > 0;
        let Some(sel) = self.sel.filter(|_| shown) else {
            self.sel_off(self.by_key);
            return;
        };
        let pp = self.per_page();
        let (where_, slot) = if self.searching { (None, sel) } else { (Some(grid::page_of(sel, pp)), sel % pp) };
        let parent = match where_ {
            None => Some(self.results.container.clone()),
            Some(p) => self.pages.get(&p).map(|v| v.container.clone()),
        };
        let Some(parent) = parent else {
            self.sel_off(true);
            return;
        };
        let cell = self.grid.item(slot, self.rtl).offset(self.work.l, self.work.t);
        let icon = self.icon_rect(&self.grid, slot);
        self.sel_on(where_, &parent, cell, icon, icon.w() * crate::theme::SQUIRCLE_ARC);
    }

    /// A press of the pointer: the keyboard's rings go (the selection's,
    /// the field's, the open folder's; crate::input), the last input wins.
    fn pointer_takes_over(&mut self) {
        let was = (self.by_key, self.field_ring);
        self.by_key = false;
        self.field_ring = false;
        self.reader_ring = false;
        self.done_focus_changed();
        if was.0 {
            self.update_sel();
            if self.arr.folder.is_some() {
                self.draw_folder_sel(false);
            }
        }
        if was.1 {
            self.draw_field();
        }
    }

    /// Selects listing index `i` from the keyboard (said aloud).
    fn select(&mut self, i: usize) {
        self.sel = Some(i);
        if !self.searching {
            let p = grid::page_of(i, self.per_page());
            if p != self.page {
                self.go_page(p, true);
            }
        }
        self.update_sel();
        // An app's name; a folder's name and how many apps it holds.
        let name = match (!self.searching).then(|| self.item_at(i)).flatten() {
            Some(LpItem::Folder { name, apps, .. }) => {
                let name = folders::shown_name(name, t("launchpad.folder.default"));
                Some(tf("launchpad.folder.a11y", &[("name", &name), ("count", &apps.len().to_string())]))
            }
            _ => self.app_of(i).map(|a| self.app_said(a)),
        };
        if let Some(n) = name {
            self.announce(n);
        }
    }

    /// The keyboard went to the field: a selection not
    /// on the page shown (browsing) is forgotten, never followed later.
    fn drop_stale_sel(&mut self) {
        if !self.searching {
            self.sel = sel_kept(self.sel, self.page, self.per_page());
        }
    }

    fn set_focus(&mut self, f: Focus) {
        if self.focus == f {
            return;
        }
        let was_field = self.focus == Focus::Field;
        self.focus = f;
        // (The keyboard's way in: Tab, an arrow, a screen reader. A click
        // in the field takes the ring off again: on_up.)
        self.field_ring = f == Focus::Field && self.by_key;
        match f {
            Focus::Grid if self.on_lib() => {
                let n = self.lib.groups.len();
                if n > 0 {
                    self.lib_select(self.lib.sel.unwrap_or(0).min(n - 1));
                }
            }
            Focus::Grid => {
                let (n, pp, page, searching) = (self.listing_len(), self.per_page(), self.page, self.searching);
                let _ = n;
                if self.sel.is_none_or(|s| !self.valid_sel(s) || (!searching && grid::page_of(s, pp) != page)) {
                    self.sel = self.first_on_page();
                }
                if let Some(i) = self.sel {
                    self.select(i);
                }
            }
            Focus::Field => {
                self.drop_stale_sel();
                self.announce(t("launchpad.placeholder").to_string());
            }
        }
        if was_field || f == Focus::Field {
            self.caret_on = true;
            self.draw_field();
        }
        self.update_sel();
    }

    // ---- the search field -----------------------------------------------------------

    fn query(&self) -> String {
        self.field.with_composition(&self.comp).0.into_iter().collect()
    }

    /// The text (or its composition) changed: filter again.
    fn changed(&mut self) {
        let q = self.query();
        let searching = !q.trim().is_empty();
        if searching {
            self.found = list::filter(&self.apps, &q, self.per_page());
            self.sel = (!self.found.is_empty()).then_some(0);
            // (Typed: Enter opens the first result, lit by the ground; the
            // ring only once a key moves the focus: crate::input.)
            self.typed = true;
        } else {
            self.typed = false;
            self.found.clear();
            if self.searching {
                self.sel = None;
            }
        }
        if searching != self.searching {
            self.searching = searching;
            let _ = self.strip.SetIsVisible(!searching);
            let _ = self.results.container.SetIsVisible(searching);
            self.draw_dots();
        }
        if searching {
            self.bind_results();
            if let Some(a) = self.app_of(0) {
                let said = self.app_said(a);
                self.announce(said);
            }
        }
        self.caret_on = true;
        unsafe {
            let blink = GetCaretBlinkTime();
            if blink != 0 && blink != u32::MAX {
                SetTimer(Some(self.hwnd), CARET_TIMER, blink, None);
            }
        }
        self.draw_field();
        self.update_msg();
        self.update_sel();
    }

    /// Where things go (relative to the work area): the top controls, the
    /// grid and the dots, never over one another.
    pub(super) fn regions(&self) -> grid::Regions {
        grid::regions(self.work.w(), self.work.h(), self.rtl)
    }

    fn field_rect(&self) -> Rect {
        self.regions().field().offset(self.work.l, self.work.t)
    }

    /// The text part of the field (x from its left edge): 12 from its
    /// ends, past the glass (16) and 8 more at its leading end.
    fn text_span(&self) -> (f32, f32) {
        let fw = self.field_rect().w();
        let lead = FIELD_PAD + size::ICON_S + space::S3;
        if self.rtl { (FIELD_PAD, fw - lead) } else { (lead, fw - FIELD_PAD) }
    }

    /// A text layout (own DirectWrite format: wrapping, trimming, RTL).
    fn layout(&self, text: &str, size: f32, bold: bool, max_w: f32, lines: usize, align: DWRITE_TEXT_ALIGNMENT) -> Option<(IDWriteTextLayout, f32, f32)> {
        self.layout_in(text, size, bold, max_w, lines, align, f32::INFINITY)
    }

    /// Like layout, inside a box `box_h` DIP tall: at most `lines` lines,
    /// as tall as the font really lays them out (its line metrics, with
    /// whatever fallback fonts the text needs), cut with "…" after the
    /// last that fits the box. Never a line cut through by the box.
    #[allow(clippy::too_many_arguments)]
    fn layout_in(&self, text: &str, size: f32, bold: bool, max_w: f32, lines: usize, align: DWRITE_TEXT_ALIGNMENT, box_h: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
        unsafe {
            let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
            let fam = HSTRING::from(self.theme.font.family.as_str());
            // The shared format: the UI language's locale, Hangul in Malgun
            // Gothic, never Bold (gfx::ui_format).
            let format = crate::gfx::ui_format(&self.dw, &fam, weight, size * crate::fonts::scale()).ok()?;
            let _ = format.SetWordWrapping(if lines > 1 { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
            let _ = format.SetTextAlignment(align);
            if self.rtl {
                let _ = format.SetReadingDirection(DWRITE_READING_DIRECTION_RIGHT_TO_LEFT);
            }
            if let Ok(sign) = self.dw.CreateEllipsisTrimmingSign(&format) {
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                let _ = format.SetTrimming(&trim, &sign);
            }
            // Names on more than one line: Korean breaks between words.
            let wide: Vec<u16> = if lines > 1 { crate::gfx::keep_words(text).encode_utf16().collect() } else { text.encode_utf16().collect() };
            let l = self.dw.CreateTextLayout(&wide, &format, max_w.max(1.0), UNCUT_H).ok()?;
            // Cut to the lines it may have, as tall as they are laid out
            // (their metrics not to be had: as tall as the font says).
            let hs = line_heights(&l);
            let guess = if hs.is_empty() { lines_guess(&self.dw, &self.theme.font.family, size * crate::fonts::scale(), lines) } else { 0.0 };
            let max_h = grid::lines_room(&hs, lines, box_h, guess);
            l.SetMaxHeight(max_h).ok()?;
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m).ok()?;
            Some((l, m.widthIncludingTrailingWhitespace.min(max_w), m.height.min(max_h)))
        }
    }

    fn x_at(l: &IDWriteTextLayout, pos: u32) -> f32 {
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = l.HitTestTextPosition(pos, false, &mut x, &mut y, &mut m);
        }
        x
    }

    /// The field's text: (text shown, where the composition starts, its
    /// whole layout, height, where the layout box starts and how wide the
    /// text is, from the text span's left). Never trimmed: the box is as
    /// wide as the text (at least the span), the field's clip and `shift`
    /// show the part around the caret. None when empty.
    fn field_layout(&self) -> Option<(Vec<char>, usize, IDWriteTextLayout, f32, f32, f32)> {
        let (shown, comp_at) = self.field.with_composition(&self.comp);
        if shown.is_empty() {
            return None;
        }
        let (x0, x1) = self.text_span();
        let span = x1 - x0;
        let wide: Vec<u16> = shown.iter().collect::<String>().encode_utf16().collect();
        unsafe {
            let fam = HSTRING::from(self.theme.font.family.as_str());
            let format = crate::gfx::ui_format(&self.dw, &fam, DWRITE_FONT_WEIGHT_NORMAL, FIELD_TEXT * crate::fonts::scale()).ok()?;
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            if self.rtl {
                let _ = format.SetReadingDirection(DWRITE_READING_DIRECTION_RIGHT_TO_LEFT);
            }
            // Measured first, then laid out in a box that holds all of it.
            let probe = self.dw.CreateTextLayout(&wide, &format, 100_000.0, 100.0).ok()?;
            let mut m = DWRITE_TEXT_METRICS::default();
            probe.GetMetrics(&mut m).ok()?;
            let text_w = m.widthIncludingTrailingWhitespace;
            let boxw = text_w.ceil().max(span) + 1.0;
            let l = self.dw.CreateTextLayout(&wide, &format, boxw, 100.0).ok()?;
            l.GetMetrics(&mut m).ok()?;
            // Right to left, the box's right edge sits on the span's.
            let base = if self.rtl { span - boxw } else { 0.0 };
            Some((shown, comp_at, l, m.height, base, text_w))
        }
    }

    fn caret_index(&self, comp_at: usize) -> usize {
        if self.comp.is_empty() { self.field.caret() } else { comp_at + self.comp_cursor.min(self.comp.len()) }
    }

    /// Keeps the caret inside the field (the text scrolls under it).
    fn update_shift(&mut self) {
        let (x0, x1) = self.text_span();
        let width = x1 - x0;
        let Some((shown, comp_at, l, _, base, text_w)) = self.field_layout() else {
            self.shift = 0.0;
            self.caret_x = if self.rtl { width } else { 0.0 };
            return;
        };
        // caret_x: from the span's left edge, before scrolling.
        let x = base + Self::x_at(&l, crate::search::utf16_len(&shown[..self.caret_index(comp_at)]));
        if text_w <= width {
            self.shift = 0.0;
        } else if x + self.shift > width - 2.0 {
            self.shift = width - 2.0 - x;
        } else if x + self.shift < 2.0 {
            self.shift = 2.0 - x;
        }
        self.caret_x = x;
    }

    fn draw_field(&mut self) {
        self.update_shift();
        let f = self.field_rect();
        // (Room for the keyboard's ring round it.)
        let m = FIELD_RING_M;
        let r = Rect::new(f.l - m, f.t - m, f.r + m, f.b + m);
        let me = &*self;
        self.paint_into(&self.field_v, r, |dc| unsafe {
            let mut t = Matrix3x2::default();
            dc.GetTransform(&mut t);
            dc.SetTransform(&Matrix3x2 { M31: t.M31 + m * t.M11, M32: t.M32 + m * t.M22, ..t });
            let res = me.paint_field(dc, f.w(), f.h());
            dc.SetTransform(&t);
            res
        });
        self.place_ime();
    }

    /// The field: its plate (paint_field_plate; the keyboard's focus, by
    /// Tab, is the ring round all of it) and its text.
    unsafe fn paint_field(&self, dc: &ID2D1DeviceContext, fw: f32, fh: f32) -> Result<()> {
        let ring = self.focus == Focus::Field && self.field_ring && !self.menu_open;
        paint_field_plate(dc, fw, fh, &self.theme, ring, self.rtl, self.dim())?;
        let (x0, x1) = self.text_span();
        dc.PushAxisAlignedClip(&D2D_RECT_F { left: x0 - 2.0, top: 0.0, right: x1 + 2.0, bottom: fh }, D2D1_ANTIALIAS_MODE_ALIASED);
        let res = self.paint_text(dc, x0, x1, fh);
        dc.PopAxisAlignedClip();
        res
    }

    unsafe fn paint_text(&self, dc: &ID2D1DeviceContext, x0: f32, x1: f32, fh: f32) -> Result<()> {
        let mid = fh / 2.0;
        let caret = self.focus == Focus::Field && self.caret_on && !self.menu_open;
        let Some((shown, comp_at, l, th, base, _)) = self.field_layout() else {
            // In edit mode it says how arranging works.
            let hint = if self.arr.edit { t("launchpad.edit.hint") } else { t("launchpad.placeholder") };
            // (A placeholder: the tertiary level, 4.2.)
            if let Some((l, _, h)) = self.layout(hint, FIELD_TEXT, false, x1 - x0, 1, DWRITE_TEXT_ALIGNMENT_LEADING) {
                draw_layout(dc, &l, x0, mid - h / 2.0, rgba(self.theme.text_of(crate::theme::Level::Tertiary, State::Rest)))?;
            }
            if caret {
                let (cx, half) = (x0 + self.caret_x, crate::theme::ramp::BODY.line / 2.0);
                fill(dc, self.ink(), Rect::new(cx, mid - half, cx + 1.5, mid + half))?;
            }
            return Ok(());
        };
        let left = x0 + self.shift;
        let top = mid - th / 2.0;
        let pos = |i: usize| left + base + Self::x_at(&l, crate::search::utf16_len(&shown[..i]));
        let (a, b) = self.field.selection();
        if self.comp.is_empty() && a != b {
            let c = match self.hc() {
                Some(hc) => rgba(hc.highlight),
                None => with_alpha(rgba(self.theme.accent()), 0.28),
            };
            let (pa, pb) = (pos(a), pos(b));
            fill(dc, c, Rect::new(pa.min(pb), top, pa.max(pb), top + th))?;
        }
        draw_layout(dc, &l, left + base, top, self.ink())?;
        if !self.comp.is_empty() {
            let (ca, cb) = (pos(comp_at), pos(comp_at + self.comp.len()));
            fill(dc, self.ink(), Rect::new(ca.min(cb), top + th - 1.0, ca.max(cb), top + th))?;
        }
        if caret {
            let cx = left + self.caret_x;
            fill(dc, self.ink(), Rect::new(cx, top + 2.0, cx + 1.5, top + th - 2.0))?;
        }
        Ok(())
    }

    /// Tells the IME where the caret is (its candidate list opens there).
    fn place_ime(&self) {
        let r = self.field_rect();
        let (x0, _) = self.text_span();
        let ds = self.s;
        let x = ((r.l + x0 + self.shift + self.caret_x) * ds) as i32;
        let top = ((r.t + 6.0) * ds) as i32;
        let bottom = ((r.b - 6.0) * ds) as i32;
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.is_invalid() {
                return;
            }
            let cf = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x, y: top }, rcArea: RECT::default() };
            let _ = ImmSetCompositionWindow(himc, &cf);
            let cand = CANDIDATEFORM { dwIndex: 0, dwStyle: CFS_EXCLUDE, ptCurrentPos: POINT { x, y: bottom }, rcArea: RECT { left: x, top, right: x + 1, bottom } };
            let _ = ImmSetCandidateWindow(himc, &cand);
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
    }

    // ---- small surfaces ---------------------------------------------------------------

    /// Shows a surface drawn for `at` (ceil(size x scale) px): on whole
    /// device pixels, exactly its pixel size (1:1, so text stays sharp).
    fn set_surface(&self, v: &SpriteVisual, surface: Result<windows::UI::Composition::CompositionDrawingSurface>, at: Rect) {
        match surface.and_then(|s| self.gfx.compositor.CreateSurfaceBrushWithSurface(&s)) {
            Ok(b) => {
                let s = self.s;
                let _ = b.SetStretch(CompositionStretch::Fill);
                let _ = v.SetBrush(&b);
                let _ = v.SetOffset(Vector3 { X: self.snap(at.l), Y: self.snap(at.t), Z: 0.0 });
                let _ = v.SetSize(Vector2 { X: (at.w() * s).ceil() / s, Y: (at.h() * s).ceil() / s });
                let _ = v.SetIsVisible(true);
            }
            Err(e) => crate::log!("launchpad: draw failed: {e}"),
        }
    }

    /// Draws a surface of `r`'s size into visual `v` at `r`.
    fn paint_into(&self, v: &SpriteVisual, r: Rect, f: impl FnOnce(&ID2D1DeviceContext) -> Result<()>) {
        let s = self.s;
        let surface = self.gfx.draw((r.w() * s).ceil() as u32, (r.h() * s).ceil() as u32, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
            let res = f(dc);
            dc.SetTransform(&Matrix3x2::identity());
            res
        });
        self.set_surface(v, surface, r);
    }

    /// Page dots under the grid (hidden while searching or with one page).
    fn draw_dots(&mut self) {
        let pages = self.pages_n();
        if self.searching || pages < 2 {
            let _ = self.dots_v.SetIsVisible(false);
            return;
        }
        // (In the dots' region: centered, on a narrow work area closer
        // together and smaller if need be, a
        // window of them around the page shown past that: drawn exactly
        // where a click finds them.)
        let row = self.regions().dots_row(pages, self.page, self.dots_back);
        if row.shown == 0 {
            let _ = self.dots_v.SetIsVisible(false);
            return;
        }
        let span = row.span();
        let r = span.offset(self.work.l, self.work.t);
        // The page shown in the primary text, the others in the disabled
        // level (the text tokens: no alphas of its own).
        let (on_c, off_c, page, hc) = (self.ink(), rgba(self.theme.text_disabled()), self.page, self.hc().is_some());
        let lib = self.lib_page();
        self.paint_into(&self.dots_v, r, |dc| unsafe {
            for p in row.shown_pages() {
                let Some(d) = row.shape(p) else { continue };
                let (x, y, size) = (d.cx() - span.l, d.cy() - span.t, d.w());
                let on = p == page;
                let c = if on || hc { on_c } else { off_c };
                if super::library::dot(p, lib) == super::library::Dot::Library {
                    applib::paint_library_dot(dc, x, y, size, c, on || !hc)?;
                    continue;
                }
                let e = D2D1_ELLIPSE { point: Vector2 { X: x, Y: y }, radiusX: size / 2.0, radiusY: size / 2.0 };
                let b = dc.CreateSolidColorBrush(&c, None)?;
                if on || !hc {
                    dc.FillEllipse(&e, &b);
                } else {
                    dc.DrawEllipse(&e, &b, 1.0, None);
                }
            }
            Ok(())
        });
    }

    /// Where a quiet block goes: the middle of the grid's area, `h` tall.
    fn quiet_rect(&self, h: f32) -> Rect {
        let (gx, gy) = grid_centre(&self.grid, &self.work);
        let area_h = self.grid.cell_h * self.grid.rows as f32;
        // (As wide as a line may get: it is never cut.)
        let w = (self.work.w() - space::S6 * 2.0).max(1.0);
        // (An area shorter than the block: from its top.)
        let top = if area_h >= h { gy - h / 2.0 } else { self.work.t + self.grid.top };
        Rect::new(gx - w / 2.0, top, gx + w / 2.0, top + h)
    }

    /// The progress ring at `at` (its middle; None: none). It is drawn
    /// once and turned by a Composition animation, one turn in
    /// SPINNER_TURN_MS; not animating: a still arc (12.2). Drawn again
    /// only when where it is or whether it turns changes (ring_plan).
    fn spin(&mut self, at: Option<(f32, f32)>) {
        let Some(next) = ring_plan(self.spin_at, at, self.animate()) else { return };
        self.spin_at = next;
        let turn = HSTRING::from("RotationAngleInDegrees");
        let _ = self.spin_v.StopAnimation(&turn);
        let Some(Ring { at: (cx, cy), turns }) = next else {
            let _ = self.spin_v.SetIsVisible(false);
            return;
        };
        let side = size::SPINNER + space::S1 * 2.0;
        let r = Rect::new(cx - side / 2.0, cy - side / 2.0, cx + side / 2.0, cy + side / 2.0);
        let theme = self.theme.clone();
        self.paint_into(&self.spin_v, r, |dc| unsafe { crate::gfx::parts::spinner(dc, side / 2.0, side / 2.0, 0.0, &theme, 1.0) });
        if let Ok(sz) = self.spin_v.Size() {
            let _ = self.spin_v.SetCenterPoint(Vector3 { X: sz.X / 2.0, Y: sz.Y / 2.0, Z: 0.0 });
        }
        let _ = self.spin_v.SetRotationAngleInDegrees(0.0);
        if turns {
            let c = &self.gfx.compositor;
            let _ = (|| -> Result<()> {
                let a = c.CreateScalarKeyFrameAnimation()?;
                a.InsertKeyFrame(0.0, 0.0)?;
                a.InsertKeyFrameWithEasingFunction(1.0, 360.0, &c.CreateLinearEasingFunction()?)?;
                a.SetDuration(real(crate::gfx::parts::SPINNER_TURN_MS as i64))?;
                a.SetIterationBehavior(windows::UI::Composition::AnimationIterationBehavior::Forever)?;
                self.spin_v.StartAnimation(&turn, &a)
            })();
        }
    }

    /// The ring shown is drawn again where it is (the theme changed).
    fn respin(&mut self) {
        if let Some(r) = self.spin_at.take() {
            self.spin(Some(r.at));
        }
    }

    /// The quiet states (design system 11): nothing matches the search
    /// (the glass and one line), no apps at all, the list still being read
    /// (nothing for TIP_DELAY_MS, then the ring, a line with it past
    /// WAIT_LINE_MS) or not read (the warning, one line and the "try again"
    /// button; Enter does it too). An incomplete list is one line above the
    /// page dots: what happened, then what a click (or Enter) does. A
    /// screen reader's tree gains or loses the button with it.
    fn update_msg(&mut self) {
        let had = self.msg_retry.is_some();
        self.paint_msg();
        if had != self.msg_retry.is_some() {
            self.publish();
        }
    }

    fn paint_msg(&mut self) {
        use crate::gfx::parts::Quiet;
        self.msg_retry = None;
        let state = crate::search::apps_state();
        let empty = empty_text(self.apps.is_empty(), self.searching, self.found.is_empty(), state);
        let partial = if empty.is_none() { partial_text(self.apps.is_empty(), self.searching, state) } else { None };
        let waiting = empty == Some("launchpad.loading");
        if !waiting {
            self.wait_since = None;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), WAIT_TIMER);
            }
            self.spin(None);
        }
        let Some(key) = empty else {
            let Some(k) = partial else {
                let _ = self.msg_v.SetIsVisible(false);
                return;
            };
            let text = format!("{} \u{b7} {}", t(k), t("launch.action.retry"));
            let Some((l, tw, th)) = self.layout(&text, crate::theme::ramp::BODY.size, false, self.work.w() - space::S6 * 2.0, 2, DWRITE_TEXT_ALIGNMENT_CENTER) else { return };
            let cy = self.work.t + self.regions().dots.cy() - PARTIAL_UP;
            let r = Rect::new(self.work.cx() - tw / 2.0 - space::S2, cy - th / 2.0, self.work.cx() + tw / 2.0 + space::S2, cy + th / 2.0);
            // The line is the "try again" button (with Enter).
            self.msg_retry = Some(Rect::new(r.l - space::S4, r.t - space::S3, r.r + space::S4, r.b + space::S3));
            self.retry_why = k;
            let dim = self.dim();
            let off = (r.w() - (self.work.w() - space::S6 * 2.0)) / 2.0;
            self.paint_into(&self.msg_v, r, |dc| unsafe { draw_layout(dc, &l, off, 0.0, dim) });
            return;
        };
        let block = size::QUIET_MIN_H;
        if waiting {
            let since = *self.wait_since.get_or_insert_with(Instant::now);
            let waited = since.elapsed().as_millis();
            let (show, line, again) = wait_stage(waited);
            if let Some(ms) = again {
                unsafe {
                    SetTimer(Some(self.hwnd), WAIT_TIMER, ms, None);
                }
            }
            if !show {
                let _ = self.msg_v.SetIsVisible(false);
                self.spin(None);
                return;
            }
            let r = self.quiet_rect(block);
            let text = if line { t(key).to_string() } else { String::new() };
            let laid = (!text.is_empty()).then(|| self.gfx.text_style(&text, &self.theme.font.family, crate::theme::ramp::BODY, 1.0).ok()).flatten();
            let (gy, ty) = crate::gfx::parts::quiet_rows(block, laid.as_ref().map_or(0.0, |l| l.2));
            self.spin(Some((r.cx(), r.t + gy)));
            match laid {
                Some((l, tw, _)) => {
                    let (dim, w) = (self.dim(), r.w());
                    self.paint_into(&self.msg_v, r, |dc| unsafe { draw_layout(dc, &l, ((w - tw) / 2.0).round(), ty.round(), dim) });
                }
                None => {
                    let _ = self.msg_v.SetIsVisible(false);
                }
            }
            return;
        }
        let failed = key == "launchpad.load_failed";
        let (kind, glyph) = match key {
            "launchpad.load_failed" => (Quiet::Error, Glyph::Warning),
            "launchpad.no_apps" => (Quiet::Empty, Glyph::Launchpad),
            _ => (Quiet::Empty, G_SEARCH),
        };
        let r = self.quiet_rect(block);
        let text = t(if failed { FAILED_LINE } else { key });
        let button = failed.then(|| t("launch.action.retry"));
        let style = |what: &str| self.gfx.text_style(what, &self.theme.font.family, crate::theme::ramp::BODY, 1.0).ok();
        if let (Some(label), Some(line)) = (button.and_then(style), style(text)) {
            // The button tries again (with Enter).
            self.msg_retry = Some(retry_rect(r.w(), label.1, quiet_stack(line.2, true).1).offset(r.l, r.t));
            self.retry_why = FAILED_LINE;
        }
        let (gfx, theme, w) = (self.gfx.clone(), self.theme.clone(), r.w());
        self.paint_into(&self.msg_v, r, |dc| unsafe { paint_quiet(&gfx, dc, w, kind, glyph, text, button, &theme) });
    }

    // ---- the card (menu) ---------------------------------------------------

    /// The menu goes (one up is cancelled: nothing is picked).
    fn close_menu(&mut self) {
        self.menu = None;
        if self.menu_open {
            crate::popup::cancel();
        }
    }

    /// Asks for a menu of `rows` on `app` at `at` (DIP): the shared menu
    /// (crate::popup), shown by the loop once this message is done.
    fn ask_menu(&mut self, rows: Vec<Option<(String, MenuAct)>>, app: App, at: (f32, f32)) {
        if self.closing || self.done || rows.iter().all(|r| r.is_none()) {
            return;
        }
        let cats = |act: &MenuAct| match act {
            MenuAct::MoveTo => Some(self.category_rows(&app)),
            MenuAct::ManyMoveTo => Some(self.picks_category_rows()),
            _ => None,
        };
        let (items, acts) = menu_items(rows, &cats);
        self.menu = Some(Menu { items, acts, app, at });
        unsafe {
            // (Asked from a sent message: wake the loop.)
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }

    /// The menu asked for, for the loop to show: the overlay's part of it
    /// (the caret hides while it is up).
    fn take_menu(&mut self) -> Option<MenuRun> {
        let menu = self.menu.take()?;
        if self.closing || self.done || self.menu_open {
            return None;
        }
        let mut at = POINT { x: (menu.at.0 * self.s).round() as i32, y: (menu.at.1 * self.s).round() as i32 };
        // A composition under way must not go on in the menu's window: it
        // goes from the field here (what the IME sends while it cancels
        // finds the overlay borrowed and is lost; what it sends later is
        // ignored: ime_gate), and the results go by the text alone.
        let dropped = drop_composition(&mut self.comp, &mut self.arr.ime_dropped);
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut at);
            let himc = ImmGetContext(self.hwnd);
            if !himc.is_invalid() {
                let _ = ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                let _ = ImmReleaseContext(self.hwnd, himc);
            }
        }
        self.unpress();
        self.press = None;
        self.menu_open = true;
        if dropped {
            self.changed();
        }
        self.draw_field();
        Some(MenuRun { menu, gfx: self.gfx.clone(), theme: self.theme.clone(), scale: self.s, at, rtl: self.rtl })
    }

    /// The menu closed with `out`. Launchpad takes the keyboard back (the
    /// menu had the foreground), unless another app has it now: then it
    /// closes as for any activation lost. `fading`: the menu's windows,
    /// still fading out: kept above the overlay meanwhile. Then what was
    /// picked is done.
    fn menu_closed(&mut self, out: crate::popup::Outcome, acts: Vec<MenuAct>, app: App, fading: &[HWND]) {
        self.menu_open = false;
        if self.closing || self.done {
            return;
        }
        unsafe {
            let fg = GetForegroundWindow();
            let mut pid = 0u32;
            if !fg.is_invalid() {
                GetWindowThreadProcessId(fg, Some(&mut pid));
            }
            if !keeps_after_menu(fg == self.hwnd, fg.is_invalid(), pid == windows::Win32::System::Threading::GetCurrentProcessId()) || !take_foreground(self.hwnd) {
                self.finish(None);
                return;
            }
            let _ = SetFocus(Some(self.hwnd));
            for h in fading {
                let _ = SetWindowPos(*h, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
            }
        }
        self.caret_on = true;
        self.draw_field();
        self.update_sel();
        if let crate::popup::Outcome::Picked(id) = out {
            if let Some(act) = (id as usize).checked_sub(1).and_then(|k| acts.get(k)).cloned() {
                self.menu_pick(act, app);
            }
        }
    }

    /// An app's menu at (x, y) DIP (`keyboard`: asked for by key; the
    /// shared menu takes the keys from there).
    fn open_menu(&mut self, idx: usize, at: (f32, f32), keyboard: bool) {
        if self.arr.edit {
            // Edit mode: the picked icons' menu (organize.rs).
            if !self.searching {
                self.open_picks_menu(idx, at, keyboard);
            }
            return;
        }
        // A folder: open it, or arrange.
        if let Some(LpItem::Folder { name, .. }) = (!self.searching).then(|| self.item_at(idx)).flatten() {
            let name = folders::shown_name(name, t("launchpad.folder.default"));
            let mut rows: Vec<Option<(String, MenuAct)>> = vec![Some((t("launchpad.menu.open").to_string(), MenuAct::OpenFolder(idx)))];
            if self.can_arrange() {
                rows.push(None);
                rows.extend(self.arrange_rows());
            }
            self.sel = Some(idx);
            self.update_sel();
            let app = App { name: name.clone(), id: String::new() };
            self.announce(name);
            self.ask_menu(rows, app, at);
            return;
        }
        let Some(app) = self.app_of(idx).cloned() else { return };
        self.sel = Some(idx);
        self.update_sel();
        // Arranging is from the pages (search results are by name).
        let arrange = !self.searching && self.can_arrange();
        self.open_app_menu(app, at, keyboard, arrange);
    }

    /// An app's menu (on the pages, in search results, in a folder or a
    /// Library category): open it, the Dock, Home and the Library;
    /// `arrange`: "Edit Home Screen" too.
    fn open_app_menu(&mut self, app: App, at: (f32, f32), keyboard: bool, arrange: bool) {
        let _ = keyboard;
        let target = list::target_path(&app);
        let path = list::pin_path(&app, target.as_deref());
        let entry = app.target();
        let pinned = list::is_pinned(&self.pinned, &[path.as_str(), entry.as_str()]);
        let mut rows = app_commands(list::packaged(&app), target, crate::taskbar::hidden_by_us());
        let pin_label = if pinned { t("launchpad.menu.remove_from_dock") } else { t("launchpad.menu.keep_in_dock") };
        // Unpinning names what is pinned (it may be the Start entry).
        let pin_path = if pinned { self.pinned.iter().find(|p| p.eq_ignore_ascii_case(&path) || p.eq_ignore_ascii_case(&entry)).cloned().unwrap_or(path) } else { path };
        rows.push(Some((pin_label.to_string(), MenuAct::Pin { name: app.name.clone(), path: pin_path, on: !pinned })));
        // Home and the Library: add / remove, move to a category, hide.
        let lib_rows = self.lib_menu_rows(&app);
        if !lib_rows.is_empty() {
            rows.push(None);
            rows.extend(lib_rows.into_iter().map(Some));
        }
        if arrange {
            rows.push(None);
            rows.extend(self.arrange_rows());
        }
        self.announce(app.name.clone());
        self.ask_menu(rows, app, at);
    }

    /// "Edit Home Screen" and, next to it, "Tidy Home" (when it can).
    fn arrange_rows(&self) -> Vec<Option<(String, MenuAct)>> {
        menu_arrange_rows(self.can_tidy())
    }

    /// A row of the menu was picked: `act`, on `a`.
    fn menu_pick(&mut self, act: MenuAct, a: App) {
        match act {
            MenuAct::Open | MenuAct::Admin => {
                self.lib_seen(&a.id);
                let after = if act == MenuAct::Open { After::Open(a.target()) } else { After::Admin(a.target()) };
                self.finish(Some(after));
            }
            MenuAct::Uninstall => self.finish(Some(After::Open("ms-settings:appsfeatures".into()))),
            MenuAct::Reveal(p) => self.finish(Some(After::Reveal(p))),
            MenuAct::Arrange => {
                self.start_edit();
            }
            MenuAct::OpenFolder(i) => self.open_folder(i),
            MenuAct::Pin { name, path, on } => {
                if on {
                    self.pinned.push(path.clone());
                } else {
                    self.pinned.retain(|p| !p.eq_ignore_ascii_case(&path));
                }
                self.deferred.push(Deferred::Pin { name, path, on });
            }
            MenuAct::AddHome | MenuAct::RemoveHome | MenuAct::Hide | MenuAct::Unhide | MenuAct::SetCat(_) => self.lib_act(&act, &a),
            // (Rows that open the categories: their submenu's rows are picked.)
            MenuAct::MoveTo | MenuAct::ManyMoveTo => {}
            MenuAct::OpenCategory(k) => self.open_category(k),
            MenuAct::Tidy => self.start_tidy(),
            MenuAct::Group | MenuAct::ManyRemoveHome | MenuAct::ManyHide | MenuAct::ManySetCat(_) => self.picks_act(&act),
        }
    }

    // ---- keyboard -------------------------------------------------------------------

    fn launch(&mut self, i: usize) {
        // A folder opens; nothing starts in edit mode.
        if !self.searching && matches!(self.item_at(i), Some(LpItem::Folder { .. })) {
            self.open_folder(i);
            return;
        }
        if self.arr.edit {
            return;
        }
        if let Some(a) = self.app_of(i) {
            let (target, id) = (a.target(), a.id.clone());
            self.lib_seen(&id);
            self.finish(Some(After::Open(target)));
        }
    }

    fn on_key(&mut self, vk: u16) {
        if crate::input::navigates(vk) {
            self.by_key = true;
            self.field_ring = self.focus == Focus::Field;
            self.update_sel();
            self.draw_field();
            if self.arr.folder.is_some() {
                self.draw_folder_sel(false);
            }
        }
        self.eat_space = false;
        // (A key moves on from a button a screen reader had focused.)
        self.ax_focus = None;
        self.done_focus_changed();
        // Edit mode, the open folder and renaming it have their keys.
        if vk != VK_PROCESSKEY.0 && self.comp.is_empty() && self.arr_key(vk) {
            return;
        }
        // The Library's tiles have theirs.
        if vk != VK_PROCESSKEY.0 && self.comp.is_empty() && self.lib_key(vk) {
            return;
        }
        if vk == VK_SPACE.0 && self.comp.is_empty() {
            // Space presses what has the focus (its WM_CHAR is eaten).
            if self.field.text().is_empty() {
                match self.focus {
                    Focus::Grid => {
                        self.eat_space = true;
                        if let Some(i) = self.sel {
                            self.launch(i);
                        }
                        return;
                    }
                    Focus::Field => {}
                }
            }
        }
        if vk == VK_PROCESSKEY.0 || !self.comp.is_empty() {
            // The IME has it.
            return;
        }
        let ctrl = key_down(VK_CONTROL);
        let shift = key_down(VK_SHIFT);
        match vk {
            v if v == VK_ESCAPE.0 => {
                if self.field.text().is_empty() {
                    self.finish(None);
                } else {
                    self.field = Field::default();
                    self.changed();
                }
            }
            v if v == VK_TAB.0 => {
                // (An empty grid is skipped.)
                let to = tab_next(self.focus, self.listing_len() > 0, shift);
                self.set_focus(to);
            }
            v if v == VK_RETURN.0 => match self.focus {
                // The app list could not be read (or only part of it):
                // Enter tries again, also while searching.
                Focus::Field if enter_retries(self.msg_retry.is_some(), self.apps.is_empty(), self.searching, self.sel.is_some()) => self.retry_apps(),
                // An empty field: nothing is selected to open.
                Focus::Field if !self.searching => {}
                _ => {
                    let i = self.sel.or_else(|| (self.searching && !self.found.is_empty()).then_some(0));
                    if let Some(i) = i {
                        self.launch(i);
                    }
                }
            },
            v if v == VK_APPS.0 || (shift && v == VK_F10.0) => {
                if let Some(i) = self.sel.filter(|_| self.focus == Focus::Grid || self.searching) {
                    let slot = if self.searching { i } else { i % self.per_page() };
                    let r = self.grid.icon(slot, self.rtl).offset(self.work.l, self.work.t);
                    self.open_menu(i, (r.cx(), r.cy()), true);
                }
            }
            v if v == VK_PRIOR.0 && !self.searching => self.page_visual(self.rtl),
            v if v == VK_NEXT.0 && !self.searching => self.page_visual(!self.rtl),
            v if v == VK_LEFT.0 => self.arrow(Arrow::Left),
            v if v == VK_RIGHT.0 => self.arrow(Arrow::Right),
            v if v == VK_UP.0 => self.arrow(Arrow::Up),
            v if v == VK_DOWN.0 => self.arrow(Arrow::Down),
            v if v == VK_HOME.0 || v == VK_END.0 => {
                if self.focus == Focus::Field || self.searching {
                    if v == VK_HOME.0 { self.field.home(shift) } else { self.field.end(shift) }
                    self.caret_on = true;
                    self.draw_field();
                } else {
                    let p = if v == VK_HOME.0 { 0 } else { self.pages_n() - 1 };
                    self.go_page(p, true);
                }
            }
            v if v == VK_BACK.0 => {
                self.field.backspace(ctrl);
                self.focus_field_quietly();
                self.changed();
            }
            v if v == VK_DELETE.0 => {
                self.field.delete(ctrl);
                self.focus_field_quietly();
                self.changed();
            }
            // Home back as it was before "Tidy Home" (once, while open).
            v if ctrl && v == b'Z' as u16 => self.undo_tidy(),
            v if ctrl && v == b'A' as u16 => {
                self.field.select_all();
                self.focus_field_quietly();
                self.draw_field();
            }
            v if ctrl && v == b'V' as u16 => {
                if let Some(s) = unsafe { crate::search::clipboard_text(self.hwnd) }.filter(|s| !s.is_empty()) {
                    // A paste searches like typing: edit mode and the folder end.
                    self.arr_leave_for_typing();
                    self.field.insert(&s);
                    self.focus_field_quietly();
                    self.changed();
                }
            }
            _ => {}
        }
    }

    /// Typing goes to the field from anywhere.
    fn focus_field_quietly(&mut self) {
        if self.focus != Focus::Field {
            self.focus = Focus::Field;
            self.field_ring = false;
            self.update_sel();
        }
    }

    fn arrow(&mut self, a: Arrow) {
        match self.focus {
            // In the field ←/→ move the caret (Ctrl: by word, Shift: select);
            // only an empty field lets them turn pages. ↓ goes into the
            // apps (the search results), ↑ does nothing.
            Focus::Field => match a {
                Arrow::Left | Arrow::Right if self.field.text().is_empty() && self.comp.is_empty() => self.page_visual(a == Arrow::Right),
                Arrow::Left | Arrow::Right => {
                    let (ctrl, shift) = (key_down(VK_CONTROL), key_down(VK_SHIFT));
                    // Right to left, the arrow keys go the way the text reads.
                    let back = (a == Arrow::Left) != self.rtl;
                    if back { self.field.left(ctrl, shift) } else { self.field.right(ctrl, shift) }
                    self.caret_on = true;
                    self.draw_field();
                }
                Arrow::Down if self.listing_len() > 0 => self.set_focus(Focus::Grid),
                _ => {}
            },
            _ => {
                let n = self.listing_len();
                let Some(cur) = self.sel.filter(|&s| self.valid_sel(s)) else {
                    if let Some(i) = self.first_on_page() {
                        self.focus = if self.searching { self.focus } else { Focus::Grid };
                        self.select(i);
                    }
                    return;
                };
                let next = if self.searching {
                    grid::step(cur, n, &self.grid, a, self.rtl)
                } else {
                    let lens: Vec<usize> = self.layout.iter().map(|p| p.len()).collect();
                    edit::step(self.pos_of(cur), &lens, &self.grid, a, self.rtl).map(|p| self.flat(p))
                };
                // Past Home's last app: on to the Library's first tile.
                let onward = matches!(a, Arrow::Left | Arrow::Right) && ((a == Arrow::Right) != self.rtl);
                if next.is_none() && onward && !self.searching && !self.arr.edit {
                    if let Some(l) = self.lib_page().filter(|_| !self.lib.groups.is_empty()) {
                        self.go_page(l, true);
                        self.lib_select(0);
                        return;
                    }
                }
                match next {
                    Some(next) => self.select(next),
                    None if a == Arrow::Up => self.set_focus(Focus::Field),
                    None => {}
                }
            }
        }
    }

    fn on_char(&mut self, unit: u16) {
        let c = match (self.high.take(), unit) {
            (_, 0xD800..=0xDBFF) => {
                self.high = Some(unit);
                return;
            }
            (Some(hi), 0xDC00..=0xDFFF) => char::decode_utf16([hi, unit]).next().and_then(|r| r.ok()),
            (_, u) => char::from_u32(u as u32),
        };
        let Some(c) = c else { return };
        if (c as u32) < 0x20 || c == '\u{7F}' {
            return;
        }
        // A folder's new name; else typing ends edit mode and searches.
        if self.arr_char(c) {
            return;
        }
        // The Space that pressed a button (on key down) is not typed.
        if c == ' ' && std::mem::take(&mut self.eat_space) {
            return;
        }
        self.field.insert(&c.to_string());
        self.focus_field_quietly();
        self.changed();
    }

    fn ime_string(&self, himc: HIMC, what: IME_COMPOSITION_STRING) -> String {
        unsafe {
            let bytes = ImmGetCompositionStringW(himc, what, None, 0);
            if bytes <= 0 {
                return String::new();
            }
            let mut buf = vec![0u16; bytes as usize / 2];
            let got = ImmGetCompositionStringW(himc, what, Some(buf.as_mut_ptr() as *mut _), bytes as u32);
            if got <= 0 {
                return String::new();
            }
            buf.truncate(got as usize / 2);
            String::from_utf16_lossy(&buf)
        }
    }

    fn on_composition(&mut self, flags: u32) {
        let himc = unsafe { ImmGetContext(self.hwnd) };
        if himc.is_invalid() {
            return;
        }
        // A composition cancelled (the folder's rename, a menu): what is
        // left of it (a late result) goes nowhere, not into the field.
        if !ime_gate(ImeMsg::Composition, &mut self.comp, &mut self.arr.ime_dropped) {
            unsafe {
                let _ = ImmReleaseContext(self.hwnd, himc);
            }
            return;
        }
        let renaming = self.arr.rename.is_some();
        if flags & GCS_RESULTSTR.0 != 0 {
            let s = self.ime_string(himc, GCS_RESULTSTR);
            self.comp.clear();
            match self.arr.rename.as_mut() {
                Some(f) => f.insert(&s),
                None => self.field.insert(&s),
            }
        }
        self.comp = if flags & GCS_COMPSTR.0 != 0 { self.ime_string(himc, GCS_COMPSTR).chars().collect() } else { Vec::new() };
        let pos = unsafe { ImmGetCompositionStringW(himc, GCS_CURSORPOS, None, 0) };
        self.comp_cursor = if pos >= 0 { crate::search::char_index(&self.comp, pos as u32) } else { self.comp.len() };
        unsafe {
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
        if renaming {
            self.draw_panel();
            return;
        }
        self.arr_leave_for_typing();
        self.focus_field_quietly();
        self.changed();
    }

    // ---- mouse ----------------------------------------------------------------------

    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let x = (lp.0 & 0xFFFF) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
        (x / self.s, y / self.s)
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        retry_hit(self.hit_under(x, y), self.msg_retry, x, y)
    }

    fn hit_under(&self, x: f32, y: f32) -> Hit {
        // The open folder covers the rest; Done in edit mode.
        if let Some(h) = self.arr_hit(x, y) {
            return h;
        }
        // The rest by region: only what is in the region under the point
        // can be hit there (grid::Regions; they never overlap).
        let r = self.regions();
        let (rx, ry) = (x - self.work.l, y - self.work.t);
        match r.at(rx, ry) {
            // The field.
            Some(grid::Area::Top) if self.field_rect().contains(x, y) => Hit::Field,
            Some(grid::Area::Top) => Hit::Empty,
            // The Library's tiles, or an app. (In edit mode the grid shows
            // a little smaller: its own coordinates.)
            Some(grid::Area::Grid) if self.on_lib() => self.tile_hit(x, y).unwrap_or(Hit::Empty),
            Some(grid::Area::Grid) => {
                let (gx, gy) = self.to_grid(x, y);
                let slot = self.grid.slot_at(gx - self.work.l, gy - self.work.t, self.rtl);
                slot.filter(|&slot| self.index_of_slot(slot).is_some()).map_or(Hit::Empty, Hit::App)
            }
            _ => control_hit(&r, rx, ry, self.pages_n(), self.page, self.dots_back, !self.searching).unwrap_or(Hit::Empty),
        }
    }

    /// The icon visual of slot `slot` on screen.
    fn cell_icon(&self, slot: usize) -> Option<SpriteVisual> {
        let v = if self.searching { Some(&self.results) } else { self.pages.get(&self.page) };
        v.and_then(|v| v.cells.get(slot)).map(|c| c.0.clone())
    }

    /// Let go: the icon comes back over 90 ms (6.3).
    fn unpress(&mut self) {
        if let Some((v, rest)) = self.pressed_cell.take() {
            let plan = motion::pointer(self.animate(), true, self.hc().is_some(), crate::effects::current().speed);
            let back = (plan.0 > 0).then(|| fade(&self.gfx.compositor, None, rest, plan)).and_then(|a| a.ok());
            match back {
                Some(a) if v.StartAnimation(&HSTRING::from("Opacity"), &a).is_ok() => {}
                _ => {
                    let _ = v.SetOpacity(rest);
                }
            }
        }
    }

    fn on_move(&mut self, lp: LPARAM) {
        let raw = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
        let moved = raw != self.last_mouse && self.last_mouse != (i32::MIN, i32::MIN);
        self.last_mouse = raw;
        let (x, y) = self.dip(lp);
        // An icon picked up (or about to be), a press held for edit mode.
        if self.arr_move(x, y) {
            return;
        }
        // A swipe.
        if let Some((px, py, phit)) = self.press.as_ref().map(|p| (p.x, p.y, p.hit)) {
            let dx = x - px;
            let dy = y - py;
            let swipable = !self.searching && matches!(phit, Hit::App(_) | Hit::Empty | Hit::Dot(_) | Hit::LibTile(..));
            if self.drag.is_none() && swipable && dx.abs() > DRAG_START && dx.abs() > dy.abs() {
                // Taken where it is (a turn under way stops under the finger).
                let at = self.strip_run.state(Instant::now()).0;
                let _ = self.strip.StopAnimation(&HSTRING::from("Offset.X"));
                let _ = self.strip.SetOffset(Vector3 { X: at, Y: 0.0, Z: 0.0 });
                self.unpress();
                self.drag = Some(Drag { base: at, at, start_x: px, last_x: x, last_t: Instant::now(), speed: 0.0 });
                self.update_sel();
            }
        }
        let pages = self.pages_n();
        if let Some(d) = self.drag.as_mut() {
            let now = Instant::now();
            let dt = now.duration_since(d.last_t).as_secs_f32().max(0.001);
            let v = (x - d.last_x) / dt;
            d.speed = d.speed * 0.6 + v * 0.4;
            d.last_x = x;
            d.last_t = now;
            let mut dx = x - d.start_x;
            // Past the first or last page it gives less and less.
            let at_start = self.page == 0 && ((dx > 0.0) != self.rtl);
            let at_end = self.page + 1 >= pages && ((dx < 0.0) != self.rtl);
            if at_start || at_end {
                dx = motion::rubber(dx, self.w);
            }
            d.at = d.base + dx;
            let _ = self.strip.SetOffset(Vector3 { X: d.at, Y: 0.0, Z: 0.0 });
            return;
        }
        if !moved {
            return;
        }
        let h = self.hit(x, y);
        // Off what lights (empty space, the field, the dots): what the
        // pointer lit goes, the keyboard's ring stays; back on, it lights
        // again, the same tile too (select.rs).
        let step = select::pointer_step(self.pointer_out, select::lights(h), self.pointer_lit());
        self.pointer_out = step.out;
        if step.off {
            self.sel_off(false);
        }
        if let Hit::LibTile(k, _) = h {
            self.lib_hover(k, step.again);
        }
        // (An app of the open folder: arrange.rs.)
        if let Hit::FolderApp(i) = h {
            self.folder_hover(i, step.again);
        }
        if let Hit::App(slot) = h {
            if let Some(i) = self.index_of_slot(slot) {
                if self.sel != Some(i) || self.by_key || step.again || (self.focus != Focus::Grid && !self.searching) {
                    self.sel = Some(i);
                    self.by_key = false;
                    self.typed = false;
                    if !self.searching && self.focus == Focus::Field {
                        // Hover shows where a click goes; typing still goes to the field.
                        self.focus = Focus::Grid;
                        self.draw_field();
                    }
                    self.update_sel();
                }
            }
        }
    }

    fn on_down(&mut self, lp: LPARAM) {
        self.pointer_takes_over();
        let (x, y) = self.dip(lp);
        let hit = self.hit(x, y);
        // Edit mode, the open folder, and the long press that starts it.
        if self.arr_down(x, y, hit) {
            return;
        }
        if let Hit::App(slot) = hit {
            // Pressed: dimmed to PRESS_DIM at once (6.2; high contrast
            // keeps its outline instead).
            if let Some(v) = self.cell_icon(slot).filter(|_| self.hc().is_none()) {
                let rest = v.Opacity().unwrap_or(1.0);
                let _ = v.StopAnimation(&HSTRING::from("Opacity"));
                let _ = v.SetOpacity(rest * crate::motion::PRESS_DIM);
                self.pressed_cell = Some((v, rest));
            }
        }
        self.press = Some(Press { x, y, hit });
        unsafe {
            SetCapture(self.hwnd);
        }
    }

    fn on_up(&mut self, lp: LPARAM) {
        let (x, y) = self.dip(lp);
        self.arr_cancel_long_press();
        if self.arr_up(x, y) {
            return;
        }
        self.unpress();
        let press = self.press.take();
        unsafe {
            let _ = ReleaseCapture();
        }
        if let Some(d) = self.drag.take() {
            let dx = x - d.start_x;
            let target = grid::swipe_target(self.page, self.pages_n(), dx, d.speed, self.w, self.rtl);
            self.fling = Some((d.at, d.speed));
            self.go_page(target, true);
            return;
        }
        let Some(press) = press else { return };
        let Some(hit) = released_on(press.hit, self.hit(x, y)) else { return };
        match hit {
            Hit::App(slot) => {
                if let Some(i) = self.index_of_slot(slot) {
                    self.launch(i);
                }
            }
            Hit::Dot(p) => self.go_page(p, true),
            Hit::Field => {
                self.set_focus(Focus::Field);
                // (The pointer's: no ring.)
                self.field_ring = false;
                self.draw_field();
                if self.comp.is_empty() {
                    self.click_field(x);
                }
            }
            Hit::Retry => self.retry_apps(),
            Hit::Empty => self.finish(None),
            Hit::LibTile(k, part) => self.lib_click(k, part),
            // (Edit mode's and the open folder's: arrange.rs.)
            Hit::Done | Hit::FolderApp(_) | Hit::FolderName | Hit::FolderPanel | Hit::FolderOutside => {}
        }
    }

    /// Reads the app list again (it could not be read).
    fn retry_apps(&mut self) {
        crate::search::refresh_apps();
        self.update_msg();
    }

    /// Caret where the field text was clicked.
    fn click_field(&mut self, x: f32) {
        let Some((shown, _, l, _, base, _)) = self.field_layout() else { return };
        let (x0, _) = self.text_span();
        let lx = x - self.field_rect().l - x0 - self.shift - base;
        let mut trailing = BOOL(0);
        let mut inside = BOOL(0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = l.HitTestPoint(lx, 4.0, &mut trailing, &mut inside, &mut m);
        }
        let pos = m.textPosition + if trailing.as_bool() { m.length } else { 0 };
        self.field.click(crate::search::char_index(&shown, pos), key_down(VK_SHIFT));
        self.caret_on = true;
        self.draw_field();
    }

    fn on_right_up(&mut self, lp: LPARAM) {
        let (x, y) = self.dip(lp);
        // While arranging only the picked icons' menu (organize.rs); over
        // an open folder or category, its apps' menus.
        if self.arr.edit {
            match self.hit(x, y) {
                Hit::App(slot) if self.arr.folder.is_none() && !self.arr.dragging() && !self.searching => {
                    if let Some(i) = self.index_of_slot(slot) {
                        self.open_picks_menu(i, (x, y), false);
                    }
                }
                _ => {}
            }
            return;
        }
        match self.hit(x, y) {
            Hit::App(slot) => {
                if let Some(i) = self.index_of_slot(slot) {
                    self.open_menu(i, (x, y), false);
                }
            }
            Hit::FolderApp(i) => self.panel_menu(i, (x, y), false),
            Hit::LibTile(k, part) => self.lib_menu(k, part, (x, y), false),
            _ => {}
        }
    }

    /// Wheel: one page per notch (a burst of small trackpad steps turns
    /// one page, then waits a moment).
    fn on_wheel(&mut self, delta: i32, horizontal: bool) {
        if self.arr.folder.is_some() {
            self.folder_wheel(delta);
            return;
        }
        // (Never while a press or a dragged icon is under way: its page holds.)
        if self.searching || self.drag.is_some() || self.arr.dragging() || self.press.is_some() {
            return;
        }
        let recent = self.last_flip.is_some_and(|t| t.elapsed().as_millis() < WHEEL_GAP_MS);
        if recent {
            self.wheel = 0;
            return;
        }
        self.wheel += delta;
        if self.wheel.abs() >= 120 {
            // Down / right: on to the next page.
            let forward = if horizontal { self.wheel > 0 } else { self.wheel < 0 };
            self.wheel = 0;
            self.last_flip = Some(Instant::now());
            self.page_visual(forward != self.rtl);
        }
    }

    fn cancel_drag(&mut self) {
        self.arr_cancel_long_press();
        // A band being dragged stops where it is.
        self.band_cancel();
        // An icon being dragged goes back where it was.
        self.arr_cancel_drag();
        self.unpress();
        self.press = None;
        if let Some(d) = self.drag.take() {
            let p = self.page;
            // Back from where it was left, from rest.
            self.fling = Some((d.at, 0.0));
            self.go_page(p, true);
        }
    }
}

/// Pure: where Launchpad goes when its pages are made again for a new
/// grid (`pp` things a page now, `layout` the pages): the page the user is
/// looking at stays in view. The page with the thing that was selected
/// (`sel`, with the page it was on, selected again: its listing index)
/// when it was on page `page` shown (or no page showed anything); else
/// the page with the one that led the page shown (`first`; a selection
/// left on another page is dropped); else page `page` as it was (the
/// caller keeps it to the pages there are).
fn regrid_spot(layout: &Pages, pp: usize, sel: Option<(&LpItem, usize)>, first: Option<&LpItem>, page: usize) -> (usize, Option<usize>) {
    let pp = pp.max(1);
    let find = |it: &LpItem| layout.iter().enumerate().find_map(|(p, v)| v.iter().position(|x| x == it).map(|s| p * pp + s));
    let sel = sel.filter(|&(_, on)| on == page || first.is_none()).and_then(|(it, _)| find(it));
    let page = sel.or_else(|| first.and_then(find)).map_or(page, |i| i / pp);
    (page, sel)
}

/// Pure: the selection (browsing: a listing index, `pp` a page) kept when
/// page `page` is shown and the keyboard is not on the apps: only one on
/// that page. One left on another page is stale: dropped, so nothing
/// (regrid_spot, set_focus) ever goes back to it.
fn sel_kept(sel: Option<usize>, page: usize, pp: usize) -> Option<usize> {
    sel.filter(|&s| grid::page_of(s, pp) == page)
}

/// Pure: what the controls at the bottom take of a click at (x, y) (work
/// area DIP) of regions `r`: a page dot of `pages` (page `page` shown,
/// `back` as in Regions::dots_row) in the dots' region (`dots`: shown, not
/// searching); nothing anywhere else. The grid has a region of its own,
/// so a click on an app (or its name) is never a page turn.
fn control_hit(r: &grid::Regions, x: f32, y: f32, pages: usize, page: usize, back: bool, dots: bool) -> Option<Hit> {
    match r.at(x, y)? {
        grid::Area::Dots if dots => r.dot_at(x, y, pages, page, back).map(Hit::Dot),
        _ => None,
    }
}

/// Pure: does a newly read app list snapshot replace the one shown? Not an
/// incomplete one over arranged, unsaved pages.
fn takes_snapshot(dirty: bool, complete: bool) -> bool {
    !dirty || complete
}

/// Pure: where Tab (`back`: Shift+Tab) goes from `focus`: the field, the
/// apps (when there are any), round again.
fn tab_next(focus: Focus, grid: bool, back: bool) -> Focus {
    let mut order = vec![Focus::Field];
    if grid {
        order.push(Focus::Grid);
    }
    let key = match focus {
        Focus::Grid if !grid => Focus::Field,
        f => f,
    };
    let at = order.iter().position(|f| *f == key).unwrap_or(0);
    let n = order.len();
    order[if back { (at + n - 1) % n } else { (at + 1) % n }]
}

/// Pure: are the pages made again for a newly read list: another list,
/// or the same one now read in full (it was partial)?
fn remake_needed(list_changed: bool, was_complete: bool, complete: bool) -> bool {
    list_changed || (!was_complete && complete)
}

/// Pure: the menu rows that arrange Home: "Edit Home Screen", and "Tidy
/// Home" right after it when Home can be tidied (`tidy`).
fn menu_arrange_rows(tidy: bool) -> Vec<Option<(String, MenuAct)>> {
    let mut rows = vec![Some((t("launchpad.menu.arrange").to_string(), MenuAct::Arrange))];
    if tidy {
        rows.push(Some((t("launchpad.menu.tidy").to_string(), MenuAct::Tidy)));
    }
    rows
}

/// Pure: can arranging start, by the snapshot shown (`complete`)?
fn arrange_ok(complete: bool, searching: bool, apps: usize) -> bool {
    complete && !searching && apps > 0
}

/// Folders of `new` that match one of `old` (name and apps) take its
/// session id.
fn keep_folder_ids(old: &Pages, new: &mut Pages) {
    let ids: Vec<(&String, &Vec<String>, u64)> = old
        .iter()
        .flatten()
        .filter_map(|it| match it {
            LpItem::Folder { name, apps, id, .. } => Some((name, apps, *id)),
            _ => None,
        })
        .collect();
    for it in new.iter_mut().flatten() {
        if let LpItem::Folder { name, apps, id, .. } = it {
            if let Some(&(_, _, o)) = ids.iter().find(|(n, a, _)| *n == name && *a == apps) {
                *id = o;
            }
        }
    }
}

/// Pure: `v` DIP moved onto the nearest whole device pixel at scale `s`.
fn snap_to(v: f32, s: f32) -> f32 {
    (v * s).round() / s
}

/// Pure: an icon's edge in DIP as drawn on grid `g` at scale `s` (`px`:
/// the usual icon's device pixels): whole device pixels, `px` unless the
/// grid's icons are smaller (a work area too small for whole squares).
fn icon_edge(g: &Grid, s: f32, px: u32) -> f32 {
    if g.icon_edge < grid::ICON { (g.icon_edge * s).floor().max(1.0) / s } else { px as f32 / s }
}

/// Pure: the icon of slot `slot` of grid `g` in work area `work` (monitor
/// DIP) as drawn: icon_edge square, where the grid centres it, on whole
/// device pixels. The dragged icon and its ghost take the same square.
fn icon_box(g: &Grid, slot: usize, rtl: bool, work: &Rect, s: f32, px: u32) -> Rect {
    let r = g.icon(slot, rtl).offset(work.l, work.t);
    let e = icon_edge(g, s, px);
    let (l, t) = (snap_to(r.cx() - e / 2.0, s), snap_to(r.cy() - e / 2.0, s));
    Rect::new(l, t, l + e, t + e)
}

/// Pure: a dragged icon `e` DIP across centred on `centre` (before the
/// lift's scale, which is about its middle).
fn dragged_box(centre: (f32, f32), e: f32) -> Rect {
    let (l, t) = (centre.0 - e / 2.0, centre.1 - e / 2.0);
    Rect::new(l, t, l + e, t + e)
}

// ---- drawing helpers ---------------------------------------------------------------------

unsafe fn fill(dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: Rect) -> Result<()> {
    if c.a > 0.0 {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        dc.FillRectangle(&d2r(r), &b);
    }
    Ok(())
}

unsafe fn fill_rr(dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, rr: &D2D1_ROUNDED_RECT) -> Result<()> {
    if c.a > 0.0 {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        dc.FillRoundedRectangle(rr, &b);
    }
    Ok(())
}

unsafe fn stroke_rr(dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, rr: &D2D1_ROUNDED_RECT, w: f32) -> Result<()> {
    if c.a > 0.0 {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        dc.DrawRoundedRectangle(rr, &b, w, None);
    }
    Ok(())
}

unsafe fn draw_layout(dc: &ID2D1DeviceContext, l: &IDWriteTextLayout, x: f32, y: f32, c: D2D1_COLOR_F) -> Result<()> {
    let b = dc.CreateSolidColorBrush(&c, None)?;
    dc.DrawTextLayout(Vector2 { X: x, Y: y }, l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
    Ok(())
}

// ---- text ------------------------------------------------------------------------------------

/// A layout box tall enough for any text before it is cut to its lines.
const UNCUT_H: f32 = 100_000.0;

/// What the names' box is measured with: Latin with ascenders and
/// descenders, Hangul, kana, Han and Thai on each line (the scripts an
/// app's name is likely in; each may come from a fallback font with its
/// own line height).
const LINES_SAMPLE: &str = "Agy\u{AC00}\u{3042}\u{4E2D}\u{0E01}";

/// The heights (DIP) of a layout's lines, as DirectWrite lays them out.
fn line_heights(l: &IDWriteTextLayout) -> Vec<f32> {
    let mut n = 0u32;
    unsafe {
        // (Asked with no room first: it says how many there are.)
        let _ = l.GetLineMetrics(None, &mut n);
        let mut m = vec![DWRITE_LINE_METRICS::default(); n as usize];
        if n == 0 || l.GetLineMetrics(Some(&mut m), &mut n).is_err() {
            return Vec::new();
        }
        m.iter().take(n as usize).map(|x| x.height).collect()
    }
}

/// How tall `lines` lines of `size` DIP text are when a layout's own line
/// metrics cannot be had: as lines_h measures them, else LINE_EM a line.
fn lines_guess(dw: &IDWriteFactory, family: &str, size: f32, lines: usize) -> f32 {
    lines_h(dw, family, size, lines).unwrap_or(size * grid::LINE_EM * lines.max(1) as f32)
}

/// How tall `lines` lines of `size` DIP text (at the text size already) in
/// `family` are laid out (DIP): real font metrics, LINES_SAMPLE on each
/// line. None if DirectWrite cannot say.
fn lines_h(dw: &IDWriteFactory, family: &str, size: f32, lines: usize) -> Option<f32> {
    unsafe {
        let fam = HSTRING::from(family);
        let format = crate::gfx::ui_format(dw, &fam, DWRITE_FONT_WEIGHT_NORMAL, size).ok()?;
        let text = vec![LINES_SAMPLE; lines.max(1)].join("\n");
        let wide: Vec<u16> = text.encode_utf16().collect();
        let l = dw.CreateTextLayout(&wide, &format, UNCUT_H, UNCUT_H).ok()?;
        let mut m = DWRITE_TEXT_METRICS::default();
        l.GetMetrics(&mut m).ok()?;
        (m.height.is_finite() && m.height > 0.0).then_some(m.height)
    }
}

// ---- window procedure ------------------------------------------------------------------------

/// Pure: input the overlay ignores while it animates away.
fn ignored_while_closing(msg: u32) -> bool {
    matches!(
        msg,
        WM_MOUSEMOVE
            | WM_LBUTTONDOWN
            | WM_LBUTTONUP
            | WM_LBUTTONDBLCLK
            | WM_RBUTTONDOWN
            | WM_RBUTTONUP
            | WM_MBUTTONDOWN
            | WM_MBUTTONUP
            | WM_MOUSEWHEEL
            | WM_MOUSEHWHEEL
            | WM_KEYDOWN
            | WM_KEYUP
            | WM_SYSKEYDOWN
            | WM_SYSKEYUP
            | WM_CHAR
            | WM_SYSCHAR
            | WM_IME_STARTCOMPOSITION
            | WM_IME_COMPOSITION
            | WM_IME_ENDCOMPOSITION
            | WM_CONTEXTMENU
    )
}

/// What a window of ours that is not the open Launchpad's (retired: fading
/// away, or on its way out) does with a message.
#[derive(Debug, PartialEq)]
enum Stale {
    /// The pointer goes to whatever is under it (this thread's windows
    /// too; other apps' already, the window being layered + transparent).
    Transparent,
    /// Its closing animation is over: it goes now.
    Faded,
    /// It is being destroyed: its screen reader tree goes.
    Destroyed,
    /// Input, and asking it to close (Alt+F4 while it still has the
    /// focus): ignored, as while it was closing; it goes when its fade is
    /// over, never on its own.
    Ignored,
    /// Anything else: nothing of the open Launchpad's is touched.
    Default,
}

/// Pure: the answer of a stale window to `msg`.
fn stale(msg: u32) -> Stale {
    match msg {
        WM_NCHITTEST => Stale::Transparent,
        WM_LP_CLOSED => Stale::Faded,
        WM_DESTROY => Stale::Destroyed,
        WM_CLOSE => Stale::Ignored,
        m if ignored_while_closing(m) => Stale::Ignored,
        _ => Stale::Default,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // A retired window (or one that is not the open Launchpad's, such as
    // the old one fading while a new one opened) never reaches the open
    // one's state: its activation, timers or input must not close it.
    if crate::motion::retired(hwnd) || with_pad(|p| p.hwnd != hwnd).unwrap_or(false) {
        return match stale(msg) {
            Stale::Transparent => LRESULT(HTTRANSPARENT as isize),
            Stale::Faded => {
                crate::motion::faded(hwnd);
                LRESULT(0)
            }
            Stale::Destroyed => {
                crate::a11y::forget(hwnd);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            Stale::Ignored => LRESULT(0),
            Stale::Default => DefWindowProcW(hwnd, msg, wp, lp),
        };
    }
    if ignored_while_closing(msg) && with_pad(|p| p.closing).unwrap_or(false) {
        return LRESULT(0);
    }
    match msg {
        WM_MOUSEMOVE => {
            with_pad(|p| p.on_move(lp));
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            with_pad(|p| p.on_down(lp));
            LRESULT(0)
        }
        WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            with_pad(|p| p.pointer_takes_over());
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            with_pad(|p| p.on_up(lp));
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            with_pad(|p| p.on_right_up(lp));
            LRESULT(0)
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
            with_pad(|p| p.on_wheel(delta, msg == WM_MOUSEHWHEEL));
            LRESULT(0)
        }
        WM_KEYDOWN => {
            with_pad(|p| p.on_key(wp.0 as u16));
            LRESULT(0)
        }
        WM_SYSKEYDOWN if wp.0 as u16 == VK_F10.0 => {
            // Shift+F10: the selected app's menu (not the system menu).
            with_pad(|p| p.on_key(wp.0 as u16));
            LRESULT(0)
        }
        WM_CHAR => {
            with_pad(|p| p.on_char(wp.0 as u16));
            LRESULT(0)
        }
        WM_SYSCHAR => LRESULT(0),
        WM_SYSCOMMAND if (wp.0 & 0xFFF0) as u32 == SC_KEYMENU => LRESULT(0),
        WM_IME_SETCONTEXT => {
            // We draw the composition ourselves; the candidate list stays.
            let lp = LPARAM(lp.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize));
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_IME_STARTCOMPOSITION => {
            with_pad(|p| {
                // A new composition: its own.
                ime_gate(ImeMsg::Start, &mut p.comp, &mut p.arr.ime_dropped);
                p.place_ime();
            });
            LRESULT(0)
        }
        WM_IME_COMPOSITION => {
            with_pad(|p| p.on_composition(lp.0 as u32));
            LRESULT(0)
        }
        WM_IME_ENDCOMPOSITION => {
            with_pad(|p| {
                if ime_gate(ImeMsg::End, &mut p.comp, &mut p.arr.ime_dropped) {
                    p.changed();
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CARET_TIMER => {
            with_pad(|p| {
                p.caret_on = !p.caret_on;
                if p.focus == Focus::Field {
                    p.draw_field();
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == LONG_PRESS_TIMER || wp.0 == HOLD_TIMER || wp.0 == EDGE_TIMER || wp.0 == SETTLE_TIMER => {
            if wp.0 != EDGE_TIMER {
                let _ = KillTimer(Some(hwnd), wp.0);
            }
            with_pad(|p| p.arr_timer(wp.0));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == WAIT_TIMER => {
            let _ = KillTimer(Some(hwnd), WAIT_TIMER);
            with_pad(|p| p.update_msg());
            LRESULT(0)
        }
        WM_LP_MORE => {
            with_pad(|p| p.ensure_neighbours());
            LRESULT(0)
        }
        WM_LP_APPS => {
            with_pad(|p| {
                p.reload_apps();
                // Still empty: loading, failed, or no apps (said so).
                p.update_msg();
            });
            LRESULT(0)
        }
        loader::WM_LP_ICONS => {
            with_pad(|p| p.icons_arrived());
            LRESULT(0)
        }
        organize::WM_LP_USAGE => {
            with_pad(|p| p.usage_arrived(wp.0 as u64));
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            with_pad(|p| p.cancel_drag());
            LRESULT(0)
        }
        WM_CLOSE => {
            with_pad(|p| p.finish(None));
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            // Another window took the focus (an app just opened, a click
            // elsewhere): it animates away over it. Not for its own menu
            // (the shared menu takes the foreground while it is up; how it
            // closed says whether Launchpad stays: menu_closed).
            with_pad(|p| {
                if !p.menu_open {
                    p.finish(None);
                }
            });
            LRESULT(0)
        }
        WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETCLIENTAREAANIMATION.0 => {
            // Windows' animations turned off while open: what moves ends now.
            with_pad(|p| p.motion_changed());
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            // Laid out for the old screen: close.
            with_pad(|p| p.done = true);
            LRESULT(0)
        }
        WM_GETOBJECT => {
            let answer = crate::a11y::on_getobject(hwnd, wp, lp, || {
                with_pad(|p| p.publish());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == crate::a11y::wm_action() => {
            for (key, act) in crate::a11y::take(hwnd) {
                with_pad(|p| p.act(&key, act));
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            crate::a11y::forget(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The Launchpad's picture for the promo film (tests only). Declared
/// down here, with the tests: the tests that read this file's code read
/// it up to its first `#[cfg(test)]`.
#[cfg(test)]
mod promo;
#[cfg(test)]
pub(crate) use promo::font_compare;
#[cfg(test)]
pub(crate) use applib::paint_site_all_apps;

/// Its states for the design audit (tests only).
#[cfg(test)]
mod states_harmony;

#[cfg(test)]
mod tests {
    use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL};
    use super::empty_text;
    use crate::search::AppsState;

    /// Whether this PC has font family `name` (else DirectWrite would lay
    /// the text out in a fallback).
    fn installed(dw: &super::IDWriteFactory, name: &str) -> bool {
        let mut fonts = None;
        let (mut at, mut found) = (0u32, windows::core::BOOL(0));
        unsafe {
            dw.GetSystemFontCollection(&mut fonts, false).is_ok()
                && fonts.is_some_and(|f| f.FindFamilyName(&windows::core::HSTRING::from(name), &mut at, &mut found).is_ok() && found.as_bool())
        }
    }

    #[test]
    fn names_get_the_lines_their_font_lays_out() {
        use super::*;
        let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
        // Segoe UI and Malgun Gothic (the defaults): the usual box at every
        // text size, so the grid looks as it always has.
        for family in ["Segoe UI", "Malgun Gothic"] {
            for scale in crate::fonts::TEXT_SCALES {
                let two = lines_h(&dw, family, grid::LABEL_SIZE * scale, 2).expect("measured");
                assert!(two > grid::LABEL_SIZE * scale * 2.0, "{family} {scale}: {two}");
                assert_eq!(grid::label_box(two), grid::LABEL_H, "{family} {scale}: {two}");
            }
        }
        // A font with tall lines (Javanese Text, part of Windows): a box
        // that holds its two lines, and a long name keeps both lines in it
        // (the old 1.36 em estimate cut it to one line and "…").
        if !installed(&dw, "Javanese Text") {
            eprintln!("Javanese Text is not installed: its part is skipped");
            return;
        }
        for scale in crate::fonts::TEXT_SCALES {
            let size = grid::LABEL_SIZE * scale;
            let two = lines_h(&dw, "Javanese Text", size, 2).expect("measured");
            let box_h = grid::label_box(two);
            assert!(box_h > grid::LABEL_H && box_h >= two, "{scale}: {two} -> {box_h}");
            let g = grid::grid(1920.0, 1048.0, two);
            assert_eq!(g.label_h, box_h);
            let lay = |box_h: f32| unsafe {
                let format = dw.CreateTextFormat(w!("Javanese Text"), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("")).unwrap();
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP).unwrap();
                let sign = dw.CreateEllipsisTrimmingSign(&format).unwrap();
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                format.SetTrimming(&trim, &sign).unwrap();
                let wide: Vec<u16> = "Microsoft Visual Studio Installer Preview Edition for Windows Desktop Developers".encode_utf16().collect();
                let l = dw.CreateTextLayout(&wide, &format, g.label_w(), UNCUT_H).unwrap();
                assert!(line_heights(&l).len() > 2, "{scale}: long enough to be cut");
                l.SetMaxHeight(grid::lines_room(&line_heights(&l), 2, box_h, 0.0)).unwrap();
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m).unwrap();
                // (Lines cut off by the box stay in the count, 0 tall.)
                (line_heights(&l).iter().filter(|&&h| h > 0.0).count(), m.height)
            };
            let (lines, h) = lay(box_h);
            assert_eq!(lines, 2, "{scale}: both lines");
            assert!(h <= box_h, "{scale}: {h} inside {box_h}");
            let old = (size * 1.36 * 2.0).ceil();
            assert_eq!(lay(old).0, 1, "{scale}: the estimate's box held one line");
        }
    }

    #[test]
    fn closing_ignores_input_only() {
        use super::*;
        for m in [WM_MOUSEMOVE, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONUP, WM_MOUSEWHEEL, WM_KEYDOWN, WM_SYSKEYDOWN, WM_CHAR, WM_IME_COMPOSITION] {
            assert!(ignored_while_closing(m), "{m:#x}");
        }
        // What ends the close, and the rest, still gets through.
        for m in [WM_TIMER, WM_LP_CLOSED, WM_ACTIVATE, WM_CAPTURECHANGED, WM_DISPLAYCHANGE, WM_CLOSE, loader::WM_LP_ICONS, WM_LP_APPS] {
            assert!(!ignored_while_closing(m), "{m:#x}");
        }
        // The posted messages do not collide.
        let mine = [WM_LP_APPS, loader::WM_LP_ICONS, WM_LP_MORE, WM_LP_CLOSED];
        let set: std::collections::HashSet<_> = mine.iter().collect();
        assert_eq!(set.len(), mine.len());
    }

    #[test]
    fn arranging_goes_by_the_list_shown() {
        use super::{arrange_ok, remake_needed, tab_next, takes_snapshot, Focus};
        // Partial shown; a Loaded reading published but not applied yet:
        // arranging stays off (the global state says Loaded, the pages
        // show the partial list).
        let shown_partial = false;
        assert!(!arrange_ok(shown_partial, false, 10));
        // The Loaded snapshot applied (nothing arranged yet): on.
        assert!(takes_snapshot(false, true));
        assert!(arrange_ok(true, false, 10));
        // Arranged, unsaved, then an incomplete reading: not applied.
        assert!(!takes_snapshot(true, false));
        assert!(takes_snapshot(true, true), "a complete one is merged");
        assert!(takes_snapshot(false, false), "nothing arranged: shown as it is");
        // The same list read again in full (Partial -> Loaded): the pages are
        // made again (the migration and the Library wait for that).
        assert!(remake_needed(false, false, true));
        assert!(!remake_needed(false, true, true), "the same, complete before too");
        assert!(!remake_needed(false, false, false), "still partial");
        assert!(remake_needed(true, true, true));
        // Tab: the field and the apps, round again (no banner, no power
        // buttons).
        assert_eq!(tab_next(Focus::Field, true, false), Focus::Grid);
        assert_eq!(tab_next(Focus::Grid, true, false), Focus::Field, "round again");
        assert_eq!(tab_next(Focus::Field, true, true), Focus::Grid);
        assert_eq!(tab_next(Focus::Field, false, false), Focus::Field, "the field alone");
        assert_eq!(tab_next(Focus::Grid, false, true), Focus::Field, "no apps: the field");
        assert!(!arrange_ok(true, true, 10), "searching");
        assert!(!arrange_ok(true, false, 0), "no apps");
        // The snapshot pairs a list with its own reading's state.
        let snap = crate::search::AppsSnapshot { list: Default::default(), state: crate::search::AppsState::Loaded, gen: 3 };
        assert!(!snap.complete(), "loaded but empty");
        let one = std::sync::Arc::new(vec![crate::search::sources::App { name: "a".into(), id: "a".into() }]);
        assert!(crate::search::AppsSnapshot { list: one.clone(), state: crate::search::AppsState::Loaded, gen: 4 }.complete());
        assert!(!crate::search::AppsSnapshot { list: one, state: crate::search::AppsState::Partial, gen: 5 }.complete());
    }

    #[test]
    fn a_new_grid_keeps_the_page_and_the_selection() {
        use super::*;
        let ids: Vec<String> = (0..20).map(|i| format!("a{i:02}")).collect();
        let pages = |pp: usize| layout_store::home(&[], &ids, &[], true, pp);
        let before = pages(10);
        assert_eq!(before.len(), 2);
        // a13 selected on the second page; a font with taller lines makes
        // pages of 4: it is still selected, on its new page.
        let (sel, first) = (before[1][3].clone(), before[1][0].clone());
        let after = pages(4);
        let (page, i) = regrid_spot(&after, 4, Some((&sel, 1)), Some(&first), 1);
        assert_eq!(i, Some(13));
        assert_eq!(page, 3);
        assert_eq!(after[page][i.unwrap() % 4], sel);
        // Nothing selected: the page with what led the page shown.
        assert_eq!(regrid_spot(&after, 4, None, Some(&first), 1), (2, None));
        // Back to pages of 10: the same apps where they were.
        assert_eq!(regrid_spot(&before, 10, Some((&sel, 3)), Some(&first), 3), (1, Some(13)));
        // Gone meanwhile (or nothing to go by): the page as it was.
        let gone = LpItem::App("zz".into());
        assert_eq!(regrid_spot(&after, 4, Some((&gone, 1)), None, 1), (1, None));
        assert_eq!(regrid_spot(&after, 0, None, None, 2), (2, None));
    }

    #[test]
    fn a_new_grid_stays_on_the_page_shown_not_an_old_selection() {
        use super::*;
        // The review's case (fix-lpcc3 R02): two Home pages of 35, the
        // sixth app selected on the first, the keyboard put in the field,
        // PageDown to the second page; then a font with other lines.
        let ids: Vec<String> = (0..70).map(|i| format!("a{i:02}")).collect();
        let home = layout_store::home(&[], &ids, &[], true, 35);
        assert_eq!(home.len(), 2);
        let (old_sel, first) = (home[0][5].clone(), home[1][0].clone());
        // Even with that selection still about: the page shown wins, and the
        // selection off it is dropped.
        assert_eq!(regrid_spot(&home, 35, Some((&old_sel, 0)), Some(&first), 1), (1, None));
        // Pages of 20 (a taller name box): still where a35 is, not a05's.
        let small = layout_store::home(&[], &ids, &[], true, 20);
        assert_eq!(regrid_spot(&small, 20, Some((&old_sel, 0)), Some(&first), 1), (1, None));
        // A selection on the page shown leads, as before.
        let here = home[1][3].clone();
        assert_eq!(regrid_spot(&small, 20, Some((&here, 1)), Some(&first), 1), (1, Some(38)));
        // No page shows anything (nothing leads it): the selection may.
        assert_eq!(regrid_spot(&small, 20, Some((&old_sel, 0)), None, 1), (0, Some(5)));

        // And the selection never gets stale that way: paging with the
        // keyboard in the field (or on a button) keeps one only on the new
        // page, so a retheme after it has nothing old to follow.
        let pp = 35;
        assert_eq!(sel_kept(Some(5), 1, pp), None, "a05 left behind on page 0");
        assert_eq!(sel_kept(Some(38), 1, pp), Some(38), "on the page shown: kept");
        assert_eq!(sel_kept(None, 1, pp), None);
        assert_eq!(sel_kept(Some(5), 0, pp), Some(5), "focus moved off the apps on its own page");
        let after_page_down = sel_kept(Some(5), 1, pp);
        let sel = after_page_down.map(|i| (&home[i / pp][i % pp], i / pp));
        assert_eq!(regrid_spot(&small, 20, sel, Some(&first), 1), (1, None));
    }

    #[test]
    fn the_dragged_icon_and_its_ghost_take_the_cells_icon_size() {
        use super::*;
        // The review's case: 640 x 220 DIP, Javanese Text at Large: 7 x 1
        // icons of 32 DIP, no names. The ghost was 64 DIP from the small
        // icon's corner: 16 DIP off its middle both ways, past the grid's
        // bottom (y = 154) into the dots.
        let small = grid::grid(640.0, 220.0, 64.94);
        assert_eq!((small.cols, small.rows, small.icon_edge, small.label_h), (7, 1, 32.0, 0.0));
        let odd = Grid { icon_edge: 33.0, ..small };
        let usual = grid::grid(1920.0, 1048.0, grid::LABEL_H);
        for (g, w, h, want) in [(small, 640.0, 220.0, 32.0), (odd, 640.0, 220.0, 33.0), (usual, 1920.0, 1048.0, grid::ICON)] {
            for s in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
                let px = (grid::ICON * s).round() as u32;
                let work = Rect::new(100.0, 40.0, 100.0 + w, 40.0 + h);
                for rtl in [false, true] {
                    let r = grid::regions(w, h, rtl);
                    for slot in 0..g.per_page() {
                        let what = format!("{w}x{h} icon {want} at {s} rtl={rtl} slot {slot}");
                        let ghost = icon_box(&g, slot, rtl, &work, s, px);
                        let icon = g.icon(slot, rtl).offset(work.l, work.t);
                        // As big as the cell's icon (whole device pixels).
                        assert!((ghost.w() - want).abs() <= 1.0 / s + 0.001 && (ghost.w() - ghost.h()).abs() < 0.001, "{what}: {ghost:?}");
                        assert!((ghost.w() * s - (ghost.w() * s).round()).abs() < 0.001, "{what}: whole pixels");
                        // On its middle (a pixel's snap at the most).
                        assert!((ghost.cx() - icon.cx()).abs() <= 1.0 / s && (ghost.cy() - icon.cy()).abs() <= 1.0 / s, "{what}: {ghost:?} on {icon:?}");
                        // In the grid's region, never over the dots.
                        let area = r.grid.offset(work.l, work.t);
                        assert!(ghost.t >= area.t - 1.0 / s && ghost.b <= area.b + 1.0 / s, "{what}: {ghost:?} in {area:?}");
                        assert!(ghost.b <= r.dots.t + work.t, "{what}: over the dots");
                        // The dragged icon lifted from there: the same square.
                        let d = dragged_box((ghost.cx(), ghost.cy()), ghost.w());
                        for (a, b) in [(d.l, ghost.l), (d.t, ghost.t), (d.r, ghost.r), (d.b, ghost.b)] {
                            assert!((a - b).abs() < 0.001, "{what}: {d:?} {ghost:?}");
                        }
                    }
                }
                // The usual screen: the icon's own pixels, as always.
                if want == grid::ICON {
                    assert_eq!(icon_edge(&g, s, px), px as f32 / s);
                }
            }
        }
    }

    #[test]
    fn hits_follow_the_regions() {
        use super::*;
        // The first review's grid (fix-look2): 640 x 300 DIP, 65 DIP names;
        // the point where the second power button was: the dots' band now,
        // beside the dots: nothing.
        let r = grid::regions(640.0, 300.0, false);
        let (x, y) = (440.0, 245.0);
        assert_eq!(r.at(x, y), Some(grid::Area::Dots));
        assert_eq!(control_hit(&r, x, y, 3, 0, false, true), None);
        // The grid made for that screen has no app there.
        assert_eq!(grid::grid_in(&r, 64.94).slot_at(x, y, false), None);
        // A page dot, only in the dots' region and when they show.
        let d = r.dots_row(4, 0, false).hit(2).expect("shown");
        assert_eq!(control_hit(&r, d.cx(), d.cy(), 4, 0, false, true), Some(Hit::Dot(2)));
        assert_eq!(control_hit(&r, d.cx(), d.cy(), 4, 0, false, false), None, "searching: no dots");
        assert_eq!(control_hit(&r, d.cx(), d.cy(), 1, 0, false, true), None, "one page: no dots");
        // Beside the dots, in the grid or the top: no control.
        assert_eq!(control_hit(&r, 5.0, d.cy(), 4, 0, false, true), None);
        assert_eq!(control_hit(&r, 320.0, 150.0, 4, 0, false, true), None);
        assert_eq!(control_hit(&r, 320.0, 50.0, 4, 0, false, true), None);
        // The latest review's screen: 640 x 220 DIP, Javanese Text at
        // Large. The point that hit the field over an app is the field's
        // region, and no app is there.
        let r = grid::regions(640.0, 220.0, false);
        assert_eq!(r.at(320.0, 50.0), Some(grid::Area::Top));
        assert!(r.field().contains(320.0, 50.0));
        let g = grid::grid_in(&r, 64.94);
        assert_eq!(g.slot_at(320.0, 50.0, false), None);
        // Each app's middle is in the grid's region, under nothing else.
        for rtl in [false, true] {
            let r = grid::regions(640.0, 220.0, rtl);
            for slot in 0..g.per_page() {
                let it = g.item(slot, rtl);
                assert_eq!(r.at(it.cx(), it.cy()), Some(grid::Area::Grid));
                assert_eq!(control_hit(&r, it.cx(), it.cy(), 4, 0, false, true), None);
            }
        }
    }

    #[test]
    fn folder_ids_survive_arranging_again() {
        use super::*;
        let mut old: Pages = vec![vec![LpItem::folder("F", vec!["a".into(), "b".into()])]];
        if let LpItem::Folder { id, .. } = &mut old[0][0] {
            *id = 42;
        }
        let mut new: Pages = vec![vec![LpItem::App("x".into()), LpItem::folder("F", vec!["a".into(), "b".into()]), LpItem::folder("G", vec!["c".into(), "d".into()])]];
        keep_folder_ids(&old, &mut new);
        assert!(matches!(new[0][1], LpItem::Folder { id: 42, .. }));
        assert!(matches!(new[0][2], LpItem::Folder { id: 0, .. }), "no match: as it was");
    }

    #[test]
    fn a_closing_launchpad_never_holds_on_to_a_new_one() {
        use super::*;
        // Every open is a new window that takes clicks and the focus: a
        // Launchpad opened while the last one fades is never click-through.
        assert_eq!(OPEN_EX.0 & (WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0), 0);
        assert_ne!(OPEN_EX.0 & WS_EX_LAYERED.0, 0, "layered from the start");
        // The old one, fading: the pointer passes through it, its animation
        // ending removes it, its destruction its screen reader tree, and
        // nothing else it gets reaches the open one.
        assert_eq!(stale(WM_NCHITTEST), Stale::Transparent);
        assert_eq!(stale(WM_LP_CLOSED), Stale::Faded);
        assert_eq!(stale(WM_DESTROY), Stale::Destroyed);
        // Input and Alt+F4 never reach it (or end it early).
        for m in [WM_CLOSE, WM_KEYDOWN, WM_SYSKEYDOWN, WM_LBUTTONDOWN, WM_MOUSEWHEEL, WM_CHAR] {
            assert_eq!(stale(m), Stale::Ignored, "{m:#x}");
        }
        for m in [WM_ACTIVATE, WM_TIMER, WM_DISPLAYCHANGE, WM_LP_APPS, loader::WM_LP_ICONS] {
            assert_eq!(stale(m), Stale::Default, "{m:#x}");
        }
    }

    #[test]
    fn timers_are_distinct() {
        use super::*;
        let mine = [CARET_TIMER, LONG_PRESS_TIMER, HOLD_TIMER, EDGE_TIMER, SETTLE_TIMER];
        let set: std::collections::HashSet<_> = mine.iter().collect();
        assert_eq!(set.len(), mine.len());
    }

    #[test]
    fn icons_land_on_whole_device_pixels() {
        use super::*;
        // Every scale Windows offers: a grid cell's icon corner, snapped,
        // is a whole pixel, and the icon's edge (px / scale) spans exactly px.
        for s in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 3.0, 3.5] {
            let g = grid::grid(1920.0 / s, 1040.0 / s, grid::LABEL_H);
            let px = (grid::ICON * s).round();
            for slot in 0..g.per_page() {
                let r = g.icon(slot, false);
                let e = px / s;
                let l = snap_to(r.cx() - e / 2.0, s);
                let t_ = snap_to(r.cy() - e / 2.0, s);
                for v in [l * s, t_ * s, (l + e) * s] {
                    assert!((v - v.round()).abs() < 0.01, "{s}: {v}");
                }
                assert!((r.cx() - (l + e / 2.0)).abs() <= 0.5 / s + 0.01, "{s}: still centred");
            }
        }
    }

    #[test]
    fn motion_only_when_everything_allows_it() {
        use super::motion::{allowed, scrim};
        assert!(allowed(true, true, false));
        assert!(!allowed(false, true, false), "reduced motion (crate::motion: Windows' animation effects off, any preset)");
        assert!(!allowed(true, false, false), "Launchpad's effect off");
        assert!(!allowed(true, true, true), "high contrast");
        // Not animating: a short fade, never an instant pop.
        assert_eq!(scrim(true, true, 1.0).0, crate::motion::SCRIM_MS);
        assert_eq!(scrim(false, true, 1.0).0, crate::motion::REDUCED_MS);
        assert_eq!(scrim(false, false, 1.0).0, crate::motion::REDUCED_MS);
    }

    #[test]
    fn empty_screen_says_what_is_going_on() {
        assert_eq!(empty_text(false, false, true, AppsState::Loaded), None, "apps shown");
        assert_eq!(empty_text(true, false, true, AppsState::Loading), Some("launchpad.loading"));
        assert_eq!(empty_text(true, false, true, AppsState::Failed), Some("launchpad.load_failed"), "not an endless loading");
        assert_eq!(empty_text(true, false, true, AppsState::Empty), Some("launchpad.no_apps"));
        // Typing while the list loads: not a premature "no results".
        assert_eq!(empty_text(true, true, true, AppsState::Loading), Some("launchpad.loading"));
        assert_eq!(empty_text(false, true, true, AppsState::Loaded), Some("launchpad.no_results"));
        assert_eq!(empty_text(false, true, false, AppsState::Loaded), None);
        assert_eq!(empty_text(true, true, true, AppsState::Failed), Some("launchpad.load_failed"), "failure shown while searching too");
    }

    #[test]
    fn incomplete_lists_offer_a_retry() {
        use super::{enter_retries, partial_text};
        assert_eq!(partial_text(false, false, AppsState::Partial), Some("launchpad.partial"));
        assert_eq!(partial_text(false, true, AppsState::Partial), None);
        assert_eq!(partial_text(false, false, AppsState::Loaded), None);
        // The failure screen's Enter works while searching.
        assert!(enter_retries(true, true, true, false));
        assert!(enter_retries(true, false, false, false));
        assert!(!enter_retries(true, false, false, true), "an app is chosen: Enter opens it");
        assert!(!enter_retries(true, false, true, false), "searching an incomplete list: Enter opens a result");
        assert!(!enter_retries(false, true, true, false));
    }
    /// Every Launchpad source, without its tests (they may make formats of
    /// their own to probe fonts).
    fn launchpad_code() -> Vec<(&'static str, &'static str)> {
        let all = [
            ("view.rs", include_str!("view.rs")),
            ("view/access.rs", include_str!("view/access.rs")),
            ("view/applib.rs", include_str!("view/applib.rs")),
            ("view/arrange.rs", include_str!("view/arrange.rs")),
            ("cleanup.rs", include_str!("cleanup.rs")),
            ("edit.rs", include_str!("edit.rs")),
            ("folders.rs", include_str!("folders.rs")),
            ("glyph.rs", include_str!("glyph.rs")),
            ("grid.rs", include_str!("grid.rs")),
            ("hotkey.rs", include_str!("hotkey.rs")),
            ("layout_store.rs", include_str!("layout_store.rs")),
            ("library.rs", include_str!("library.rs")),
            ("list.rs", include_str!("list.rs")),
            ("loader.rs", include_str!("loader.rs")),
            ("mod.rs", include_str!("mod.rs")),
            ("motion.rs", include_str!("motion.rs")),
            ("wintap.rs", include_str!("wintap.rs")),
        ];
        all.into_iter().map(|(n, src)| (n, src.find("#[cfg(test)]").map_or(src, |i| &src[..i]))).collect()
    }

    #[test]
    fn launchpad_text_goes_through_the_shared_format() {
        for (name, code) in launchpad_code() {
            assert!(!code.contains("CreateTextFormat("), "{name}: a format of its own (use gfx::ui_format)");
            assert!(!code.contains("FONT_WEIGHT_BOLD"), "{name}: never Bold");
            assert!(!code.contains("w!(\"\")"), "{name}: an empty locale");
        }
    }

    #[test]
    #[ignore = "offscreen Start app menu, no action dispatched"]
    fn taskbar_start_menu_snapshots() {
        use super::{app_commands, menu_items, MenuAct};
        use crate::i18n::t;
        let _lang = crate::i18n::test_lock("ko");
        for hidden in [false, true] {
            let mut rows = app_commands(false, Some(r"C:\Apps\Sample.exe".into()), hidden);
            rows.push(Some((t("launchpad.menu.keep_in_dock").into(), MenuAct::Pin { name: "Sample".into(), path: "Sample.exe".into(), on: true })));
            let (items, actions) = menu_items(rows, &|_| None);
            assert_eq!(actions.contains(&MenuAct::Uninstall), hidden);
            let name = if hidden { "start-hidden-taskbar" } else { "start-visible-taskbar" };
            crate::popup::harmony::shoot(name, &items, &crate::popup::harmony::Shot::hover(t("launchpad.menu.uninstall")));
        }
    }

    #[test]
    fn launchpads_menu_is_the_shared_menu() {
        use super::{menu_items, MenuAct};
        // Rows become the shared menu's items; ids are 1 + the index of
        // what they do. A row that opens the categories is a submenu, its
        // ellipsis gone, the category in force checked.
        let rows = vec![
            Some(("Open".to_string(), MenuAct::Open)),
            None,
            Some(("Move to category\u{2026}".to_string(), MenuAct::MoveTo)),
            Some(("Hide".to_string(), MenuAct::Hide)),
        ];
        let cats = |a: &MenuAct| (*a == MenuAct::MoveTo).then(|| vec![("Games".to_string(), MenuAct::AddHome, false), ("Tools".to_string(), MenuAct::RemoveHome, true)]);
        let (items, acts) = menu_items(rows, &cats);
        assert_eq!(acts, vec![MenuAct::Open, MenuAct::MoveTo, MenuAct::AddHome, MenuAct::RemoveHome, MenuAct::Hide]);
        assert_eq!(items.len(), 4);
        assert_eq!((items[0].id, items[0].label.as_str()), (1, "Open"));
        assert!(items[1].separator);
        assert_eq!((items[2].id, items[2].label.as_str()), (2, "Move to category"));
        let sub = &items[2].sub;
        assert_eq!(sub.iter().map(|i| (i.id, i.checked)).collect::<Vec<_>>(), vec![(3, false), (4, true)]);
        assert_eq!(items[3].id, 5);
        // The overlay never draws a menu of its own.
        let code = include_str!("view.rs");
        let code = &code[..code.find("#[cfg(test)]").unwrap()];
        for gone in ["paint_card", "Card::Menu", "MENU_ROW"] {
            assert!(!code.contains(gone), "{gone}");
        }
    }

    #[test]
    fn after_its_menu_launchpad_stays_unless_another_app_is_in_front() {
        use super::keeps_after_menu;
        assert!(keeps_after_menu(true, false, true));
        assert!(keeps_after_menu(false, false, true), "the menu's own window, fading");
        assert!(keeps_after_menu(false, true, false), "nothing in front");
        assert!(!keeps_after_menu(false, false, false), "another app");
    }

    #[test]
    fn a_quiet_blocks_button_is_12_under_its_line() {
        use super::{quiet_stack, retry_rect};
        use crate::theme::metrics::size;
        // The glyph 24, 8, a line of 18: 50; alone, centered in 96.
        assert_eq!(quiet_stack(18.0, false).0, 23.0);
        // With the button (12 and 28 more: 90): from 3, the button at 65.
        let (top, button) = quiet_stack(18.0, true);
        assert_eq!((top, button), (3.0, 65.0));
        assert_eq!(button - (top + 24.0 + 8.0 + 18.0), 12.0);
        assert!(button + size::CONTROL_H <= size::QUIET_MIN_H);
        // The button: its label 12 from its ends, centered in the block.
        let b = retry_rect(300.0, 56.0, button);
        assert_eq!((b.w(), b.h(), b.cx()), (80.0, 28.0, 150.0));
    }

    #[test]
    fn a_slow_app_list_shows_nothing_then_the_ring_then_its_line() {
        use super::wait_stage;
        assert_eq!(wait_stage(0), (false, false, Some(300)));
        assert_eq!(wait_stage(299), (false, false, Some(1)));
        assert_eq!(wait_stage(300), (true, false, Some(4700)));
        assert_eq!(wait_stage(4999), (true, false, Some(1)));
        assert_eq!(wait_stage(5000), (true, true, None));
    }

    #[test]
    fn a_menu_drops_the_composition_and_what_the_ime_sends_for_it_later() {
        use super::{drop_composition, ime_gate, ImeMsg};
        // A composition shown in the field ("ㅎ" being typed).
        let (mut comp, mut dropped) = (vec!['ㅎ'], false);
        // The menu opens: it goes from the field (filter again) before the
        // IME is told to cancel it.
        assert!(drop_composition(&mut comp, &mut dropped));
        assert!(comp.is_empty() && dropped);
        // What the IME sends while it cancels finds the overlay borrowed
        // (with_pad: nothing runs). Then, late, its result and its end:
        assert!(!ime_gate(ImeMsg::Composition, &mut comp, &mut dropped), "a late result is not read");
        assert!(comp.is_empty(), "nothing of it comes back");
        assert!(!ime_gate(ImeMsg::End, &mut comp, &mut dropped), "nothing shown went: no filtering");
        assert!(!dropped);
        // The end lost too (sent during the cancel): a late result is
        // still ignored, until the next composition starts.
        let (mut comp, mut dropped) = (vec!['한'], false);
        drop_composition(&mut comp, &mut dropped);
        assert!(!ime_gate(ImeMsg::Composition, &mut comp, &mut dropped));
        ime_gate(ImeMsg::Start, &mut comp, &mut dropped);
        assert!(ime_gate(ImeMsg::Composition, &mut comp, &mut dropped), "a new composition is read");
        // Nothing being composed: nothing to drop (no filtering, and the
        // next composition is not held back).
        let (mut comp, mut dropped) = (Vec::new(), false);
        assert!(!drop_composition(&mut comp, &mut dropped));
        assert!(!dropped);
        // A composition that ends as usual: filtered again without it.
        let (mut comp, mut dropped) = (vec!['ㄱ'], false);
        assert!(ime_gate(ImeMsg::End, &mut comp, &mut dropped));
        assert!(comp.is_empty());
        // The menu's part runs it before the IME's cancel.
        let code = include_str!("view.rs");
        let code = &code[code.find("fn take_menu").unwrap()..];
        let code = &code[..code.find("fn menu_closed").unwrap()];
        let (dropped_at, cancel_at) = (code.find("drop_composition(").unwrap(), code.find("CPS_CANCEL").unwrap());
        assert!(dropped_at < cancel_at);
    }

    #[test]
    fn the_try_again_button_runs_only_for_a_press_begun_and_ended_on_it() {
        use super::{released_on, retry_hit, Hit, Rect};
        let button = Some(Rect::new(100.0, 200.0, 180.0, 228.0));
        let at = |x: f32, y: f32| retry_hit(Hit::Empty, button, x, y);
        let (on, beside, above) = (at(140.0, 214.0), at(190.0, 214.0), at(140.0, 190.0));
        assert_eq!((on, beside, above), (Hit::Retry, Hit::Empty, Hit::Empty));
        // An app or a dot under it stays itself; no button, nothing to hit.
        assert_eq!(retry_hit(Hit::App(3), button, 140.0, 214.0), Hit::App(3));
        assert_eq!(retry_hit(Hit::Dot(1), button, 140.0, 214.0), Hit::Dot(1));
        assert_eq!(retry_hit(Hit::Empty, None, 140.0, 214.0), Hit::Empty);
        // Outside, then onto it: nothing (not a retry).
        assert_eq!(released_on(above, on), None);
        // On it, then off it: cancelled (not a click on empty space, which
        // would close Launchpad).
        assert_eq!(released_on(on, beside), None);
        // On it and let go on it: it runs.
        assert_eq!(released_on(on, on), Some(Hit::Retry));
        // Empty space to empty space still closes (as before).
        assert_eq!(released_on(above, beside), Some(Hit::Empty));
    }

    #[test]
    fn the_ring_stops_turning_when_motion_is_turned_off_while_it_shows() {
        use super::{ring_plan, Ring};
        let at = (400.0, 300.0);
        // Shown while animating: it turns.
        let turning = ring_plan(None, Some(at), true).unwrap();
        assert_eq!(turning, Some(Ring { at, turns: true }));
        // Asked again, the same: left as it is.
        assert_eq!(ring_plan(turning, Some(at), true), None);
        // Full -> reduced while it shows (same place): drawn again, still.
        assert_eq!(ring_plan(turning, Some(at), false), Some(Some(Ring { at, turns: false })));
        // Reduced from the start: still; back to full, it turns again.
        let still = ring_plan(None, Some(at), false).unwrap();
        assert_eq!(still, Some(Ring { at, turns: false }));
        assert_eq!(ring_plan(still, Some(at), true), Some(Some(Ring { at, turns: true })));
        // Gone: hidden once.
        assert_eq!(ring_plan(still, None, false), Some(None));
        assert_eq!(ring_plan(None, None, true), None);
        // The settings and the theme changing reach it.
        let code = include_str!("view.rs");
        let body = |f: &str| {
            let c = &code[code.find(f).unwrap()..];
            c[..c[1..].find("\n    fn ").unwrap()].to_string()
        };
        assert!(body("fn motion_changed").contains("self.spin("), "motion_changed");
        assert!(body("fn restyle").contains("self.respin()"), "restyle");
    }

    #[test]
    fn the_field_and_the_names_are_on_the_ramp() {
        // Folder and clean-up titles, names and the field: on the ramp.
        let ramp: Vec<f32> = crate::theme::ramp::RAMP.iter().map(|r| r.size).collect();
        for v in [super::super::folders::TITLE.size, super::super::grid::LABEL_SIZE, super::FIELD_TEXT] {
            assert!(ramp.contains(&v), "{v} is off the ramp");
        }
    }
}

