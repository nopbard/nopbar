//! Stacks (folders pinned to the dock): clicking one lists the folder off
//! the UI thread (crate::stacks on the "stacks" worker), then opens a panel
//! next to the icon: a grid of icons with names for a few items
//! (Downloads-style), the menu list with small icons for more. The last row
//! opens the folder in File Explorer.
//!
//! Items drag out of the panel (a real shell drag: Explorer,
//! the desktop or any app decides what the drop does) and files dropped on
//! the panel go into the folder (dock/drag.rs does the same for the icon).

use super::*;
use crate::i18n::t;
use crate::popup::Align;
use crate::stacks::Listing;
use crate::theme::metrics::{size, space};
use crate::theme::{Level, Role, State};
use crate::wash::{raised_state, Hover, Lit, Mark, Raised, Shape};
use windows::UI::Composition::ShapeVisual;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP};
use std::cell::Cell;
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, E_POINTER, POINTL, SIZE, S_OK};
use windows::Win32::Graphics::Gdi::{CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, WIN32_FIND_DATAW};
use windows::Win32::System::Com::{CoCreateInstance, CreateBindCtx, IDataObject, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Ole::{
    IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, RegisterDragDrop, RevokeDragDrop, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK,
    DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MODIFIERKEYS_FLAGS, MK_LBUTTON, MK_RBUTTON};
use windows::Win32::UI::Shell::{
    IDragSourceHelper, IDropTargetHelper, IFileSystemBindData, IFileSystemBindData_Impl, IShellItem, SHCreateItemFromParsingName, SHDoDragDrop,
    BHID_DataObject, CLSID_DragDropHelper, SHDRAGIMAGE, STR_FILE_SYS_BIND_DATA,
};
use windows_core::implement;

/// Posted when a listing is ready to be shown (outside the dock's borrow).
pub(super) const WM_SHOW_STACK: u32 = WM_APP + 73;
/// A listing not back after this is given up: the panel opens with the
/// "can't open" row and "Open in File Explorer" instead (the work itself is
/// bounded by crate::stacks::BUDGET, a little shorter).
const STALE_MS: u32 = 2500;
/// One-shot: the listing did not come back in time.
pub(super) const STACK_TIMER: usize = 14;
/// Grid metrics (DIP; design system 3.2, 3.3). The panel's two lines: the
/// tiles and the footer's row (what fills under the pointer) on the
/// container line, the heading, the hairline and the footer's words on the
/// content line; mirrored on the right.
const PAD: f32 = size::PANEL_PAD;
const LINE: f32 = size::PANEL_PAD + size::CARD_PAD;
const HEADER_H: f32 = size::HEAD_H;
/// A tile (the pointer's fill is all of it) and the gap to the next: the
/// panel's "edge to container 8, container to container 8".
const TILE_W: f32 = 88.0;
const TILE_H: f32 = 96.0;
const TILE_GAP: f32 = space::S3;
pub(super) const GRID_ICON: f32 = 48.0;
const NAME_SIZE: f32 = crate::theme::ramp::FOOTNOTE.size;
/// Above the footer: a hairline in 8 DIP of space (the menus' separator),
/// from content line to content line.
const SEP_H: f32 = size::SEP_H;
/// The footer: a one-line row.
const FOOTER_H: f32 = size::ROW_M;
/// A tile's icon from its top, the name's gap under it and its side inset.
const ICON_TOP: f32 = space::S3;
const NAME_GAP: f32 = space::S2;
const NAME_INSET: f32 = space::S2;
// Two lines of names fit under the icon.
const _: () = assert!(ICON_TOP + GRID_ICON + NAME_GAP + 2.0 * crate::theme::ramp::FOOTNOTE.line <= TILE_H);
/// An empty folder: the quiet block (11.1). One that cannot be opened: the
/// block, then RETRY_GAP and its button.
const EMPTY_H: f32 = size::QUIET_MIN_H;
const FAIL_H: f32 = size::QUIET_MIN_H + space::S5;
const RETRY_GAP: f32 = space::S4;
const COLS: usize = 4;
/// Room around the panel for its shadow.
const SHADOW_M: f32 = crate::theme::metrics::SHADOW_M;
/// List rows: "open folder" id (entries are 1..=LIMIT), and "try again".
const OPEN_FOLDER_ID: u32 = 1000;
const RETRY_ID: u32 = 1001;
/// Panel timer: the fade-out after a drag left it is over; hide it.
const HIDE_TIMER: usize = 1;

/// A listing ready to show.
pub(super) struct StackPlan {
    title: String,
    listing: Box<Listing>,
    at: POINT,
    align: Align,
    scale: f32,
    theme: Theme,
    gfx: Rc<Gfx>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Entry(usize),
    OpenFolder,
    /// Entry dragged out with this button (MK_LBUTTON / MK_RBUTTON).
    Drag(usize, u32),
    /// "Try again": the folder is listed anew.
    Retry,
    Nothing,
}

impl Dock {
    /// Item `i` (a stack) was clicked: list its folder in the background.
    pub(super) fn open_stack(&mut self, i: usize) {
        let Some(it) = self.items.get(i).filter(|it| it.stack) else { return };
        super::views::stack_from(self.hwnd);
        self.stack_seq += 1;
        let path = it.path.clone();
        self.last_stack = path.clone();
        self.stack_req = Some((self.stack_seq, path.clone(), Instant::now()));
        let grid_px = (GRID_ICON * self.scale).round() as u32;
        let list_px = (16.0 * self.scale).round() as u32;
        self.workers.request_stack(self.stack_seq, &path, grid_px, list_px);
        unsafe {
            SetTimer(Some(self.hwnd), STACK_TIMER, STALE_MS, None);
        }
        preview::hide();
        self.hide_label();
    }

    /// STACK_TIMER: the listing is late. Open the panel without it (the
    /// folder can still be opened in File Explorer); a late result is dropped.
    pub(super) fn stack_timed_out(&mut self) {
        let Some((seq, path, _)) = self.stack_req.clone() else { return };
        crate::log!("stack: listing not back in {STALE_MS} ms; showing the fallback");
        self.stack_listed(Box::new(Listing::failed(seq, &path)));
    }

    /// A stack asked for and not shown yet (its listing on the way, its
    /// timeout, or a plan waiting for WM_SHOW_STACK) is forgotten: a
    /// fullscreen app came to the front. A late listing is dropped.
    pub(super) fn drop_stack_request(&mut self) {
        if self.stack_req.take().is_some() || self.stack_plan.take().is_some() {
            crate::log!("stack: a fullscreen app came; the stack waiting to open is dropped");
        }
        self.stack_plan = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), STACK_TIMER);
        }
    }

    /// A listing came back (or the wait ended).
    pub(super) fn stack_listed(&mut self, l: Box<Listing>) {
        if self.fullscreen {
            // (Asked for just before the app came to the front.)
            self.drop_stack_request();
            return;
        }
        let req = self.stack_req.as_ref().map(|(seq, path, _)| (*seq, path.as_str()));
        if !listing_wanted(req, l.seq, &l.folder, self.fullscreen) {
            return;
        }
        let Some((_, path, _)) = self.stack_req.as_ref() else { return };
        unsafe {
            let _ = KillTimer(Some(self.hwnd), STACK_TIMER);
        }
        let Some(i) = self.items.iter().position(|it| it.stack && it.path.eq_ignore_ascii_case(path)) else {
            self.stack_req = None;
            return;
        };
        self.stack_req = None;
        let it = &self.items[i];
        // The popup gap off the dock, past the bar and the tallest a
        // magnified icon gets, centered on the icon.
        let gap = crate::placement::popup_gap(self.scale);
        let at = super::menu::menu_point(self.frame.pos, self.bar_screen_rect(), Some(self.peak_rect(i)), POINT::default(), gap);
        let align = match self.frame.pos {
            Position::Bottom => Align::Above,
            Position::Left => Align::RightOf,
            Position::Right => Align::LeftOf,
        };
        // Stopped at the deadline: say these are not the whole folder.
        let title = if l.partial { format!("{} …", it.name) } else { it.name.clone() };
        self.stack_plan = Some(StackPlan { title, listing: l, at, align, scale: self.scale, theme: self.theme.clone(), gfx: self.gfx.clone() });
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_SHOW_STACK, WPARAM(0), LPARAM(0));
        }
    }
}

/// A listing (`seq`, `folder`) opens a panel: it answers the request
/// waiting now (`req`: none once dropped) and the dock is not covered by a
/// fullscreen app (pure, tested).
fn listing_wanted(req: Option<(u64, &str)>, seq: u64, folder: &str, covered: bool) -> bool {
    !covered && req.is_some_and(|(s, p)| s == seq && p.eq_ignore_ascii_case(folder))
}

/// Shows a ready stack (call outside the dock's borrow: it runs a nested
/// message loop) and opens what was picked.
pub(super) fn show() {
    let _companion_pause = crate::companion::pause_ui();
    let Some(owner) = super::views::token() else { return };
    // Taken in any case; shown only while the dock is not covered.
    let Some(plan) = super::views::with_token(owner, |d| d.stack_plan.take().filter(|_| !d.fullscreen)).flatten() else { return };
    if crate::popup::is_tracking() || is_open() {
        return;
    }
    // The dock stays while its stack is up (views::reconcile waits).
    let _modal = super::views::Modal::begin(owner);
    // The grid starts its own drags (it stays up until the pointer leaves).
    // A grid too big for the room past the anchor (a small or high-DPI
    // screen) is the list instead, which scrolls in that room. A folder
    // that could not be opened says so in the panel (its quiet block).
    let pick = if shows_grid(plan.listing.ok, plan.listing.grid) && unsafe { grid_fits_screen(&plan) } { unsafe { grid(&plan) } } else { list(&plan) };
    match pick {
        Pick::Entry(k) => {
            if let Some(e) = plan.listing.entries.get(k) {
                // Deleted or moved since the folder was read: said so.
                crate::launch::open(&crate::launch::Target::new(&e.path, "", crate::launch::Source::Stack));
            }
        }
        Pick::OpenFolder => {
            let folder = plan.listing.folder.clone();
            crate::launch::open(&crate::launch::Target::new("explorer.exe", &plan.title, crate::launch::Source::Stack).args(&format!("\"{folder}\""), ""));
        }
        // From the list (it closed when the drag began).
        Pick::Drag(k, button) => unsafe { drag_out(&plan, k, button, None) },
        // Listed anew: its panel opens again when the listing is back.
        Pick::Retry => {
            let folder = plan.listing.folder.clone();
            with_dock(|d| {
                if let Some(i) = d.items.iter().position(|it| it.stack && it.path.eq_ignore_ascii_case(&folder)) {
                    d.open_stack(i);
                }
            });
        }
        Pick::Nothing => {}
    }
}

/// Pure: a listing opens as the grid panel: a folder that asks for one,
/// or one that could not be opened (the panel's quiet block and "Try
/// again"); else the list.
fn shows_grid(ok: bool, grid: bool) -> bool {
    !ok || grid
}

/// The list form's rows for `l` under `title`: the header, the entries
/// (ids 1..), what is said of an unreadable, empty or cut-short folder,
/// and "Open in File Explorer" last.
fn list_rows(title: &str, l: &Listing) -> Vec<crate::popup::Item> {
    use crate::popup::Item as Row;
    // The folder's and its files' names: literal (UIA reads "R&D" whole).
    // The header parts it from the files (no separator under it).
    let mut rows = vec![Row::new(0, title.to_string()).head().literal()];
    if !l.ok {
        // Something went wrong (design system 11.4): one line and its
        // button.
        rows.push(Row::failed(RETRY_ID, t("dock.stack.unavailable"), t("launch.action.retry")));
    } else if l.entries.is_empty() {
        // An empty list's one line (11.1).
        rows.push(Row::quiet(t("dock.stack.empty")));
    }
    for (k, e) in l.entries.iter().enumerate() {
        let mut r = Row::new(k as u32 + 1, e.name.clone()).literal();
        r.icon = l.icons.get(k).cloned().flatten();
        // No icon: the folder or file glyph (the menu's own glyph ink).
        if r.icon.is_none() {
            r.glyph = Some(missing_icon(e.dir));
        }
        rows.push(r);
    }
    if l.partial {
        rows.push(Row::new(0, "…").enabled(false));
    }
    rows.push(Row::sep());
    rows.push(Row::new(OPEN_FOLDER_ID, t("dock.stack.open_in_explorer")));
    rows
}

/// The list form: our menu with small icons.
fn list(plan: &StackPlan) -> Pick {
    let l = &plan.listing;
    let rows = list_rows(&plan.title, l);
    let out =crate::popup::track(crate::popup::Request {
        gfx: &plan.gfx,
        theme: &plan.theme,
        items: rows,
        at: plan.at,
        align: plan.align,
        scale: plan.scale,
        titles: Default::default(),
        on_slide: None,
        // The entries (not the "open in File Explorer" row).
        drag_ids: 1..OPEN_FOLDER_ID,
        // Space on an entry: Quick Look (the list stays open).
        on_space: Some(Box::new({
            let paths: Vec<String> = l.entries.iter().map(|e| e.path.clone()).collect();
            move |id, _, _| {
                if let Some(p) = (id as usize).checked_sub(1).and_then(|k| paths.get(k)) {
                    crate::launchers::preview(p);
                }
                true
            }
        })),
        on_pick: None,
        // Off the dock like its menus: it keeps to the room past the
        // anchor (scrolling), never over the gap.
        hangs: true,
    });
    match out {
        crate::popup::Outcome::Picked(OPEN_FOLDER_ID) => Pick::OpenFolder,
        crate::popup::Outcome::Picked(RETRY_ID) => Pick::Retry,
        crate::popup::Outcome::Dragged(id) if (1..OPEN_FOLDER_ID).contains(&id) => Pick::Drag(id as usize - 1, MK_LBUTTON.0),
        crate::popup::Outcome::Picked(id) if (1..OPEN_FOLDER_ID).contains(&id) => Pick::Entry(id as usize - 1),
        _ => Pick::Nothing,
    }
}

// ---- the grid panel -----------------------------------------------------------------

/// What a grid panel holds, laid out (DIP).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct GridPlan {
    pub tiles: Vec<Rect>,
    /// Under the heading, over the footer's separator: the tiles (or an
    /// empty or unreadable folder's quiet block).
    pub body: Rect,
    /// The footer's row.
    pub footer: Rect,
    pub width: f32,
    pub height: f32,
}

/// Pure layout of the grid for `n` entries (`failed`: the folder could not
/// be opened: its block and button instead): PAD, the heading, the tiles
/// TILE_GAP apart, the separator's slot, the footer's row, PAD.
pub(super) fn grid_layout(n: usize, failed: bool) -> GridPlan {
    let n = if failed { 0 } else { n };
    let cols = n.clamp(3, COLS);
    let rows = n.div_ceil(cols);
    let pitch = TILE_W + TILE_GAP;
    let width = PAD * 2.0 + cols as f32 * pitch - TILE_GAP;
    let body_h = match (n, failed) {
        (_, true) => FAIL_H,
        (0, _) => EMPTY_H,
        _ => rows as f32 * (TILE_H + TILE_GAP) - TILE_GAP,
    };
    let top = PAD + HEADER_H;
    // A single short row sits in the middle.
    let shift = if n < cols { (cols - n) as f32 * pitch / 2.0 } else { 0.0 };
    let tiles = (0..n)
        .map(|k| {
            let (r, c) = (k / cols, k % cols);
            Rect { x: PAD + shift + c as f32 * pitch, y: top + r as f32 * (TILE_H + TILE_GAP), w: TILE_W, h: TILE_H }
        })
        .collect();
    let body = Rect { x: PAD, y: top, w: width - PAD * 2.0, h: body_h };
    let footer = Rect { x: PAD, y: top + body_h + SEP_H, w: width - PAD * 2.0, h: FOOTER_H };
    GridPlan { tiles, body, footer, width, height: footer.y + FOOTER_H + PAD }
}

/// Keyboard move inside the grid: `cur` (n = the footer) by (dx, dy).
pub(super) fn grid_step(n: usize, cur: Option<usize>, dx: i32, dy: i32) -> usize {
    let cols = n.clamp(3, COLS) as i32;
    let Some(c) = cur else { return 0 };
    if c >= n {
        // On the footer: up goes back to the last tile.
        return if dy < 0 && n > 0 { n - 1 } else { n };
    }
    let c = c as i32;
    let next = if dx != 0 { c + dx } else { c + dy * cols };
    if next < 0 {
        0
    } else if next as usize >= n {
        n
    } else {
        next as usize
    }
}

/// What can be pointed at or reached by key in the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    Tile(usize),
    /// "Try again" (a folder that could not be opened).
    Retry,
    /// "Open in File Explorer".
    Footer,
}

/// Pure: where key `vk` takes the keyboard's cursor (Tab, Shift+Tab with
/// `back`; the arrows, left and right in reading order: the grid is
/// mirrored right to left), from `cur` over `n` tiles (`failed`: the
/// button alone). None: the key does not move it (a media or letter key
/// shows nothing: crate::input).
fn cursor_after(vk: u16, back: bool, rtl: bool, n: usize, failed: bool, cur: Option<Spot>) -> Option<Spot> {
    Some(match vk {
        v if v == VK_TAB.0 => tab_step(n, failed, cur, back),
        v if v == VK_LEFT.0 || v == VK_RIGHT.0 => key_step(n, failed, cur, crate::i18n::mirror::arrow(rtl, v == VK_RIGHT.0), 0),
        v if v == VK_UP.0 => key_step(n, failed, cur, 0, -1),
        v if v == VK_DOWN.0 => key_step(n, failed, cur, 0, 1),
        _ => return None,
    })
}

/// Pure: where an arrow key takes the keyboard's cursor: over the tiles and
/// down to the footer (grid_step); with the folder not opened, between its
/// button and the footer.
fn key_step(n: usize, failed: bool, cur: Option<Spot>, dx: i32, dy: i32) -> Spot {
    if failed {
        return match cur {
            Some(Spot::Retry) => Spot::Footer,
            Some(_) => Spot::Retry,
            None => if dx + dy >= 0 { Spot::Retry } else { Spot::Footer },
        };
    }
    let at = cur.and_then(|s| match s {
        Spot::Tile(k) => Some(k),
        Spot::Footer => Some(n),
        Spot::Retry => None,
    });
    let k = grid_step(n, at, dx, dy);
    if k < n { Spot::Tile(k) } else { Spot::Footer }
}

/// Pure: Tab's next stop (`back`: Shift): the tiles are one stop (arrows
/// move inside them), the footer the other.
fn tab_step(n: usize, failed: bool, cur: Option<Spot>, back: bool) -> Spot {
    if failed {
        return key_step(n, true, cur, 0, if back { -1 } else { 1 });
    }
    match cur {
        Some(Spot::Tile(_)) => Spot::Footer,
        Some(Spot::Footer) if n > 0 => Spot::Tile(0),
        None if n > 0 && !back => Spot::Tile(0),
        _ => Spot::Footer,
    }
}

/// What the grid's spots show: the one under the pointer, the keyboard's
/// cursor, the one held down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct Lights {
    hover: Option<Spot>,
    cursor: Option<Spot>,
    ring: bool,
    pressed: Option<Spot>,
}

impl Lights {
    fn focus_to(&mut self, s: Spot, ring: bool) {
        self.cursor = Some(s);
        self.ring = ring;
        self.hover = None;
    }

    fn visible_cursor(&self) -> Option<Spot> {
        self.cursor.filter(|_| self.ring)
    }

    /// Pure: `s`'s state for the theme's colors (what high contrast draws
    /// from): held down, else the keyboard's cursor, else under the pointer.
    fn state(&self, s: Spot) -> State {
        if self.pressed == Some(s) {
            State::Pressed
        } else if self.visible_cursor() == Some(s) && s != Spot::Retry {
            State::Cursor
        } else if self.hover == Some(s) {
            State::Hover
        } else {
            State::Rest
        }
    }

    /// Pure: the pointer's fill (crate::wash::Hover): the tile or the
    /// footer held down, else the one under the pointer; never where the
    /// keyboard's cursor is (one fill a spot), never the button (it is
    /// raised: its own fill).
    fn pointer(&self) -> Option<(Spot, bool)> {
        let want = match self.pressed {
            Some(s) => Some((s, true)),
            None => self.hover.map(|s| (s, false)),
        };
        want.filter(|(s, _)| *s != Spot::Retry && self.visible_cursor() != Some(*s))
    }

    /// Pure: the keyboard's cursor fill (crate::wash::Mark): under a tile
    /// or the footer. On the button the cursor is Tab's focus ring.
    fn mark(&self) -> Option<Spot> {
        self.visible_cursor().filter(|s| *s != Spot::Retry)
    }

    /// Pure: what of this the picture itself shows (the fills are visuals
    /// under it): everything in high contrast (outlines, inverted text),
    /// else only the ring on the button.
    fn drawn(&self, hc: bool) -> Lights {
        if hc {
            *self
        } else {
            Lights { cursor: self.visible_cursor().filter(|s| *s == Spot::Retry), ring: self.visible_cursor() == Some(Spot::Retry), ..Lights::default() }
        }
    }

    /// Pure: what Enter acts on: the one highlight (6.2): the keyboard's
    /// cursor, else what the pointer is on.
    fn chosen(&self) -> Option<Spot> {
        self.cursor.or(self.hover)
    }
}

struct Grid {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    /// Everything (its opacity fades the panel in and out).
    root: ContainerVisual,
    /// The picture: heading, icons, names, footer.
    text_v: SpriteVisual,
    /// Under the picture: the fills (crate::wash).
    wash: Hover<Spot>,
    mark: Mark,
    raised: Raised,
    gfx: Rc<Gfx>,
    theme: Theme,
    title: String,
    listing: Listing,
    /// The layout, the tiles already mirrored right to left.
    lay: GridPlan,
    /// "Try again" (DIP), with a folder that could not be opened.
    retry: Option<Rect>,
    rtl: bool,
    /// The tiles' paths (Space previews one: crate::launchers::preview).
    paths: Vec<String>,
    /// Screen px of the panel (hit testing).
    rect: RECT,
    scale: f32,
    lights: Lights,
    /// What the picture was last drawn with (Lights::drawn).
    drawn: Lights,
    /// Item the press began on, with which button (MK_*) and where (px).
    down: Option<(Spot, u32, POINT)>,
    /// Where the pointer last was (a move that is none does not count).
    last_mouse: Option<POINT>,
    done: Option<Pick>,
}

thread_local! {
    static GRID: RefCell<Option<Grid>> = const { RefCell::new(None) };
}

fn with_grid<R>(f: impl FnOnce(&mut Grid) -> R) -> Option<R> {
    GRID.with(|g| g.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// A stack panel is open.
pub(super) fn is_open() -> bool {
    GRID.with(|g| g.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

/// Close an open stack panel (theme change, quitting).
pub(super) fn cancel() {
    if let Some(h) = with_grid(|g| {
        g.done.get_or_insert(Pick::Nothing);
        g.hwnd
    }) {
        unsafe {
            let _ = PostMessageW(Some(h), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

/// Pure: a spot's key in the screen readers' tree.
fn spot_key(s: Spot) -> String {
    match s {
        Spot::Tile(k) => format!("tile:{k}"),
        Spot::Retry => "retry".into(),
        Spot::Footer => "footer".into(),
    }
}

/// Pure: the spot a tree key names, if it is there (`n` tiles, the button
/// or not).
fn spot_of(key: &str, n: usize, retry: bool) -> Option<Spot> {
    match key {
        "retry" if retry => Some(Spot::Retry),
        "footer" => Some(Spot::Footer),
        _ => key.strip_prefix("tile:").and_then(|k| k.parse::<usize>().ok()).filter(|&k| k < n).map(Spot::Tile),
    }
}

/// Pure: a spot's shape for its fill: a tile and the footer's row sit on
/// the container line (the panel's corner less that), the button is a
/// control.
fn spot_shape(lay: &GridPlan, retry: Option<Rect>, radii: &crate::theme::Radii, s: Spot) -> Option<Shape> {
    let row = crate::theme::concentric(radii.panel, PAD);
    let (r, rad) = match s {
        Spot::Tile(k) => (*lay.tiles.get(k)?, row),
        Spot::Footer => (lay.footer, row),
        Spot::Retry => (retry?, radii.control),
    };
    Some(Shape::new(r.x, r.y, r.w, r.h, rad))
}

impl Grid {
    /// The spot under screen point `p`.
    fn hit(&self, p: POINT) -> Option<Spot> {
        let (x, y) = ((p.x - self.rect.left) as f32 / self.scale, (p.y - self.rect.top) as f32 / self.scale);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
        if let Some(k) = self.lay.tiles.iter().position(inside) {
            return Some(Spot::Tile(k));
        }
        if self.retry.as_ref().is_some_and(inside) {
            return Some(Spot::Retry);
        }
        inside(&self.lay.footer).then_some(Spot::Footer)
    }

    fn shape(&self, s: Spot) -> Option<Shape> {
        spot_shape(&self.lay, self.retry, &self.theme.radii(), s)
    }

    /// The fills under the picture follow the lights (crate::wash): the
    /// pointer's fading on and off (a press at once), the keyboard's
    /// cursor at once, the button's own. What the picture itself shows of
    /// them (Lights::drawn) is drawn again when it changed.
    fn sync(&mut self) {
        self.lights.pressed = self.down.map(|d| d.0);
        let c = self.gfx.compositor.clone();
        let l = self.lights;
        let lit = l.pointer().and_then(|(t, down)| self.shape(t).map(|shape| Lit { t, down, shape }));
        self.wash.show(&c, &self.theme, lit);
        self.mark.show(&self.theme, l.mark().and_then(|s| self.shape(s)));
        let button = self.shape(Spot::Retry).map(|shape| (shape, raised_state(l.hover == Some(Spot::Retry), l.pressed == Some(Spot::Retry))));
        self.raised.show(&c, &self.theme, button);
        let want = l.drawn(self.theme.hc.is_some());
        if want != self.drawn {
            self.drawn = want;
            self.redraw();
        }
        self.publish();
    }

    /// Screen readers: the tree, while one reads this window.
    fn publish(&self) {
        if crate::a11y::wanted(self.hwnd) {
            crate::a11y::publish(self.hwnd, self.tree());
        }
    }

    /// The heading, each file (an item), what is said of an empty or
    /// unreadable folder and its "Try again", then "Open in File
    /// Explorer". The keyboard's cursor is the focus.
    fn tree(&self) -> crate::a11y::Tree {
        use crate::a11y::{px_rect, Node, Role as Ax, Tree};
        let (origin, s) = ((self.rect.left, self.rect.top), self.scale);
        let at = |r: Rect| px_rect(origin, s, r.x, r.y, r.w, r.h);
        let lay = &self.lay;
        let mut nodes = vec![Node::new("head", Ax::Text, self.title.clone(), at(Rect { x: LINE, y: PAD, w: lay.width - 2.0 * LINE, h: HEADER_H }))];
        for (k, r) in lay.tiles.iter().enumerate() {
            if let Some(e) = self.listing.entries.get(k) {
                nodes.push(Node::new(spot_key(Spot::Tile(k)), Ax::MenuItem, e.name.clone(), at(*r)).invoke());
            }
        }
        if !self.listing.ok {
            nodes.push(Node::new("quiet", Ax::Text, t("dock.stack.unavailable"), at(lay.body)));
        } else if lay.tiles.is_empty() {
            nodes.push(Node::new("quiet", Ax::Text, t("dock.stack.empty"), at(lay.body)));
        }
        if let Some(r) = self.retry {
            nodes.push(Node::new(spot_key(Spot::Retry), Ax::Button, t("launch.action.retry"), at(r)).invoke());
        }
        nodes.push(Node::new(spot_key(Spot::Footer), Ax::MenuItem, t("dock.stack.open_in_explorer"), at(lay.footer)).invoke());
        let focus = self.lights.cursor.map(spot_key);
        Tree { role: Ax::Menu, name: self.title.clone(), nodes, focus, focusable: true, active: true, focus_in: None }
    }

    /// What a screen reader asked of child `key`.
    fn asked(&mut self, key: &str, act: crate::a11y::Act) {
        use crate::a11y::Act;
        let spot = spot_of(key, self.lay.tiles.len(), self.retry.is_some());
        match (spot, act) {
            (_, Act::Refresh) => self.publish(),
            (Some(s), Act::Invoke) => self.pick(s),
            (Some(s), Act::Focus) => self.key_to(s, crate::input::reader_focus(self.lights.ring)),
            _ => {}
        }
    }

    /// The picture again, with what it shows of the lights.
    fn redraw(&self) {
        let s = self.scale;
        let (w_px, h_px) = ((self.lay.width * s).ceil() as i32, (self.lay.height * s).ceil() as i32);
        let art = grid_art(&self.gfx, &self.theme, &self.title, &self.listing, &self.lay, w_px, s, self.rtl);
        let drawn = self.drawn;
        let surface = self.gfx.draw(w_px as u32, h_px as u32, |dc, ox, oy| unsafe { paint_grid(dc, ox, oy, &art, &self.gfx, &self.theme, drawn) });
        if let Ok(brush) = surface.and_then(|surf| self.gfx.compositor.CreateSurfaceBrushWithSurface(&surf)) {
            let _ = brush.SetStretch(CompositionStretch::Fill);
            let _ = self.text_v.SetBrush(&brush);
        }
    }

    /// The pointer is on `h` now: its fill there; the keyboard's cursor
    /// goes (6.2: one highlight, the later input wins).
    fn point(&mut self, h: Option<Spot>) {
        if self.lights.hover == h && self.lights.cursor.is_none() {
            return;
        }
        self.lights.hover = h;
        self.lights.cursor = None;
        self.lights.ring = false;
        self.sync();
    }

    /// Key `vk`: Esc closes, Space looks at the chosen file (or presses
    /// the footer, the button), Enter picks, Tab and the arrows move the
    /// keyboard's cursor (only they: crate::input, no other key shows it).
    /// The file to look at, after the grid's borrow.
    fn on_key(&mut self, vk: u16) -> Option<String> {
        self.lights.ring |= crate::input::navigates(vk);
        self.sync();
        let (n, failed) = (self.lay.tiles.len(), !self.listing.ok);
        let cur = self.lights.cursor.or(self.lights.hover);
        match vk {
            v if v == VK_ESCAPE.0 => self.done = Some(Pick::Nothing),
            // Quick Look (shortcut "stack_quick_look"): the chosen file. On
            // the footer (or the button) Space presses it, like Enter.
            v if v == VK_SPACE.0 => match self.lights.chosen() {
                Some(Spot::Tile(k)) => return self.paths.get(k).cloned(),
                Some(s) => self.pick(s),
                None => {}
            },
            v if v == VK_RETURN.0 => {
                if let Some(s) = self.lights.chosen() {
                    self.pick(s);
                }
            }
            _ => {
                let back = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                if let Some(s) = cursor_after(vk, back, crate::i18n::rtl(), n, failed, cur) {
                    self.key_to(s, self.lights.ring);
                }
            }
        }
        None
    }

    /// The keyboard's cursor to `s`: at once, and the pointer's fill goes.
    fn key_to(&mut self, s: Spot, ring: bool) {
        self.lights.focus_to(s, ring);
        self.wash.clear();
        self.sync();
    }

    fn pick(&mut self, s: Spot) {
        self.done = Some(match s {
            Spot::Tile(k) => Pick::Entry(k),
            Spot::Retry => Pick::Retry,
            Spot::Footer => Pick::OpenFolder,
        });
    }

    /// A drag left the panel: fade it out, then
    /// hide it so drops go to whatever is below.
    fn fade_out(&self) {
        let c = self.gfx.compositor.clone();
        // (Hidden when the fade it really got is over.)
        let ms = crate::motion::fade_to(&c, &self.root, 0.0, crate::motion::FADE_OUT_MS, crate::motion::EASE_IN).unwrap_or(0);
        unsafe {
            SetTimer(Some(self.hwnd), HIDE_TIMER, ms.max(1) as u32, None);
        }
    }
}

fn d2d(c: crate::theme::Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

unsafe fn register_class() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(grid_proc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarStack"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassExW(&wc);
}

/// Pure: whether a grid panel `w_px` x `h_px` fits the room past `at` by
/// `align` (above it for a bottom dock, beside it for a side dock) inside
/// the work area `wa`: placed there (grid_rect) it keeps the gap.
fn grid_fits(align: Align, at: POINT, w_px: i32, h_px: i32, wa: RECT) -> bool {
    let (ww, wh) = (wa.right - wa.left, wa.bottom - wa.top);
    match align {
        Align::Above => w_px <= ww && h_px <= at.y - wa.top,
        Align::RightOf => w_px <= wa.right - at.x && h_px <= wh,
        Align::LeftOf => w_px <= at.x - wa.left && h_px <= wh,
        _ => w_px <= ww && h_px <= wh,
    }
}

/// grid_fits for `plan`'s grid on the monitor of its anchor.
unsafe fn grid_fits_screen(plan: &StackPlan) -> bool {
    let lay = grid_layout(plan.listing.entries.len(), !plan.listing.ok);
    let (width, height, s) = (lay.width, lay.height, plan.scale);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(MonitorFromPoint(plan.at, MONITOR_DEFAULTTONEAREST), &mut mi);
    grid_fits(plan.align, plan.at, (width * s).ceil() as i32, (height * s).ceil() as i32, mi.rcWork)
}

/// Pure: the grid panel's own rect (screen px; its window adds the shadow
/// margin around it) for a panel `w_px` x `h_px`: its near side on `at`
/// (the popup gap off the dock already in it) by `align`, kept inside the
/// work area `wa`.
fn grid_rect(align: Align, at: POINT, w_px: i32, h_px: i32, wa: RECT) -> RECT {
    let (x, y) = match align {
        Align::Above => (at.x - w_px / 2, at.y - h_px),
        Align::RightOf => (at.x, at.y - h_px / 2),
        Align::LeftOf => (at.x - w_px, at.y - h_px / 2),
        Align::TopLeft => (at.x, at.y),
        Align::TopRight => (at.x - w_px, at.y),
        Align::BottomLeft => (at.x, at.y - h_px),
        Align::BottomRight => (at.x - w_px, at.y - h_px),
    };
    let x = x.min(wa.right - w_px).max(wa.left);
    let y = y.min(wa.bottom - h_px).max(wa.top);
    RECT { left: x, top: y, right: x + w_px, bottom: y + h_px }
}

type Layout = windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;

/// The ink of the glyph that stands for an icon (missing_icon) in the
/// grid: the secondary text's, 3:1 at least on every plate, card and row
/// fill as text sees them (Theme::dim_for).
fn missing_ink(theme: &Theme, st: State) -> crate::theme::Rgba {
    theme.text_of(Level::Secondary, st)
}

/// What stands for an icon that could not be loaded: a folder's or a
/// file's glyph.
fn missing_icon(dir: bool) -> crate::glyphs::Glyph {
    if dir {
        crate::glyphs::Glyph::Folder
    } else {
        crate::glyphs::Glyph::File
    }
}

/// That glyph's size, a share of GRID_ICON (a mark in the icon's box, not
/// a tile as large as one).
const MISSING_ICON: f32 = 0.6;

/// What the grid's picture is drawn from (px at `s`): the heading, the
/// tiles' names (two lines at most), the footer and, for a folder that
/// could not be opened, its button; with the layout (DIP, the tiles
/// already mirrored) and the icons.
struct GridArt<'a> {
    title: Option<(Layout, f32, f32)>,
    foot: Option<(Layout, f32, f32)>,
    names: Vec<Option<(Layout, f32)>>,
    /// "Try again": its words and where it is (DIP).
    retry: Option<((Layout, f32, f32), Rect)>,
    lay: &'a GridPlan,
    icons: &'a [Option<(u32, Vec<u8>)>],
    /// The entries (a folder or a file: the glyph that stands for an icon
    /// that could not be loaded).
    entries: &'a [crate::stacks::Entry],
    /// The folder is empty / could not be opened: the quiet block.
    empty: bool,
    failed: bool,
    w_px: i32,
    s: f32,
    rtl: bool,
}

/// The grid's texts laid out for `l` under `title`, the panel `w_px` wide.
#[allow(clippy::too_many_arguments)]
fn grid_art<'a>(gfx: &Gfx, theme: &Theme, title: &str, l: &'a Listing, lay: &'a GridPlan, w_px: i32, s: f32, rtl: bool) -> GridArt<'a> {
    use crate::theme::ramp;
    let family = theme.font.family.clone();
    // The heading (the menus': Footnote, the tertiary color) and the
    // footer's row (Body) in the ramp's styles.
    let title = gfx.text_style(title, &family, ramp::FOOTNOTE, s).ok();
    let foot = gfx.text_style(t("dock.stack.open_in_explorer"), &family, ramp::BODY, s).ok();
    let names: Vec<_> = l
        .entries
        .iter()
        .take(lay.tiles.len())
        .map(|e| gfx.text_layout_wrapped(&e.name, &family, NAME_SIZE * s, windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL, (TILE_W - NAME_INSET * 2.0) * s, 2).ok())
        .collect();
    // The button: its words and BUTTON_PAD_X each side, under the block,
    // in the middle.
    let retry = (!l.ok)
        .then(|| gfx.text_style(t("launch.action.retry"), &family, ramp::BODY, s).ok())
        .flatten()
        .map(|label| {
            let w = (label.1 / s + 2.0 * size::BUTTON_PAD_X).ceil();
            let b = lay.body;
            let top = b.y + (b.h - retry_block_h()) / 2.0 + retry_block_h() - size::CONTROL_H;
            (label, Rect { x: ((lay.width - w) / 2.0).round(), y: top.round(), w, h: size::CONTROL_H })
        });
    GridArt { title, foot, names, retry, lay, icons: &l.icons, entries: &l.entries, empty: l.ok && l.entries.is_empty(), failed: !l.ok, w_px, s, rtl }
}

/// Pure: the height of an unreadable folder's block with its button: the
/// quiet block (a 24 glyph, 8, one Body line), RETRY_GAP, the button.
fn retry_block_h() -> f32 {
    size::ICON_L + space::S3 + crate::theme::ramp::BODY.line + RETRY_GAP + size::CONTROL_H
}

fn px_rect(ox: f32, oy: f32, r: Rect, s: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: ox + r.x * s, top: oy + r.y * s, right: ox + (r.x + r.w) * s, bottom: oy + (r.y + r.h) * s }
}

/// A fill and, in high contrast, an outline (inside its edge) for
/// something of `role` in state `st` (theme::fill_of / outline_of), px.
unsafe fn plate(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, theme: &Theme, role: Role, st: State, r: D2D_RECT_F, rad: f32, s: f32) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT;
    let fill = theme.fill_of(role, st);
    if fill.a > 0 {
        dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &dc.CreateSolidColorBrush(&d2d(fill), None)?);
    }
    if let Some((c, w)) = theme.outline_of(role, st) {
        let (w, h) = (w * s, w * s / 2.0);
        let inner = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: r.left + h, top: r.top + h, right: r.right - h, bottom: r.bottom - h }, radiusX: (rad - h).max(0.0), radiusY: (rad - h).max(0.0) };
        dc.DrawRoundedRectangle(&inner, &dc.CreateSolidColorBrush(&d2d(c), None)?, w, None);
    }
    Ok(())
}

/// Draws the grid's content (px) from (`ox`, `oy`): the heading, the tiles'
/// icons and names (or the quiet block), the hairline and the footer. The
/// plate and the fills are visuals; `lit` is what the picture itself shows
/// of the state (Lights::drawn): high contrast's outlines, fills and
/// inverted text, and the keyboard's ring on the button.
unsafe fn paint_grid(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, ox: f32, oy: f32, a: &GridArt, gfx: &Gfx, theme: &Theme, lit: Lights) -> Result<()> {
    let (s, w_px, rtl, lay) = (a.s, a.w_px, a.rtl, a.lay);
    let hc = theme.hc.is_some();
    let ink = |level: Level, st: State| dc.CreateSolidColorBrush(&d2d(theme.text_of(level, st)), None);
    let opts = windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT;
    let mx = |x: f32, w: f32| crate::i18n::mirror::span(rtl, x, w, ox, ox + w_px as f32);
    let radii = theme.radii();
    let row_rad = crate::theme::concentric(radii.panel, PAD) * s;
    if let Some((tl, tw, th)) = &a.title {
        crate::gfx::draw_text(dc, Vector2 { X: mx(ox + LINE * s, *tw), Y: (oy + PAD * s + (HEADER_H * s - th) / 2.0).round() }, tl, &ink(Level::Tertiary, State::Rest)?, opts);
    }
    if a.empty || a.failed {
        // The quiet block (11.1): the folder's glyph and "Empty"; the
        // warning and "Can't open the folder", with its button under it.
        let b = lay.body;
        let (top, h) = if a.failed { (b.y + (b.h - retry_block_h()) / 2.0, retry_block_h() - RETRY_GAP - size::CONTROL_H) } else { (b.y, b.h) };
        let area = D2D_RECT_F { left: ox, top: oy + top * s, right: ox + w_px as f32, bottom: oy + (top + h) * s };
        let (kind, text) = if a.failed { (crate::gfx::Quiet::Error, t("dock.stack.unavailable")) } else { (crate::gfx::Quiet::Empty, t("dock.stack.empty")) };
        gfx.quiet(dc, area, kind, crate::glyphs::Glyph::Folder, text, theme, s, 0.0)?;
    }
    if let Some(((bl, bw, bh), r)) = &a.retry {
        let st = raised_state(lit.hover == Some(Spot::Retry), lit.pressed == Some(Spot::Retry));
        let br = px_rect(ox, oy, *r, s);
        if hc {
            plate(dc, theme, Role::Raised, st, br, radii.control * s, s)?;
        }
        if lit.visible_cursor() == Some(Spot::Retry) {
            crate::gfx::focus_ring(dc, br, radii.control * s, theme, s)?;
        }
        crate::gfx::draw_text(dc, Vector2 { X: ((br.left + br.right - bw) / 2.0).round(), Y: ((br.top + br.bottom - bh) / 2.0).round() }, bl, &ink(Level::Primary, st)?, opts);
    }
    for (k, r) in lay.tiles.iter().enumerate() {
        let st = lit.state(Spot::Tile(k));
        if hc {
            plate(dc, theme, Role::Row, st, px_rect(ox, oy, *r, s), row_rad, s)?;
        }
        if let Some(Some((px, pixels))) = a.icons.get(k) {
            use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
            use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC};
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            if let Ok(bmp) = dc.CreateBitmap(D2D_SIZE_U { width: *px, height: *px }, Some(pixels.as_ptr() as _), px * 4, &props) {
                let side = GRID_ICON * s;
                let ix = ox + (r.x + (r.w - GRID_ICON) / 2.0) * s;
                let iy = oy + (r.y + ICON_TOP) * s;
                let dest = D2D_RECT_F { left: ix, top: iy, right: ix + side, bottom: iy + side };
                dc.DrawBitmap(&bmp, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
            }
        } else if let Some(e) = a.entries.get(k) {
            // No icon (not loaded in time, or none to load): the folder or
            // file glyph in the secondary ink, not an empty spot (final
            // audit 9: a pale stand-in vanished on a light glass plate).
            let (cx, cy) = (ox + (r.x + r.w / 2.0) * s, oy + (r.y + ICON_TOP + GRID_ICON / 2.0) * s);
            crate::gfx::draw_glyph(dc, missing_icon(e.dir), cx, cy, GRID_ICON * MISSING_ICON * s, d2d(missing_ink(theme, st)))?;
        }
        if let Some(Some((nl, _))) = a.names.get(k) {
            crate::gfx::draw_text(dc, Vector2 { X: ox + (r.x + NAME_INSET) * s, Y: (oy + (r.y + ICON_TOP + GRID_ICON + NAME_GAP) * s).round() }, nl, &ink(Level::Primary, st)?, opts);
        }
    }
    // One device pixel (1 DIP in high contrast), on a whole pixel, from
    // content line to content line.
    let footer = lay.footer;
    let (ly, lh) = crate::popup::hairline_px(oy + (footer.y - SEP_H / 2.0) * s, if hc { 1.0 } else { 0.0 }, s);
    let line = dc.CreateSolidColorBrush(&d2d(theme.hairline()), None)?;
    dc.FillRectangle(&D2D_RECT_F { left: ox + LINE * s, top: ly, right: ox + w_px as f32 - LINE * s, bottom: ly + lh }, &line);
    let st = lit.state(Spot::Footer);
    if hc {
        plate(dc, theme, Role::Row, st, px_rect(ox, oy, footer, s), row_rad, s)?;
    }
    if let Some((fl, fw, fh)) = &a.foot {
        crate::gfx::draw_text(dc, Vector2 { X: mx(ox + LINE * s, *fw), Y: (oy + footer.y * s + (FOOTER_H * s - fh) / 2.0).round() }, fl, &ink(Level::Primary, st)?, opts);
    }
    Ok(())
}

/// Builds the panel window and its visuals.
unsafe fn build_grid(plan: &StackPlan) -> Result<Grid> {
    let s = plan.scale;
    let l = &plan.listing;
    let mut lay = grid_layout(l.entries.len(), !l.ok);
    let (width, height) = (lay.width, lay.height);
    // Right to left, the first file is top right (the grid is mirrored;
    // icons and names are not).
    let rtl = crate::i18n::rtl();
    for r in &mut lay.tiles {
        r.x = crate::i18n::mirror::span(rtl, r.x, r.w, 0.0, width);
    }
    let (w_px, h_px) = ((width * s).ceil() as i32, (height * s).ceil() as i32);
    let at = plan.at;
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST), &mut mi);
    let rect = grid_rect(plan.align, at, w_px, h_px, mi.rcWork);
    let m_px = (SHADOW_M * s).ceil() as i32;
    register_class();
    let hwnd = CreateWindowExW(
        WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        w!("NopBarStack"),
        // The folder's name (what a screen reader says of the window).
        &HSTRING::from(plan.title.as_str()),
        WS_POPUP,
        rect.left - m_px,
        rect.top - m_px,
        w_px + m_px * 2,
        h_px + m_px * 2,
        None,
        None,
        Some(GetModuleHandleW(None)?.into()),
        None,
    )?;
    let built = (|| -> Result<Grid> {
        let enable = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &enable as *const _ as *const _, 4);
        let c = plan.gfx.compositor.clone();
        let interop: ICompositorDesktopInterop = c.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
        let outer = c.CreateContainerVisual()?;
        outer.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
        outer.SetOffset(Vector3 { X: m_px as f32, Y: m_px as f32, Z: 0.0 })?;
        target.SetRoot(&outer)?;
        let (root, border): (ContainerVisual, ShapeVisual) = crate::popup::chrome(&plan.gfx, &plan.theme, &outer, width, height, s, plan.theme.radii().panel)?;
        // The fills under the picture (crate::wash).
        let under = c.CreateContainerVisual()?;
        let (mark, wash, raised) = (Mark::new(&c, &under)?, Hover::new(&c, &under, false)?, Raised::new(&c, &under)?);

        let art = grid_art(&plan.gfx, &plan.theme, &plan.title, l, &lay, w_px, s, rtl);
        let retry = art.retry.as_ref().map(|(_, r)| *r);
        let content = plan.gfx.draw(w_px as u32, h_px as u32, |dc, ox, oy| paint_grid(dc, ox, oy, &art, &plan.gfx, &plan.theme, Lights::default()))?;
        drop(art);
        let text_v = c.CreateSpriteVisual()?;
        let brush = c.CreateSurfaceBrushWithSurface(&content)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        text_v.SetBrush(&brush)?;
        text_v.SetSize(Vector2 { X: width, Y: height })?;
        root.Children()?.InsertBelow(&under, &border)?;
        root.Children()?.InsertBelow(&text_v, &border)?;
        // It opens as panels do (crate::motion::open: it grows on GLIDE,
        // its light comes up after it; a short fade only with reduced
        // motion), from the point of its edge nearest the icon that
        // opened it (7.4: `at` is over the icon's middle, the gap away).
        let from = crate::motion::origin((0.0, 0.0, width, height), Some(((at.x - rect.left) as f32 / s, (at.y - rect.top) as f32 / s, 0.0, 0.0)));
        crate::motion::open(&c, &outer, (width, height), from, crate::motion::Kind::Panel, s)?;
        // Files dropped on the open panel go into its folder.
        let drop: IDropTarget = PanelDrop {
            folder: l.folder.clone(),
            hwnd,
            helper: super::drag::drop_helper(),
            files: RefCell::new(Vec::new()),
            source: Cell::new(0),
            right: Cell::new(0),
        }
        .into();
        if let Err(e) = RegisterDragDrop(hwnd, &drop) {
            crate::log!("stack: panel takes no drops: {e}");
        }
        let paths = l.entries.iter().map(|e| e.path.clone()).collect();
        let mut g = Grid {
            hwnd,
            _target: target,
            root: outer,
            text_v,
            wash,
            mark,
            raised,
            gfx: plan.gfx.clone(),
            theme: plan.theme.clone(),
            title: plan.title.clone(),
            listing: (**l).clone(),
            lay,
            retry,
            rtl,
            paths,
            rect,
            scale: s,
            lights: Lights::default(),
            drawn: Lights::default(),
            down: None,
            last_mouse: None,
            done: None,
        };
        // (The button's fill is there from the start.)
        g.sync();
        Ok(g)
    })();
    if built.is_err() {
        let _ = DestroyWindow(hwnd);
    }
    built
}

/// Shows the grid panel until something is picked or it is dismissed.
unsafe fn grid(plan: &StackPlan) -> Pick {
    let g = match build_grid(plan) {
        Ok(g) => g,
        Err(e) => {
            crate::log!("stack: cannot open the panel: {e}");
            return list(plan);
        }
    };
    let hwnd = g.hwnd;
    GRID.with(|s| *s.borrow_mut() = Some(g));
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetFocus(Some(hwnd));
    SetCapture(hwnd);
    crate::a11y::menu_event(hwnd, true);
    let mut msg = MSG::default();
    if GetForegroundWindow() == hwnd && GetCapture() == hwnd {
        loop {
            if with_grid(|g| g.done.is_some()).unwrap_or(true) {
                break;
            }
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 || r.0 == -1 {
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    } else {
        crate::log!("stack: could not take foreground/capture; not opening");
    }
    let _ = ReleaseCapture();
    let mut pick = with_grid(|g| g.done).flatten().unwrap_or(Pick::Nothing);
    // Dragged out: it already faded and hid as the drag left it.
    let dragged = matches!(pick, Pick::Drag(..));
    if let Pick::Drag(k, button) = pick {
        // The panel stays up (and fades once the pointer leaves it); no
        // borrow is held across the drag's modal loop.
        let rect = with_grid(|g| g.rect);
        drag_out(plan, k, button, rect);
        pick = Pick::Nothing;
    }
    let _ = RevokeDragDrop(hwnd);
    crate::a11y::menu_event(hwnd, false);
    // It fades out as menus do (crate::motion::retire: click-through
    // meanwhile), then goes.
    match GRID.with(|s| s.borrow_mut().take()) {
        Some(g) if !dragged => match g.root.Compositor() {
            Ok(c) => {
                let root = g.root.clone();
                crate::motion::retire(hwnd, &c, &root, Box::new(g));
            }
            Err(_) => {
                let _ = DestroyWindow(hwnd);
            }
        },
        _ => {
            let _ = DestroyWindow(hwnd);
        }
    }
    pick
}

fn screen_pt(hwnd: HWND, lp: LPARAM) -> POINT {
    let mut p = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
    unsafe {
        let _ = ClientToScreen(hwnd, &mut p);
    }
    p
}

unsafe extern "system" fn grid_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Fading out after it closed: nothing reaches it (another may be open).
    if crate::motion::retired(hwnd) {
        return if msg == WM_NCHITTEST { LRESULT(HTTRANSPARENT as isize) } else { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    match msg {
        WM_MOUSEMOVE => {
            let p = screen_pt(hwnd, lp);
            let (cx, cy) = (GetSystemMetrics(SM_CXDRAG).max(4), GetSystemMetrics(SM_CYDRAG).max(4));
            with_grid(|g| {
                // A pressed tile moved past the drag box: drag it out.
                if let Some((Spot::Tile(k), button, at)) = g.down {
                    if crate::stacks::past_drag_threshold(p.x - at.x, p.y - at.y, cx, cy) {
                        g.down = None;
                        g.done = Some(Pick::Drag(k, button));
                        return;
                    }
                }
                // Only a real move counts (the first message tells where
                // the pointer was when the panel came up under it).
                let moved = g.last_mouse.is_some_and(|q| q != p);
                g.last_mouse = Some(p);
                if moved {
                    let h = g.hit(p);
                    g.point(h);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let p = screen_pt(hwnd, lp);
            with_grid(|g| {
                let inside = p.x >= g.rect.left && p.x < g.rect.right && p.y >= g.rect.top && p.y < g.rect.bottom;
                if !inside {
                    // A press outside closes it (and is not passed on).
                    g.done = Some(Pick::Nothing);
                } else {
                    // Left: click or drag; right: drag only (Explorer then
                    // asks what to do), a tile only.
                    let k = g.hit(p);
                    g.down = match msg {
                        WM_LBUTTONDOWN => k.map(|k| (k, MK_LBUTTON.0, p)),
                        WM_RBUTTONDOWN => k.filter(|k| matches!(k, Spot::Tile(_))).map(|k| (k, MK_RBUTTON.0, p)),
                        _ => None,
                    };
                    g.last_mouse = Some(p);
                    g.lights.hover = k;
                    g.lights.cursor = None;
                    g.lights.ring = false;
                    // (Its pressed fill, at once.)
                    g.sync();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let p = screen_pt(hwnd, lp);
            with_grid(|g| {
                // Picks only what the (left) press began on.
                let down = g.down.take();
                if let (Some(k), Some((d, button, _))) = (g.hit(p), down) {
                    if k == d && button == MK_LBUTTON.0 {
                        g.pick(k);
                    }
                }
                g.sync();
            });
            LRESULT(0)
        }
        WM_RBUTTONUP | WM_MBUTTONUP => {
            with_grid(|g| {
                g.down = None;
                g.sync();
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == HIDE_TIMER => {
            let _ = KillTimer(Some(hwnd), HIDE_TIMER);
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u16;
            let look = with_grid(|g| g.on_key(vk))
            .flatten();
            // Outside the grid's borrow.
            if let Some(path) = look {
                crate::launchers::preview(&path);
            }
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            with_grid(|g| {
                g.done.get_or_insert(Pick::Nothing);
            });
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if GetCapture() != hwnd {
                with_grid(|g| {
                    g.done.get_or_insert(Pick::Nothing);
                });
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_GETOBJECT => {
            let answer = crate::a11y::on_getobject(hwnd, wp, lp, || {
                with_grid(|g| g.publish());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == crate::a11y::wm_action() => {
            for (key, act) in crate::a11y::take(hwnd) {
                with_grid(|g| g.asked(&key, act));
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

// ---- dragging out ------------------------------------------------------------------

/// Find data handed to the shell's parser, so a listed path becomes an item
/// without touching the disk (any path may sit behind a link to a dead
/// share, whatever its drive letter says).
#[implement(IFileSystemBindData)]
struct FindData(Cell<WIN32_FIND_DATAW>);

impl IFileSystemBindData_Impl for FindData_Impl {
    fn SetFindData(&self, pfd: *const WIN32_FIND_DATAW) -> windows_core::Result<()> {
        if !pfd.is_null() {
            self.0.set(unsafe { *pfd });
        }
        Ok(())
    }

    fn GetFindData(&self, pfd: *mut WIN32_FIND_DATAW) -> windows_core::Result<()> {
        if pfd.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *pfd = self.0.get() };
        Ok(())
    }
}

/// The shell's own data object for one listed item (what Explorer would
/// drag: file names, shell ids, everything targets look for). Built from
/// the listing (folder or file), never from the disk: no existence check
/// either. An item gone since the listing is reported by the drop target.
unsafe fn shell_data(path: &str, dir: bool) -> Result<IDataObject> {
    let p = HSTRING::from(path);
    let mut fd = WIN32_FIND_DATAW { dwFileAttributes: if dir { FILE_ATTRIBUTE_DIRECTORY.0 } else { FILE_ATTRIBUTE_NORMAL.0 }, ..Default::default() };
    let name: Vec<u16> = crate::stacks::last_part(path).encode_utf16().take(fd.cFileName.len() - 1).collect();
    fd.cFileName[..name.len()].copy_from_slice(&name);
    let bind: IFileSystemBindData = FindData(Cell::new(fd)).into();
    let ctx = CreateBindCtx(0)?;
    ctx.RegisterObjectParam(STR_FILE_SYS_BIND_DATA, &bind)?;
    let item: IShellItem = SHCreateItemFromParsingName(&p, &ctx)?;
    item.BindToHandler(None, &BHID_DataObject)
}

/// A top-down 32-bit bitmap of premultiplied BGRA pixels.
unsafe fn dib(px: u32, pixels: &[u8]) -> Option<HBITMAP> {
    let n = (px * px * 4) as usize;
    if px == 0 || pixels.len() < n {
        return None;
    }
    let bi = BITMAPINFO {
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
    let mut bits = std::ptr::null_mut();
    let bmp = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(bmp.into());
        return None;
    }
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, n);
    Some(bmp)
}

/// The picture under the pointer while dragging: the icon from the listing
/// (never the shell's own picture: making it would read the file).
unsafe fn set_drag_image(data: &IDataObject, icon: Option<&(u32, Vec<u8>)>) {
    let Some((px, pixels)) = icon else { return };
    let Ok(helper) = CoCreateInstance::<_, IDragSourceHelper>(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) else { return };
    {
        if let Some(bmp) = dib(*px, pixels) {
            let side = *px as i32;
            let img = SHDRAGIMAGE {
                sizeDragImage: SIZE { cx: side, cy: side },
                ptOffset: POINT { x: side / 2, y: side / 2 },
                hbmpDragImage: bmp,
                crColorKey: COLORREF(0xFFFF_FFFF),
            };
            // On success the helper owns the bitmap.
            if helper.InitializeFromBitmap(&img, data).is_ok() {
                return;
            }
            let _ = DeleteObject(bmp.into());
        }
    }
}

/// Our side of the drag: drop when the button that started it comes up,
/// cancel on Esc or the other button; fades the panel once the pointer
/// leaves it.
#[implement(IDropSource)]
struct PanelSource {
    button: u32,
    panel: Option<RECT>,
    left: Cell<bool>,
}

impl IDropSource_Impl for PanelSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> windows_core::HRESULT {
        if escape.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        let other = if self.button == MK_RBUTTON.0 { MK_LBUTTON.0 } else { MK_RBUTTON.0 };
        if keys.0 & other != 0 {
            return DRAGDROP_S_CANCEL;
        }
        if keys.0 & self.button == 0 {
            return DRAGDROP_S_DROP;
        }
        if let (false, Some(r)) = (self.left.get(), self.panel) {
            let mut p = POINT::default();
            let outside = unsafe { GetCursorPos(&mut p) }.is_ok() && !(p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom);
            if outside {
                self.left.set(true);
                with_grid(|g| g.fade_out());
            }
        }
        S_OK
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> windows_core::HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// The mouse button (MK_LBUTTON / MK_RBUTTON, logical) is held right now.
fn still_down(button: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    // GetAsyncKeyState reads physical buttons.
    let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
    let left = (button == MK_LBUTTON.0) != swapped;
    let vk = if left { VK_LBUTTON } else { VK_RBUTTON };
    let state = unsafe { GetAsyncKeyState(vk.0 as i32) };
    state < 0
}

/// Drags entry `k` out with `button` (a shell drag: the target decides
/// between move, copy and shortcut like with any Explorer drag). Runs a
/// modal loop: call with no RefCell borrowed. `panel`: the grid's rect.
unsafe fn drag_out(plan: &StackPlan, k: usize, button: u32, panel: Option<RECT>) {
    let Some(e) = plan.listing.entries.get(k) else { return };
    // Released already (a very quick flick): nothing to drag, and a drop
    // under the pointer right away would surprise.
    if !still_down(button) {
        return;
    }
    let data = match shell_data(&e.path, e.dir) {
        Ok(d) => d,
        Err(err) => {
            crate::log!("stack: cannot drag {:?}: {err}", e.path);
            return;
        }
    };
    set_drag_image(&data, plan.listing.icons.get(k).and_then(|i| i.as_ref()));
    let source: IDropSource = PanelSource { button, panel, left: Cell::new(false) }.into();
    let all = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0 | DROPEFFECT_LINK.0);
    let effect = match SHDoDragDrop(None, &data, &source, all) {
        Ok(effect) => effect,
        Err(err) => {
            crate::log!("stack: drag failed: {err}");
            return;
        }
    };
    let performed = super::drag::performed_effect(&data);
    match move_result(effect.0, performed) {
        MoveResult::Moved => crate::log!("stack: item moved out by the target"),
        // An old-style target copied and expects us to delete the original.
        // We never delete: it stays, so the move ended as a copy.
        MoveResult::LeftToUs => crate::log!("stack: target asked the source to finish a move; original kept (never deleted by us)"),
        MoveResult::Other => crate::log!("stack: dragged out an item, effect {}", effect.0),
    }
    if move_result(effect.0, performed) != MoveResult::Other {
        // The folder changed (or may have): no stale listing is shown.
        let folder = plan.listing.folder.clone();
        with_dock(|d| d.stack_refresh(&folder));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MoveResult {
    /// The target moved it (optimized move: it reports MOVE as performed
    /// and returns NONE, or returns NONE after doing it).
    Moved,
    /// The target returned MOVE: by the old protocol the source should now
    /// delete the original. We never do.
    LeftToUs,
    /// Copy, shortcut, cancelled.
    Other,
}

/// What a finished drag-out did, from the effect DoDragDrop returned and
/// the "Performed DropEffect" the target set (0 if none). Pure.
fn move_result(returned: u32, performed: u32) -> MoveResult {
    let mv = DROPEFFECT_MOVE.0;
    if returned & mv != 0 {
        MoveResult::LeftToUs
    } else if performed & mv != 0 {
        MoveResult::Moved
    } else {
        MoveResult::Other
    }
}

// ---- dropping on the open panel ---------------------------------------------------

/// Files dropped on an open stack panel go into its folder (same rules as
/// dropping on the stack's dock icon).
#[implement(IDropTarget)]
struct PanelDrop {
    folder: String,
    hwnd: HWND,
    helper: Option<IDropTargetHelper>,
    files: RefCell<Vec<String>>,
    /// Effects the drag source allows.
    source: Cell<u32>,
    /// KEY_RIGHT if the drag is a right-button one (from DragEnter).
    right: Cell<u32>,
}

impl PanelDrop_Impl {
    fn effect(&self, keys: MODIFIERKEYS_FLAGS) -> DROPEFFECT {
        DROPEFFECT(crate::stacks::drop_into(keys.0 | self.right.get(), &self.files.borrow(), &self.folder, self.source.get()))
    }
}

impl IDropTarget_Impl for PanelDrop_Impl {
    fn DragEnter(&self, data: windows_core::Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        *self.files.borrow_mut() = data.as_ref().map(|d| unsafe { super::drag::files_of(d) }).unwrap_or_default();
        self.source.set(if effect.is_null() { 0 } else { unsafe { (*effect).0 } });
        self.right.set(keys.0 & crate::stacks::KEY_RIGHT);
        let e = self.effect(keys);
        if let (Some(h), Some(d)) = (&self.helper, data.as_ref()) {
            unsafe {
                let _ = h.DragEnter(self.hwnd, d, &POINT { x: pt.x, y: pt.y }, e);
            }
        }
        if !effect.is_null() {
            unsafe { *effect = e };
        }
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        let e = self.effect(keys);
        if let Some(h) = &self.helper {
            unsafe {
                let _ = h.DragOver(&POINT { x: pt.x, y: pt.y }, e);
            }
        }
        if !effect.is_null() {
            unsafe { *effect = e };
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows_core::Result<()> {
        if let Some(h) = &self.helper {
            unsafe {
                let _ = h.DragLeave();
            }
        }
        self.files.borrow_mut().clear();
        Ok(())
    }

    fn Drop(&self, data: windows_core::Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        let mut e = self.effect(keys);
        if let (Some(h), Some(d)) = (&self.helper, data.as_ref()) {
            unsafe {
                let _ = h.Drop(d, &POINT { x: pt.x, y: pt.y }, e);
            }
        }
        let files = std::mem::take(&mut *self.files.borrow_mut());
        if e != DROPEFFECT_NONE {
            let folder = self.folder.clone();
            if with_dock(|d| d.workers.drop_into_stack(files, folder, e.0)).is_none() {
                crate::log!("stack: dock busy; panel drop ignored");
                e = DROPEFFECT_NONE;
            }
        }
        if e == DROPEFFECT_MOVE {
            // An optimized move: we do it, the source must not delete.
            if let Some(d) = data.as_ref() {
                unsafe { super::drag::set_performed_effect(d, DROPEFFECT_MOVE.0) };
            }
        }
        if !effect.is_null() {
            // We move them ourselves: the source must not delete.
            unsafe { *effect = if e == DROPEFFECT_MOVE { DROPEFFECT_NONE } else { e } };
        }
        Ok(())
    }
}

// ---- pinning folders ---------------------------------------------------------------

/// SID_STopLevelBrowser.
const SID_STOP_LEVEL_BROWSER: windows::core::GUID = windows::core::GUID::from_u128(0x4C96BE40_915C_11CF_99D3_00AA004AE837);

/// Folders shown in File Explorer windows `wins` (tabs too) with their
/// shell names, asked on a helper thread. Empty if Explorer does not answer
/// within `ms` (the thread is abandoned, not waited for).
pub(super) fn explorer_folders(wins: &[HWND], ms: u64) -> Vec<(String, String)> {
    // One at a time: an Explorer that never answers keeps one thread, not
    // one per menu opened (audit A05).
    static CAP: crate::worker::HelperCap = crate::worker::HelperCap::new(1);
    let Some(slot) = CAP.try_take() else {
        crate::log!("explorer-folders: the last request is still waiting on Explorer; skipped");
        return Vec::new();
    };
    let raw: Vec<isize> = wins.iter().map(|h| h.0 as isize).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("explorer-folders".into()).spawn(move || {
        let _slot = slot;
        let r = unsafe { read_explorer_folders(&raw) };
        let _ = tx.send(r);
    });
    if spawned.is_err() {
        return Vec::new();
    }
    rx.recv_timeout(std::time::Duration::from_millis(ms)).unwrap_or_default()
}

unsafe fn read_explorer_folders(wins: &[isize]) -> Vec<(String, String)> {
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IServiceProvider, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::{IFolderView, IPersistFolder2, IShellBrowser, IShellWindows, SHGetPathFromIDListW, ShellWindows};
    let mut out: Vec<(String, String)> = Vec::new();
    if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
        return out;
    }
    if let Ok(sw) = CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_ALL) {
        let n = sw.Count().unwrap_or(0).clamp(0, 64);
        for i in 0..n {
            let Ok(disp) = sw.Item(&VARIANT::from(i)) else { continue };
            let Ok(sp) = disp.cast::<IServiceProvider>() else { continue };
            let Ok(sb) = sp.QueryService::<IShellBrowser>(&SID_STOP_LEVEL_BROWSER) else { continue };
            let Ok(h) = sb.GetWindow() else { continue };
            if !wins.contains(&(h.0 as isize)) {
                continue;
            }
            let Ok(view) = sb.QueryActiveShellView() else { continue };
            let Ok(fv) = view.cast::<IFolderView>() else { continue };
            let Ok(pf) = fv.GetFolder::<IPersistFolder2>() else { continue };
            let Ok(pidl) = pf.GetCurFolder() else { continue };
            let mut buf = [0u16; 260];
            let ok = SHGetPathFromIDListW(pidl, &mut buf).as_bool();
            if ok {
                let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                let p = String::from_utf16_lossy(&buf[..end]);
                if !p.is_empty() && !out.iter().any(|o| o.0.eq_ignore_ascii_case(&p)) {
                    // The view's own folder: a file-system folder for sure.
                    let name = shell_name(pidl).unwrap_or_else(|| crate::stacks::last_part(&p));
                    crate::stacks::remember(&p, true, &name);
                    out.push((p, name));
                }
            }
            CoTaskMemFree(Some(pidl as *const _));
        }
    }
    CoUninitialize();
    out
}

/// Display name of an absolute folder id ("다운로드").
unsafe fn shell_name(pidl: *const windows::Win32::UI::Shell::Common::ITEMIDLIST) -> Option<String> {
    use windows::Win32::UI::Shell::{SHGetNameFromIDList, SIGDN_NORMALDISPLAY};
    let p = SHGetNameFromIDList(pidl, SIGDN_NORMALDISPLAY).ok()?;
    let s = p.to_string().ok();
    windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
    s.filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn review_stack_focus_position_and_visibility() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_MENU, VK_LWIN};
        let stage = crate::gfx::promo_kit::stage();
        let plan = StackPlan {
            title: "Focus test".into(),
            listing: Box::new(Listing { seq: 1, folder: "Z:\\".into(), entries: vec![], icons: vec![], grid: true, ok: false, partial: false }),
            at: POINT { x: 400, y: 600 }, align: Align::Above, scale: 1.0,
            theme: Theme::graphite(), gfx: stage.gfx.clone(),
        };
        let mut g = unsafe { build_grid(&plan).unwrap() };
        let keys = crate::input::sample::Keys::new();
        keys.set(None);
        // UIA moves the position; the OS cue preference alone decides visibility.
        g.asked("retry", crate::a11y::Act::Focus);
        assert_eq!(g.lights.chosen(), Some(Spot::Retry));
        assert_eq!(g.lights.ring, crate::input::keyboard_cues());
        assert_eq!(g.drawn.visible_cursor().is_some(), crate::input::keyboard_cues());
        for modifier in [VK_CONTROL.0, VK_MENU.0, VK_LWIN.0] {
            g.point(None);
            keys.set(Some(modifier));
            g.on_key(VK_TAB.0);
            assert!(g.lights.cursor.is_some());
            assert!(!g.lights.ring);
            assert_eq!(g.drawn.visible_cursor(), None);
            keys.set(None);
            g.on_key(VK_TAB.0);
            assert!(g.lights.ring);
            g.asked("retry", crate::a11y::Act::Focus);
            assert_eq!(g.drawn.visible_cursor(), Some(Spot::Retry));
        }
        unsafe { let _ = DestroyWindow(g.hwnd); }
    }

    /// The keyboard's cursor (and the button's ring) shows only after a
    /// key that moves it (crate::input): media, volume and letter keys
    /// leave it off, Tab and the arrows show it, a press of the pointer
    /// hides it (the window's handler: the cursor goes with the press).
    #[test]
    fn the_cursor_shows_only_after_a_key_that_moves_it() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_TAB};
        let mut lights = Lights::default();
        crate::input::sample::sequence(
            "stack",
            &mut lights,
            VK_TAB.0,
            VK_DOWN.0,
            |l| l.cursor.is_some(),
            |l, vk| {
                if let Some(s) = cursor_after(vk, false, false, 6, false, l.cursor) {
                    l.cursor = Some(s);
                }
            },
            |l| l.cursor = None,
        );
        let src = include_str!("stack.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(code.contains("let look = with_grid(|g| g.on_key(vk))"));
        assert!(code.contains("if let Some(s) = cursor_after(vk, back, crate::i18n::rtl(), n, failed, cur) {"));
        assert!(code.contains("g.lights.cursor = None;"), "a press takes the cursor away");
    }

    use super::*;

    #[test]
    fn a_grid_too_big_for_the_room_is_the_list_which_keeps_the_gap() {
        use crate::popup::{level_origin, level_room};
        // Review P2 #2: 1024 x 768 at 200 %, 12 files: the grid is 752 px
        // tall, the room over the anchor 506 px.
        let wa = RECT { left: 0, top: 64, right: 1024, bottom: 640 };
        let at = POINT { x: 512, y: 570 };
        let (w, h) = (grid_layout(12, false).width, grid_layout(12, false).height);
        let (w_px, h_px) = ((w * 2.0).ceil() as i32, (h * 2.0).ceil() as i32);
        assert_eq!(h_px, 760);
        assert!(!grid_fits(Align::Above, at, w_px, h_px, wa), "the grid is not shown");
        // The list instead (hangs): its room is over the anchor, so laid
        // out to it, it ends on the anchor and stays inside.
        let room = level_room(Align::Above, true, at, wa);
        assert_eq!(room, 570 - 64);
        let (_, y) = level_origin(Align::Above, None, false, at, 400, room, wa, 2.0);
        assert_eq!((y, y + room), (64, 570));
        // A grid that fits is placed there, the gap kept.
        let (w3, h3) = (grid_layout(3, false).width, grid_layout(3, false).height);
        let (w3, h3) = ((w3 * 2.0).ceil() as i32, (h3 * 2.0).ceil() as i32);
        assert!(grid_fits(Align::Above, at, w3, h3, wa));
        let r = grid_rect(Align::Above, at, w3, h3, wa);
        assert_eq!(r.bottom, at.y);
        assert!(r.top >= wa.top);
        // Side docks: the room beside the anchor.
        let side = RECT { left: 136, top: 0, right: 1024, bottom: 768 };
        let at_l = POINT { x: 160, y: 380 };
        assert!(grid_fits(Align::RightOf, at_l, w3, h3, side));
        assert!(!grid_fits(Align::RightOf, POINT { x: 900, y: 380 }, w3, h3, side), "too little room right of it");
        let r = grid_rect(Align::RightOf, at_l, w3, h3, side);
        assert_eq!(r.left, at_l.x);
        assert!(r.right <= side.right);
        assert!(!grid_fits(Align::LeftOf, POINT { x: 300, y: 380 }, w3, h3, side));
        assert!(grid_fits(Align::LeftOf, POINT { x: 1000, y: 380 }, w3, h3, side));
    }

    #[test]
    fn a_stack_sits_the_popup_gap_off_the_dock() {
        // The grid's own rect (its window adds the shadow margin outside
        // it) exactly popup_gap px off the dock's outer edge: the bar's, or
        // the tallest a magnified icon gets when that stands out further.
        let wa = RECT { left: 0, top: 0, right: 2560, bottom: 1440 };
        for s in [1.0f32, 1.25, 1.5, 2.0] {
            let gap = crate::placement::popup_gap(s);
            let d = (64.0 * s) as i32;
            for out in [-(8.0 * s) as i32, (40.0 * s) as i32] {
                let bar = RECT { left: 800, top: 1440 - d, right: 1800, bottom: 1440 };
                let peak = RECT { left: 1000, top: bar.top - out, right: 1000 + d, bottom: 1430 };
                let at = super::super::menu::menu_point(Position::Bottom, bar, Some(peak), POINT::default(), gap);
                let r = grid_rect(Align::Above, at, 600, 400, wa);
                assert_eq!(peak.top.min(bar.top) - r.bottom, gap, "bottom {s} {out}");
                let side = RECT { left: 0, top: 300, right: d, bottom: 1100 };
                let pk = RECT { left: 4, top: 500, right: d + out, bottom: 500 + d };
                let at = super::super::menu::menu_point(Position::Left, side, Some(pk), POINT::default(), gap);
                assert_eq!(grid_rect(Align::RightOf, at, 600, 400, wa).left - pk.right.max(side.right), gap, "left {s} {out}");
                let side_r = RECT { left: 2560 - d, top: 300, right: 2560, bottom: 1100 };
                let pk_r = RECT { left: side_r.left - out, top: 500, right: 2556, bottom: 500 + d };
                let at = super::super::menu::menu_point(Position::Right, side_r, Some(pk_r), POINT::default(), gap);
                assert_eq!(pk_r.left.min(side_r.left) - grid_rect(Align::LeftOf, at, 600, 400, wa).right, gap, "right {s} {out}");
            }
        }
    }

    #[test]
    fn a_stack_asked_for_before_a_fullscreen_app_never_opens_over_it() {
        let req = Some((5, r"C:\Users\a\Downloads"));
        assert!(listing_wanted(req, 5, r"c:\users\a\downloads", false));
        // Listing on its way, then the app came: the request was dropped,
        // so the late listing (or the timeout's fallback) opens nothing.
        assert!(!listing_wanted(None, 5, r"C:\Users\a\Downloads", false));
        // Covered when it arrives: not shown either.
        assert!(!listing_wanted(req, 5, r"C:\Users\a\Downloads", true));
        // An older request's listing.
        assert!(!listing_wanted(req, 4, r"C:\Users\a\Downloads", false));
    }

    #[test]
    fn the_grid_sits_on_the_4_dip_grid() {
        for v in [HEADER_H, SEP_H, FOOTER_H, TILE_W, TILE_H, TILE_GAP, GRID_ICON, ICON_TOP, NAME_GAP, NAME_INSET, PAD, LINE, EMPTY_H, FAIL_H, RETRY_GAP] {
            assert_eq!(v % 4.0, 0.0, "{v}");
        }
        for (n, failed) in [(0usize, false), (1, false), (5, false), (9, false), (0, true)] {
            let l = grid_layout(n, failed);
            assert!(l.tiles.iter().all(|r| r.x % 4.0 == 0.0 && r.y % 4.0 == 0.0), "{n}");
            assert_eq!((l.footer.y % 4.0, l.width % 4.0, l.height % 4.0), (0.0, 0.0, 0.0), "{n}");
        }
    }

    #[test]
    fn the_grid_stands_on_the_panels_two_lines() {
        // Design system 3.3: what fills (tiles, the footer's row) on the
        // container line 8, what is read on the content line 20.
        assert_eq!((PAD, LINE, HEADER_H, FOOTER_H, TILE_GAP), (8.0, 20.0, 24.0, 28.0, 8.0));
        let l = grid_layout(6, false);
        assert_eq!(l.tiles.len(), 6);
        // Edge to tile 8, tile to tile 8, both ways.
        assert_eq!(l.width, PAD * 2.0 + 4.0 * TILE_W + 3.0 * TILE_GAP);
        assert_eq!((l.tiles[0].x, l.tiles[0].y), (PAD, PAD + HEADER_H));
        assert_eq!(l.tiles[1].x - (l.tiles[0].x + TILE_W), TILE_GAP);
        assert_eq!((l.tiles[4].x, l.tiles[4].y), (PAD, PAD + HEADER_H + TILE_H + TILE_GAP));
        assert_eq!(l.tiles[3].x + TILE_W, l.width - PAD, "the last column ends on the container line's mirror");
        // The separator's slot, the footer's row on the container line,
        // the panel's pad under it.
        assert_eq!(l.footer.y, l.tiles[5].y + TILE_H + SEP_H);
        assert_eq!((l.footer.x, l.footer.w, l.footer.h), (PAD, l.width - 2.0 * PAD, size::ROW_M));
        assert_eq!(l.height, l.footer.y + FOOTER_H + PAD);
        // A small stack is never narrower than three tiles.
        assert_eq!(grid_layout(1, false).width, PAD * 2.0 + 3.0 * TILE_W + 2.0 * TILE_GAP);
        // ... and a lone item is centered in it.
        assert_eq!(grid_layout(1, false).tiles[0].x, PAD + TILE_W + TILE_GAP);
        // Empty: the quiet block (at least QUIET_MIN_H) and the footer; a
        // folder that could not be opened: room for its button too.
        let empty = grid_layout(0, false);
        assert!(empty.tiles.is_empty());
        assert_eq!(empty.body.h, size::QUIET_MIN_H);
        let failed = grid_layout(7, true);
        assert!(failed.tiles.is_empty(), "nothing listed");
        assert!(failed.body.h >= retry_block_h() && failed.body.h > empty.body.h);
        assert_eq!(retry_block_h(), 24.0 + 8.0 + 18.0 + 12.0 + size::CONTROL_H);
    }

    #[test]
    fn a_folder_that_cannot_be_opened_says_so_in_the_panel() {
        // The grid panel for a grid folder and for an unreadable one; the
        // list otherwise.
        assert!(shows_grid(true, true) && shows_grid(false, false) && shows_grid(false, true));
        assert!(!shows_grid(true, false));
        // The list form (a screen too small for the panel): the failure's
        // row with its button, an empty folder's one quiet line.
        let _l = crate::i18n::test_lock("en");
        let rows = list_rows("Downloads", &Listing::failed(1, "Z:\\nowhere"));
        assert!(rows.iter().any(|r| r.id == RETRY_ID), "try again");
        assert!(rows.iter().any(|r| r.id == OPEN_FOLDER_ID));
    }

    /// Final audit 9: a file whose icon could not be loaded showed
    /// nothing (an empty spot in the grid, no mark in the list). The
    /// folder's or the file's glyph stands there in the secondary ink,
    /// which keeps 3:1 on every plate: the grid draws it in the icon's box,
    /// the list gives the row the glyph.
    #[test]
    fn a_missing_icon_is_a_glyph() {
        use crate::stacks::Entry;
        let _l = crate::i18n::test_lock("en");
        let entries = vec![Entry { name: "a.txt".into(), path: "Z:/a.txt".into(), time: 0, dir: false }, Entry { name: "b".into(), path: "Z:/b".into(), time: 0, dir: true }];
        let l = Listing { seq: 1, folder: "Z:\\".into(), entries, icons: vec![None, None], grid: true, ok: true, partial: false };
        let rows = list_rows("Z", &l);
        let glyphs: Vec<_> = rows.iter().filter(|r| r.id >= 1 && r.id <= 2).map(|r| r.glyph).collect();
        assert_eq!(glyphs, vec![Some(crate::glyphs::Glyph::File), Some(crate::glyphs::Glyph::Folder)]);
        // In the grid: ink in each icon's box.
        let stage = crate::gfx::promo_kit::stage();
        let theme = Theme::graphite().resolve(&crate::syslook::SysLook { light: true, ..Default::default() });
        let s = 1.5;
        let lay = grid_layout(2, false);
        let art = grid_art(&stage.gfx, &theme, "Z", &l, &lay, (lay.width * s).ceil() as i32, s, false);
        let cv = crate::gfx::promo_kit::Canvas::at(lay.width, lay.height, 1.0);
        unsafe { paint_grid(&cv.dc, 0.0, 0.0, &art, &stage.gfx, &theme, Lights::default()).unwrap() };
        let w = cv.w as usize;
        let (px, _, _) = cv.finish();
        for r in &lay.tiles {
            let (x0, y0) = (((r.x + (r.w - GRID_ICON) / 2.0) * s) as usize, ((r.y + ICON_TOP) * s) as usize);
            let side = (GRID_ICON * s) as usize;
            let inked = (y0..y0 + side).flat_map(|y| (x0..x0 + side).map(move |x| (y, x))).filter(|&(y, x)| px[(y * w + x) * 4 + 3] > 128).count();
            assert!(inked > side, "a glyph in the icon's box: {inked}");
        }
        // The secondary ink keeps a graphic's 3:1 on the panel.
        let ink = theme.text_of(Level::Secondary, State::Rest);
        let plate = crate::theme::Rgba { a: 255, ..theme.popup_tint() };
        assert!(crate::theme::contrast(crate::theme::over(ink, plate), plate) >= 3.0);
    }

    /// The grid's stand-in glyph (final audit 9) keeps 3:1 (a graphic's)
    /// on every plate as text sees it, at rest and under the pointer's,
    /// the press's and the cursor's fills, light and dark, Flat and Glass;
    /// in high contrast on each state's own fill.
    #[test]
    fn the_stand_in_glyph_keeps_3_to_1() {
        use crate::syslook::{HighContrast, SysLook};
        use crate::theme::{contrast, over, Finish, Material, Rgba};
        for light in [false, true] {
            for f in [Finish::Flat, Finish::Glass] {
                let mut raw = Theme::graphite();
                raw.finish = f;
                let theme = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
                let what = format!("{f:?} light {light}");
                // The picture's state is drawn only in high contrast: at
                // rest whatever fill the wash lays under it.
                let ink = missing_ink(&theme, State::Rest);
                let fills = [State::Rest, State::Hover, State::Pressed, State::Selected].map(|st| theme.fill_row(st));
                for (p, _) in theme.plate_seen_all(Material::Panel) {
                    for fill in fills {
                        let under = over(fill, p);
                        let c = contrast(over(ink, under), under);
                        assert!(c >= 3.0, "{what}: {c:.2} on {under:?}");
                    }
                }
            }
            // High contrast: the state's own fill and ink.
            let black = Rgba { r: 0, g: 0, b: 0, a: 255 };
            let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
            let hc = HighContrast { window: black, text: white, highlight: Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 }, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT };
            let theme = Theme::graphite().resolve(&SysLook { light, high_contrast: Some(hc), ..Default::default() });
            for st in [State::Rest, State::Hover, State::Pressed, State::Selected, State::Cursor] {
                let under = over(theme.fill_of(Role::Row, st), Rgba { a: 255, ..theme.popup_tint() });
                let c = contrast(over(missing_ink(&theme, st), under), under);
                assert!(c >= 3.0, "high contrast {st:?}: {c:.2}");
            }
        }
    }

    #[test]
    fn one_highlight_and_the_later_input_wins() {
        let (a, b) = (Spot::Tile(1), Spot::Tile(2));
        // The pointer on a tile: its fill; pressed: the same fill, darker.
        let l = Lights { hover: Some(a), ..Default::default() };
        assert_eq!((l.pointer(), l.mark(), l.state(a)), (Some((a, false)), None, State::Hover));
        let held = Lights { pressed: Some(a), ..l };
        assert_eq!((held.pointer(), held.state(a)), (Some((a, true)), State::Pressed));
        // The keyboard's cursor: the selected fill, no ring; the pointer's
        // fill never lies on it too.
        let k = Lights { hover: Some(b), cursor: Some(b), ring: true, ..Default::default() };
        assert_eq!((k.pointer(), k.mark(), k.state(b)), (None, Some(b), State::Cursor));
        // What Enter acts on: the cursor, else what the pointer is on.
        assert_eq!(Lights { hover: Some(a), cursor: Some(b), ring: true, pressed: None }.chosen(), Some(b));
        assert_eq!(l.chosen(), Some(a));
        // The button is raised: never the row's fill; the keyboard on it
        // is Tab's ring (drawn in the picture), not a cursor fill.
        let r = Lights { hover: Some(Spot::Retry), cursor: Some(Spot::Retry), ring: true, pressed: None };
        assert_eq!((r.pointer(), r.mark(), r.state(Spot::Retry)), (None, None, State::Hover));
        assert_eq!(r.drawn(false), Lights { cursor: Some(Spot::Retry), ring: true, ..Default::default() });
        // The picture itself shows the state only in high contrast.
        assert_eq!(k.drawn(false), Lights::default());
        assert_eq!(k.drawn(true), k);
    }

    #[test]
    fn screen_readers_name_every_spot() {
        for s in [Spot::Tile(0), Spot::Tile(6), Spot::Retry, Spot::Footer] {
            assert_eq!(spot_of(&spot_key(s), 7, true), Some(s));
        }
        // Gone since the tree was read: nothing.
        assert_eq!(spot_of("tile:7", 7, false), None);
        assert_eq!(spot_of("retry", 0, false), None);
        assert_eq!(spot_of("tile:x", 3, false), None);
        // Nothing the panel shows is nameless.
        let _l = crate::i18n::test_lock("ko");
        for k in ["dock.stack.unavailable", "dock.stack.empty", "launch.action.retry", "dock.stack.open_in_explorer"] {
            assert!(crate::i18n::get(k).is_some_and(|v| !v.is_empty()), "{k}");
        }
    }

    #[test]
    fn its_states_are_the_themes() {
        let src = include_str!("stack.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for gone in ["LEGACY_PAD", ".hover(false)", "text_primary", "text_secondary", "high_contrast", "effects::ms(", "SetOpacity(0.0)"] {
            assert!(!code.contains(gone), "{gone} is still in stack.rs");
        }
        for used in ["fill_of(", "outline_of(", "text_of(", "crate::gfx::focus_ring(", "gfx.quiet(", "crate::motion::open(", "crate::motion::fade_to("] {
            assert!(code.contains(used), "{used}");
        }
    }

    #[test]
    fn move_results_are_told_apart_and_never_delete() {
        let (copy, mv, link) = (DROPEFFECT_COPY.0, DROPEFFECT_MOVE.0, DROPEFFECT_LINK.0);
        // Explorer's optimized move: NONE returned, MOVE performed.
        assert_eq!(move_result(0, mv), MoveResult::Moved);
        // Old-style move: MOVE returned (the source should delete; we don't).
        assert_eq!(move_result(mv, mv), MoveResult::LeftToUs);
        assert_eq!(move_result(mv, 0), MoveResult::LeftToUs);
        assert_eq!(move_result(copy, copy), MoveResult::Other);
        assert_eq!(move_result(link, 0), MoveResult::Other);
        assert_eq!(move_result(0, 0), MoveResult::Other, "cancelled");
    }

    #[test]
    fn grid_keys_move_between_tiles_and_footer() {
        // 6 tiles, 4 columns; 6 = footer.
        assert_eq!(grid_step(6, None, 1, 0), 0);
        assert_eq!(grid_step(6, Some(0), 1, 0), 1);
        assert_eq!(grid_step(6, Some(1), 0, 1), 5);
        assert_eq!(grid_step(6, Some(3), 0, 1), 6, "below the last row: footer");
        assert_eq!(grid_step(6, Some(6), 0, -1), 5);
        assert_eq!(grid_step(6, Some(0), -1, 0), 0);
        // As spots.
        assert_eq!(key_step(6, false, None, 1, 0), Spot::Tile(0));
        assert_eq!(key_step(6, false, Some(Spot::Tile(3)), 0, 1), Spot::Footer);
        assert_eq!(key_step(6, false, Some(Spot::Footer), 0, -1), Spot::Tile(5));
        // Tab: the tiles are one stop, the footer the other.
        assert_eq!(tab_step(6, false, None, false), Spot::Tile(0));
        assert_eq!(tab_step(6, false, Some(Spot::Tile(4)), false), Spot::Footer);
        assert_eq!(tab_step(6, false, Some(Spot::Footer), true), Spot::Tile(0));
        assert_eq!(tab_step(0, false, None, false), Spot::Footer, "an empty folder: the footer alone");
        // A folder that could not be opened: its button, then the footer.
        assert_eq!(key_step(0, true, None, 0, 1), Spot::Retry);
        assert_eq!(key_step(0, true, Some(Spot::Retry), 0, 1), Spot::Footer);
        assert_eq!(tab_step(0, true, Some(Spot::Footer), false), Spot::Retry);
    }
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "stack_harmony.rs"]
mod harmony;
