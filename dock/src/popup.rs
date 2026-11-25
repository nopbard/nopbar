//! Our own popup menu (top bar app menus now, dock context menus in M3).
//!
//! `track()` is modal like TrackPopupMenu: it runs a nested message loop
//! until the user picks, cancels, or moves to another menu title. Other
//! windows keep receiving their messages meanwhile. The first level is
//! activated (keyboard), the mouse is captured, submenus open on hover or
//! with the arrow keys.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windows::core::{w, Interface, Result, BOOL, HSTRING};
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionPropertySet, CompositionStretch, ContainerVisual, ShapeVisual, SpriteVisual};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, SetFocus, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Vector2, Vector3};

use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::theme::{Elevation, Rgba, Role, State, Theme};
use parts::Target;

/// One menu row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub id: u32,
    pub label: String,
    /// Right-aligned hint ("Ctrl+S").
    pub shortcut: String,
    pub enabled: bool,
    pub checked: bool,
    pub separator: bool,
    pub sub: Vec<Item>,
    /// A slider row (0..1) instead of a label; dragging it calls the
    /// request's `on_slide` live.
    pub slider: Option<f32>,
    /// A grid row: equal columns of short texts (calendar weeks).
    pub cells: Vec<String>,
    /// Grid cell drawn highlighted (today).
    pub mark: Option<usize>,
    /// Grid cells that may be shortened in a narrow menu ("Mon" -> "M"):
    /// weekday names. Other cells (day numbers) are always drawn whole,
    /// smaller if need be (grid_texts).
    pub abbrev: bool,
    /// A picture in the check column: (size px, premultiplied BGRA).
    pub icon: Option<(u32, Vec<u8>)>,
    /// One of NopBar's glyphs in the check column, in the row's text color
    /// (the design kit's menus: an icon before each command).
    pub glyph: Option<Glyph>,
    /// Glyphs at the row's end, in the hint color, before its shortcut (a
    /// Wi-Fi network's strength and lock): drawn, never typed as symbols.
    pub trail: Vec<Glyph>,
    /// Access key (A-Z / 0-9): pressing it picks the row in its level.
    /// Shown by the caller (as the row's shortcut hint).
    pub key: Option<char>,
    /// The label is text as it is (a file name, a window title), not a
    /// menu label with an access key: UIA reads it whole, `&` and all,
    /// as the row shows it (a11y_name).
    pub literal: bool,
    /// A text row (not pickable) that wraps to the menu's width bound
    /// instead of widening the menu past the screen: a warning, a reason.
    pub wrap: bool,
    /// Foldable group (0 = always shown, as is). When the menu is taller
    /// than the screen, runs of rows with the same `fold` move into a
    /// submenu, the highest number first (a jump list's recent items, then
    /// its tasks), so the app's own actions and Quit stay in reach. A
    /// disabled first row without an id is the group's header: the folded
    /// row takes its name ("최근 플레이 ›").
    pub fold: u8,
    /// A live clock row (the calendar's top): the time now in large
    /// tabular digits with the long date under it, drawn again on every
    /// second while the menu is open. Never picked; UIA reads it as text.
    pub clock: bool,
    /// An agenda row (the calendar's events): drawn by draw_event_row
    /// instead of `label`, which stays what UIA reads ("09:30, Standup,
    /// Room 4"). It never widens the menu: its texts are cut to fit.
    pub event: Option<EventRow>,
    /// A grid row whose cells can be picked: the id each cell picks (0:
    /// none), like a row's `id`. The pointer only; the keyboard and UIA
    /// keep the rows' own items.
    pub cell_ids: Vec<u32>,
    /// Grid cell drawn with a thin ring (the day picked, not today).
    pub ring: Option<usize>,
    /// A group's header (Item::head, audit #11): small semibold tertiary
    /// text on a shorter row, never highlighted or picked.
    pub head: bool,
    /// A month header (Item::month): the label is the month's name, with
    /// ‹ › buttons at the row's end over the grid's last two columns; the
    /// title itself goes back to today.
    pub nav: Option<MonthNav>,
    /// A notice's title (Item::title): body strong in the text color, over
    /// the note that follows it (its reason: design system 11.4).
    pub title: bool,
    /// An empty list's one line (Item::quiet, 11.1): tertiary text from
    /// the content line, never highlighted or picked.
    pub quiet: bool,
    /// A wrapping row's lines as the level laid them out (build_level);
    /// empty before that: one line.
    pub lines: Vec<String>,
    /// A line that only says something (Item::info: a status, a count):
    /// read, never picked, in the secondary text color (it carries
    /// information; a command that cannot be used now is the disabled
    /// color instead).
    pub info: bool,
    /// Something is still being fetched (Item::busy, 11.3): the progress
    /// ring alone on a row, turning.
    pub busy: bool,
    /// Something went wrong (Item::failed, 11.4): a warning glyph, the
    /// label, and at the row's end a button with this text that picks the
    /// row's id.
    pub retry: Option<String>,
}

/// What a month header's parts pick (0: nothing) and what screen readers
/// call them (Item::month).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MonthNav {
    pub prev: u32,
    pub next: u32,
    /// Back to today's month (0: it is showing already).
    pub today: u32,
    pub prev_name: String,
    pub next_name: String,
    pub today_name: String,
}

/// A part of a month header (nav_part).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavPart {
    Title,
    Prev,
    Next,
}

impl MonthNav {
    /// The id `p` picks (0: none).
    pub fn id(&self, p: NavPart) -> u32 {
        match p {
            NavPart::Title => self.today,
            NavPart::Prev => self.prev,
            NavPart::Next => self.next,
        }
    }
}

/// Pure: the part of a month header at `x` DIP in a menu `width` wide: the
/// previous and next buttons over the grid's last two columns (grid_cell:
/// the reading end, so right to left they sit on the left, next outermost),
/// the title over the rest. None in the margins.
pub fn nav_part(x: f32, width: f32, rtl: bool) -> Option<NavPart> {
    match grid_cell(x, width, NAV_COLS, rtl)? {
        6 => Some(NavPart::Next),
        5 => Some(NavPart::Prev),
        _ => Some(NavPart::Title),
    }
}

/// Pure: where part `p` of a month header is drawn and hovered, (left x,
/// width) in DIP, mirrored right to left: a button its grid column, the
/// title the columns before them.
pub fn nav_span(p: NavPart, width: f32, rtl: bool) -> (f32, f32) {
    let cw = (width - 24.0).max(0.0) / NAV_COLS as f32;
    let (x, w) = match p {
        NavPart::Title => (12.0, cw * 5.0),
        NavPart::Prev => (12.0 + cw * 5.0, cw),
        NavPart::Next => (12.0 + cw * 6.0, cw),
    };
    if rtl { (width - x - w, w) } else { (x, w) }
}

/// Pure: the id a key picks in a menu with month header `nav` (None: the
/// key does what it does in any menu): Page Up / Page Down the previous /
/// next month, Home today's, and the left and right arrows the months in
/// reading order, but for the forward one on a row with a submenu (it
/// opens it).
pub fn nav_key(nav: &MonthNav, vk: u16, rtl: bool, on_sub: bool) -> Option<u32> {
    let id = match vk {
        0x21 => nav.prev,
        0x22 => nav.next,
        0x24 => nav.today,
        v if v == VK_LEFT.0 || v == VK_RIGHT.0 => {
            if crate::i18n::mirror::arrow(rtl, v == VK_RIGHT.0) > 0 {
                if on_sub {
                    return None;
                }
                nav.next
            } else {
                nav.prev
            }
        }
        _ => return None,
    };
    (id != 0).then_some(id)
}

/// Pure: a month header's ‹ › for UIA, to follow its row `r` (`row`: the
/// row's screen rect, offscreen when it is scrolled out; the level `width`
/// DIP at `scale`): buttons named for what they do, each on its column,
/// keyed "r{r}.prev" / "r{r}.next" (a11y_act), dim with nothing to pick.
pub fn nav_nodes(nav: &MonthNav, r: usize, row: RECT, width: f32, scale: f32, rtl: bool) -> Vec<crate::a11y::Node> {
    use crate::a11y::{Node, Role};
    [(NavPart::Prev, "prev", &nav.prev_name), (NavPart::Next, "next", &nav.next_name)]
        .into_iter()
        .map(|(p, k, name)| {
            let (x, w) = nav_span(p, width, rtl);
            let rect = if row.right <= row.left { row } else { RECT { left: row.left + (x * scale).round() as i32, right: row.left + ((x + w) * scale).round() as i32, ..row } };
            Node::new(format!("r{r}.{k}"), Role::Button, name.clone(), rect).invoke().enabled(nav.id(p) != 0)
        })
        .collect()
}

/// Pure: the id a UIA request on a month header's part picks (`part`:
/// None its title, "prev", "next"); Invoke only.
pub fn nav_act(nav: &MonthNav, part: Option<&str>, act: crate::a11y::Act) -> Option<u32> {
    if !matches!(act, crate::a11y::Act::Invoke) {
        return None;
    }
    match part {
        None => Some(nav.today),
        Some("prev") => Some(nav.prev),
        Some("next") => Some(nav.next),
        Some(_) => None,
    }
}

/// Pure: the part a UIA key's suffix names (None: the title; "prev",
/// "next"); None for anything else.
pub fn nav_key_part(part: Option<&str>) -> Option<NavPart> {
    match part {
        None => Some(NavPart::Title),
        Some("prev") => Some(NavPart::Prev),
        Some("next") => Some(NavPart::Next),
        Some(_) => None,
    }
}

/// Pure: the UIA key of what has the focus in a level: its highlighted
/// row ("r3"), or a month header's part when that has it ("r3.prev").
pub fn focus_key(hover: Option<usize>, nav_focus: Option<(usize, NavPart)>) -> Option<String> {
    let r = hover?;
    Some(match nav_focus.filter(|&(at, _)| at == r).map(|(_, p)| p) {
        Some(NavPart::Prev) => format!("r{r}.prev"),
        Some(NavPart::Next) => format!("r{r}.next"),
        Some(NavPart::Title) | None => format!("r{r}"),
    })
}

/// A month header spans the calendar grid's seven columns.
const NAV_COLS: usize = 7;
/// The side of a month header's button highlight (DIP, at most its column).
const NAV_BTN: f32 = 28.0;

/// What an agenda row shows (Item::event).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventRow {
    /// "09:30" or "All day".
    pub time: String,
    pub title: String,
    /// Shown dim after the title ("" = none).
    pub place: String,
    /// Happening now: a small mark in the text color before the time.
    pub now: bool,
    /// Over already: the whole row dim.
    pub past: bool,
}

impl Item {
    pub fn new(id: u32, label: impl Into<String>) -> Self {
        Self { id, label: label.into(), enabled: true, ..Default::default() }
    }
    /// A wrapping text row (see `wrap`): a paragraph, its lines a line
    /// height apart (not a row each).
    pub fn note(text: impl Into<String>) -> Self {
        Self { label: text.into(), wrap: true, ..Default::default() }
    }
    /// A notice's title (see `title`); it wraps like a note.
    pub fn title(text: impl Into<String>) -> Self {
        Self { label: text.into(), wrap: true, title: true, ..Default::default() }
    }
    /// A line that only says something (see `info`).
    pub fn info(text: impl Into<String>) -> Self {
        Self { label: text.into(), info: true, ..Default::default() }
    }
    /// An empty list's line (see `quiet`).
    pub fn quiet(text: impl Into<String>) -> Self {
        Self { label: text.into(), quiet: true, ..Default::default() }
    }
    /// The progress ring's row (see `busy`); `name` is what UIA reads.
    pub fn busy(name: impl Into<String>) -> Self {
        Self { label: name.into(), busy: true, ..Default::default() }
    }
    /// A failure's row (see `retry`): `text`, and a button `button` that
    /// picks `id`.
    pub fn failed(id: u32, text: impl Into<String>, button: impl Into<String>) -> Self {
        Self { id, label: text.into(), enabled: true, retry: Some(button.into()), ..Default::default() }
    }
    pub fn sep() -> Self {
        Self { separator: true, ..Default::default() }
    }
    /// Makes the row its group's header (see `head`): not pickable.
    pub fn head(mut self) -> Self {
        self.head = true;
        self.enabled = false;
        self.id = 0;
        self
    }
    pub fn shortcut(mut self, s: impl Into<String>) -> Self {
        self.shortcut = s.into();
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
    pub fn submenu(mut self, sub: Vec<Item>) -> Self {
        self.sub = sub;
        self
    }
    pub fn slider(value: f32) -> Self {
        Self { slider: Some(value.clamp(0.0, 1.0)), enabled: true, ..Default::default() }
    }
    pub fn grid(cells: Vec<String>, mark: Option<usize>) -> Self {
        Self { cells, mark, enabled: false, ..Default::default() }
    }
    /// A month header (see `nav`): `title` with its ‹ › buttons.
    pub fn month(title: impl Into<String>, nav: MonthNav) -> Self {
        Self { label: title.into(), nav: Some(nav), ..Default::default() }
    }
    /// The live clock row (see `clock`).
    pub fn clock() -> Self {
        Self { clock: true, ..Default::default() }
    }
    /// An agenda row picking `id` (see `event`); UIA reads "time, title,
    /// place".
    pub fn event(id: u32, row: EventRow) -> Self {
        let label = [row.time.as_str(), row.title.as_str(), row.place.as_str()].iter().filter(|s| !s.trim().is_empty()).copied().collect::<Vec<_>>().join(", ");
        Self { id, label, enabled: true, event: Some(row), ..Default::default() }
    }
    /// In foldable group `group` (see `fold`).
    pub fn fold(mut self, group: u8) -> Self {
        self.fold = group;
        self
    }
    /// Its label is literal text (see `literal`).
    pub fn literal(mut self) -> Self {
        self.literal = true;
        self
    }
    /// What UIA reads for the row: its label as shown when literal, else
    /// without the access key marks ("&Open" -> "Open").
    pub fn a11y_name(&self) -> String {
        let name = if self.literal { self.label.clone() } else { crate::a11y::strip_mnemonic(&self.label) };
        // What the row's end glyphs say (a Wi-Fi network's strength and
        // lock: design B1 review P2 #3), read right after its name. The
        // name only: putting it in the help too made readers say it twice.
        let said = trail_help(&self.trail);
        if said.is_empty() { name } else { format!("{name}, {said}") }
    }
    /// A group's title row: shown dimmed, never picked.
    fn header(&self) -> bool {
        !self.separator && !self.enabled && self.id == 0 && self.sub.is_empty() && self.slider.is_none() && self.cells.is_empty() && !self.clock
    }
    /// Can be picked (runs something).
    fn selectable(&self) -> bool {
        !self.separator && self.enabled && self.slider.is_none() && self.cells.is_empty() && !self.clock
    }
    /// Can take the keyboard focus (pickable rows and sliders).
    fn focusable(&self) -> bool {
        self.selectable() || (self.slider.is_some() && self.enabled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Picked(u32),
    /// Closed by the user (Esc, outside click, title clicked again).
    Cancelled,
    /// Closed because something else took over (another window activated,
    /// capture lost, or our side cancelled): do not steal focus back.
    Lost,
    /// The mouse went onto another title of the menu bar (its index).
    Switch(usize),
    /// Left/right arrow on the top level: previous (-1) / next (+1) title.
    Neighbor(i32),
    /// A row with an id in the request's `drag_ids` was pressed and dragged
    /// past the system drag box (its id): the caller starts a drag and drop.
    Dragged(u32),
}

/// Where the menu bar titles are (screen px), so the pointer can slide
/// from one menu to the next like a real menu bar.
#[derive(Clone, Debug, Default)]
pub struct Titles {
    pub rects: Vec<RECT>,
    pub current: Option<usize>,
    /// The bar cell the menu hangs from (screen px), if one opened it: the
    /// first level grows from the point of its edge nearest that cell's
    /// center (open_origin; design system 7.4). None: from `Request::at`.
    pub anchor: Option<RECT>,
}

/// Pure: the point a first level grows from, in its own DIPs: `size` DIP,
/// its top left at `pos` (screen px), at `s` px per DIP. Opened by a bar
/// cell (`cell`, screen px): the point of its edge nearest the cell's
/// center (motion::origin: under a bar its top edge at the cell's middle,
/// over a bar at the bottom its bottom edge). Otherwise where it was
/// opened at (`at`, kept inside it by motion::open).
pub fn open_origin(cell: Option<RECT>, at: POINT, pos: (i32, i32), size: (f32, f32), s: f32) -> (f32, f32) {
    let s = if s > 0.0 { s } else { 1.0 };
    match cell {
        Some(r) => {
            let anchor = ((r.left - pos.0) as f32 / s, (r.top - pos.1) as f32 / s, (r.right - r.left) as f32 / s, (r.bottom - r.top) as f32 / s);
            crate::motion::origin((0.0, 0.0, size.0, size.1), Some(anchor))
        }
        None => ((at.x - pos.0) as f32 / s, (at.y - pos.1) as f32 / s),
    }
}

/// How the first level sits relative to `at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    /// `at` is the top-left corner (menu bar menus).
    #[default]
    TopLeft,
    /// Centered above `at` (bottom dock).
    Above,
    /// To the right of `at`, vertically centered (left dock).
    RightOf,
    /// To the left of `at`, vertically centered (right dock).
    LeftOf,
    /// `at` is the top-right corner (menu bar menus right to left).
    TopRight,
    /// `at` is the bottom-left corner: the menu opens upward from it (menu
    /// bar menus while the bar sits at the bottom of the screen).
    BottomLeft,
    /// `at` is the bottom-right corner (the same, right to left).
    BottomRight,
}

impl Align {
    /// The corner a menu hangs from on the menu bar: its top (`up`: its
    /// bottom, for a bar at the bottom) reading-start corner.
    pub fn hanging(up: bool, rtl: bool) -> Self {
        match (up, rtl) {
            (false, false) => Align::TopLeft,
            (false, true) => Align::TopRight,
            (true, false) => Align::BottomLeft,
            (true, true) => Align::BottomRight,
        }
    }

    /// For a corner (hanging): whether the menu opens upward from `at`
    /// (Some(true)) or downward (Some(false)); None for the others.
    pub fn opens_up(self) -> Option<bool> {
        match self {
            Align::TopLeft | Align::TopRight => Some(false),
            Align::BottomLeft | Align::BottomRight => Some(true),
            Align::Above | Align::RightOf | Align::LeftOf => None,
        }
    }
}

/// Pure: how tall (px) the first level may be: a menu hanging from the
/// bar (`hangs`, at a corner) keeps to the room between `at` (the bar's
/// gap already in it) and the far edge of the work area `wa`, so it never
/// moves over the gap or the bar (compact rows or scrolling instead), and
/// so does a bottom dock's menu (Above, `hangs`) over its anchor;
/// anything else, the whole work area (it slides to fit).
pub fn level_room(align: Align, hangs: bool, at: POINT, wa: RECT) -> i32 {
    // A bottom dock's menu (Above) opens up from `at` the same way.
    let up = if align == Align::Above { Some(true) } else { align.opens_up() };
    match up.filter(|_| hangs) {
        Some(up) => crate::placement::room_from(at.y, up, wa),
        None => (wa.bottom - wa.top).max(1),
    }
}

/// Pure: how wide (px) the first level may be: a side dock's menu or
/// notice (RightOf / LeftOf, `hangs`) keeps to the room beside its anchor
/// (the gap already in it), so it never slides back over the gap or the
/// dock; anything else, the whole work area's width.
pub fn level_span(align: Align, hangs: bool, at: POINT, wa: RECT) -> i32 {
    let span = match (align, hangs) {
        (Align::RightOf, true) => wa.right - at.x,
        (Align::LeftOf, true) => at.x - wa.left,
        _ => wa.right - wa.left,
    };
    span.clamp(1, (wa.right - wa.left).max(1))
}

/// The display a level goes on: the anchor's for the first level; a
/// submenu stays on its parent's display (its anchor, the parent's edge
/// plus the gap, may already lie on the next display, where it would
/// open instead of flipping to the parent's other side).
unsafe fn level_monitor(at: POINT, parent: Option<RECT>) -> HMONITOR {
    match parent {
        Some(p) => MonitorFromRect(&p, MONITOR_DEFAULTTONEAREST),
        None => MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST),
    }
}

/// Pure: where a menu `w` x `h` px goes (its top-left corner, screen px)
/// for anchor `at`: the first level by `align`, a submenu (`parent`: the
/// row it hangs off) beside its row, to the left right to left (`rtl`).
/// It slides or flips to stay inside the work area `wa`, the top edge
/// winning when it is taller.
pub fn level_origin(align: Align, parent: Option<RECT>, rtl: bool, at: POINT, w: i32, h: i32, wa: RECT, s: f32) -> (i32, i32) {
    let (mut x, mut y) = match (parent, align) {
        (None, Align::Above) => (at.x - w / 2, at.y - h),
        (None, Align::RightOf) => (at.x, at.y - h / 2),
        (None, Align::LeftOf) => (at.x - w, at.y - h / 2),
        (None, Align::TopRight) => (at.x - w, at.y),
        (None, Align::BottomLeft) => (at.x, at.y - h),
        (None, Align::BottomRight) => (at.x - w, at.y - h),
        // Submenus: `at` is the parent's near edge; right to left they
        // open to its left.
        (Some(_), _) if rtl => (at.x - w, at.y),
        _ => (at.x, at.y),
    };
    // A submenu keeps sub_gap from its parent on either side (7.4: they
    // never overlap).
    let gap = sub_gap(s);
    if let (Some(p), true) = (parent, rtl) {
        if x < wa.left {
            // No room on the left: flip to the parent's right side.
            x = p.right + gap;
        }
    }
    if x + w > wa.right {
        // Submenus flip to the parent's left side; menus slide left.
        x = match parent {
            Some(p) if !rtl => p.left - w - gap,
            _ => wa.right - w,
        };
    }
    x = x.max(wa.left);
    if y + h > wa.bottom {
        y = (wa.bottom - h).max(wa.top);
    }
    y = y.max(wa.top);
    (x, y)
}

/// Pure: the gap (px) between a submenu and its parent at `s` px per DIP:
/// the menu's own padding (4), so the two plates never overlap.
pub fn sub_gap(s: f32) -> i32 {
    (size::MENU_PAD * s).round() as i32
}

fn outward_room(edge:i32,left:bool,work:RECT)->i32 {
    (if left {edge-work.left}else{work.right-edge}).max(1)
}
fn beside_width(room:i32,gap:i32,has_sub:bool)->i32 {
    if has_sub {((room-gap)/2).max(1)}else{room.max(1)}
}

/// Pure: where a submenu is asked to open (level_origin's `at`, screen
/// px) for its parent's rect and the top of the row it hangs off (DIP
/// from the parent's top, as it shows now): sub_gap past the parent's near
/// side (its left side right to left), level with that row.
/// Pure: the side (`true`: left) the submenus of level `li` open to, given
/// the open levels' rects in order. The first level's go to `base` (right to
/// left, or a companion menu's side); a deeper level's keep going the way
/// that level itself went from its parent, so a chain flipped at a display
/// edge never turns back over the menus it came from.
pub fn sub_side(levels: &[RECT], li: usize, base: bool) -> bool {
    match (li.checked_sub(1).and_then(|p| levels.get(p)), levels.get(li)) {
        (Some(parent), Some(level)) => level.left < parent.left,
        _ => base,
    }
}

pub fn sub_anchor(parent: RECT, row_top: f32, rtl: bool, s: f32) -> POINT {
    let x = if rtl { parent.left - sub_gap(s) } else { parent.right + sub_gap(s) };
    POINT { x, y: parent.top + (row_top * s) as i32 - (PAD_Y * s) as i32 }
}

/// Pure: `p` (screen px) is in the gap between two open levels (rects in
/// order, each the next one's parent): a press there is not a click
/// outside the menu.
pub fn in_gap(levels: &[RECT], p: POINT) -> bool {
    levels.windows(2).any(|w| {
        let (a, b) = (w[0], w[1]);
        let (lo, hi) = if b.left >= a.right { (a.right, b.left) } else { (b.right, a.left) };
        p.x >= lo && p.x < hi && p.y >= a.top.min(b.top) && p.y < a.bottom.max(b.bottom)
    })
}

/// Pure: what of `label` is left after `lines` of it (wrap_text's, in
/// order): the text from where they end, without leading spaces. A
/// paragraph cut to a few lines puts it on its last line (cut with "…").
pub fn rest_after(label: &str, lines: &[String]) -> String {
    let used: usize = lines.iter().map(|l| l.chars().filter(|c| *c != ' ').count()).sum();
    let mut seen = 0;
    for (i, c) in label.char_indices() {
        if seen == used {
            return label[i..].trim_start().to_string();
        }
        if c != ' ' {
            seen += 1;
        }
    }
    String::new()
}

/// Pure: which way the day grid slides when row id `id` is picked in a
/// calendar with month header `nav` (7.4: a month turns like a page): 1
/// for the next month (it comes in from the reading end), -1 for the
/// previous, 0 for anything else (a day, back to today: a cross-fade in
/// place).
pub fn month_turn(nav: Option<&MonthNav>, id: u32) -> i32 {
    match nav {
        Some(n) if id != 0 && id == n.next => 1,
        Some(n) if id != 0 && id == n.prev => -1,
        _ => 0,
    }
}

/// Space on a row (Request::on_space).
pub type OnSpace = Box<dyn Fn(u32, HWND, POINT) -> bool>;

/// A row picked that changes the open menu in place (the calendar's month
/// buttons and days): called with its id and `swap`, which shows new rows
/// in the same menu, its day grid cross-fading (true once they are on
/// screen). True: handled, the menu stays open; false: an ordinary pick.
pub type OnPick = Box<dyn Fn(u32, &mut dyn FnMut(Vec<Item>) -> bool) -> bool>;

pub struct Request<'a> {
    pub gfx: &'a Rc<Gfx>,
    pub theme: &'a Theme,
    pub items: Vec<Item>,
    /// Anchor of the menu, screen px (see `align`).
    pub at: POINT,
    pub align: Align,
    pub scale: f32,
    pub titles: Titles,
    /// Called while a slider row is dragged (value 0..1).
    pub on_slide: Option<Box<dyn Fn(f32)>>,
    /// Rows with these ids can be dragged out (Outcome::Dragged), e.g. a
    /// stack's files; `0..0` for none.
    pub drag_ids: std::ops::Range<u32>,
    /// Space on one of the `drag_ids` rows: called with its id, the menu
    /// window and a point under the row (screen px); true = handled (the
    /// menu stays open, nothing is picked). A stack's Quick Look.
    pub on_space: Option<OnSpace>,
    /// See OnPick; None: every pick closes the menu.
    pub on_pick: Option<OnPick>,
    /// It hangs from the bar (`at` is placement::hang_edge's, the gap in
    /// it; a corner `align`): its first level keeps to the room past `at`
    /// (level_room) instead of sliding over the bar. False for a menu at
    /// the pointer.
    pub hangs: bool,
}

// ---- metrics (DIP) ------------------------------------------------------------
// Design system v2.1 (3.2, 3.3): rows 28 (ROW_M), the menu's edge to its
// rows 4, and two vertical lines only: the vessel line at 4 (a row's fill)
// and the content line at 16 (4 + 12: text, glyphs, the separator's line,
// a slider's track), mirrored at the other side. With marks, the check
// column is a 16 box on the content line and 8 before the label (24):
// labels at 40. Other menus (Launchpad's) use these too.
use crate::theme::metrics::{size, space};
pub(crate) const ROW_H: f32 = size::ROW_M;
/// A separator's slot: 8, a hairline across its middle between the
/// content lines.
pub(crate) const SEP_H: f32 = size::SEP_H;
/// Compact density (a menu that would not fit the screen at 28).
pub(crate) const ROW_H_COMPACT: f32 = size::ROW_S;
pub(crate) const SEP_H_COMPACT: f32 = space::S2;
/// A group's header row: footnote, tertiary, never highlighted. The same
/// in a compact menu (its rows are as tall).
pub(crate) const HEADER_H: f32 = size::HEAD_H;
pub(crate) const HEADER_H_COMPACT: f32 = size::HEAD_H;
/// Rows that hold a 28 DIP thing (3.3): a month header's round buttons, a
/// week's day circles, a failure's button.
const ROW_TALL: f32 = size::ROW_L;
const ROW_TALL_COMPACT: f32 = size::ROW_M;
/// Shortcut hints: the secondary text's size (footnote, 12).
const SHORT_SIZE: f32 = crate::theme::ramp::FOOTNOTE.size;
/// Group headers: footnote, regular (a step of the ramp: 4.1).
const HEAD_SIZE: f32 = crate::theme::ramp::FOOTNOTE.size;
pub(crate) const PAD_Y: f32 = size::MENU_PAD;
/// The rows' highlight inset from the menu's sides (the vessel line); its
/// corners are concentric to the menu's (menu radius - this: 12 - 4 = 8).
pub(crate) const ROW_INSET: f32 = size::MENU_PAD;
/// The content line: where everything that starts at the left starts.
pub(crate) const CONTENT_X: f32 = size::MENU_PAD + size::CARD_PAD;
/// A paragraph's line pitch (a note, a notice: body's line height), and a
/// notice (11.4): its width, the room over its title (12 with the menu's
/// own 4), between the title and its reason, and under the reason.
const PARA_LINE: f32 = crate::theme::ramp::BODY.line;
pub(crate) const NOTICE_W: f32 = 280.0;
const NOTICE_TOP: f32 = space::S3;
const NOTICE_GAP: f32 = space::S2;
const NOTICE_END: f32 = space::S3;
/// A notice's reason shows at most this many lines (the last cut with "…").
const NOTICE_LINES: usize = 3;
/// The live clock row (Item::clock): 48 DIP semibold digits (the ramp's
/// DISPLAY) with its AM / PM in the date's size, over a 15 DIP date, the
/// gap between them, and the room around the pair (normal and compact).
/// The row is as tall as the two lines are drawn (Tracker::clock_text_h)
/// plus that room, so no text size or font spills out of it.
const CLOCK_TIME: f32 = crate::theme::ramp::DISPLAY.size;
const CLOCK_DATE: f32 = crate::theme::ramp::HEADLINE.size;
const CLOCK_MARK: f32 = crate::theme::ramp::HEADLINE.size;
const CLOCK_GAP: f32 = space::S1;
const CLOCK_PAD: f32 = space::S4;
const CLOCK_PAD_COMPACT: f32 = space::S2;
/// A line of text is about this many times its size tall (Segoe UI's and
/// Malgun Gothic's 1.33, and a little room): only for a clock whose lines
/// could not be measured.
const LINE_H: f32 = 1.36;
/// An agenda row: a two-line row (ROW_2XL) whose card keeps EVENT_GAP / 2
/// from the row's top and bottom (cards sit 4 apart, as vessels do in a
/// menu), on the vessel line with the row highlight's corners.
const EVENT_ROW_H: f32 = size::ROW_2XL;
const EVENT_ROW_H_COMPACT: f32 = size::ROW_XL;
const EVENT_GAP: f32 = size::MENU_PAD;
/// Its glyph: 24 in a 28 box on the content line; its texts 12 after it.
const EVENT_BOX: f32 = 28.0;
const EVENT_ICON: f32 = size::ICON_L;
const EVENT_TEXT_X: f32 = CONTENT_X + EVENT_BOX + space::S4;
/// One-shot on a level with a clock row: its next second.
const CLOCK_TIMER: usize = 0x5C02;
/// The band at a cut edge of a menu taller than the screen (2.5): its
/// rows fade out over it, and pointing at it scrolls.
const SCROLL_BAND: f32 = size::SCROLL_FADE;
/// The narrowest band on a short room (View::new). Thinner still only
/// when the room is under four of them.
const SCROLL_BAND_MIN: f32 = space::S3;
/// Hover scrolling: a step every SCROLL_TICK_MS, of this part of a row.
const SCROLL_TICK_MS: u32 = 20;
const SCROLL_TICK_ROWS: f32 = 0.25;
/// Rows per wheel notch.
const WHEEL_ROWS: f32 = 3.0;
const SCROLL_TIMER: usize = 0x5C01;
/// One-shot on the first level: its day grid's cross-fade is over.
const CROSSFADE_TIMER: usize = 0x5C03;
/// One-shot on a level: the pointer rested on a row with a submenu for
/// motion::SUBMENU_DELAY_MS (6.3).
const SUBMENU_TIMER: usize = 0x5C04;
/// A menu left for another title of the menu bar stays this long at most
/// while the next one comes (ms), then fades out.
const SWAP_WAIT_MS: u32 = 400;
/// The check column: a mark's box on the content line, the gap after it,
/// and so where labels start in a menu with marks.
const CHECK_BOX: f32 = size::ICON_S;
const CHECK_W: f32 = CONTENT_X + CHECK_BOX + space::S3;
const CHECK_CENTER: f32 = CONTENT_X + CHECK_BOX / 2.0;
/// A failure row's text: after its warning glyph on the content line.
const FAIL_TEXT_X: f32 = CONTENT_X + size::ICON_S + space::S3;
/// Where labels start in a menu with no check marks and no icons: the
/// content line (the check column is only kept where something uses it).
pub(crate) const PLAIN_LEFT: f32 = CONTENT_X;

/// The labels' left inset: room for check marks and icons only when a row
/// of this menu has one (grid rows' `checked` means "other month", not a
/// mark).
fn gutter(items: &[Item]) -> f32 {
    let marked = items.iter().any(|i| !i.separator && i.cells.is_empty() && (i.checked || i.icon.is_some() || i.glyph.is_some()));
    if marked {
        CHECK_W
    } else {
        PLAIN_LEFT
    }
}
/// The content line's mirror: where values, shortcuts and arrows end.
pub(crate) const RIGHT_PAD: f32 = CONTENT_X;
/// A submenu's arrow (a 12 DIP mark) and the room before it.
const ARROW: f32 = size::ICON_XS;
const ARROW_W: f32 = ARROW + space::S2;
const SHORTCUT_GAP: f32 = space::S6;
/// Trailing glyphs (Item::trail): their side and the gap between them and
/// before a shortcut.
const TRAIL_SIDE: f32 = size::ICON_S;
const TRAIL_GAP: f32 = space::S2;

/// What a row's trailing glyphs say, for UIA ("" for none): a network's
/// strength ("Signal 3 of 4") and whether it is secured.
pub fn trail_help(trail: &[Glyph]) -> String {
    let mut parts = Vec::new();
    let mut wifi = false;
    for g in trail {
        match g {
            Glyph::Wifi(n) => {
                wifi = true;
                parts.push(crate::i18n::tf("a11y.popup.signal", &[("n", &(*n).min(4).to_string())]));
            }
            Glyph::Lock => parts.push(crate::i18n::t("a11y.popup.secured").to_string()),
            _ => {}
        }
    }
    if wifi && !trail.contains(&Glyph::Lock) {
        parts.push(crate::i18n::t("a11y.popup.open_net").to_string());
    }
    parts.join(", ")
}

/// Pure: the room a row's trailing glyphs take (0 for none).
fn trail_w(it: &Item) -> f32 {
    let n = it.trail.len() as f32;
    if n == 0.0 {
        0.0
    } else {
        n * TRAIL_SIDE + (n - 1.0) * TRAIL_GAP
    }
}

/// What a row's end column shows in `room` DIP (row_columns' share):
/// its trailing glyphs whole or not at all, and the room left for its
/// shortcut (0: none).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EndPlan {
    pub trail: bool,
    pub short: f32,
}

/// Pure: the end column's plan for `room`, glyphs `trail` and a shortcut
/// `short` DIP wide (either 0 for none). The glyphs stay while they fit
/// (they carry meaning: a network's lock), the shortcut gets the rest.
pub fn end_plan(room: f32, trail: f32, short: f32) -> EndPlan {
    if trail <= 0.0 {
        return EndPlan { trail: false, short: room.max(0.0) };
    }
    if room + 1e-3 < trail {
        // No room for the glyphs: none, nor a shortcut beside them.
        return EndPlan { trail: false, short: if short > 0.0 { room.max(0.0) } else { 0.0 } };
    }
    let left = if short > 0.0 { room - trail - TRAIL_GAP * 2.0 } else { 0.0 };
    EndPlan { trail: true, short: left.max(0.0) }
}

/// Pure: a row's end column, its shortcut hint (`short_w` wide) and its
/// trailing glyphs side by side.
fn end_w(it: &Item, short_w: f32) -> f32 {
    let t = trail_w(it);
    if t > 0.0 && short_w > 0.0 {
        t + TRAIL_GAP * 2.0 + short_w
    } else {
        t + short_w
    }
}
pub(crate) const MIN_W: f32 = 200.0;
const FONT_SIZE: f32 = crate::theme::metrics::BODY;
/// Room around the menu for its shadow.
const SHADOW_M: f32 = crate::theme::metrics::SHADOW_M;

/// How tightly rows sit: Fluent's 32 DIP rows, or 28 when the menu would
/// not fit the screen otherwise (small screens, handhelds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Density {
    #[default]
    Normal,
    Compact,
}

impl Density {
    pub fn row_h(self) -> f32 {
        match self {
            Density::Normal => ROW_H,
            Density::Compact => ROW_H_COMPACT,
        }
    }
    pub fn sep_h(self) -> f32 {
        match self {
            Density::Normal => SEP_H,
            Density::Compact => SEP_H_COMPACT,
        }
    }
    /// A group header's row (Item::head), and a calendar's weekday names.
    pub fn head_h(self) -> f32 {
        match self {
            Density::Normal => HEADER_H,
            Density::Compact => HEADER_H_COMPACT,
        }
    }
    /// A row that holds a 28 DIP thing: a month header, a week of days, a
    /// failure with its button.
    pub fn tall_h(self) -> f32 {
        match self {
            Density::Normal => ROW_TALL,
            Density::Compact => ROW_TALL_COMPACT,
        }
    }
    /// The clock row for its two lines `text_h` DIP tall (with their gap).
    pub fn clock_h(self, text_h: f32) -> f32 {
        let pad = match self {
            Density::Normal => CLOCK_PAD,
            Density::Compact => CLOCK_PAD_COMPACT,
        };
        (text_h + pad).ceil()
    }
    pub fn event_h(self) -> f32 {
        match self {
            Density::Normal => EVENT_ROW_H,
            Density::Compact => EVENT_ROW_H_COMPACT,
        }
    }
}

/// Pure: how tall a wrapping row of `lines` lines is (DIP): a notice's
/// title (NOTICE_TOP over it), the reason under a title (`after_title`:
/// NOTICE_GAP over it, NOTICE_END under), or a note by itself (a row for
/// one line, a paragraph with 4 over and under for more).
pub fn para_h(lines: usize, title: bool, after_title: bool, d: Density) -> f32 {
    let text = lines.max(1) as f32 * PARA_LINE;
    if title {
        NOTICE_TOP + text
    } else if after_title {
        NOTICE_GAP + text + NOTICE_END
    } else {
        // (One line: a row, as tall as the menu's rows.)
        if lines <= 1 { d.row_h() } else { text + 2.0 * space::S2 }
    }
}

/// Pure: where a wrapping row's first line box starts under its top
/// (DIP), for para_h's height `h`.
pub fn para_top(lines: usize, title: bool, after_title: bool, h: f32) -> f32 {
    if title {
        NOTICE_TOP
    } else if after_title {
        NOTICE_GAP
    } else {
        (h - lines.max(1) as f32 * PARA_LINE) / 2.0
    }
}

/// Pure: row `i` of `items` is the reason under a notice's title.
fn after_title(items: &[Item], i: usize) -> bool {
    i > 0 && items[i].wrap && !items[i].title && items[i - 1].title
}

/// What the live clock row says at `now`: the time with its seconds (the
/// region's format: 12 h keeps its AM / PM) and the long date.
pub fn clock_lines(now: &windows::Win32::Foundation::SYSTEMTIME) -> (String, String) {
    (crate::i18n::date::time_seconds(now), crate::i18n::date::long(now))
}

/// Pure: when the clock row is next drawn: the next second, or None while
/// nobody can see it (`idle`: locked, asleep, the display off). It then
/// rests with no timer; session_idle_changed draws it and starts again.
pub fn clock_wait(idle: bool, millis: u16) -> Option<u32> {
    (!idle).then(|| crate::topbar::status::ms_to_next_second(millis))
}

/// Someone can see the screen again, or no one can (dock/session.rs): an
/// open menu's clock row is drawn and ticks again, or rests.
pub fn session_idle_changed() {
    with_track(|t| {
        for li in 0..t.levels.len() {
            if t.levels[li].clock.is_some() {
                if !crate::dock::session_idle() {
                    t.draw_clock(li);
                }
                t.arm_clock(li);
            }
        }
        t.publish_a11y();
    });
}

/// How tall the clock row's time and date are drawn (DIP), with their
/// gap: measured with DirectWrite in the menu's font `family` at the text
/// size, at display scale `scale` (px per DIP), exactly as the row draws
/// them, so the row holds them whatever the font (clock_text_h if they
/// cannot be measured). The menu's rows (fit, row_layout) and what the
/// calendar's agenda leaves room for (topbar::panels) both use this.
pub fn measured_clock_text_h(gfx: &Gfx, family: &str, scale: f32) -> f32 {
    let s = scale;
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let (time, date) = clock_lines(&now);
    let time_h = clock_time_layout(gfx, &time, family, s).ok().map(|l| l.2 / s);
    let date_h = gfx.text_layout(&date, family, CLOCK_DATE * s, DWRITE_FONT_WEIGHT_NORMAL).ok().map(|l| l.2 / s);
    match (time_h, date_h) {
        (Some(t), Some(d)) => t + CLOCK_GAP + d,
        _ => clock_text_h(crate::fonts::scale()),
    }
}

/// The clock row's time laid out at `s` px per DIP (px): tabular digits in
/// the display face (the ramp's DISPLAY), its day period ("AM", "오전":
/// parts::period_runs) in the date's size on the same baseline.
pub(crate) fn clock_time_layout(gfx: &Gfx, time: &str, family: &str, s: f32) -> Result<(IDWriteTextLayout, f32, f32)> {
    use windows::Win32::Graphics::DirectWrite::{DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE};
    let face = crate::fonts::face_for(&crate::fonts::map(family), CLOCK_TIME).into_owned();
    let (l, w, h) = gfx.text_layout_tabular(time, &face, CLOCK_TIME * s, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
    let runs = parts::period_runs(time);
    if runs.is_empty() {
        return Ok((l, w, h));
    }
    unsafe {
        let small = crate::fonts::face_for(&crate::fonts::map(family), CLOCK_MARK).into_owned();
        for (start, len) in runs {
            let range = DWRITE_TEXT_RANGE { startPosition: start, length: len };
            let _ = l.SetFontSize(CLOCK_MARK * s * crate::fonts::scale(), range);
            let _ = l.SetFontFamilyName(&windows::core::HSTRING::from(crate::fonts::dwrite_name(&small)), range);
        }
        let _ = l.SetMaxWidth(4000.0);
        let mut m = DWRITE_TEXT_METRICS::default();
        l.GetMetrics(&mut m)?;
        let w = m.widthIncludingTrailingWhitespace.ceil();
        if crate::i18n::rtl() {
            let _ = l.SetMaxWidth(w.max(1.0));
        }
        Ok((l, w, m.height.ceil()))
    }
}

/// Pure: how tall the clock's two lines and their gap are at text size
/// `scale`, from the usual line spacing (when they cannot be measured).
pub fn clock_text_h(scale: f32) -> f32 {
    (CLOCK_TIME * scale * LINE_H).ceil() + CLOCK_GAP + (CLOCK_DATE * scale * LINE_H).ceil()
}

/// Pure: the times whose width the clock row is made for, on `now`'s day:
/// two-digit hours before and after noon (the widest a 12 or 24 hour
/// clock gets), so the menu never needs to grow as the time goes on.
pub fn clock_samples(now: &windows::Win32::Foundation::SYSTEMTIME) -> [windows::Win32::Foundation::SYSTEMTIME; 2] {
    let at = |h: u16| windows::Win32::Foundation::SYSTEMTIME { wHour: h, wMinute: 58, wSecond: 58, wMilliseconds: 0, ..*now };
    [at(10), at(22)]
}

/// Pure layout: row tops and heights (DIP) and the total height. `clock`:
/// how tall a clock row's two lines are (Density::clock_h).
pub fn row_layout(items: &[Item], d: Density, clock: f32) -> (Vec<(f32, f32)>, f32) {
    let mut y = PAD_Y;
    let rows = items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let h = if it.separator {
                d.sep_h()
            } else if it.clock {
                d.clock_h(clock)
            } else if it.event.is_some() {
                d.event_h()
            } else if it.wrap {
                para_h(it.lines.len(), it.title, after_title(items, i), d)
            } else if it.head || it.abbrev {
                d.head_h()
            } else if it.nav.is_some() || !it.cells.is_empty() || it.retry.is_some() {
                d.tall_h()
            } else {
                d.row_h()
            };
            let r = (y, h);
            y += h;
            r
        })
        .collect();
    (rows, y + PAD_Y)
}

/// Pure: what to show in a work area `max_h` DIP tall. The rows as given
/// if they fit; else compact rows; else, still compact, foldable groups
/// (`Item::fold`) moved into submenus, the highest `fold` first, until it
/// fits. Nothing is dropped: whatever still does not fit scrolls (View).
/// `clock`: as for row_layout.
pub fn fit(items: Vec<Item>, max_h: f32, clock: f32) -> (Vec<Item>, Density) {
    if row_layout(&items, Density::Normal, clock).1 <= max_h {
        return (items, Density::Normal);
    }
    let d = Density::Compact;
    let fits = |v: &[Item]| row_layout(v, d, clock).1 <= max_h;
    let mut items = items;
    while !fits(&items) {
        let Some((s, e)) = next_fold(&items) else { break };
        items = fold_group(items, s, e, &fits);
    }
    (items, d)
}

/// Pure: the next group to fold, (start, end): the first run of rows
/// with the highest `fold` (None when nothing is foldable).
fn next_fold(items: &[Item]) -> Option<(usize, usize)> {
    let top = items.iter().map(|i| i.fold).max().filter(|&f| f > 0)?;
    let s = items.iter().position(|i| i.fold == top)?;
    let e = items[s..].iter().position(|i| i.fold != top).map_or(items.len(), |n| s + n);
    Some((s, e))
}

/// Pure: rows `s..e` folded. Partly if that is enough (the header, the
/// first entries, the rest under "More ›"), else wholly (one row named
/// after the header, or "More ›"). Its rows are not foldable afterwards.
fn fold_group(mut items: Vec<Item>, s: usize, e: usize, fits: &dyn Fn(&[Item]) -> bool) -> Vec<Item> {
    let mut run: Vec<Item> = items
        .drain(s..e)
        .map(|mut it| {
            it.fold = 0;
            it
        })
        .collect();
    let header = (run.len() > 1 && run[0].header()).then(|| run.remove(0));
    if header.is_none() && run.len() <= 1 {
        // One row: folding it saves nothing.
        items.splice(s..s, run);
        return items;
    }
    let more = crate::i18n::t("popup.more_rows").to_string();
    // Partly: keep the first k entries (moving fewer than two saves nothing).
    let n = run.len();
    for k in (1..n.saturating_sub(1)).rev() {
        let mut part: Vec<Item> = header.iter().cloned().chain(run[..k].iter().cloned()).collect();
        part.push(Item::new(0, more.clone()).submenu(run[k..].to_vec()));
        let mut v = items.clone();
        v.splice(s..s, part);
        if fits(&v) {
            return v;
        }
    }
    let row = match header {
        Some(h) => Item { literal: h.literal, ..Item::new(0, h.label) },
        None => Item::new(0, more),
    };
    items.insert(s, row.submenu(run));
    items
}

/// The rows are drawn in tiles this many px tall (any menu length stays
/// under the surface side limit), only those near what shows.
const TILE_PX: i32 = 1024;
/// Tiles kept drawn above and below the visible ones.
const TILE_MARGIN: usize = 1;

/// Pure: how many tiles `content_px` tall rows take.
pub fn tile_count(content_px: i32) -> usize {
    (content_px.max(1) as usize).div_ceil(TILE_PX as usize)
}

/// Pure: tile `k`'s top and height (px) in `content_px` of rows.
pub fn tile_span(k: usize, content_px: i32) -> (i32, i32) {
    let top = k as i32 * TILE_PX;
    (top, TILE_PX.min(content_px - top).max(1))
}

/// Pure: the tiles to have drawn for `view` at `scale`: those showing and
/// TILE_MARGIN more each way.
pub fn tiles_needed(view: &View, scale: f32, content_px: i32) -> std::ops::Range<usize> {
    let n = tile_count(content_px);
    let y0 = (view.offset * scale).floor().max(0.0) as i32;
    let y1 = ((view.offset + view.height) * scale).ceil() as i32;
    let first = (y0 / TILE_PX) as usize;
    let last = ((y1.max(y0 + 1) - 1) / TILE_PX) as usize;
    first.saturating_sub(TILE_MARGIN)..(last + 1 + TILE_MARGIN).min(n)
}

/// Pointing at a scroll band scrolls while the pointer stays there; the
/// keyboard (or a screen reader) taking over stops it, and only the
/// pointer really moving onto a band starts it again.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct EdgeScroll {
    /// The level's window and the way (-1 up, 1 down).
    on: Option<(HWND, i32)>,
    /// Where the pointer last was (screen px): a "move" to the same place
    /// (synthesized by Windows) is not the pointer moving.
    last: Option<POINT>,
}

impl EdgeScroll {
    /// The pointer moved to `p`, where `want` would scroll. Returns the
    /// timer to stop and the one to start.
    fn pointer(&mut self, p: POINT, want: Option<(HWND, i32)>) -> (Option<HWND>, Option<HWND>) {
        if self.last == Some(p) {
            return (None, None);
        }
        self.last = Some(p);
        if want == self.on {
            return (None, None);
        }
        let stop = self.on.take().map(|o| o.0);
        self.on = want;
        (stop, want.map(|w| w.0))
    }
    /// The keyboard moved the focus: the timer to stop.
    fn keyboard(&mut self) -> Option<HWND> {
        self.on.take().map(|o| o.0)
    }
}

/// Where a point of a level is (View::at).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    /// The top scroll band.
    Up,
    /// The bottom scroll band.
    Down,
    /// Over the rows: the one under it (None: padding, a gap).
    Row(Option<usize>),
}

/// A level's visible part (DIP): all of it, or, taller than the screen, a
/// window onto its rows. Where rows are cut off (more of them that way)
/// they fade out over a band (design system 2.5), and pointing at that
/// band scrolls; at an end the rows sit as in any menu.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct View {
    /// All rows with their padding.
    pub content: f32,
    /// The menu on screen.
    pub height: f32,
    /// The band at a cut edge: the padding in a menu that shows all of
    /// itself, else scroll_band().
    pub band: f32,
    /// How far the rows are scrolled (0..=max_offset()).
    pub offset: f32,
}

impl View {
    /// Never taller than `max_h` (the room: a menu hanging from the bar
    /// must not move over its gap): taller content scrolls in it, fading
    /// at the edges it is cut at over SCROLL_BAND, thinner on a room too
    /// short for two of them and a row (scroll_band).
    pub fn new(content: f32, max_h: f32) -> Self {
        if content <= max_h {
            return Self { content, height: content, band: PAD_Y, offset: 0.0 };
        }
        let height = max_h.max(1.0);
        Self { content, height, band: scroll_band(height), offset: 0.0 }
    }
    /// Taller than it shows (the wheel, the keyboard and the bands move it).
    pub fn scrolls(&self) -> bool {
        self.content > self.height
    }
    pub fn max_offset(&self) -> f32 {
        (self.content - self.height).max(0.0)
    }
    /// Where the rows' y = 0 is in the menu.
    pub fn shift(&self) -> f32 {
        -self.offset
    }
    /// The height rows show whole in, clear of both bands (a page).
    pub fn page(&self) -> f32 {
        self.height - 2.0 * self.band
    }
    pub fn clamp(&self, offset: f32) -> f32 {
        offset.clamp(0.0, self.max_offset())
    }
    /// The offset that shows row (y, h) whole and clear of the bands,
    /// moving as little as possible (at an end the offset stops there: the
    /// first and last rows sit where they do in any menu). A row taller
    /// than the page (a very short room) shows its middle, where its text
    /// is, not its bottom edge.
    pub fn reveal(&self, (y, h): (f32, f32)) -> f32 {
        if h > self.page() {
            return self.clamp(y + h / 2.0 - self.height / 2.0);
        }
        let top = y - self.band;
        let bottom = y + h + self.band - self.height;
        self.clamp(self.offset.min(top).max(bottom))
    }
    /// Can scroll further up (dir < 0) / down.
    pub fn can(&self, dir: i32) -> bool {
        if dir < 0 { self.offset > 0.01 } else { self.offset < self.max_offset() - 0.01 }
    }
    /// The bands that show now (top, bottom): where rows are cut off.
    pub fn bands(&self) -> (bool, bool) {
        (self.scrolls() && self.can(-1), self.scrolls() && self.can(1))
    }
    /// What is at menu y `y` (DIP from the menu's top).
    pub fn at(&self, rows: &[(f32, f32)], y: f32) -> Spot {
        let (up, down) = self.bands();
        if up && y < self.band {
            return Spot::Up;
        }
        if down && y >= self.height - self.band {
            return Spot::Down;
        }
        let c = y - self.shift();
        Spot::Row(rows.iter().position(|&(ry, rh)| c >= ry && c < ry + rh))
    }
    /// Row (y, h) in menu coordinates, cut to what shows clear of the
    /// bands; None when it is scrolled out.
    pub fn visible(&self, (y, h): (f32, f32)) -> Option<(f32, f32)> {
        let (up, down) = self.bands();
        let (lo, hi) = (if up { self.band } else { 0.0 }, if down { self.height - self.band } else { self.height });
        let (a, b) = ((y + self.shift()).max(lo), (y + h + self.shift()).min(hi));
        (b > a).then_some((a, b))
    }
}

/// Pure: the band at a cut edge of a scrolling menu `height` DIP tall:
/// SCROLL_BAND with a row between two of them; on a shorter room thinner,
/// down to SCROLL_BAND_MIN, and at most a quarter of it below that (half
/// the room still shows rows).
pub fn scroll_band(height: f32) -> f32 {
    ((height - ROW_H) / 2.0).clamp(SCROLL_BAND_MIN, SCROLL_BAND).min(height / 4.0).max(0.0)
}

/// Pure: a level's side in px, `dip` at `scale`, never past `room_px` (the
/// room it was given, px): rounding the DIP back up never makes it a
/// pixel more (at 175 % 1010 px is 577.142.. DIP, 1010.00006 px again).
pub fn level_px(dip: f32, scale: f32, room_px: i32) -> i32 {
    ((dip * scale).ceil() as i32).min(room_px).max(1)
}

/// Pure: how wide (DIP) a menu that wants `want` DIP is on a work area
/// `work_px` px wide at `scale`: never wider, so every column of a grid
/// and the right edge stay on screen (its content fits itself to it:
/// shorter weekday names, notes cut with "…").
pub fn level_width(want: f32, work_px: i32, scale: f32) -> f32 {
    want.min(work_px.max(1) as f32 / scale)
}

/// Pure: the room (DIP) for a grid cell's text in a menu `width` wide with
/// `cols` columns (as grid_cell and the paint lay them out), GRID_CELL_PAD
/// either side.
pub fn grid_text_room(width: f32, cols: usize) -> f32 {
    if cols == 0 {
        return 0.0;
    }
    (grid_col(width, cols) - GRID_CELL_PAD * 2.0).max(0.0)
}

/// Pure: `text` short enough for `room` (`width` measures): as it is when
/// it fits, else its longest beginning that does, without a trailing dot
/// or space ("Mo." -> "Mo" -> "M"; a short weekday name stays readable),
/// at least its first letter.
pub fn shorten(text: &str, room: f32, width: &dyn Fn(&str) -> f32) -> String {
    if width(text) <= room {
        return text.to_string();
    }
    let ends: Vec<usize> = text.char_indices().map(|(i, c)| i + c.len_utf8()).collect();
    for &e in ends.iter().rev().skip(1) {
        let cut = text[..e].trim_end_matches(|c: char| c == '.' || c.is_whitespace());
        if !cut.is_empty() && width(cut) <= room {
            return cut.to_string();
        }
    }
    text.chars().next().map(String::from).unwrap_or_default()
}

/// Grid cells' text size (DIP), and the least whole cells shrink to while
/// their column has room for it.
const GRID_TEXT: f32 = crate::theme::ramp::BODY.size;
/// Weekday names over the days: footnote size, secondary text.
const GRID_HEAD: f32 = crate::theme::ramp::FOOTNOTE.size;
const GRID_TEXT_MIN: f32 = 6.0;

/// The day circles (today, the day picked, the day under the pointer):
/// their side (DIP) in a roomy column (a capsule: a circle), and the room
/// kept from the column's edges.
const GRID_MARK: f32 = 28.0;
const GRID_MARK_GAP: f32 = 1.0;

/// Pure: one grid column's width (DIP) in a menu `width` wide with `cols`
/// columns between the 12 DIP margins (grid_cell, the paint).
pub fn grid_col(width: f32, cols: usize) -> f32 {
    if cols == 0 {
        return 0.0;
    }
    ((width - 24.0) / cols as f32).max(0.0)
}

/// Pure: the side and corner radius (DIP) of a day circle in a column
/// `col` wide: GRID_MARK, smaller in a narrow column so it stays inside
/// its own cell (never over the next day's number); always a circle.
pub fn grid_mark(col: f32) -> (f32, f32) {
    let side = GRID_MARK.min(col - 2.0 * GRID_MARK_GAP).max(0.0);
    (side, crate::theme::capsule(side))
}

/// Pure: the room (DIP) a day number may take in a column `col` wide: the
/// whole column but GRID_MARK_GAP either side, so in a narrow column it
/// is inside its day square too (grid_mark).
fn grid_day_room(col: f32) -> f32 {
    (col - 2.0 * GRID_MARK_GAP).max(0.0)
}

/// Pure: what each grid cell of `items` draws in a menu `width` wide with
/// `cols` columns, (text, size DIP); `measure` gives a text's width at a
/// size. Weekday names (`Item::abbrev`) are shortened to their room;
/// other cells (day numbers) keep every digit, so the day shown is the day
/// a click picks: they first use their padding (grid_day_room), then all
/// get the same smaller size, measured again at it. Never wider than
/// their room, so two never overlap: GRID_TEXT_MIN while that fits, still
/// smaller on a work area too narrow even for it (tiny numbers, each in
/// its own column, rather than numbers over each other).
pub fn grid_texts(items: &[Item], width: f32, cols: usize, measure: &dyn Fn(&str, f32) -> f32) -> Vec<Vec<(String, f32)>> {
    let room = grid_text_room(width, cols);
    let day_room = grid_day_room(grid_col(width, cols));
    let widest_at = |size: f32| items.iter().filter(|i| !i.abbrev).flat_map(|i| &i.cells).map(|c| measure(c, size)).fold(0.0, f32::max);
    let widest = widest_at(GRID_TEXT);
    let mut whole = if widest <= day_room { GRID_TEXT } else { (GRID_TEXT * day_room / widest).max(GRID_TEXT_MIN) };
    // (A text does not shrink exactly with its size: measured again.)
    for _ in 0..8 {
        let w = widest_at(whole);
        if w <= day_room {
            break;
        }
        whole *= day_room / w * 0.999;
    }
    let at = |t: &str| measure(t, GRID_TEXT);
    items
        .iter()
        .map(|it| it.cells.iter().map(|c| if it.abbrev { (shorten(c, room, &at), GRID_TEXT) } else { (c.clone(), whole) }).collect())
        .collect()
}

/// A label keeps at least this much (DIP) before a long shortcut hint is
/// cut; a hint with less room than SHORTCUT_MIN is not shown.
const LABEL_MIN: f32 = 64.0;
const SHORTCUT_MIN: f32 = 24.0;

/// Pure: how wide (DIP) a row's label and shortcut hint may be drawn in a
/// menu `width` wide whose labels start at `left` (`label_w`, `short_w`:
/// their whole widths; `sub`: the row opens a submenu, its arrow instead
/// of a hint). They never meet: left + label + SHORTCUT_GAP + hint stays
/// within width - RIGHT_PAD (- ARROW_W for the arrow). Short of room the
/// hint gives way first: cut with "…" while the label keeps LABEL_MIN,
/// dropped (0) when that leaves it under SHORTCUT_MIN.
pub fn row_columns(width: f32, left: f32, label_w: f32, short_w: f32, sub: bool) -> (f32, f32) {
    let room = (width - left - RIGHT_PAD - if sub { ARROW_W } else { 0.0 }).max(0.0);
    if sub || short_w <= 0.0 {
        return (room, 0.0);
    }
    if label_w + SHORTCUT_GAP + short_w <= room {
        return (room - SHORTCUT_GAP - short_w, short_w);
    }
    let short = short_w.min(room - SHORTCUT_GAP - label_w.min(LABEL_MIN));
    if short >= SHORTCUT_MIN {
        (room - SHORTCUT_GAP - short, short)
    } else {
        (room, 0.0)
    }
}

/// Pure: Page Up (dir < 0) / Page Down from `from`: the focusable row
/// farthest that way within `page` DIP, at least the next one; it stops at
/// the ends (no wrapping).
pub fn page_step(items: &[Item], rows: &[(f32, f32)], from: Option<usize>, dir: i32, page: f32) -> Option<usize> {
    let Some(f) = from.filter(|&f| f < rows.len()) else { return step(items, None, dir) };
    let target = rows[f].0 + dir as f32 * page.max(0.0);
    let focusable = |i: &usize| items[*i].focusable();
    if dir > 0 {
        let after: Vec<usize> = (f + 1..items.len()).filter(focusable).collect();
        after.iter().rev().find(|&&i| rows[i].0 <= target).or(after.first()).copied().or(Some(f))
    } else {
        let before: Vec<usize> = (0..f).filter(focusable).collect();
        before.iter().find(|&&i| rows[i].0 >= target).or(before.last()).copied().or(Some(f))
    }
}

/// Pure: `text` broken into lines at most `max_w` wide (`width` measures
/// a piece): at spaces, and inside a word only when it alone is too wide
/// (languages written without spaces break anywhere that way).
pub fn wrap_text(text: &str, max_w: f32, width: &dyn Fn(&str) -> f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split(' ').filter(|w| !w.is_empty()) {
        let joined = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if width(&joined) <= max_w {
            cur = joined;
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        if width(word) <= max_w {
            cur = word.to_string();
            continue;
        }
        // One word wider than the line: by characters.
        for ch in word.chars() {
            let next = format!("{cur}{ch}");
            if !cur.is_empty() && width(&next) > max_w {
                lines.push(std::mem::take(&mut cur));
                cur.push(ch);
            } else {
                cur = next;
            }
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Next selectable row from `from` in direction `dir` (wrapping).
pub fn step(items: &[Item], from: Option<usize>, dir: i32) -> Option<usize> {
    let n = items.len() as i32;
    if n == 0 {
        return None;
    }
    let mut i = match from {
        Some(f) => f as i32,
        None if dir > 0 => -1,
        None => n,
    };
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if items[i as usize].focusable() {
            return Some(i as usize);
        }
    }
    None
}

/// The inks a level's rows are painted in (the theme's; nothing of their
/// own). High contrast paints a marked row again with `inverse`.
#[derive(Clone, Copy)]
struct Inks {
    on: D2D1_COLOR_F,
    dim: D2D1_COLOR_F,
    off: D2D1_COLOR_F,
    hint: D2D1_COLOR_F,
    line: D2D1_COLOR_F,
    mark: D2D1_COLOR_F,
    on_mark: D2D1_COLOR_F,
    pick: D2D1_COLOR_F,
    card: D2D1_COLOR_F,
}

impl Inks {
    /// The translucent text levels: they sit on whatever the menu shows
    /// through, so Glass keeps their contrast on any wallpaper (the
    /// opaque text_dim() did not: design B1 review P2 #1).
    fn of(theme: &Theme) -> Inks {
        let c = parts::ink;
        Inks {
            on: c(theme.text_primary()),
            dim: c(theme.text_tertiary()),
            off: c(theme.text_disabled()),
            hint: c(theme.text_secondary()),
            // The separator's hairline; high contrast keeps a 1 DIP line.
            line: c(if theme.high_contrast { theme.separator() } else { theme.hairline() }),
            mark: c(theme.accent()),
            on_mark: c(theme.on_accent()),
            pick: c(theme.fill_of(Role::Row, State::Selected)),
            card: c(theme.menu_card()),
        }
    }
    /// High contrast, on a Highlight fill (12.3): everything in
    /// HighlightText, no fills of its own.
    fn inverse(theme: &Theme) -> Inks {
        let t = parts::ink(theme.text_of(crate::theme::Level::Primary, State::Cursor));
        let clear = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
        Inks { on: t, dim: t, off: t, hint: t, line: t, mark: clear, on_mark: t, pick: clear, card: clear }
    }
}

/// The pointer's fills of a level: one comes on where the pointer is while
/// the one before fades where it was (6.3: sweeping fast, two rows are lit
/// at once, on purpose), so a few take turns.
const HOVER_POOL: usize = 4;

struct Level {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    _root: ContainerVisual,
    /// The pointer's fills (HOVER_POOL of them, see there) and which one
    /// is lit now.
    hovers: Vec<parts::Glow>,
    hover_i: usize,
    /// The one instant fill: a press, the row whose submenu is up, the
    /// keyboard's cursor (parts::glows). In high contrast it is the target
    /// painted again in HighlightText on Highlight, over the rows.
    mark: parts::Glow,
    /// What the fills show now (sync_glow).
    shown: parts::Glows,
    /// What the pointer is on (None: nothing that answers, or it is not
    /// over this level).
    pointer: Option<Target>,
    /// The keyboard (or a screen reader) moved the highlight last: its
    /// cursor shows, not the pointer's hover (6.2: one emphasis).
    by_key: bool,
    /// What the button is down on.
    pressed: Option<Target>,
    /// The row whose submenu is up.
    open_row: Option<usize>,
    /// The rows' fill corners (concentric to the menu's).
    row_r: f32,
    inks: Rc<Cell<Inks>>,
    /// The rows it was asked to show (before wrapping and fitting): a
    /// submenu already open for the same rows is kept.
    src: Vec<Item>,
    items: Vec<Item>,
    /// Row tops and heights in the rows' own space (scrolled by `view`).
    rows: Vec<(f32, f32)>,
    density: Density,
    view: View,
    /// Holds the rows, the fills and the sliders; moved by the scroll.
    scroller: ContainerVisual,
    /// The fading bands of a menu taller than the screen (parts::Edges).
    edges: Option<parts::Edges>,
    /// The rows' text, drawn in tiles (tiles_needed) inside `text`.
    paint: Box<Paint>,
    paint_items: Rc<RefCell<Vec<Item>>>,
    paint_labels: Rc<RefCell<Vec<Option<(IDWriteTextLayout, f32, f32)>>>>,
    text: ContainerVisual,
    tiles: Vec<Option<SpriteVisual>>,
    /// Width and height of all rows, px.
    w_px: i32,
    /// The labels' left inset (gutter()).
    left: f32,
    content_px: i32,
    width: f32,
    /// Screen px.
    rect: RECT,
    hover: Option<usize>,
    /// A month header's part with the keyboard focus (its row, the part):
    /// set by UIA's SetFocus on it (Tracker::focus_nav), gone when another
    /// row is highlighted.
    nav_focus: Option<(usize, NavPart)>,
    /// Slider rows: (row, fill visual, knob visual, track x0, track width).
    sliders: Vec<(usize, SpriteVisual, SpriteVisual, f32, f32)>,
    /// Failure rows' buttons: (row, the button, its top left in the rows'
    /// space).
    buttons: Vec<(usize, parts::Button, (f32, f32))>,
    /// The live clock row and its visual (drawn on every second).
    clock: Option<(usize, SpriteVisual)>,
    /// The day grid cross-fading or sliding after an in-place swap
    /// (Tracker::crossfade): its layers over `text`, which is clipped
    /// meanwhile.
    fade: Vec<ContainerVisual>,
}

struct Tracker {
    gfx: Rc<Gfx>,
    theme: Theme,
    scale: f32,
    levels: Vec<Level>,
    titles: Titles,
    done: Option<Outcome>,
    align: Align,
    /// Row the (left) button went down on: a release picks only that row.
    down_row: Option<(usize, usize)>,
    /// What on that row it went down on (a failure's button answers only
    /// a press that began on it).
    down_on: Option<Target>,
    /// The (left) button went down inside the menu, on a row or not (its
    /// margins): a failure's button then answers only a press begun on it;
    /// false, the press opened the menu from the bar.
    down_in: bool,
    on_slide: Option<Box<dyn Fn(f32)>>,
    on_space: Option<OnSpace>,
    /// Dragging the slider of (level, row).
    sliding: Option<(usize, usize)>,
    drag_ids: std::ops::Range<u32>,
    /// Where the left button went down (screen px), to tell a drag.
    down_at: Option<POINT>,
    /// Right-to-left language: rows are mirrored (check marks and icons on
    /// the right, submenu arrows and shortcuts on the left), submenus open
    /// to the left and sliders grow to the left.
    rtl: bool,
    sub_left: bool,
    /// The side (`true`: left) the submenu being opened goes to (sub_side).
    sub_dir: bool,
    companion_body: Option<crate::companion::convenience::MenuBody>,
    /// Hover scrolling at a scroll band.
    edge: EdgeScroll,
    /// The first level's anchor (Request::at), for replace_top.
    anchor: POINT,
    /// Request::hangs.
    hangs: bool,
    on_pick: Option<OnPick>,
    /// The first level does not grow open: another title's menu was just
    /// open and this one takes over from it (scrubbing, design system 7.4:
    /// the two cross over SCRUB_MS at full size, see drop_swap).
    instant: bool,
    /// The row (level, row) the pointer rests on whose submenu opens when
    /// SUBMENU_TIMER fires (6.3).
    pending_sub: Option<(usize, usize)>,
    /// The submenu being built takes over from a sibling: it does not
    /// grow open, the two cross (open_sub).
    swap_in: bool,
    /// The row a submenu being built hangs off (screen px): where it
    /// grows from.
    sub_row: Option<RECT>,
    /// Which way the day grid slides in the in-place swap under way
    /// (month_turn; 0: it cross-fades).
    turn: i32,
}

/// Draws the rows whose px span meets `y0..y1` into a device context, the
/// rows' px y = 0 at (ox, oy).
type Paint = dyn Fn(&windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, f32, f32, f32, f32) -> Result<()>;

thread_local! {
    static TRACK: RefCell<Option<Tracker>> = const { RefCell::new(None) };
}

thread_local! {
    /// The time the clock row is drawn for instead of now: an audit
    /// picture's fixed time (only the tests set it; None: now).
    static CLOCK_AT: Cell<Option<windows::Win32::Foundation::SYSTEMTIME>> = const { Cell::new(None) };
}

/// The time the clock row shows: now (CLOCK_AT's in an audit picture).
fn clock_now() -> windows::Win32::Foundation::SYSTEMTIME {
    if let Some(t) = CLOCK_AT.with(|c| c.get()) {
        return t;
    }
    unsafe { windows::Win32::System::SystemInformation::GetLocalTime() }
}

/// A menu is open right now.
pub fn is_tracking() -> bool {
    TRACK.with(|t| t.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

/// Close the open menu (its owner is going away or changed). Safe to call
/// from inside the nested loop's handlers.
pub fn cancel() {
    let first = with_track(|t| {
        t.done.get_or_insert(Outcome::Lost);
        t.levels.first().map(|l| l.hwnd)
    })
    .flatten();
    if let Some(h) = first {
        unsafe {
            // Wake the loop so it sees `done`.
            let _ = PostMessageW(Some(h), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

/// The open menu's first level shows `items` instead, in place (its rows
/// changed while it was open: the calendar's agenda came in). Safe to call
/// from inside the nested loop's handlers. False if no menu is open or it
/// could not be drawn.
pub fn replace(items: Vec<Item>) -> bool {
    with_track(|t| unsafe { t.replace_top(items, false) }).unwrap_or(false)
}

/// Update a reserved command without resizing, closing submenus, or resetting
/// pointer/keyboard/press state. Longer labels are clipped to the existing width.
pub fn update_command(row: Item) -> bool {
    with_track(|t| t.update_command(row)).unwrap_or(false)
}

fn command_update(items: &mut [Item], row: &Item) -> Option<usize> {
    let at = items.iter().position(|old| old.id != 0 && old.id == row.id && old.sub.is_empty() && !old.separator)?;
    let mut expected = items[at].clone();
    expected.label = row.label.clone();
    expected.enabled = row.enabled;
    if expected != *row { return None; }
    items[at] = row.clone();
    Some(at)
}

fn with_track<R>(f: impl FnOnce(&mut Tracker) -> R) -> Option<R> {
    TRACK.with(|t| t.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn contains(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// Pure: a hairline at `y` DIP on a surface `scale` px per DIP (audit #13):
/// its top and thickness in DIP, one device pixel on a whole pixel, so it
/// never smears 1 DIP over 1.25 or 1.5 px. Draw it in Theme::hairline().
/// Most groups are parted by space alone; this is for the few lines left.
pub fn hairline(y: f32, scale: f32) -> (f32, f32) {
    let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    ((y * s).round() / s, 1.0 / s)
}

/// Pure: hairline() for a surface drawn in device pixels: (top, thickness)
/// px. `thick_dip`: 0 for one device pixel, else that many DIP rounded to
/// whole pixels (high contrast's lines), at least one.
pub fn hairline_px(y_px: f32, thick_dip: f32, scale: f32) -> (f32, f32) {
    let t = if thick_dip > 0.0 { (thick_dip * scale).round().max(1.0) } else { 1.0 };
    (y_px.round(), t)
}

fn d2d(c: (u8, u8, u8, u8)) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a: c.3 as f32 / 255.0 }
}

/// The plate every menu and panel sits on (control center, search, stacks,
/// the island card, PiP): the contact shadow and the soft shadows of its
/// elevation shaped like it, the rounded clip (`radius` DIP), the backdrop
/// and its material's tint (or the tint alone, opaque), and the rim.
/// Fills `outer` (already scaled and offset by the shadow margin) with
/// them; returns the clipped root and the rim's visual, so content can go
/// between the tint and the rim (`InsertBelow(border)`). Panels sit at
/// elevation E3 (the theme's panel shadows) on material M2.
pub(crate) fn chrome(gfx: &Gfx, theme: &Theme, outer: &ContainerVisual, width: f32, height: f32, s: f32, radius: f32) -> Result<(ContainerVisual, ShapeVisual)> {
    chrome_at(gfx, theme, outer, width, height, s, radius, Elevation::E3)
}

/// chrome() at elevation `e`, on its material (menus and dropdown lists:
/// E2, M3). Its shadows are real blurs (DropShadow), kept inside SHADOW_M.
///
/// The light (design system v2.1, 2.2 and 2.6). The rim: a stroke one
/// device pixel thick just inside the edge, white from the top through
/// 38 % to the bottom (spike S7: one clean row at 100 to 200 %). The
/// contact shadow: a ring CONTACT_W wide just outside it. Both and the
/// soft shadows are marked (motion::TAG_RIM on the rim, TAG_SHADE on the
/// container of the shadows): their opacity is the light's strength, which
/// motion::open brings up apart from the plate's. High contrast has
/// neither: its 1 DIP outline instead.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chrome_at(gfx: &Gfx, theme: &Theme, outer: &ContainerVisual, width: f32, height: f32, s: f32, radius: f32, e: Elevation) -> Result<(ContainerVisual, ShapeVisual)> {
    chrome_with(gfx, theme, outer, width, height, s, radius, e, None)
}

/// chrome() whose height follows the scalar "h" (DIP) of `live`: every
/// part of the plate (its clip, backdrop, tint, top light, rim, shadows
/// and contact ring) is an expression of it, so a spring on "h" moves the
/// plate's free edge while its top stays (a panel's height on GLIDE, 7.4:
/// the control center going into a shorter list). `height`: "h" now (set
/// on `live`); the corners keep their shape at any height.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chrome_live(gfx: &Gfx, theme: &Theme, outer: &ContainerVisual, width: f32, height: f32, s: f32, radius: f32, live: &CompositionPropertySet) -> Result<(ContainerVisual, ShapeVisual)> {
    live.InsertScalar(&HSTRING::from("h"), height)?;
    chrome_with(gfx, theme, outer, width, height, s, radius, Elevation::E3, Some(live))
}

/// Sets the size of `obj` (a visual or a rounded rectangle) to (`w`,
/// `height` + `dh`), or, with `live`, binds it to (`w`, live.h + `dh`).
fn size_to(c: &windows::UI::Composition::Compositor, obj: &impl Interface, w: f32, height: f32, dh: f32, live: Option<&CompositionPropertySet>) -> Result<()> {
    if let Some(m) = live {
        let e = c.CreateExpressionAnimationWithExpression(&HSTRING::from(format!("Vector2({w}, m.h + {dh})")))?;
        e.SetReferenceParameter(&HSTRING::from("m"), m)?;
        return obj.cast::<windows::UI::Composition::CompositionObject>()?.StartAnimation(&HSTRING::from("Size"), &e);
    }
    let size = Vector2 { X: w, Y: height + dh };
    if let Ok(v) = obj.cast::<windows::UI::Composition::Visual>() {
        v.SetSize(size)
    } else {
        obj.cast::<windows::UI::Composition::CompositionRoundedRectangleGeometry>()?.SetSize(size)
    }
}

#[allow(clippy::too_many_arguments)]
fn chrome_with(gfx: &Gfx, theme: &Theme, outer: &ContainerVisual, width: f32, height: f32, s: f32, radius: f32, e: Elevation, live: Option<&CompositionPropertySet>) -> Result<(ContainerVisual, ShapeVisual)> {
    let c = &gfx.compositor;
    let root = c.CreateContainerVisual()?;
    size_to(c, &root, width, height, 0.0, live)?;
    let radius = radius.clamp(0.0, (width.min(height) / 2.0).max(0.0));
    let clip = c.CreateRoundedRectangleGeometry()?;
    size_to(c, &clip, width, height, 0.0, live)?;
    clip.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
    root.SetClip(&c.CreateGeometricClipWithGeometry(&clip)?)?;

    let size = Vector2 { X: width, Y: height };
    let shade = shade_with(gfx, theme, width, height, s, radius, e, live)?;
    outer.Children()?.InsertAtTop(&shade)?;
    outer.Children()?.InsertAtTop(&root)?;

    let material = e.material();
    // Tips have no backdrop in either finish; an opaque tint needs none.
    let solid = theme.popup.material == "solid" || !crate::effects::items().blur || material == crate::theme::Material::Tip || theme.material(material).a == 255;
    let backdrop = c.CreateSpriteVisual()?;
    // The blur through the glass's effect graph (glass v3: saturation and
    // the Luminosity layer); none needed when it is not seen.
    let glass: windows::UI::Composition::CompositionBrush = if solid { c.CreateHostBackdropBrush()?.cast()? } else { crate::glass::backdrop(gfx, theme, material)? };
    backdrop.SetBrush(&glass)?;
    // The glass's tint for that brush (crate::glass: the Luminosity layer
    // folded in where it is not drawn).
    let t = crate::glass::tint_for(theme, material, Some(&glass));
    size_to(c, &backdrop, width, height, 0.0, live)?;
    backdrop.SetOpacity(if solid { 0.0 } else { 1.0 })?;
    let tint = c.CreateSpriteVisual()?;
    let opaque = theme.popup.material == "solid" || !crate::effects::items().blur;
    // Flat v2: a fall from top to bottom (crate::glass::tint_brush).
    tint.SetBrush(&crate::glass::tint_brush(c, theme, material, Rgba { a: if opaque { 255 } else { t.a }, ..t })?)?;
    size_to(c, &tint, width, height, 0.0, live)?;
    let border = rim_with(gfx, theme, material, width, height, s, radius, live)?;
    root.Children()?.InsertAtTop(&backdrop)?;
    root.Children()?.InsertAtTop(&tint)?;
    // The glass's top light and grain over the tint, under the rim (Flat
    // v2: its grain).
    if !solid || theme.flat_grain(material) > 0.0 {
        // (Live: as large as the root, which follows "h".)
        match crate::glass::top_light(gfx, theme, material, if live.is_some() { None } else { Some(size) }) {
            Ok(Some(l)) => root.Children()?.InsertAtTop(&l)?,
            Ok(None) => {}
            Err(e) => crate::log!("glass: no top light ({e})"),
        }
    }
    // A theme's own top light (ui.highlight): a 1 DIP line inside the
    // rim, between the corners' curves. Graphite has none.
    if let Some(h) = theme.panel_highlight() {
        let line = c.CreateSpriteVisual()?;
        line.SetBrush(&c.CreateColorBrushWithColor(Color { A: h.a, R: h.r, G: h.g, B: h.b })?)?;
        line.SetSize(Vector2 { X: (width - radius * 2.0).max(0.0), Y: 1.0 })?;
        line.SetOffset(Vector3 { X: radius, Y: 1.0, Z: 0.0 })?;
        root.Children()?.InsertAtTop(&line)?;
    }
    root.Children()?.InsertAtTop(&border)?;
    Ok((root, border))
}

/// What a plate `width` x `height` DIP (corners `radius`, `s` px per DIP)
/// at elevation `e` casts: its soft shadows (real blurs shaped like it,
/// within SHADOW_M; none in high contrast or with "Shadows" off) and its
/// contact ring, in one container marked motion::TAG_SHADE (its opacity is
/// the light's strength). chrome_at lays it under the plate; a plate that
/// is its own window (the dock's preview) puts it in a click-through window
/// of its own under it.
pub(crate) fn shade_at(gfx: &Gfx, theme: &Theme, width: f32, height: f32, s: f32, radius: f32, e: Elevation) -> Result<ContainerVisual> {
    shade_with(gfx, theme, width, height, s, radius, e, None)
}

/// shade_at(), its height following `live`'s "h" (chrome_live).
#[allow(clippy::too_many_arguments)]
fn shade_with(gfx: &Gfx, theme: &Theme, width: f32, height: f32, s: f32, radius: f32, e: Elevation, live: Option<&CompositionPropertySet>) -> Result<ContainerVisual> {
    let c = &gfx.compositor;
    let (w_px, h_px) = ((width * s).ceil() as i32, (height * s).ceil() as i32);
    let radius = radius.clamp(0.0, (width.min(height) / 2.0).max(0.0));
    // What the plate casts, in one container under it.
    let shade = c.CreateContainerVisual()?;
    size_to(c, &shade, width, height, 0.0, live)?;
    shade.SetComment(&windows::core::HSTRING::from(crate::motion::TAG_SHADE))?;
    // Soft shadows shaped like the menu (the theme's; none in high contrast
    // or with the "Shadows" effect off).
    let layers = if crate::effects::items().shadows { theme.elevation(e) } else { Vec::new() };
    if !layers.is_empty() {
        let shape = gfx.draw(w_px as u32, h_px as u32, |dc, ox, oy| unsafe {
            let white = dc.CreateSolidColorBrush(&d2d((255, 255, 255, 255)), None)?;
            let rr = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: ox, top: oy, right: ox + w_px as f32, bottom: oy + h_px as f32 },
                radiusX: radius * s,
                radiusY: radius * s,
            };
            dc.FillRoundedRectangle(&rr, &white);
            Ok(())
        })?;
        let mask = c.CreateSurfaceBrushWithSurface(&shape)?;
        mask.SetStretch(CompositionStretch::Fill)?;
        // Live: a nine-grid of it, so its corners keep their shape at any
        // height.
        let mask: windows::UI::Composition::CompositionBrush = match live {
            Some(_) => {
                let nine = c.CreateNineGridBrush()?;
                nine.SetSource(&mask)?;
                nine.SetInsets((radius * s).ceil().max(1.0))?;
                nine.SetInsetScales(1.0 / s.max(0.25))?;
                nine.cast()?
            }
            None => mask.cast()?,
        };
        for (x, y, blur, col) in layers {
            let shadow_v = c.CreateSpriteVisual()?;
            size_to(c, &shadow_v, width, height, 0.0, live)?;
            let shadow = c.CreateDropShadow()?;
            // The tokens' blur is CSS's; a DropShadow's radius is 1.4 of
            // it for the same Gaussian (spike S5).
            shadow.SetBlurRadius(crate::theme::composition_blur(blur))?;
            shadow.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
            shadow.SetColor(Color { A: col.a, R: col.r, G: col.g, B: col.b })?;
            shadow.SetMask(&mask)?;
            shadow_v.SetShadow(&shadow)?;
            shade.Children()?.InsertAtTop(&shadow_v)?;
        }
    }
    // The contact shadow: a ring just outside the plate (not a fill: a
    // see-through plate must not be darkened by it).
    let contact = theme.contact();
    if contact.a > 0 {
        let k = crate::theme::CONTACT_W;
        let ring_geom = c.CreateRoundedRectangleGeometry()?;
        size_to(c, &ring_geom, width + k, height, k, live)?;
        ring_geom.SetOffset(Vector2 { X: 1.0 - k / 2.0, Y: 1.0 - k / 2.0 })?;
        ring_geom.SetCornerRadius(Vector2 { X: radius + k / 2.0, Y: radius + k / 2.0 })?;
        let ring_shape = c.CreateSpriteShapeWithGeometry(&ring_geom)?;
        ring_shape.SetStrokeBrush(&c.CreateColorBrushWithColor(Color { A: contact.a, R: contact.r, G: contact.g, B: contact.b })?)?;
        ring_shape.SetStrokeThickness(k)?;
        let ring = c.CreateShapeVisual()?;
        ring.Shapes()?.Append(&ring_shape)?;
        // (A shape visual shows nothing past its size: 1 DIP of room.)
        size_to(c, &ring, width + 2.0, height, 2.0, live)?;
        ring.SetOffset(Vector3 { X: -1.0, Y: -1.0, Z: 0.0 })?;
        shade.Children()?.InsertAtTop(&ring)?;
    }
    Ok(shade)
}

/// The rim of a plate `width` x `height` DIP with corners `radius` at
/// `s` px per DIP (design system 2.2): a stroke one device pixel thick
/// just inside its edge, white from the top through 38 % to the bottom,
/// marked motion::TAG_RIM (its opacity is the light's strength; all clear
/// on Flat and on glass A, 2026-11-12: no line). High
/// contrast: the 1 DIP outline in the text color instead. chrome_at's, and
/// what a plate that is its own window (the dock's preview) lays over its
/// tint.
pub(crate) fn rim_visual(gfx: &Gfx, theme: &Theme, m: crate::theme::Material, width: f32, height: f32, s: f32, radius: f32) -> Result<ShapeVisual> {
    rim_with(gfx, theme, m, width, height, s, radius, None)
}

/// rim_visual(), its height following `live`'s "h" (chrome_live).
#[allow(clippy::too_many_arguments)]
fn rim_with(gfx: &Gfx, theme: &Theme, m: crate::theme::Material, width: f32, height: f32, s: f32, radius: f32, live: Option<&CompositionPropertySet>) -> Result<ShapeVisual> {
    let c = &gfx.compositor;
    let border = c.CreateShapeVisual()?;
    let border_geom = c.CreateRoundedRectangleGeometry()?;
    let border_shape = match theme.rim_of(m) {
        Some(stops) => {
            // One device pixel (Flat v2: 1 DIP, Theme::rim_width), half
            // of it in from the edge.
            let px = theme.rim_width(m, s);
            size_to(c, &border_geom, width - px, height, -px, live)?;
            border_geom.SetOffset(Vector2 { X: px / 2.0, Y: px / 2.0 })?;
            border_geom.SetCornerRadius(Vector2 { X: (radius - px / 2.0).max(0.0), Y: (radius - px / 2.0).max(0.0) })?;
            let shape = c.CreateSpriteShapeWithGeometry(&border_geom)?;
            let light = c.CreateLinearGradientBrush()?;
            light.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
            light.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 })?;
            for (at, col) in crate::theme::RIM_STOPS.iter().zip(stops) {
                light.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(*at, Color { A: col.a, R: col.r, G: col.g, B: col.b })?)?;
            }
            shape.SetStrokeBrush(&light)?;
            shape.SetStrokeThickness(px)?;
            border.SetComment(&windows::core::HSTRING::from(crate::motion::TAG_RIM))?;
            shape
        }
        None => {
            // High contrast: the 1 DIP outline in the text color.
            size_to(c, &border_geom, width - 1.0, height, -1.0, live)?;
            border_geom.SetOffset(Vector2 { X: 0.5, Y: 0.5 })?;
            border_geom.SetCornerRadius(Vector2 { X: (radius - 0.5).max(0.0), Y: (radius - 0.5).max(0.0) })?;
            let shape = c.CreateSpriteShapeWithGeometry(&border_geom)?;
            let bc = theme.popup_border();
            shape.SetStrokeBrush(&c.CreateColorBrushWithColor(Color { A: bc.a, R: bc.r, G: bc.g, B: bc.b })?)?;
            shape.SetStrokeThickness(1.0)?;
            shape
        }
    };
    border.Shapes()?.Append(&border_shape)?;
    size_to(c, &border, width, height, 0.0, live)?;
    Ok(border)
}

/// Shows the menu and returns what happened. Must not be called while the
/// caller holds its own RefCell borrow (the nested loop dispatches to it).
pub fn track(req: Request) -> Outcome {
    track_beside(req,None)
}

/// Companion menus use their measured size, never a guessed width or cursor.
pub(crate) fn track_beside(req: Request, body: Option<crate::companion::convenience::MenuBody>) -> Outcome {
    let _companion_pause = crate::companion::pause_popup();
    if req.items.is_empty() || TRACK.with(|t| t.borrow().is_some()) {
        return Outcome::Cancelled;
    }
    unsafe {
        register_class();
        let tracker = Tracker {
            gfx: req.gfx.clone(),
            theme: req.theme.clone(),
            scale: req.scale,
            levels: Vec::new(),
            titles: req.titles,
            done: None,
            align: req.align,
            down_row: None,
            down_on: None,
            down_in: false,
            on_slide: req.on_slide,
            on_space: req.on_space,
            sliding: None,
            drag_ids: req.drag_ids,
            down_at: None,
            rtl: crate::i18n::rtl(),
            sub_left: crate::i18n::rtl(),
            sub_dir: crate::i18n::rtl(),
            companion_body: body,
            edge: EdgeScroll::default(),
            anchor: req.at,
            hangs: req.hangs,
            on_pick: req.on_pick,
            instant: swap_pending(),
            pending_sub: None,
            swap_in: false,
            sub_row: None,
            turn: 0,
        };
        TRACK.with(|t| *t.borrow_mut() = Some(tracker));
        let opened = with_track(|t| t.open_level(req.items, req.at, None)).unwrap_or(false);
        if !opened {
            TRACK.with(|t| t.borrow_mut().take());
            return Outcome::Cancelled;
        }
        // The menu it takes over from (another title's) fades out as this
        // one fades in, at full size (scrubbing); its submenus open as usual.
        let fresh = with_track(|t| t.levels.first().and_then(|l| l._root.cast::<windows::UI::Composition::Visual>().ok())).flatten();
        drop_swap(fresh.as_ref());
        with_track(|t| t.instant = false);
        let first = with_track(|t| t.levels[0].hwnd).unwrap_or_default();
        let _ = SetForegroundWindow(first);
        let _ = SetFocus(Some(first));
        SetCapture(first);
        // Without the foreground and the capture the menu could neither be
        // driven nor dismissed reliably: give up instead.
        if GetForegroundWindow() != first || GetCapture() != first {
            crate::log!("popup: could not take foreground/capture; not opening");
            let _ = ReleaseCapture();
            if let Some(mut t) = TRACK.with(|t| t.borrow_mut().take()) {
                for l in t.levels.drain(..).rev() {
                    let _ = DestroyWindow(l.hwnd);
                }
            }
            return Outcome::Lost;
        }

        let mut msg = MSG::default();
        loop {
            if with_track(|t| t.done.is_some()).unwrap_or(true) {
                break;
            }
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 || r.0 == -1 {
                // WM_QUIT: end the menu and let the outer loop see it.
                PostQuitMessage(msg.wParam.0 as i32);
                with_track(|t| t.done = Some(Outcome::Cancelled));
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = ReleaseCapture();
        let tracker = TRACK.with(|t| t.borrow_mut().take());
        let mut out = Outcome::Cancelled;
        if let Some(mut t) = tracker {
            out = t.done.unwrap_or(Outcome::Cancelled);
            let c = t.gfx.compositor.clone();
            let swapping = swaps(out);
            let mut kept = Vec::new();
            for l in t.levels.drain(..).rev() {
                crate::a11y::menu_event(l.hwnd, false);
                if swapping && crate::motion::click_through(l.hwnd) {
                    // Another title's menu comes next: this one stays
                    // until it is up, then the two cross (hold_swap).
                    let (hwnd, root) = (l.hwnd, l._root.cast::<windows::UI::Composition::Visual>().ok());
                    kept.push((hwnd, root, Box::new(l) as Box<dyn std::any::Any>));
                    continue;
                }
                // A quick fade, then the window goes (it lets clicks through
                // meanwhile).
                let (hwnd, root) = (l.hwnd, l._root.clone());
                crate::motion::retire(hwnd, &c, &root, Box::new(l));
            }
            if !kept.is_empty() {
                hold_swap(kept, Some(c));
            }
        }
        out
    }
}

/// Pure: the menu closed this way hands over to another title's menu of
/// the same bar (the pointer slid onto it, or the left / right arrow):
/// scrubbing (design system 7.4). Nothing closes or grows open: the two
/// cross over motion::SCRUB_MS at full size.
pub fn swaps(out: Outcome) -> bool {
    matches!(out, Outcome::Switch(_) | Outcome::Neighbor(_))
}

/// A menu that handed over (swaps): its windows, click-through, still on
/// screen; what keeps them drawn; and when it gives up waiting.
struct Swap {
    levels: Vec<(HWND, Option<windows::UI::Composition::Visual>, Box<dyn std::any::Any>)>,
    compositor: Option<windows::UI::Composition::Compositor>,
    later: Option<usize>,
}

thread_local! {
    static SWAP: RefCell<Option<Swap>> = const { RefCell::new(None) };
}

/// A menu handed over is still on screen.
fn swap_pending() -> bool {
    SWAP.with(|s| s.try_borrow().map(|s| s.is_some()).unwrap_or(false))
}

/// Keeps a handed-over menu's `levels` on screen until the next menu is
/// up (drop_swap), or SWAP_WAIT_MS at most (none came: it fades out).
fn hold_swap(levels: Vec<(HWND, Option<windows::UI::Composition::Visual>, Box<dyn std::any::Any>)>, compositor: Option<windows::UI::Composition::Compositor>) {
    fade_swap();
    let later = crate::motion::after(SWAP_WAIT_MS, Box::new(fade_swap));
    SWAP.with(|s| *s.borrow_mut() = Some(Swap { levels, compositor, later }));
}

/// The next menu (`next`: its first level's root) is up: it fades in
/// and the handed-over one out over motion::SCRUB_MS, neither changing
/// size, rim and shadows with their plates (motion::swap); then the old
/// windows go. Without something to fade (`next` None, no compositor) the
/// old one goes at once. False if no menu was handed over.
fn drop_swap(next: Option<&windows::UI::Composition::Visual>) -> bool {
    let Some(sw) = SWAP.with(|s| s.borrow_mut().take()) else { return false };
    if let Some(id) = sw.later {
        crate::motion::cancel(id);
    }
    let mut came = false;
    for (hwnd, root, keep) in sw.levels {
        let faded = match (&sw.compositor, &root, next) {
            // The new one comes in once, with the first old level's going.
            (Some(c), Some(root), Some(new)) if !came => crate::motion::swap(c, Some(root), new).ok(),
            (Some(c), Some(root), Some(_)) => crate::motion::fade_to(c, root, 0.0, crate::motion::len(crate::motion::SCRUB_MS), crate::motion::EASE_IN).ok(),
            _ => None,
        };
        match faded {
            Some(ms) => {
                came = true;
                crate::motion::retire_faded(hwnd, ms as u32 + 16, keep, None);
            }
            None => unsafe {
                let _ = DestroyWindow(hwnd);
                drop(keep);
            },
        }
    }
    true
}

/// No next menu came: the handed-over one fades out as menus close.
fn fade_swap() {
    let Some(sw) = SWAP.with(|s| s.borrow_mut().take()) else { return };
    if let Some(id) = sw.later {
        crate::motion::cancel(id);
    }
    for (hwnd, root, keep) in sw.levels {
        match (&sw.compositor, root) {
            (Some(c), Some(root)) => crate::motion::retire(hwnd, c, &root, keep),
            _ => unsafe {
                let _ = DestroyWindow(hwnd);
            },
        }
    }
}

unsafe fn register_class() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarPopup"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    // Fails harmlessly once registered.
    RegisterClassExW(&wc);
}

impl Tracker {
    /// Text layout for one string (px units).
    fn layout(&self, text: &str, family: &str, size: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
        self.gfx.text_layout(text, family, size * self.scale, DWRITE_FONT_WEIGHT_NORMAL).ok()
    }

    /// A calendar cell's text (px): day numbers in tabular figures (every
    /// column's digits line up), weekday names smaller (GRID_HEAD).
    fn cell_layout(&self, it: &Item, text: &str, family: &str, size: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
        if it.abbrev {
            self.layout(text, family, size.min(GRID_HEAD))
        } else {
            self.gfx.text_layout_tabular(text, family, size * self.scale, DWRITE_FONT_WEIGHT_NORMAL).ok()
        }
    }

    /// How tall the clock row's time and date are drawn (DIP), with their
    /// gap (measured_clock_text_h).
    fn clock_text_h(&self, family: &str) -> f32 {
        measured_clock_text_h(&self.gfx, family, self.scale)
    }

    /// Creates and shows a level; `parent_row` is the row it hangs off.
    unsafe fn open_level(&mut self, items: Vec<Item>, at: POINT, parent: Option<RECT>) -> bool {
        match self.build_level(items, at, parent, None) {
            Ok(l) => {
                let h = l.hwnd;
                let clock = l.clock.is_some();
                self.levels.push(l);
                if clock {
                    let li = self.levels.len() - 1;
                    self.draw_clock(li);
                    self.arm_clock(li);
                }
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                self.publish_a11y();
                crate::a11y::menu_event(h, true);
                true
            }
            Err(e) => {
                crate::log!("popup: cannot open menu: {e}");
                false
            }
        }
    }

    /// `reuse`: the window and target of the level it replaces (Tracker::
    /// replace_top): moved and sized instead of made, and not faded in.
    unsafe fn build_level(&mut self, items: Vec<Item>, at: POINT, parent: Option<RECT>, reuse: Option<(HWND, DesktopWindowTarget)>) -> Result<Level> {
        let src = items.clone();
        let s = self.scale;
        let family = self.theme.font.family.clone();
        let mon = level_monitor(at, parent);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        // Text rows wrap: at most 400 DIP, and never wider than the work
        // area allows (a long warning stays on screen, whole); beside a
        // side dock, than the room past its anchor (level_span). A notice
        // (a title over its reason) is NOTICE_W wide: its text wraps to
        // that.
        let span_px = if parent.is_none() {
            self.companion_body.map_or_else(||level_span(self.align,self.hangs,at,mi.rcWork),|body|body.room(mi.rcWork))
        } else { mi.rcWork.right - mi.rcWork.left };
        let work_w = span_px as f32 / s;
        let left = gutter(&items);
        let notice = items.iter().any(|i| i.title);
        let note_w = (work_w - SHADOW_M * 2.0 - left - RIGHT_PAD - 16.0).clamp(120.0, 400.0);
        let note_w = if notice { note_w.min(NOTICE_W - left - RIGHT_PAD) } else { note_w };
        let weight_of = |it: &Item| if it.title { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        // A paragraph is one row of several lines (para_h), not a row a
        // line: its lines sit a line height apart.
        let mut items = items;
        for i in 0..items.len() {
            if !items[i].wrap {
                continue;
            }
            let weight = weight_of(&items[i]);
            let measure = |t: &str| self.gfx.text_layout(t, &family, FONT_SIZE * s, weight).map(|l| l.1 / s).unwrap_or(0.0);
            let mut lines = wrap_text(&items[i].label, note_w, &measure);
            if after_title(&items, i) && lines.len() > NOTICE_LINES {
                // A notice's reason: what is left goes on its last line,
                // cut with "…" (the paint).
                lines.truncate(NOTICE_LINES - 1);
                let rest = rest_after(&items[i].label, &lines);
                lines.push(rest);
            }
            items[i].lines = lines;
        }
        // Taller than the screen (than the room under the bar, for a menu
        // hanging from it): compact rows, folded groups, scrolling.
        let first = if parent.is_none() { self.align } else { Align::RightOf };
        let room_px = level_room(first, self.hangs, at, mi.rcWork);
        let work_h = room_px as f32 / s;
        let clock_h = if items.iter().any(|i| i.clock) { self.clock_text_h(&family) } else { 0.0 };
        let (items, density) = fit(items, work_h, clock_h);
        // High contrast keeps lines (the picked day, cards).
        let hc_line = self.theme.high_contrast;
        let menu_r = self.theme.radii().menu.clamp(0.0, 16.0);
        // Rows' fills sit ROW_INSET inside the menu: concentric corners
        // (12 - 4 = 8).
        let row_r = crate::theme::concentric(menu_r, ROW_INSET);

        // Measure.
        // Agenda rows draw their own texts, cut to the menu's width below
        // (one without a time, a note, is measured like a label).
        // A header's label is footnote; every other, body. Paragraphs are
        // laid out line by line (`paras`).
        let style = |it: &Item| if it.head { (HEAD_SIZE, DWRITE_FONT_WEIGHT_NORMAL) } else { (FONT_SIZE, DWRITE_FONT_WEIGHT_NORMAL) };
        let mut labels: Vec<Option<(IDWriteTextLayout, f32, f32)>> = items
            .iter()
            .map(|it| {
                if it.separator || it.wrap || it.busy || it.event.as_ref().is_some_and(|e| !e.time.is_empty()) {
                    return None;
                }
                let (size, weight) = style(it);
                self.gfx.text_layout(&it.label, &family, size * s, weight).ok()
            })
            .collect();
        let paras: Vec<Vec<(IDWriteTextLayout, f32, f32)>> = items
            .iter()
            .map(|it| it.lines.iter().filter(|_| it.wrap).filter_map(|line| self.gfx.text_layout_line(line, &family, FONT_SIZE * s, weight_of(it), note_w * s).ok()).collect())
            .collect();
        let after: Vec<bool> = (0..items.len()).map(|i| after_title(&items, i)).collect();
        // A failure's button: its label, drawn once (the button is a
        // visual of its own).
        let retry_labels: Vec<Option<(IDWriteTextLayout, f32, f32)>> =
            items.iter().map(|it| it.retry.as_ref().and_then(|t| self.gfx.text_layout(t, &family, FONT_SIZE * s, DWRITE_FONT_WEIGHT_NORMAL).ok())).collect();
        // Shortcut hints: secondary text, footnote size, on the label's
        // baseline (see the paint).
        let mut shortcuts: Vec<Option<(IDWriteTextLayout, f32, f32)>> = items
            .iter()
            .map(|it| if it.shortcut.is_empty() { None } else { self.layout(&it.shortcut, &family, SHORT_SIZE) })
            .collect();
        // Labels start at the gutter; an agenda note (an event row without
        // a time) sits on its card after the glyph: wide enough for it whole
        // (event_note_w), never cut to "…".
        let (mut label_w, mut notes_w) = (0.0f32, 0.0f32);
        for (i, (l, it)) in labels.iter().zip(&items).enumerate() {
            if it.wrap {
                label_w = paras[i].iter().map(|p| p.1 / s).fold(label_w, f32::max);
                continue;
            }
            let Some(w) = l.as_ref().map(|l| l.1 / s) else { continue };
            if it.event.is_some() {
                notes_w = notes_w.max(event_note_w(w));
            } else if it.nav.is_some() {
                // A month header's buttons after its title.
                label_w = label_w.max(w + 2.0 * GRID_COL);
            } else if let Some((_, bw, _)) = &retry_labels[i] {
                // The warning glyph, the text, then the button.
                notes_w = notes_w.max(FAIL_TEXT_X + w + space::S4 + bw / s + 2.0 * parts::BUTTON_PAD + RIGHT_PAD);
            } else {
                label_w = label_w.max(w);
            }
        }
        let mut cell_layouts: Vec<Vec<Option<(IDWriteTextLayout, f32, f32)>>> =
            items.iter().map(|it| it.cells.iter().map(|c| self.cell_layout(it, c, &family, GRID_TEXT)).collect()).collect();
        let short_w = shortcuts.iter().zip(&items).map(|(l, it)| end_w(it, l.as_ref().map_or(0.0, |l| l.1 / s))).fold(0.0, f32::max);
        let has_sub = items.iter().any(|i| !i.sub.is_empty());
        // Grids (calendar): columns of at least 36, wider for a long
        // weekday name (grid_w).
        let cols = items.iter().map(|i| i.cells.len()).max().unwrap_or(0);
        let cell_w = cell_layouts.iter().flatten().flatten().map(|l| l.1 / s).fold(0.0, f32::max);
        let grid_w = grid_w(cols, cell_w);
        // The clock row: room for its widest time (clock_samples) and the
        // date, measured as they are drawn.
        let clock_w = if items.iter().any(|i| i.clock) {
            let now = windows::Win32::System::SystemInformation::GetLocalTime();
            let tab = |t: &str| clock_time_layout(&self.gfx, t, &family, s).map(|l| l.1 / s).unwrap_or(0.0);
            let (time, date) = clock_lines(&now);
            let time_w = clock_samples(&now).iter().map(|st| tab(&clock_lines(st).0)).fold(tab(&time), f32::max);
            let date_w = self.layout(&date, &family, CLOCK_DATE).map(|l| l.1 / s).unwrap_or(0.0);
            time_w.max(date_w) + 2.0 * CONTENT_X
        } else {
            0.0
        };
        let width = (left + label_w + if short_w > 0.0 { SHORTCUT_GAP + short_w } else { 0.0 } + if has_sub { ARROW_W } else { 0.0 } + RIGHT_PAD)
            .max(if notice { NOTICE_W } else { MIN_W })
            .max(grid_w)
            .max(clock_w)
            .max(notes_w)
            .ceil();
        // Never wider than the work area (a narrow or high-DPI screen): what
        // would run past it fits itself in instead, no column dropped.
        // Weekday names get shorter ("Mon" -> "M"), day numbers smaller
        // (grid_texts); labels are cut with "…", then shortcut hints
        // (row_columns); agenda rows already are (event_texts, for this
        // width).
        let want = width;
        // Reserve outward room for companion submenus. Ordinary menus retain
        // their usual flip-to-fit behavior; a companion submenu never flips
        // back across the body.
        let beside=self.companion_body.filter(|_|parent.is_none()).map(|body|
            crate::companion::convenience::menu_anchor(body,windows::Win32::Foundation::SIZE{cx:(want*s).ceil() as i32,cy:0},mi.rcWork));
        let work_px=if let Some((anchor,align))=beside {
            self.align=align;self.sub_left=align==Align::BottomRight;
            crate::companion::convenience::menu_side(self.sub_left);
            beside_width(outward_room(anchor.x,self.sub_left,mi.rcWork),sub_gap(s),has_sub)
        }else if let Some(parent)=parent.filter(|_|self.companion_body.is_some()) {
            outward_room(if self.sub_dir{parent.left-sub_gap(s)}else{parent.right+sub_gap(s)},self.sub_dir,mi.rcWork)
        }else{span_px};
        // Rows whose trailing glyphs had no room (end_plan).
        let mut trail_off = vec![false; items.len()];
        let width = level_width(want, work_px, s);
        if width < want {
            let cell_text_w = |t: &str, size: f32| self.layout(t, &family, size).map(|l| l.1 / s).unwrap_or(0.0);
            let fitted = grid_texts(&items, width, cols, &cell_text_w);
            for ((it, row), texts) in items.iter().zip(cell_layouts.iter_mut()).zip(fitted) {
                for ((text, l), (t, size)) in it.cells.iter().zip(row.iter_mut()).zip(texts) {
                    if t != *text || size != GRID_TEXT {
                        *l = self.cell_layout(it, &t, &family, size);
                    }
                }
            }
            let w = |l: &Option<(IDWriteTextLayout, f32, f32)>| l.as_ref().map_or(0.0, |l| l.1 / s);
            for (ri, ((it, l), sc)) in items.iter().zip(labels.iter_mut()).zip(shortcuts.iter_mut()).enumerate() {
                if it.event.is_some() || !it.cells.is_empty() || it.clock || it.slider.is_some() || it.wrap || it.retry.is_some() {
                    continue;
                }
                let (label_room, end_room) = row_columns(width, left, w(l), end_w(it, w(sc)), !it.sub.is_empty());
                // The end column as planned for its room: glyphs that do
                // not fit go whole (never drawn over the label: design B1
                // review P2 #2), the shortcut takes what is left.
                let plan = end_plan(end_room, trail_w(it), w(sc));
                if !plan.trail {
                    trail_off[ri] = true;
                }
                if w(l) > label_room {
                    let (size, weight) = style(it);
                    *l = self.gfx.text_layout_line(&it.label, &family, size * s, weight, label_room * s).ok();
                }
                if plan.short <= 0.0 {
                    *sc = None;
                } else if w(sc) > plan.short {
                    *sc = self.gfx.text_layout_line(&it.shortcut, &family, SHORT_SIZE * s, DWRITE_FONT_WEIGHT_NORMAL, plan.short * s).ok();
                }
            }
        }
        // A month header's title keeps clear of its buttons, and a
        // failure's text of its button (cut with "…").
        for (i, (it, l)) in items.iter().zip(labels.iter_mut()).enumerate() {
            let room = if it.nav.is_some() {
                let (x, w) = nav_span(NavPart::Title, width, false);
                Some(x + w - left - 4.0)
            } else {
                retry_labels[i].as_ref().map(|(_, bw, _)| width - RIGHT_PAD - (bw / s + 2.0 * parts::BUTTON_PAD) - space::S4 - FAIL_TEXT_X)
            };
            if let (Some(room), Some((_, lw, _))) = (room, l.as_ref()) {
                if *lw / s > room {
                    *l = self.gfx.text_layout_line(&it.label, &family, FONT_SIZE * s, DWRITE_FONT_WEIGHT_NORMAL, room.max(1.0) * s).ok();
                }
            }
        }
        let events = self.event_texts(&items, &family, width);
        let (rows, content_h) = row_layout(&items, density, clock_h);
        let view = View::new(content_h, work_h);
        let height = view.height;
        // Whole pixels, never past the room: the height not a pixel into
        // the bar's gap (level_px), the width inside the work area.
        let (w_px, h_px) = (level_px(width, s, work_px), level_px(height, s, room_px));
        // All rows, px (drawn in tiles near what shows; the scroll moves them).
        let c_px = (content_h * s).ceil() as i32;

        // Place on screen, inside the monitor's work area.
        let mon = level_monitor(at, parent);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        let at=beside.map_or(at,|(anchor,_)|anchor);
        let side = if parent.is_some() { self.sub_dir } else { self.sub_left };
        let (x, y) = level_origin(self.align, parent, side, at, w_px, h_px, wa, s);
        let rect = RECT { left: x, top: y, right: x + w_px, bottom: y + h_px };

        // The window is the menu plus a margin for its shadow; `rect` (hit
        // testing) is the menu itself.
        let m_px = (SHADOW_M * s).ceil() as i32;
        let fresh = reuse.is_none();
        let (hwnd, old_target) = match reuse {
            Some((h, t)) => (h, Some(t)),
            None => (
                CreateWindowExW(
                    WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                    w!("NopBarPopup"),
                    w!("Menu"),
                    WS_POPUP,
                    x - m_px,
                    y - m_px,
                    w_px + m_px * 2,
                    h_px + m_px * 2,
                    None,
                    None,
                    Some(GetModuleHandleW(None)?.into()),
                    None,
                )?,
                None,
            ),
        };
        let built = (|| -> Result<Level> {
            let c = self.gfx.compositor.clone();
            let target = match old_target {
                Some(t) => t,
                None => {
                    let enable = BOOL(1);
                    let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &enable as *const _ as *const _, 4);
                    let interop: ICompositorDesktopInterop = c.cast()?;
                    interop.CreateDesktopWindowTarget(hwnd, true)?
                }
            };
            // outer (scaled, inset by the margin) -> shadow + root (clipped).
            let outer = c.CreateContainerVisual()?;
            outer.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
            outer.SetOffset(Vector3 { X: m_px as f32, Y: m_px as f32, Z: 0.0 })?;
            // A level drawn again in place keeps showing the old rows until
            // the new ones are complete (set at the end).
            if fresh {
                target.SetRoot(&outer)?;
            }
            let size = Vector2 { X: width, Y: height };
            let (root, border) = chrome_at(&self.gfx, &self.theme, &outer, width, height, s, menu_r, Elevation::E2)?;

            // The rows' text, drawn tile by tile (draw_tiles) as they show.
            let rtl = self.rtl;
            // Chevron right, or left when submenus open to the left.
            let glyph_arrow = if self.sub_left { Glyph::ChevronLeft } else { Glyph::ChevronRight };
            // Marks (a check, an arrow, a month's ‹ ›) are 12, a row's
            // glyph and icon 16 (the icon grid, 5).
            let (mark_px, glyph_px, arrow_px) = (size::ICON_XS * s, size::ICON_S * s, ARROW * s);
            let rows_px: Vec<(f32, f32)> = rows.iter().map(|(y, h)| (y * s, h * s)).collect();
            let paint_items = Rc::new(RefCell::new(items.clone()));
            let items_p = paint_items.clone();
            let paint_labels = Rc::new(RefCell::new(labels));
            let labels_p = paint_labels.clone();
            let inks = Rc::new(Cell::new(Inks::of(&self.theme)));
            let inks_p = inks.clone();
            let paint: Box<Paint> = Box::new(move |dc, ox, oy, y0, y1| {
                let items = items_p.borrow();
                let labels = labels_p.borrow();
                let k = inks_p.get();
                let (on, off, hint) = (k.on, k.off, k.hint);
                let b_on = dc.CreateSolidColorBrush(&k.on, None)?;
                let b_dim = dc.CreateSolidColorBrush(&k.dim, None)?;
                let b_off = dc.CreateSolidColorBrush(&k.off, None)?;
                let b_hint = dc.CreateSolidColorBrush(&k.hint, None)?;
                let b_line = dc.CreateSolidColorBrush(&k.line, None)?;
                let b_mark = dc.CreateSolidColorBrush(&k.mark, None)?;
                let b_onmark = dc.CreateSolidColorBrush(&k.on_mark, None)?;
                let b_pick = dc.CreateSolidColorBrush(&k.pick, None)?;
                let b_card = dc.CreateSolidColorBrush(&k.card, None)?;
                let opts = windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT;
                // Right to left, everything is laid out as usual and then
                // reflected inside the menu (text itself is not flipped).
                let mx = |x: f32, w: f32| crate::i18n::mirror::span(rtl, x, w, ox, ox + w_px as f32);
                for (i, it) in items.iter().enumerate() {
                    let (ry, rh) = rows_px[i];
                    if ry + rh < y0 || ry > y1 {
                        continue;
                    }
                    if it.separator {
                        // A slot with a hairline across its middle, from
                        // the content line to its mirror: one device pixel
                        // (high contrast: 1 DIP in the text color).
                        let (top, t) = hairline_px(oy + ry + rh / 2.0 - 0.5, if hc_line { 1.0 } else { 0.0 }, s);
                        let inset = CONTENT_X * s;
                        let r = D2D_RECT_F { left: ox + inset, top, right: ox + w_px as f32 - inset, bottom: top + t };
                        dc.FillRectangle(&r, &b_line);
                        continue;
                    }
                    if !it.cells.is_empty() {
                        // Equal columns across the row, text centered.
                        let n = it.cells.len().max(1) as f32;
                        let (x0, cw) = (ox + 12.0 * s, (w_px as f32 - 24.0 * s) / n);
                        // Right to left, the first column is the rightmost.
                        let cx = |k: usize| mx(x0 + cw * (k as f32 + 0.5), 0.0);
                        // Today: the "on" circle; the day picked: the
                        // selected fill. No rings (borderless), but high
                        // contrast outlines the picked day. Sized to the
                        // column (grid_mark), inside its own cell, and all
                        // drawn before any number.
                        let cy = oy + ry + rh / 2.0;
                        let (side, _) = grid_mark(cw / s);
                        let half = side.min(rh / s) * s / 2.0;
                        for k in it.mark.into_iter().chain(it.ring.filter(|&r| Some(r) != it.mark)).filter(|&k| k < it.cells.len()) {
                            let c = cx(k);
                            let day = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F { left: c - half, top: cy - half, right: c + half, bottom: cy + half },
                                radiusX: half,
                                radiusY: half,
                            };
                            if it.mark == Some(k) {
                                dc.FillRoundedRectangle(&day, &b_mark);
                            } else if hc_line {
                                dc.DrawRoundedRectangle(&day, &b_on, s.max(1.0), None);
                            } else {
                                dc.FillRoundedRectangle(&day, &b_pick);
                            }
                        }
                        for k in 0..it.cells.len() {
                            let cx = cx(k);
                            if let Some((l, tw, th)) = cell_layouts[i].get(k).and_then(|x| x.as_ref()) {
                                let b = if it.mark == Some(k) {
                                    &b_onmark
                                } else if it.checked || it.abbrev {
                                    &b_hint
                                } else {
                                    &b_on
                                };
                                crate::gfx::draw_text(dc, Vector2 { X: cx - tw / 2.0, Y: oy + ry + (rh - th) / 2.0 }, l, b, opts);
                            }
                        }
                        continue;
                    }
                    if let Some(ev) = &events[i] {
                        let ink = EventInk { on: &b_on, dim: &b_dim, hint: &b_hint, card: &b_card, on_c: on, dim_c: k.dim, outline: hc_line, card_r: row_r };
                        draw_event_row(dc, ev, &ink, (ox, oy + ry, rh), (w_px as f32, s), rtl)?;
                        continue;
                    }
                    // Sliders, the clock and the progress ring are visuals
                    // of their own.
                    if it.slider.is_some() || it.clock || it.busy {
                        continue;
                    }
                    if it.wrap {
                        // A paragraph: its lines a line height apart, each
                        // in the middle of its line box. A notice's title
                        // in the text color, its reason and any other note
                        // in the secondary one (they are read, not dimmed).
                        let n = paras[i].len();
                        let top = para_top(n, it.title, after[i], rh / s) * s;
                        let brush = if it.title { &b_on } else { &b_hint };
                        for (j, (l, lw, lh)) in paras[i].iter().enumerate() {
                            let y = (oy + ry + top + j as f32 * PARA_LINE * s + (PARA_LINE * s - lh) / 2.0).round();
                            crate::gfx::draw_text(dc, Vector2 { X: mx(ox + left * s, *lw), Y: y }, l, brush, opts);
                        }
                        continue;
                    }
                    if it.retry.is_some() {
                        // A failure: the warning glyph on the content line
                        // (secondary), the text after it (primary). No
                        // color: the glyph's shape says it (11.4).
                        let x = mx(ox + CONTENT_X * s, glyph_px) + glyph_px / 2.0;
                        crate::gfx::draw_glyph(dc, Glyph::Warning, x, oy + ry + rh / 2.0, glyph_px, hint)?;
                        if let Some((l, lw, lh)) = &labels[i] {
                            let top = (oy + ry + (rh - lh) / 2.0).round();
                            crate::gfx::draw_text(dc, Vector2 { X: mx(ox + FAIL_TEXT_X * s, *lw), Y: top }, l, &b_on, opts);
                        }
                        continue;
                    }
                    // A header and an empty list's line: tertiary. A line
                    // that only says something (Item::info): the secondary
                    // color, it carries information. A command that cannot
                    // be used now: the disabled color.
                    let brush = if it.head || it.quiet {
                        &b_dim
                    } else if it.enabled || it.nav.is_some() {
                        &b_on
                    } else if it.info {
                        &b_hint
                    } else {
                        &b_off
                    };
                    if let Some((px, pixels)) = &it.icon {
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
                            let side = glyph_px;
                            let x = mx(ox + CHECK_CENTER * s - side / 2.0, side);
                            let y = oy + ry + (rh - side) / 2.0;
                            let dest = D2D_RECT_F { left: x, top: y, right: x + side, bottom: y + side };
                            dc.DrawBitmap(&bmp, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                        }
                    }
                    // The label's line box centered on the row (whole px);
                    // a shortcut stands on the same baseline.
                    let mut base = None;
                    if let Some((l, lw, lh)) = &labels[i] {
                        let top = (oy + ry + (rh - lh) / 2.0).round();
                        // A header and an empty list's line start at the
                        // content line whatever the menu's gutter (3.3).
                        let x = if it.head || it.quiet { CONTENT_X } else { left };
                        crate::gfx::draw_text(dc, Vector2 { X: mx(ox + x * s, *lw), Y: top }, l, brush, opts);
                        base = Some(top + crate::gfx::first_baseline(l, *lh));
                    }
                    // A month header's ‹ ›, centered on their grid columns
                    // (mirrored right to left, where next points left).
                    if let Some(nav) = &it.nav {
                        for (p, g) in [(NavPart::Prev, Glyph::ChevronLeft), (NavPart::Next, Glyph::ChevronRight)] {
                            let (x, w) = nav_span(p, w_px as f32 / s, rtl);
                            let g = match (rtl, g) {
                                (true, Glyph::ChevronLeft) => Glyph::ChevronRight,
                                (true, _) => Glyph::ChevronLeft,
                                (false, g) => g,
                            };
                            crate::gfx::draw_glyph(dc, g, ox + (x + w / 2.0) * s, oy + ry + rh / 2.0, mark_px, if nav.id(p) != 0 { hint } else { off })?;
                        }
                        continue;
                    }
                    let ink = if it.enabled { on } else { off };
                    if it.checked {
                        let x = mx(ox + CHECK_CENTER * s - mark_px / 2.0, mark_px) + mark_px / 2.0;
                        crate::gfx::draw_glyph(dc, Glyph::Check, x, oy + ry + rh / 2.0, mark_px, ink)?;
                    } else if let Some(g) = it.glyph {
                        let x = mx(ox + CHECK_CENTER * s - glyph_px / 2.0, glyph_px) + glyph_px / 2.0;
                        crate::gfx::draw_glyph(dc, g, x, oy + ry + rh / 2.0, glyph_px, ink)?;
                    }
                    let right = ox + w_px as f32 - RIGHT_PAD * s;
                    if !it.sub.is_empty() {
                        let x = mx(right - arrow_px, arrow_px) + arrow_px / 2.0;
                        crate::gfx::draw_glyph(dc, glyph_arrow, x, oy + ry + rh / 2.0, arrow_px, ink)?;
                    } else {
                        let mut end = right;
                        if let Some((l, sw, sh)) = &shortcuts[i] {
                            let b = if it.enabled { &b_hint } else { &b_off };
                            let top = match base {
                                Some(b) => (b - crate::gfx::first_baseline(l, *sh)).round(),
                                None => (oy + ry + (rh - sh) / 2.0).round(),
                            };
                            crate::gfx::draw_text(dc, Vector2 { X: mx(right - sw, *sw), Y: top }, l, b, opts);
                            end -= sw + TRAIL_GAP * 2.0 * s;
                        }
                        // Trailing glyphs, the last one nearest the edge
                        // (none when the column had no room for them).
                        let side = TRAIL_SIDE * s;
                        for g in it.trail.iter().rev().filter(|_| !trail_off[i]) {
                            let x = mx(end - side, side) + side / 2.0;
                            crate::gfx::draw_glyph(dc, *g, x, oy + ry + rh / 2.0, side, if it.enabled { hint } else { off })?;
                            end -= side + TRAIL_GAP * s;
                        }
                    }
                }
                Ok(())
            });
            let text_v = c.CreateContainerVisual()?;
            text_v.SetSize(Vector2 { X: width, Y: content_h })?;
            let mut tiles: Vec<Option<SpriteVisual>> = vec![None; tile_count(c_px)];
            for k in tiles_needed(&view, s, c_px) {
                let tile = draw_tile(&self.gfx, &paint, w_px, c_px, k, s)?;
                text_v.Children()?.InsertAtTop(&tile)?;
                tiles[k] = Some(tile);
            }

            // The rows sit in a scroller (moved by the scroll) inside a
            // viewport the size of the menu.
            let viewport = c.CreateContainerVisual()?;
            viewport.SetSize(size)?;
            let scroller = c.CreateContainerVisual()?;
            scroller.SetSize(Vector2 { X: width, Y: content_h })?;
            scroller.SetOffset(Vector3 { X: 0.0, Y: view.shift(), Z: 0.0 })?;
            viewport.Children()?.InsertAtTop(&scroller)?;
            // The fills, under the rows' text: the pointer's (row hover;
            // high contrast: no fill, a Highlight outline 2 DIP inside)
            // and the instant one. High contrast's instant fill is the
            // target painted again on Highlight: over the text.
            let hc = self.theme.high_contrast;
            let mut hovers = Vec::with_capacity(HOVER_POOL);
            for _ in 0..HOVER_POOL {
                let g = parts::Glow::new(&c, self.theme.fill_of(Role::Row, State::Hover), self.theme.outline_of(Role::Row, State::Hover))?;
                scroller.Children()?.InsertAtTop(&g.v)?;
                hovers.push(g);
            }
            let mark = parts::Glow::new(&c, self.theme.fill_of(Role::Row, State::Pressed), None)?;
            if !hc {
                scroller.Children()?.InsertAtTop(&mark.v)?;
            }
            scroller.Children()?.InsertAtTop(&text_v)?;
            if hc {
                scroller.Children()?.InsertAtTop(&mark.v)?;
            }
            // A menu taller than its room: its rows fade out at the edges
            // they are cut at (2.5). Should that not be had, they are cut
            // by the plate as they are.
            let edges = if view.scrolls() {
                match parts::Edges::new(&self.gfx, &self.theme, &viewport, (width, height, view.band), s, view.bands()) {
                    Ok(e) => {
                        root.Children()?.InsertBelow(&e.stage, &border)?;
                        Some(e)
                    }
                    Err(e) => {
                        crate::log!("popup: no scroll edges: {e}");
                        if let Ok(p) = viewport.Parent() {
                            let _ = p.Children().and_then(|ch| ch.Remove(&viewport));
                        }
                        viewport.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })?;
                        root.Children()?.InsertBelow(&viewport, &border)?;
                        None
                    }
                }
            } else {
                root.Children()?.InsertBelow(&viewport, &border)?;
                None
            };
            // Sliders: a well, its fill and a knob as visuals (dragging
            // moves them).
            let mut sliders = Vec::new();
            for (i, it) in items.iter().enumerate() {
                let Some(v) = it.slider else { continue };
                let (ry, rh) = rows[i];
                let (x0, tw) = slider_track(width);
                let cy = ry + rh / 2.0;
                // A capsule well (the track fill and its upper wall's
                // shade), the "on" fill inside its clip, and a knob that
                // is raised: its fill, the contact shadow, E1.
                let bar = c.CreateContainerVisual()?;
                bar.SetOffset(Vector3 { X: x0, Y: cy - SLIDER_TRACK_H / 2.0, Z: 0.0 })?;
                bar.SetSize(Vector2 { X: tw, Y: SLIDER_TRACK_H })?;
                let bar_clip = c.CreateRoundedRectangleGeometry()?;
                bar_clip.SetSize(Vector2 { X: tw, Y: SLIDER_TRACK_H })?;
                let r = crate::theme::capsule(SLIDER_TRACK_H);
                bar_clip.SetCornerRadius(Vector2 { X: r, Y: r })?;
                bar.SetClip(&c.CreateGeometricClipWithGeometry(&bar_clip)?)?;
                let track = parts::slider_well(&self.gfx, &self.theme, tw, s)?;
                let acc = self.theme.fill();
                let fill = c.CreateSpriteVisual()?;
                fill.SetBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: acc.r, G: acc.g, B: acc.b })?)?;
                let (fx, fw) = slider_fill(self.rtl, v, x0, tw);
                fill.SetOffset(Vector3 { X: fx - x0, Y: 0.0, Z: 0.0 })?;
                fill.SetSize(Vector2 { X: fw, Y: SLIDER_TRACK_H })?;
                bar.Children()?.InsertAtTop(&track)?;
                bar.Children()?.InsertAtTop(&fill)?;
                let knob = parts::slider_knob(&self.gfx, &self.theme, s)?;
                let half = slider_knob_box() / 2.0;
                knob.SetOffset(Vector3 { X: crate::i18n::mirror::slider_x(self.rtl, v, x0, x0 + tw) - half, Y: cy - half, Z: 0.0 })?;
                scroller.Children()?.InsertAtTop(&bar)?;
                scroller.Children()?.InsertAtTop(&knob)?;
                sliders.push((i, fill, knob, x0, tw));
            }
            // The progress ring of a row still waiting, on the content
            // line; a failure's button at the row's end.
            let mut buttons = Vec::new();
            for (i, it) in items.iter().enumerate() {
                let (ry, rh) = rows[i];
                if it.busy {
                    let ring = parts::spinner(&self.gfx, &self.theme, s)?;
                    let x = crate::i18n::mirror::span(rtl, CONTENT_X, size::SPINNER, 0.0, width);
                    ring.SetOffset(Vector3 { X: x, Y: ry + (rh - size::SPINNER) / 2.0, Z: 0.0 })?;
                    scroller.Children()?.InsertAtTop(&ring)?;
                }
                if let Some((l, lw, lh)) = &retry_labels[i] {
                    // Its text in each color its states use (text_of).
                    let label = |ink: crate::theme::Rgba| {
                        let ink = parts::ink(ink);
                        self.gfx.draw(lw.max(1.0) as u32, lh.max(1.0) as u32, |dc, ox, oy| {
                            let b = dc.CreateSolidColorBrush(&ink, None)?;
                            crate::gfx::draw_text(dc, Vector2 { X: ox, Y: oy }, l, &b, windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                            Ok(())
                        })
                    };
                    let b = parts::Button::new(&self.gfx, &self.theme, &label, (lw / s, lh / s))?;
                    let (bw, bh) = b.size();
                    let at = (crate::i18n::mirror::span(rtl, width - RIGHT_PAD - bw, bw, 0.0, width), ry + (rh - bh) / 2.0);
                    b.v.SetOffset(Vector3 { X: at.0, Y: at.1, Z: 0.0 })?;
                    scroller.Children()?.InsertAtTop(&b.v)?;
                    buttons.push((i, b, at));
                }
            }
            // The live clock row: a visual of its own, drawn again on every
            // second (Tracker::draw_clock).
            let clock = match items.iter().position(|it| it.clock) {
                Some(i) => {
                    let v = c.CreateSpriteVisual()?;
                    v.SetOffset(Vector3 { X: 0.0, Y: rows[i].0, Z: 0.0 })?;
                    v.SetSize(Vector2 { X: width, Y: rows[i].1 })?;
                    scroller.Children()?.InsertAtTop(&v)?;
                    Some((i, v))
                }
                None => None,
            };
            // It grows from where it was opened and fades in (crate::motion;
            // a short fade only with reduced motion); a level drawn again in
            // place just shows its new rows at its new size.
            let crosses = if parent.is_some() { self.swap_in } else { self.instant };
            if fresh && !crosses {
                // From what opened it: the bar cell, or for a submenu the
                // row it hangs off (the point of its edge nearest that:
                // motion::origin), else from where it was opened at.
                let cell = if parent.is_some() { self.sub_row } else { self.titles.anchor };
                let anchor = open_origin(cell, at, (x, y), (width, height), s);
                crate::motion::open(&c, &outer, (width, height), anchor, crate::motion::Kind::Menu, s)?;
            } else if fresh {
                // Another title's menu, or a sibling submenu, was open:
                // this one does not grow, it crosses with that one (track:
                // drop_swap; open_sub).
            } else {
                // Everything is built: the new rows go up in one swap.
                target.SetRoot(&outer)?;
                let _ = SetWindowPos(hwnd, None, x - m_px, y - m_px, w_px + m_px * 2, h_px + m_px * 2, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            Ok(Level {
                hwnd,
                _target: target,
                _root: outer,
                hovers,
                hover_i: 0,
                mark,
                shown: parts::Glows::default(),
                pointer: None,
                by_key: false,
                pressed: None,
                open_row: None,
                row_r,
                inks,
                src,
                items,
                rows,
                density,
                view,
                scroller,
                edges,
                paint,
                paint_items,
                paint_labels,
                text: text_v,
                tiles,
                w_px,
                left,
                content_px: c_px,
                width,
                rect,
                hover: None,
                nav_focus: None,
                sliders,
                buttons,
                clock,
                fade: Vec::new(),
            })
        })();
        if built.is_err() && fresh {
            let _ = DestroyWindow(hwnd);
        }
        built
    }

    fn update_command(&mut self, row: Item) -> bool {
        let Some(l) = self.levels.first_mut() else { return false };
        let mut updated = l.items.clone();
        let Some(at) = command_update(&mut updated, &row) else { return false };
        let (size, weight) = (if row.head { HEAD_SIZE } else { FONT_SIZE }, DWRITE_FONT_WEIGHT_NORMAL);
        let room = (l.width - l.left - RIGHT_PAD).max(1.0) * self.scale;
        let Ok(label) = self.gfx.text_layout_line(&row.label, &self.theme.font.family, size * self.scale, weight, room) else { return false };
        l.items = updated;
        command_update(&mut l.src, &row);
        *l.paint_items.borrow_mut() = l.items.clone();
        l.paint_labels.borrow_mut()[at] = Some(label);
        if let Ok(children) = l.text.Children() {
            for tile in l.tiles.iter_mut().filter_map(Option::take) { let _ = children.Remove(&tile); }
        }
        self.draw_tiles(0);
        self.sync_glow(0);
        self.publish_a11y();
        true
    }

    /// The first level shows `items` instead, in the same window from the
    /// same anchor (resized to them): submenus close, the clock goes on,
    /// the highlight stays on a row with the same text. `fade`: the day
    /// grid changes with motion (crossfade: a month turns like a page,
    /// anything else crosses in place). False if it could not be drawn
    /// (the old rows stay).
    unsafe fn replace_top(&mut self, items: Vec<Item>, fade: bool) -> bool {
        if self.levels.is_empty() || items.is_empty() {
            return false;
        }
        self.close_below(0);
        // (A cross-fade under way ends: its old days are gone already.)
        self.end_crossfade(0);
        let (hwnd, target) = (self.levels[0].hwnd, self.levels[0]._target.clone());
        let at = self.anchor;
        // A month header's part with the focus keeps it in the new rows.
        let nav_focus = self.levels[0].nav_focus.map(|(_, p)| p);
        let by_key = self.levels[0].by_key;
        let old_days = fade.then(|| {
            let old = &self.levels[0];
            (day_band(&old.items, &old.rows), old.tiles.iter().flatten().cloned().collect::<Vec<_>>())
        });
        match self.build_level(items, at, None, Some((hwnd, target))) {
            Ok(mut l) => {
                let old = &self.levels[0];
                let kept = old.hover.and_then(|r| old.items.get(r)).and_then(|it| l.items.iter().position(|n| n.focusable() && n.label == it.label));
                l.hover = None;
                l.by_key = by_key;
                self.levels[0] = l;
                if let Some((band, tiles)) = old_days {
                    let new = &self.levels[0];
                    if let Some(b) = join_bands(band, day_band(&new.items, &new.rows)) {
                        if let Err(e) = self.crossfade(0, b, &tiles, self.turn) {
                            crate::log!("popup: no cross-fade: {e}");
                            self.end_crossfade(0);
                        }
                    }
                }
                self.down_row = None;
                self.down_on = None;
                self.down_in = false;
                self.sliding = None;
                self.cancel_pending();
                if self.levels[0].clock.is_some() {
                    self.draw_clock(0);
                }
                self.set_hover(0, kept);
                let header = self.levels[0].items.iter().position(|it| it.nav.is_some());
                if let Some((r, p)) = header.zip(nav_focus).filter(|&(r, p)| self.levels[0].items[r].nav.as_ref().is_some_and(|n| n.id(p) != 0)) {
                    self.focus_nav(0, r, p);
                    return true;
                }
                // The pointer still on what it just clicked (a month
                // header's button, a day): its highlight stays.
                let mut p = POINT::default();
                if !by_key && GetCursorPos(&mut p).is_ok() {
                    if let Some((0, row)) = self.hit(p) {
                        self.pointer_to(0, row, p);
                    }
                }
                self.publish_a11y();
                true
            }
            Err(e) => {
                crate::log!("popup: cannot redraw the menu: {e}");
                false
            }
        }
    }

    /// Changes level `li`'s rows inside `band` (y0..y1 in the rows' own
    /// space) from `old` tiles (the rows before an in-place swap) to its
    /// own (design system 7.4). `turn` 0: in place, the old going over
    /// FADE_OUT_MS as the new come over CROSS_MS, from the same frame. A
    /// month turned (`turn` 1 the next, -1 the previous): the old slide
    /// out and the new in from the reading end (or start) on GLIDE; with
    /// reduced motion nothing slides: the short cross (12.2). The rest of
    /// its rows show at once. Its text is clipped to above the band
    /// meanwhile; copies of the tiles (sharing their surfaces) show below
    /// it and move inside it. CROSSFADE_TIMER ends it.
    unsafe fn crossfade(&mut self, li: usize, band: (f32, f32), old: &[SpriteVisual], turn: i32) -> Result<()> {
        let c = self.gfx.compositor.clone();
        let s = self.scale;
        let rtl = self.rtl;
        let Some(l) = self.levels.get_mut(li) else { return Ok(()) };
        let content_h = l.content_px as f32 / s;
        let (y0, y1) = (band.0.clamp(0.0, content_h), band.1.clamp(0.0, content_h));
        if y1 <= y0 {
            return Ok(());
        }
        let new: Vec<SpriteVisual> = l.tiles.iter().flatten().cloned().collect();
        let width = l.width;
        let layer = |tiles: &[SpriteVisual], clip: Option<(f32, f32)>| -> Result<ContainerVisual> {
            let v = c.CreateContainerVisual()?;
            v.SetSize(Vector2 { X: width, Y: content_h })?;
            if let Some((top, bottom)) = clip {
                let k = c.CreateInsetClip()?;
                k.SetTopInset(top)?;
                k.SetBottomInset((content_h - bottom).max(0.0))?;
                v.SetClip(&k)?;
            }
            for t in tiles {
                let copy = c.CreateSpriteVisual()?;
                copy.SetBrush(&t.Brush()?)?;
                copy.SetOffset(t.Offset()?)?;
                copy.SetSize(t.Size()?)?;
                v.Children()?.InsertAtTop(&copy)?;
            }
            Ok(v)
        };
        let below = layer(&new, Some((y1, content_h)))?;
        // The band's window: what moves inside it is cut to it.
        let window = layer(&[], Some((y0, y1)))?;
        let days_in = layer(&new, None)?;
        let days_out = layer(old, None)?;
        window.Children()?.InsertAtTop(&days_out)?;
        window.Children()?.InsertAtTop(&days_in)?;
        let above = c.CreateInsetClip()?;
        above.SetBottomInset((content_h - y0).max(0.0))?;
        l.text.SetClip(&above)?;
        let ch = l.scroller.Children()?;
        for v in [&below, &window] {
            ch.InsertAbove(v, &l.text)?;
        }
        l.fade = vec![below, window];
        let ms = if turn != 0 && crate::motion::full() {
            // The next month comes from the reading end (the right; right
            // to left, the left).
            let dx = width * if (turn > 0) != rtl { 1.0 } else { -1.0 };
            days_in.SetOffset(Vector3 { X: dx, Y: 0.0, Z: 0.0 })?;
            crate::motion::glide_offset(&c, &days_in, true)?;
            crate::motion::glide_offset(&c, &days_out, true)?;
            days_in.SetOffset(Vector3 { X: 0.0, Y: 0.0, Z: 0.0 })?;
            days_out.SetOffset(Vector3 { X: -dx, Y: 0.0, Z: 0.0 })?;
            crate::effects::ms(crate::motion::GLIDE_MAX_MS)
        } else {
            days_in.SetOpacity(0.0)?;
            let ms_in = crate::motion::fade_to(&c, &days_in, 1.0, crate::motion::CROSS_MS, crate::motion::EASE_OUT)?;
            let ms_out = crate::motion::fade_to(&c, &days_out, 0.0, crate::motion::FADE_OUT_MS, crate::motion::EASE_IN)?;
            ms_in.max(ms_out)
        };
        SetTimer(Some(l.hwnd), CROSSFADE_TIMER, (ms + 16) as u32, None);
        Ok(())
    }

    /// Level `li`'s cross-fade is over (or cut short): its own rows again.
    fn end_crossfade(&mut self, li: usize) {
        let Some(l) = self.levels.get_mut(li) else { return };
        if l.fade.is_empty() {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(l.hwnd), CROSSFADE_TIMER);
        }
        if let Ok(ch) = l.scroller.Children() {
            for v in l.fade.drain(..) {
                let _ = ch.Remove(&v);
            }
        }
        let _ = l.text.SetClip(None);
    }

    /// Picks row id `id`: the menu closes with it, unless the caller
    /// changes the menu in place for it (Request::on_pick). A calendar's
    /// month buttons turn its days like a page (month_turn).
    fn pick(&mut self, id: u32) {
        if let Some(f) = self.on_pick.take() {
            self.turn = month_turn(self.levels.first().and_then(|l| l.items.iter().find_map(|it| it.nav.as_ref())), id);
            let handled = f(id, &mut |items| unsafe { self.replace_top(items, true) });
            self.turn = 0;
            self.on_pick = Some(f);
            if handled {
                return;
            }
        }
        self.done = Some(Outcome::Picked(id));
    }

    /// An agenda row's texts for a menu `width` DIP wide (Item::event): a
    /// two-line row (3.3): the title (body strong) over its time and place
    /// (footnote, the time in tabular figures), each cut to the room after
    /// the glyph (event_split: the time whole, the place what is left).
    fn event_texts(&self, items: &[Item], family: &str, width: f32) -> Vec<Option<EventText>> {
        let s = self.scale;
        let (body, small) = (FONT_SIZE * s, crate::theme::ramp::FOOTNOTE.size * s);
        let room = ((width - EVENT_TEXT_X - RIGHT_PAD) * s).max(0.0);
        items
            .iter()
            .map(|it| {
                let e = it.event.as_ref()?;
                let line = |t: &str, size: f32, weight, w: f32| if w >= 1.0 && !t.is_empty() { self.gfx.text_layout_line(t, family, size, weight, w).ok() } else { None };
                if e.time.trim().is_empty() {
                    // A note: its text alone, as a row's.
                    return Some(EventText { time: None, title: line(&e.title, body, DWRITE_FONT_WEIGHT_NORMAL, room), place: None, now: e.now, past: e.past });
                }
                let time = self.gfx.text_layout_tabular(&e.time, family, small, DWRITE_FONT_WEIGHT_NORMAL).ok();
                let full = |t: &str| if t.is_empty() { 0.0 } else { self.gfx.text_layout(t, family, small, DWRITE_FONT_WEIGHT_NORMAL).map(|l| l.1).unwrap_or(0.0) };
                let (_, pw) = event_split(time.as_ref().map_or(0.0, |t| t.1), full(&e.place), room, PLACE_GAP * s, PLACE_MIN * s);
                Some(EventText { time, title: line(&e.title, body, DWRITE_FONT_WEIGHT_SEMI_BOLD, room), place: line(&e.place, small, DWRITE_FONT_WEIGHT_NORMAL, pw), now: e.now, past: e.past })
            })
            .collect()
    }

    /// Draws level `li`'s clock row for the time now: the time centered in
    /// large tabular digits (its day period small: clock_time_layout), the
    /// long date under it, in the menu's text and secondary text colors
    /// (light, dark and high contrast alike).
    fn draw_clock(&mut self, li: usize) {
        let s = self.scale;
        let Some(l) = self.levels.get(li) else { return };
        let Some((r, v)) = l.clock.clone() else { return };
        let (w_px, h_px) = (l.w_px, (l.rows[r].1 * s).ceil() as i32);
        let now = clock_now();
        let (time, date) = clock_lines(&now);
        let family = self.theme.font.family.clone();
        let Ok((tl, tw, th)) = clock_time_layout(&self.gfx, &time, &family, s) else { return };
        let Some((dl, dw, dh)) = self.layout(&date, &family, CLOCK_DATE) else { return };
        // The translucent levels, as the rows' (they read over Glass on any
        // wallpaper; design B2 review P2 #1).
        let (on, hint) = (parts::ink(self.theme.text_primary()), parts::ink(self.theme.text_secondary()));
        let drawn = self.gfx.draw(w_px.max(1) as u32, h_px.max(1) as u32, |dc, ox, oy| unsafe {
            let b_on = dc.CreateSolidColorBrush(&on, None)?;
            let b_hint = dc.CreateSolidColorBrush(&hint, None)?;
            let opts = windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT;
            // Both lines centered (the same right to left); the pair sits
            // in the middle of the row.
            let gap = CLOCK_GAP * s;
            let top = oy + ((h_px as f32 - th - gap - dh) / 2.0).max(0.0).round();
            crate::gfx::draw_text(dc, Vector2 { X: ox + ((w_px as f32 - tw) / 2.0).round(), Y: top }, &tl, &b_on, opts);
            crate::gfx::draw_text(dc, Vector2 { X: ox + ((w_px as f32 - dw) / 2.0).round(), Y: top + th + gap }, &dl, &b_hint, opts);
            Ok(())
        });
        match drawn.and_then(|surface| self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface)) {
            Ok(brush) => {
                let _ = brush.SetStretch(CompositionStretch::Fill);
                let _ = v.SetBrush(&brush);
            }
            // The device is being replaced: the next second tries again.
            Err(e) => crate::log!("popup: cannot draw the clock: {e}"),
        }
    }

    /// Sets level `li`'s clock timer for the next second, or none while
    /// nobody can see it (clock_wait). It dies with the level's window: the
    /// clock stops when the menu closes.
    fn arm_clock(&self, li: usize) {
        let Some(l) = self.levels.get(li) else { return };
        let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        unsafe {
            match clock_wait(crate::dock::session_idle(), now.wMilliseconds) {
                Some(ms) => {
                    SetTimer(Some(l.hwnd), CLOCK_TIMER, ms, None);
                }
                None => {
                    let _ = KillTimer(Some(l.hwnd), CLOCK_TIMER);
                }
            }
        }
    }

    /// CLOCK_TIMER on level window `hwnd`: the next second is drawn and the
    /// one after waited for (none while idle: session_idle_changed starts
    /// it again).
    fn on_clock_tick(&mut self, hwnd: HWND) {
        unsafe {
            let _ = KillTimer(Some(hwnd), CLOCK_TIMER);
        }
        let Some(li) = self.levels.iter().position(|l| l.hwnd == hwnd && l.clock.is_some()) else { return };
        if !crate::dock::session_idle() {
            self.draw_clock(li);
            self.publish_a11y();
        }
        self.arm_clock(li);
    }

    /// Draws the tiles of level `li` that now show (or are near), and lets
    /// go of those far away. A tile that cannot be drawn (device lost) is
    /// tried again on the next scroll.
    fn draw_tiles(&mut self, li: usize) {
        // (New tiles: a cross-fade's copies would miss them.)
        let s = self.scale;
        let missing = |l: &Level| tiles_needed(&l.view, s, l.content_px).any(|k| l.tiles.get(k).is_some_and(|t| t.is_none()));
        if self.levels.get(li).is_some_and(|l| !l.fade.is_empty() && missing(l)) {
            self.end_crossfade(li);
        }
        let Some(l) = self.levels.get_mut(li) else { return };
        let need = tiles_needed(&l.view, s, l.content_px);
        let Ok(children) = l.text.Children() else { return };
        for k in 0..l.tiles.len() {
            if need.contains(&k) {
                if l.tiles[k].is_none() {
                    match draw_tile(&self.gfx, &l.paint, l.w_px, l.content_px, k, s) {
                        Ok(t) => {
                            let _ = children.InsertAtTop(&t);
                            l.tiles[k] = Some(t);
                        }
                        Err(e) => crate::log!("popup: cannot draw rows: {e}"),
                    }
                }
            } else if let Some(t) = l.tiles[k].take() {
                let _ = children.Remove(&t);
            }
        }
    }

    /// Scrolls level `li` to `offset` (clamped): the rows move, the bands
    /// show where rows are now cut off (they come and go with a fade),
    /// submenus hanging off it close.
    fn scroll_level(&mut self, li: usize, offset: f32) -> bool {
        let c = self.gfx.compositor.clone();
        let hc = self.theme.high_contrast;
        let Some(l) = self.levels.get_mut(li) else { return false };
        let to = l.view.clamp(offset);
        if (to - l.view.offset).abs() < 0.01 {
            return false;
        }
        l.view.offset = to;
        let _ = l.scroller.SetOffset(Vector3 { X: 0.0, Y: l.view.shift(), Z: 0.0 });
        let bands = l.view.bands();
        if let Some(e) = &mut l.edges {
            e.set(&c, bands, hc);
        }
        self.draw_tiles(li);
        self.close_below(li);
        self.publish_a11y();
        true
    }

    /// Scrolls level `li` so row `r` shows whole (keyboard focus).
    fn reveal(&mut self, li: usize, r: usize) {
        let Some(l) = self.levels.get(li) else { return };
        let Some(&row) = l.rows.get(r) else { return };
        let to = l.view.reveal(row);
        self.scroll_level(li, to);
    }

    /// Highlights row `r` of level `li` (None: nothing) and scrolls it
    /// into view: the keyboard's and screen readers' way (its cursor
    /// shows, at once: the pointer's hover goes). Hover scrolling stops (it
    /// would push the row out again); the row is in view before its focus
    /// is announced.
    fn focus_row(&mut self, li: usize, r: Option<usize>) {
        if let Some(h) = self.edge.keyboard() {
            unsafe {
                let _ = KillTimer(Some(h), SCROLL_TIMER);
            }
        }
        self.cancel_pending();
        if let Some(r) = r {
            self.reveal(li, r);
        }
        if let Some(l) = self.levels.get_mut(li) {
            l.by_key = true;
        }
        self.set_hover(li, r);
    }

    /// Row `row` of level `li` is the highlighted one (None: none): what
    /// the keys act on and screen readers call focused. Whose it is (the
    /// pointer's or the keyboard's) is the level's `by_key`.
    fn set_hover(&mut self, li: usize, row: Option<usize>) {
        let Some(l) = self.levels.get_mut(li) else { return };
        let same = l.hover == row && l.nav_focus.is_none();
        l.hover = row;
        l.nav_focus = None;
        self.sync_glow(li);
        if !same {
            // The highlighted row is the focused one for screen readers.
            self.publish_a11y();
        }
    }

    /// What of level `li` the pointer at `p`, over its row `row`, is on
    /// (None: nothing that answers it): a row that can be picked, a month
    /// header's part or a day that picks something, a failure's button.
    /// Rows that only say something, disabled ones and sliders do not
    /// answer a hover (6.2).
    fn target_at(&self, li: usize, row: Option<usize>, p: POINT) -> Option<Target> {
        let l = self.levels.get(li)?;
        let r = row?;
        let it = l.items.get(r)?;
        let x = (p.x - l.rect.left) as f32 / self.scale;
        if let Some(nav) = &it.nav {
            return nav_part(x, l.width, self.rtl).filter(|&k| nav.id(k) != 0).map(|k| Target::Nav(r, k));
        }
        if !it.cell_ids.is_empty() {
            return grid_cell(x, l.width, it.cell_ids.len(), self.rtl).filter(|&k| it.cell_ids.get(k).is_some_and(|&id| id != 0)).map(|k| Target::Cell(r, k));
        }
        if it.retry.is_some() {
            let y = (p.y - l.rect.top) as f32 / self.scale - l.view.shift();
            return l
                .buttons
                .iter()
                .find(|b| b.0 == r)
                .filter(|(_, b, (bx, by))| {
                    let (w, h) = b.size();
                    x >= *bx && x < bx + w && y >= *by && y < by + h
                })
                .map(|_| Target::Button(r));
        }
        it.selectable().then_some(Target::Row(r))
    }

    /// The pointer is at `p` over row `row` of level `li`: that row is the
    /// highlighted one and what it is on there is lit (the keyboard's
    /// cursor goes: one emphasis). It left every other level: a deeper
    /// one loses its highlight, a shallower one keeps the row whose
    /// submenu this is.
    fn pointer_to(&mut self, li: usize, row: Option<usize>, p: POINT) {
        let t = self.target_at(li, row, p);
        for k in 0..self.levels.len() {
            if k == li {
                continue;
            }
            let l = &mut self.levels[k];
            if l.pointer.is_none() && (l.by_key || k < li) {
                continue;
            }
            l.pointer = None;
            if !l.by_key {
                l.hover = if k < li { l.open_row.or(l.hover) } else { None };
            }
            self.sync_glow(k);
        }
        let Some(l) = self.levels.get_mut(li) else { return };
        let same = l.hover == row && l.nav_focus.is_none() && !l.by_key;
        l.by_key = false;
        l.hover = row;
        l.nav_focus = None;
        l.pointer = t;
        self.sync_glow(li);
        if !same {
            self.publish_a11y();
        }
    }

    /// The pointer is over none of the levels: its hover goes (the row
    /// whose submenu is up stays lit as open).
    fn pointer_gone(&mut self) {
        let mut changed = false;
        for k in 0..self.levels.len() {
            let l = &mut self.levels[k];
            if l.pointer.is_none() && (l.by_key || l.hover == l.open_row) {
                continue;
            }
            l.pointer = None;
            if !l.by_key {
                l.hover = l.open_row;
            }
            changed = true;
            self.sync_glow(k);
        }
        if changed {
            self.publish_a11y();
        }
    }

    /// The keyboard's place in level `l`: its highlighted row if the keys
    /// can act on it, or a month header's part with the focus.
    fn cursor_of(l: &Level) -> Option<Target> {
        let r = l.hover?;
        if let Some((nr, part)) = l.nav_focus.filter(|&(nr, _)| nr == r) {
            return Some(Target::Nav(nr, part));
        }
        l.items.get(r).filter(|it| it.focusable()).map(|_| Target::Row(r))
    }

    /// Level `li`'s fills and its buttons as its state says now
    /// (parts::glows), each change with its own timing (6.3): the
    /// pointer's hover comes over 90 ms on a fresh fill while the one
    /// before goes over 120 where it was; a press, an open row and the
    /// keyboard's cursor are there at once.
    fn sync_glow(&mut self, li: usize) {
        let c = self.gfx.compositor.clone();
        let hc = self.theme.high_contrast;
        let rtl = self.rtl;
        let Some(l) = self.levels.get(li) else { return };
        let want = parts::glows(l.pointer, Self::cursor_of(l), l.by_key, l.pressed, l.open_row);
        let was = l.shown;
        let place = |t: Target| parts::target_box(t, &l.items, &l.rows, l.width, rtl, l.row_r);
        if want.mark != was.mark {
            match want.mark.and_then(|(t, tone)| place(t).map(|b| (b, tone))) {
                Some((b, tone)) => {
                    l.mark.place(b);
                    match self.inverse(l, b).filter(|_| hc) {
                        // High contrast: the target again, in
                        // HighlightText on Highlight.
                        Some(brush) => {
                            let _ = l.mark.v.SetBrush(&brush);
                        }
                        None => l.mark.fill(self.theme.fill_of(Role::Row, tone.state())),
                    }
                    parts::set_now(&l.mark.v, 1.0);
                }
                None => {
                    let go = was.mark.map_or(parts::Go::Now, |(_, tone)| parts::mark_off(tone, hc));
                    parts::go_to(&c, &l.mark.v, 0.0, go, false);
                }
            }
        }
        let mut next = l.hover_i;
        if want.hover != was.hover {
            if was.hover.is_some() {
                let covered = want.mark.map(|m| m.0) == was.hover;
                parts::go_to(&c, &l.hovers[l.hover_i].v, 0.0, parts::hover_off(l.by_key, covered, hc), false);
            }
            if let Some(b) = want.hover.and_then(place) {
                // A fresh fill: the one before is still fading where it was.
                next = (l.hover_i + 1) % l.hovers.len();
                let g = &l.hovers[next];
                g.place(b);
                parts::go_to(&c, &g.v, 1.0, parts::hover_on(hc), true);
            }
        }
        let (pointer, pressed, by_key) = (l.pointer, l.pressed, l.by_key);
        let theme = self.theme.clone();
        let Some(l) = self.levels.get_mut(li) else { return };
        l.hover_i = next;
        l.shown = want;
        for (r, b, _) in l.buttons.iter_mut() {
            let me = Some(Target::Button(*r));
            let state = if pressed == me {
                State::Pressed
            } else if pointer == me && !by_key {
                State::Hover
            } else {
                State::Rest
            };
            // On its row's cursor (high contrast: the Highlight fill) it
            // answers on that.
            b.set(&c, &theme, state, parts::ground_of(*r, want.mark));
        }
    }

    /// High contrast: what is in box `b` (x, y, w, h, corner DIP in the
    /// rows' space) of level `l` painted again in HighlightText on the
    /// Highlight fill (12.3: a pressed, open or cursor row). None outside
    /// high contrast, or if it could not be drawn.
    fn inverse(&self, l: &Level, b: (f32, f32, f32, f32, f32)) -> Option<windows::UI::Composition::CompositionSurfaceBrush> {
        if !self.theme.high_contrast {
            return None;
        }
        let s = self.scale;
        let (w_px, h_px) = ((b.2 * s).ceil().max(1.0), (b.3 * s).ceil().max(1.0));
        let fill = parts::ink(self.theme.fill_of(Role::Row, State::Cursor));
        let usual = l.inks.replace(Inks::inverse(&self.theme));
        let drawn = self.gfx.draw(w_px as u32, h_px as u32, |dc, ox, oy| unsafe {
            let clip = D2D_RECT_F { left: ox, top: oy, right: ox + w_px, bottom: oy + h_px };
            dc.PushAxisAlignedClip(&clip, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
            dc.FillRectangle(&clip, &dc.CreateSolidColorBrush(&fill, None)?);
            let r = (l.paint)(dc, ox - b.0 * s, oy - b.1 * s, b.1 * s, (b.1 + b.3) * s);
            dc.PopAxisAlignedClip();
            r
        });
        l.inks.set(usual);
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&drawn.ok()?).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        Some(brush)
    }

    /// Gives part `part` of level `li`'s month header `r` the keyboard
    /// focus (UIA's SetFocus, or kept over an in-place month change):
    /// highlighted, read as focused, Enter and Space pick it.
    fn focus_nav(&mut self, li: usize, r: usize, part: NavPart) {
        self.close_below(li);
        self.reveal(li, r);
        if let Some(l) = self.levels.get_mut(li) {
            l.by_key = true;
        }
        self.set_hover(li, Some(r));
        if let Some(l) = self.levels.get_mut(li) {
            l.nav_focus = Some((r, part));
        }
        self.sync_glow(li);
        self.publish_a11y();
    }

    /// Closes the submenus under level `li`: they fade out as menus do
    /// (crate::motion::retire).
    fn close_below(&mut self, li: usize) {
        self.drop_below(li, true);
    }

    /// The keyboard goes back from a submenu to level `li`: the submenu
    /// closes, and the row it hung off is the cursor's again.
    fn back_to(&mut self, li: usize) {
        if let Some(l) = self.levels.get_mut(li) {
            l.by_key = true;
        }
        self.close_below(li);
    }

    /// Takes the submenus under level `li` out of the menu (the deepest
    /// first): they are no longer tracked, their windows still stand. The
    /// row that opened them is no longer open.
    fn take_below(&mut self, li: usize) -> Vec<Level> {
        let mut out = Vec::new();
        while self.levels.len() > li + 1 {
            if let Some(l) = self.levels.pop() {
                crate::a11y::menu_event(l.hwnd, false);
                unsafe {
                    let _ = KillTimer(Some(l.hwnd), SUBMENU_TIMER);
                }
                out.push(l);
            }
        }
        if self.pending_sub.is_some_and(|(k, _)| k > li) {
            self.pending_sub = None;
        }
        if let Some(l) = self.levels.get_mut(li) {
            l.open_row = None;
        }
        out
    }

    /// Closes the submenus under level `li`, fading (`fade`) or at once.
    /// The row that opened them goes back to rest as they start to close
    /// (6.2: over 120 ms; to the cursor's fill at once if the keyboard is
    /// there).
    fn drop_below(&mut self, li: usize, fade: bool) {
        let c = self.gfx.compositor.clone();
        let gone = self.take_below(li);
        let closed = !gone.is_empty();
        for l in gone {
            if fade {
                let (hwnd, root) = (l.hwnd, l._root.clone());
                crate::motion::retire(hwnd, &c, &root, Box::new(l));
            } else {
                unsafe {
                    let _ = DestroyWindow(l.hwnd);
                }
            }
        }
        if closed {
            self.sync_glow(li);
            // The row that opened it is collapsed again.
            self.publish_a11y();
        }
    }

    /// The pointer rests on row `r` of level `li`, which has a submenu:
    /// it opens once the pointer has stayed there SUBMENU_DELAY_MS (6.3;
    /// the keyboard and a click open it at once). A wait, not a motion:
    /// neither reduced motion nor the speed setting changes it (12.2).
    fn arm_sub(&mut self, li: usize, r: usize) {
        if self.pending_sub == Some((li, r)) {
            return;
        }
        self.cancel_pending();
        let Some(l) = self.levels.get(li) else { return };
        self.pending_sub = Some((li, r));
        unsafe {
            SetTimer(Some(l.hwnd), SUBMENU_TIMER, crate::motion::SUBMENU_DELAY_MS as u32, None);
        }
    }

    /// No submenu is waiting to open any more.
    fn cancel_pending(&mut self) {
        if let Some((li, _)) = self.pending_sub.take() {
            if let Some(l) = self.levels.get(li) {
                unsafe {
                    let _ = KillTimer(Some(l.hwnd), SUBMENU_TIMER);
                }
            }
        }
    }

    /// SUBMENU_TIMER on level window `hwnd`: the submenu opens if the
    /// pointer is still on its row.
    unsafe fn on_sub_timer(&mut self, hwnd: HWND) {
        let _ = KillTimer(Some(hwnd), SUBMENU_TIMER);
        let Some((li, r)) = self.pending_sub.take() else { return };
        let still = self.levels.get(li).is_some_and(|l| l.hwnd == hwnd && !l.by_key && l.pointer == Some(Target::Row(r)));
        if still {
            self.open_sub(li, r, false);
        }
    }

    /// Every open level's tree, for UIA (while a client reads any of them:
    /// a new submenu level then joins at once).
    fn publish_a11y(&self) {
        if !self.levels.iter().any(|l| crate::a11y::wanted(l.hwnd)) {
            return;
        }
        for li in 0..self.levels.len() {
            crate::a11y::ensure(self.levels[li].hwnd);
            crate::a11y::publish(self.levels[li].hwnd, self.a11y_tree(li));
        }
    }

    /// The one level whose highlighted row has the focus: the deepest with
    /// a highlight (the keys go to the deepest level; a submenu opened by
    /// the pointer, with nothing highlighted yet, leaves it on its row).
    fn focus_level(&self) -> Option<usize> {
        focus_level(&self.levels.iter().map(|l| l.hover.filter(|&r| l.items.get(r).is_some_and(|it| !it.separator))).collect::<Vec<_>>())
    }

    /// Level `li` as UIA sees it: a menu of rows.
    fn a11y_tree(&self, li: usize) -> crate::a11y::Tree {
        use crate::a11y::{Node, Role, Tree};
        let l = &self.levels[li];
        let sub_open = self.levels.len() > li + 1;
        let nodes = l
            .items
            .iter()
            .enumerate()
            .map(|(r, it)| {
                let px = |y: f32| l.rect.top + (y * self.scale).round() as i32;
                // Scrolled out: still listed, offscreen (an empty rect at
                // the nearer edge); keyboard focus scrolls it into view.
                let rect = match l.rows.get(r).and_then(|&row| l.view.visible(row)) {
                    Some((top, bottom)) => RECT { left: l.rect.left, top: px(top), right: l.rect.right, bottom: px(bottom) },
                    None => {
                        let above = l.rows.get(r).is_some_and(|&(y, _)| y + l.view.shift() < l.view.band);
                        let edge = if above { px(l.view.band) } else { px(l.view.height - l.view.band) };
                        RECT { left: l.rect.left, top: edge, right: l.rect.left, bottom: edge }
                    }
                };
                let key = format!("r{r}");
                if it.separator {
                    return Node::new(key, Role::Separator, "", rect);
                }
                if let Some(v) = it.slider {
                    let pct = format!("{}%", (v * 100.0).round() as i32);
                    let name = if it.label.is_empty() { crate::i18n::t("a11y.popup.slider").to_string() } else { it.label.clone() };
                    return Node::new(key, Role::Slider, name, rect).value(pct).enabled(it.enabled);
                }
                if let Some(nav) = &it.nav {
                    // The month; a button back to today when it is not
                    // showing (its buttons follow: nav_nodes).
                    return if nav.today != 0 {
                        Node::new(key, Role::Button, it.a11y_name(), rect).help(nav.today_name.clone()).invoke()
                    } else {
                        Node::new(key, Role::Text, it.a11y_name(), rect)
                    };
                }
                if !it.cells.is_empty() {
                    return Node::new(key, Role::Text, it.cells.join(" "), rect);
                }
                if it.head || it.wrap || it.quiet || it.busy || it.info {
                    // Read, never picked: a header, a note, an empty
                    // list's line, the progress ring's name.
                    let name = if it.head { it.a11y_name() } else { it.label.clone() };
                    return Node::new(key, Role::Text, name, rect);
                }
                if it.clock {
                    // The date and the time now, as the row shows them.
                    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
                    let (time, date) = clock_lines(&now);
                    return Node::new(key, Role::Text, format!("{date} {time}"), rect);
                }
                // A failure reads with what its button does.
                let name = match &it.retry {
                    Some(b) => format!("{}, {b}", it.a11y_name()),
                    None => it.a11y_name(),
                };
                let mut n = Node::new(key, Role::MenuItem, name, rect).enabled(it.enabled).accel(it.shortcut.clone());
                if it.sub.is_empty() {
                    n = n.invoke();
                } else {
                    n = n.expand(sub_open && l.hover == Some(r));
                }
                if it.checked {
                    n = n.toggle(true);
                }
                // Its trailing glyphs are read with its name (a11y_name).
                n
            })
            .collect::<Vec<Node>>();
        // A month header's ‹ › right after it, each on its column.
        let mut nodes = nodes;
        for (r, it) in l.items.iter().enumerate().rev() {
            let Some(nav) = &it.nav else { continue };
            let row = nodes[r].rect;
            nodes.splice(r + 1..r + 1, nav_nodes(nav, r, row, l.width, self.scale, self.rtl));
        }
        let owner = self.focus_level();
        let focus = if owner == Some(li) { focus_key(l.hover, l.nav_focus) } else { None };
        // The first level holds the window focus; its GetFocus answers with
        // the row in the level that has it.
        let focus_in = if li == 0 { owner.filter(|&o| o != 0).map(|o| self.levels[o].hwnd.0 as isize) } else { None };
        Tree { role: Role::Menu, name: String::new(), nodes, focus, focusable: true, active: li == 0, focus_in }
    }

    /// A UIA client's request for row `key` of the level in window `hwnd`.
    unsafe fn a11y_act(&mut self, hwnd: HWND, key: &str, act: crate::a11y::Act) {
        use crate::a11y::Act;
        let Some(li) = self.levels.iter().position(|l| l.hwnd == hwnd) else { return };
        // A month header's parts: "r3" its title, "r3.prev" / "r3.next".
        let (row_key, part) = match key.split_once('.') {
            Some((k, p)) => (k, Some(p)),
            None => (key, None),
        };
        let Some(r) = row_key.strip_prefix('r').and_then(|n| n.parse::<usize>().ok()) else { return };
        let Some(it) = self.levels[li].items.get(r) else { return };
        if let Some(nav) = &it.nav {
            if let Some(id) = nav_act(nav, part, act).filter(|&id| id != 0) {
                self.pick(id);
            } else if act == Act::Focus {
                if let Some(p) = nav_key_part(part).filter(|&p| nav.id(p) != 0) {
                    self.focus_nav(li, r, p);
                }
            }
            return;
        }
        if part.is_some() {
            return;
        }
        let (selectable, focusable, has_sub, id) = (it.selectable(), it.focusable(), !it.sub.is_empty(), it.id);
        match act {
            Act::Invoke | Act::Toggle if selectable => {
                self.focus_row(li, Some(r));
                if has_sub {
                    self.open_sub(li, r, true);
                } else {
                    self.pick(id);
                }
            }
            Act::Expand if selectable && has_sub => {
                self.focus_row(li, Some(r));
                self.open_sub(li, r, true);
            }
            Act::Collapse => self.close_below(li),
            Act::Focus if focusable => {
                // Deeper levels close: the focus is on this one now.
                self.close_below(li);
                self.focus_row(li, Some(r));
            }
            _ => {}
        }
    }

    /// Opens the submenu of level `li`'s row `row` (if it has one), the
    /// menu's own padding from its parent (7.4), growing from the side of
    /// that row. A sibling submenu up: the new one takes over from it
    /// (scrubbing: the two cross over SCRUB_MS at full size, nothing grows
    /// again). The row keeps the pressed fill while its submenu is up.
    unsafe fn open_sub(&mut self, li: usize, row: usize, select_first: bool) {
        self.cancel_pending();
        let Some(l) = self.levels.get(li) else { return };
        let Some(it) = l.items.get(row) else { return };
        if it.sub.is_empty() || !it.enabled {
            self.drop_below(li, true);
            return;
        }
        let sub = it.sub.clone();
        let parent = l.rect;
        let s = self.scale;
        // Level with the row where it shows now (the parent may be scrolled).
        let (ry, rh) = l.rows[row];
        let row_y = ry + l.view.shift();
        let rects: Vec<RECT> = self.levels.iter().map(|l| l.rect).collect();
        let left = sub_side(&rects, li, self.sub_left);
        self.sub_dir = left;
        let at = sub_anchor(parent, row_y, left, s);
        let row_rect = RECT { left: parent.left, right: parent.right, top: parent.top + (row_y * s).round() as i32, bottom: parent.top + ((row_y + rh) * s).round() as i32 };
        let old = self.take_below(li);
        self.swap_in = !old.is_empty();
        self.sub_row = Some(row_rect);
        let opened = self.open_level(sub, at, Some(parent));
        self.swap_in = false;
        self.sub_row = None;
        let fresh = if opened { self.levels.last().and_then(|l| l._root.cast::<windows::UI::Composition::Visual>().ok()) } else { None };
        self.cross_out(old, fresh.as_ref());
        if opened {
            self.levels[li].open_row = Some(row);
            self.levels[li].hover = Some(row);
        }
        self.sync_glow(li);
        if opened && select_first {
            let n = self.levels.len() - 1;
            let first = step(&self.levels[n].items, None, 1);
            // Past a screenful of disabled rows it is scrolled to.
            self.focus_row(n, first);
        }
    }

    /// Submenus `old` (taken out of the menu) hand over to `next`, the
    /// submenu that took their place (its root): they cross over SCRUB_MS
    /// at full size (motion::swap), then their windows go, kept for as
    /// long as the fade really takes. Without one (it could not open) they
    /// fade out as menus close.
    unsafe fn cross_out(&mut self, old: Vec<Level>, next: Option<&windows::UI::Composition::Visual>) {
        let c = self.gfx.compositor.clone();
        let mut came = false;
        for l in old {
            let (hwnd, root) = (l.hwnd, l._root.clone());
            let Some(new) = next.filter(|_| crate::motion::click_through(hwnd)) else {
                crate::motion::retire(hwnd, &c, &root, Box::new(l));
                continue;
            };
            let faded = match root.cast::<windows::UI::Composition::Visual>() {
                // The new one comes in once, with the first old level's going.
                Ok(r) if !came => crate::motion::swap(&c, Some(&r), new).ok(),
                Ok(r) => crate::motion::fade_to(&c, &r, 0.0, crate::motion::SCRUB_MS, crate::motion::EASE_IN).ok(),
                Err(_) => None,
            };
            match faded {
                Some(ms) => {
                    came = true;
                    crate::motion::retire_faded(hwnd, ms as u32 + 16, Box::new(l), None);
                }
                None => {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }

    /// Deepest level containing `p`, and what is under it there.
    fn spot(&self, p: POINT) -> Option<(usize, Spot)> {
        for (li, l) in self.levels.iter().enumerate().rev() {
            if contains(&l.rect, p) {
                let y = (p.y - l.rect.top) as f32 / self.scale;
                return Some((li, l.view.at(&l.rows, y)));
            }
        }
        None
    }

    /// Deepest level containing `p`, and the row under it (none over a
    /// scroll band).
    fn hit(&self, p: POINT) -> Option<(usize, Option<usize>)> {
        self.spot(p).map(|(li, s)| (li, if let Spot::Row(r) = s { r } else { None }))
    }

    /// The pointer moved to `p`: pointing at a scroll band scrolls that way
    /// while it stays there.
    unsafe fn hover_scroll(&mut self, p: POINT) {
        let want = match self.spot(p) {
            Some((li, Spot::Up)) if self.levels[li].view.can(-1) => Some((self.levels[li].hwnd, -1)),
            Some((li, Spot::Down)) if self.levels[li].view.can(1) => Some((self.levels[li].hwnd, 1)),
            _ => None,
        };
        let (stop, start) = self.edge.pointer(p, want);
        if let Some(h) = stop {
            let _ = KillTimer(Some(h), SCROLL_TIMER);
        }
        if let Some(h) = start {
            SetTimer(Some(h), SCROLL_TIMER, SCROLL_TICK_MS, None);
        }
    }

    /// A hover-scroll tick: a step, while the pointer stays on the band.
    unsafe fn on_scroll_tick(&mut self) {
        let Some((h, dir)) = self.edge.on else {
            return;
        };
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let li = self.levels.iter().position(|l| l.hwnd == h);
        let still = li.zip(self.spot(p)).is_some_and(|(li, (at, s))| at == li && s == if dir < 0 { Spot::Up } else { Spot::Down });
        let moved = match li {
            Some(li) if still => {
                let v = self.levels[li].view;
                self.scroll_level(li, v.offset + dir as f32 * self.levels[li].density.row_h() * SCROLL_TICK_ROWS)
            }
            _ => false,
        };
        if !moved {
            self.edge.on = None;
            let _ = KillTimer(Some(h), SCROLL_TIMER);
        }
    }

    /// The wheel scrolls the level under the pointer (else the deepest).
    fn on_wheel(&mut self, p: POINT, delta: i16) {
        let li = self.spot(p).map(|(li, _)| li).unwrap_or(self.levels.len().saturating_sub(1));
        let Some(l) = self.levels.get(li) else { return };
        if !l.view.scrolls() {
            return;
        }
        let by = -(delta as f32 / 120.0) * WHEEL_ROWS * l.density.row_h();
        let to = l.view.offset + by;
        if self.scroll_level(li, to) {
            // The row now under the pointer is the highlighted one.
            let row = self.hit(p).filter(|&(at, _)| at == li).and_then(|(_, r)| r);
            self.pointer_to(li, row, p);
        }
    }

    fn title_at(&self, p: POINT) -> Option<usize> {
        self.titles.rects.iter().position(|r| contains(r, p))
    }

    /// Moves the slider of (level, row) to the pointer and reports it.
    fn slide_to(&mut self, li: usize, row: usize, p: POINT) {
        let Some(l) = self.levels.get(li) else { return };
        let Some((_, _, _, x0, tw)) = l.sliders.iter().find(|s| s.0 == row) else { return };
        let x = (p.x - l.rect.left) as f32 / self.scale;
        let v = crate::i18n::mirror::slider_value(self.rtl, x, *x0, x0 + tw);
        self.set_slider(li, row, v);
    }

    /// Sets the slider of (level, row) to `v` and reports it: the pointer
    /// (slide_to) and the keyboard (exact steps, never through pixels).
    fn set_slider(&mut self, li: usize, row: usize, v: f32) {
        let v = v.clamp(0.0, 1.0);
        let Some(l) = self.levels.get(li) else { return };
        let Some((_, fill, knob, x0, tw)) = l.sliders.iter().find(|s| s.0 == row) else { return };
        let (ry, rh) = l.rows[row];
        let (fx, fw) = slider_fill(self.rtl, v, *x0, *tw);
        // The fill sits in the track's clip (offsets from its start).
        let _ = fill.SetOffset(Vector3 { X: fx - x0, Y: 0.0, Z: 0.0 });
        let _ = fill.SetSize(Vector2 { X: fw, Y: SLIDER_TRACK_H });
        let half = slider_knob_box() / 2.0;
        let _ = knob.SetOffset(Vector3 { X: crate::i18n::mirror::slider_x(self.rtl, v, *x0, x0 + tw) - half, Y: ry + rh / 2.0 - half, Z: 0.0 });
        if let Some(cb) = &self.on_slide {
            cb(v);
        }
        // The row keeps the live value (its accessible value reads it);
        // clients hear only whole-percent changes.
        let pct = |v: Option<f32>| v.map(|v| (v * 100.0).round() as i32);
        let old = self.levels[li].items[row].slider;
        self.levels[li].items[row].slider = Some(v);
        if pct(old) != pct(Some(v)) {
            self.publish_a11y();
        }
    }

    unsafe fn on_move(&mut self, p: POINT) {
        if let Some((li, row)) = self.sliding {
            self.slide_to(li, row, p);
            return;
        }
        // A pressed row dragged away: hand it to the caller to drag out.
        if let (Some(at), Some((li, r))) = (self.down_at, self.down_row) {
            let (cx, cy) = (GetSystemMetrics(SM_CXDRAG).max(4), GetSystemMetrics(SM_CYDRAG).max(4));
            if crate::stacks::past_drag_threshold(p.x - at.x, p.y - at.y, cx, cy) {
                self.down_at = None;
                let ids = &self.drag_ids;
                if let Some(it) = self.levels.get(li).and_then(|l| l.items.get(r)).filter(|it| it.selectable() && it.sub.is_empty() && ids.contains(&it.id)) {
                    self.done = Some(Outcome::Dragged(it.id));
                    return;
                }
            }
        }
        // A "move" to where the pointer already was (Windows makes those)
        // is not the user pointing: the keyboard's row stays.
        if self.edge.last == Some(p) {
            return;
        }
        self.hover_scroll(p);
        match self.hit(p) {
            Some((li, row)) => {
                self.pointer_to(li, row, p);
                // A press that slid off what it began on: no longer down
                // on it (it would not pick there either).
                if self.levels[li].pressed.is_some() && self.levels[li].pressed != self.levels[li].pointer {
                    self.levels[li].pressed = None;
                    self.sync_glow(li);
                }
                if let Some(r) = row {
                    let it = &self.levels[li].items[r];
                    if !it.sub.is_empty() && it.enabled {
                        // Its submenu opens once the pointer has rested
                        // here (arm_sub); one already up for it stays.
                        let already = self.levels.len() > li + 1 && self.levels[li + 1].src == it.sub;
                        if already {
                            self.cancel_pending();
                        } else {
                            self.arm_sub(li, r);
                        }
                    } else {
                        self.cancel_pending();
                        self.close_below(li);
                    }
                }
            }
            None => {
                self.cancel_pending();
                self.pointer_gone();
                if let Some(t) = self.title_at(p) {
                    if Some(t) != self.titles.current {
                        self.done = Some(Outcome::Switch(t));
                    }
                }
            }
        }
    }

    /// The button went down on what the pointer is on in level `li` (its
    /// fill is the pressed one at once, 6.3), or came up (None: the hover
    /// comes back over 90 ms).
    fn press(&mut self, li: usize, down: bool) {
        let Some(l) = self.levels.get_mut(li) else { return };
        let to = if down { l.pointer } else { None };
        if l.pressed != to {
            l.pressed = to;
            self.sync_glow(li);
        }
    }

    /// A slider's knob held (`on`: 1.12 of its size, on SNAP) or let go
    /// (6.2). With reduced motion its size does not change.
    fn hold_knob(&self, li: usize, row: usize, on: bool) {
        let Some(l) = self.levels.get(li) else { return };
        let Some((_, _, knob, _, _)) = l.sliders.iter().find(|s| s.0 == row) else { return };
        let to = if on { size::KNOB_HELD } else { 1.0 };
        let _ = crate::motion::scale_to(&self.gfx.compositor, knob, (slider_knob_box(), slider_knob_box()), to);
    }

    unsafe fn on_button(&mut self, p: POINT, down: bool, left: bool) {
        self.down_at = (down && left).then_some(p);
        if down {
            // A new press: an older one's row no longer counts.
            self.down_row = None;
            self.down_on = None;
            self.down_in = self.hit(p).is_some();
        }
        // Sliders follow the left button only, until it is released.
        if !down && left {
            if let Some((li, row)) = self.sliding.take() {
                self.hold_knob(li, row, false);
                return;
            }
        }
        if !down {
            for li in 0..self.levels.len() {
                self.press(li, false);
            }
        }
        if down {
            if let Some((li, Some(r))) = self.hit(p) {
                if left && self.levels[li].items[r].slider.is_some() {
                    self.sliding = Some((li, r));
                    self.hold_knob(li, r, true);
                    self.slide_to(li, r, p);
                    return;
                }
                self.down_row = Some((li, r));
                self.down_on = self.target_at(li, Some(r), p);
                // (Synthesized moves may not have told where it is yet.)
                self.pointer_to(li, Some(r), p);
                self.press(li, true);
            }
        }
        match self.hit(p) {
            Some((li, Some(r))) => {
                if down {
                    return;
                }
                // A press inside the menu picks only the row it began on
                // (no drag-across picking). A press that opened the menu
                // from the bar lets the release pick, like a menu bar.
                let began = self.down_row.take().map(|d| (d, self.down_on.take()));
                let inside = std::mem::take(&mut self.down_in);
                if began.is_some_and(|(d, _)| d != (li, r)) {
                    return;
                }
                let it = &self.levels[li].items[r];
                if let Some(nav) = &it.nav {
                    // A month header's ‹ ›, or its title (back to today).
                    let l = &self.levels[li];
                    let x = (p.x - l.rect.left) as f32 / self.scale;
                    if let Some(id) = nav_part(x, l.width, self.rtl).map(|k| nav.id(k)).filter(|&id| id != 0) {
                        self.pick(id);
                    }
                    return;
                }
                if !it.cell_ids.is_empty() {
                    // A grid cell that picks (a day of the calendar).
                    let l = &self.levels[li];
                    let x = (p.x - l.rect.left) as f32 / self.scale;
                    let k = grid_cell(x, l.width, it.cell_ids.len(), self.rtl);
                    if let Some(&id) = k.and_then(|k| it.cell_ids.get(k)).filter(|&&id| id != 0) {
                        self.pick(id);
                    }
                    return;
                }
                if !it.selectable() {
                    return;
                }
                if it.retry.is_some() {
                    // A failure: only its button picks, let go on it; a
                    // press in the menu must have begun on it too (not on
                    // the row's text).
                    let id = it.id;
                    let on = Some(Target::Button(r));
                    let from_here = match began {
                        Some((_, t)) => t == on,
                        None => !inside,
                    };
                    if self.target_at(li, Some(r), p) == on && from_here {
                        self.pick(id);
                    }
                    return;
                }
                if it.sub.is_empty() {
                    let id = it.id;
                    self.pick(id);
                } else {
                    self.open_sub(li, r, false);
                }
            }
            Some((_, None)) => {}
            // Between a menu and its submenu: not outside the menu.
            None if in_gap(&self.levels.iter().map(|l| l.rect).collect::<Vec<_>>(), p) => {}
            None => match self.title_at(p) {
                // Releasing on the title that opened the menu keeps it open
                // (click to open); pressing it again closes it.
                Some(t) if Some(t) == self.titles.current => {
                    if down {
                        self.done = Some(Outcome::Cancelled);
                    }
                }
                Some(t) => self.done = Some(Outcome::Switch(t)),
                None => {
                    if down {
                        self.done = Some(Outcome::Cancelled);
                    }
                }
            },
        }
    }

    unsafe fn on_key(&mut self, vk: u16) {
        let li = self.levels.len() - 1;
        let cur = self.levels[li].hover;
        if vk == VK_SPACE.0 {
            if let (Some(cb), Some(r)) = (&self.on_space, cur) {
                let l = &self.levels[li];
                let id = l.items[r].id;
                if self.drag_ids.contains(&id) {
                    let (top, h) = l.rows.get(r).copied().unwrap_or_default();
                    let bottom = top + h + l.view.shift();
                    let at = POINT { x: l.rect.left + (l.left * self.scale).round() as i32, y: l.rect.top + (bottom * self.scale).round() as i32 };
                    if cb(id, l.hwnd, at) {
                        return;
                    }
                }
            }
        }
        if let Some(r) = by_key(&self.levels[li].items, vk) {
            let it = &self.levels[li].items[r];
            if it.sub.is_empty() {
                let id = it.id;
                    self.pick(id);
            } else {
                self.focus_row(li, Some(r));
                self.open_sub(li, r, true);
            }
            return;
        }
        // A month header's part with the focus: Enter and Space pick it.
        if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
            let l = &self.levels[li];
            if let Some(id) = l.nav_focus.filter(|&(r, _)| cur == Some(r)).and_then(|(r, p)| l.items.get(r)?.nav.as_ref().map(|n| n.id(p))).filter(|&id| id != 0) {
                self.pick(id);
                return;
            }
        }
        // A calendar: Page Up / Down, Home and the arrows move the months.
        let on_sub = cur.is_some_and(|r| !self.levels[li].items[r].sub.is_empty());
        if let Some(id) = self.levels[li].items.iter().find_map(|it| it.nav.as_ref()).and_then(|nav| nav_key(nav, vk, self.rtl, on_sub)) {
            self.pick(id);
            return;
        }
        match vk {
            v if v == VK_DOWN.0 || v == VK_UP.0 => {
                let dir = if v == VK_DOWN.0 { 1 } else { -1 };
                let next = step(&self.levels[li].items, cur, dir);
                self.focus_row(li, next);
            }
            // Arrows on a slider move it (5 %), Home/End to the ends.
            v if (v == VK_RIGHT.0 || v == VK_LEFT.0 || v == 0x24 || v == 0x23) && cur.is_some_and(|r| self.levels[li].items[r].slider.is_some()) => {
                let r = cur.unwrap_or(0);
                if let Some(old) = self.levels[li].items[r].slider {
                    // Exact steps, whatever the track's length in pixels.
                    self.set_slider(li, r, slider_key(self.rtl, v, old));
                }
            }
            // Home / End: the first / last row; Page Up / Down: a screenful.
            0x24 | 0x23 => {
                let next = step(&self.levels[li].items, None, if vk == 0x24 { 1 } else { -1 });
                self.focus_row(li, next);
            }
            0x21 | 0x22 => {
                let l = &self.levels[li];
                let next = page_step(&l.items, &l.rows, cur, if vk == 0x21 { -1 } else { 1 }, l.view.page());
                self.focus_row(li, next);
            }
            // Right opens a submenu / goes to the next title; right to left
            // (submenus open to the left, titles run leftwards) Left does.
            v if (v == VK_RIGHT.0 || v == VK_LEFT.0) && crate::i18n::mirror::arrow(self.sub_left, v == VK_RIGHT.0) > 0 => match cur {
                Some(r) if !self.levels[li].items[r].sub.is_empty() => self.open_sub(li, r, true),
                _ => self.done = Some(Outcome::Neighbor(1)),
            },
            v if v == VK_RIGHT.0 || v == VK_LEFT.0 => {
                if li > 0 {
                    self.back_to(li - 1);
                } else {
                    self.done = Some(Outcome::Neighbor(-1));
                }
            }
            v if v == VK_RETURN.0 || v == VK_SPACE.0 => {
                if let Some(r) = cur {
                    let it = &self.levels[li].items[r];
                    if it.selectable() {
                        if it.sub.is_empty() {
                            let id = it.id;
                    self.pick(id);
                        } else {
                            self.open_sub(li, r, true);
                        }
                    }
                }
            }
            v if v == VK_ESCAPE.0 => {
                if li > 0 {
                    self.back_to(li - 1);
                } else {
                    self.done = Some(Outcome::Cancelled);
                }
            }
            v if v == VK_MENU.0 => self.done = Some(Outcome::Cancelled),
            _ => {}
        }
    }
}

/// Tile `k` of a level's rows: its own surface (at most TILE_PX tall),
/// placed at its y in the scroller.
/// Pure: the rows' span (y0, y1 in the rows' own space) of the day
/// numbers of a calendar (grid rows but the weekday names): what
/// cross-fades when the month or the day picked changes in place. None
/// without one.
pub fn day_band(items: &[Item], rows: &[(f32, f32)]) -> Option<(f32, f32)> {
    items.iter().zip(rows).filter(|(it, _)| !it.cells.is_empty() && !it.abbrev).fold(None, |b, (_, &(y, h))| match b {
        None => Some((y, y + h)),
        Some((a, z)) => Some((a.min(y), z.max(y + h))),
    })
}

/// Pure: one span covering both (a month of six weeks after one of five).
pub fn join_bands(a: Option<(f32, f32)>, b: Option<(f32, f32)>) -> Option<(f32, f32)> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
        (x, None) | (None, x) => x,
    }
}

/// Pure: the grid cell (column) at `x` DIP in a menu `width` wide with `n`
/// equal columns between 12 DIP margins (the first column on the right,
/// right to left), as the grid is drawn. None in the margins.
pub fn grid_cell(x: f32, width: f32, n: usize, rtl: bool) -> Option<usize> {
    let span = width - 24.0;
    if n == 0 || span <= 0.0 {
        return None;
    }
    let x = if rtl { width - x } else { x } - 12.0;
    if x < 0.0 || x >= span {
        return None;
    }
    Some(((x / span * n as f32) as usize).min(n - 1))
}

/// Agenda rows: the room between the time and the place on their line,
/// and the least a place is shown in (DIP).
const PLACE_GAP: f32 = space::S3;
const PLACE_MIN: f32 = 40.0;
/// A grid's narrowest column (the calendar's day squares are 28) and the
/// room each keeps either side of its text.
const GRID_COL: f32 = 36.0;
const GRID_CELL_PAD: f32 = 4.0;

/// Pure: how wide (DIP) a menu with a grid of `cols` columns must be:
/// GRID_COL each, or as wide as the widest cell's text (`cell_w`, DIP)
/// and GRID_CELL_PAD either side (a weekday name longer than 28 DIP in
/// some languages would run into its neighbours), and 12 either side of
/// the grid (the rows' paint). 0 without a grid.
pub fn grid_w(cols: usize, cell_w: f32) -> f32 {
    if cols == 0 {
        return 0.0;
    }
    cols as f32 * GRID_COL.max(cell_w + GRID_CELL_PAD * 2.0) + 24.0
}

/// Pure: how wide (DIP) a menu must be to show an agenda note (an event
/// row without a time) whose text is `text_w` DIP whole: the glyph before
/// it, from the content line, and the content line's mirror after it
/// (event_texts' room).
pub fn event_note_w(text_w: f32) -> f32 {
    EVENT_TEXT_X + RIGHT_PAD + text_w
}

/// Pure: how wide an agenda row's title and place are drawn (px), given
/// their full widths and the `room` after the time: both whole when they
/// fit; else the place gets what the title leaves, or at least 40 % of the
/// room (never more than it needs), and is dropped under `min_place`.
pub fn event_split(title: f32, place: f32, room: f32, gap: f32, min_place: f32) -> (f32, f32) {
    if place <= 0.0 {
        return (title.min(room), 0.0);
    }
    if title + gap + place <= room {
        return (title, place);
    }
    let place_w = (room - gap - title).max(room * 0.4).min(place);
    if place_w < min_place {
        return (title.min(room), 0.0);
    }
    (title.min(room - gap - place_w).max(0.0), place_w)
}

/// An agenda row laid out (Tracker::event_texts), px.
struct EventText {
    /// None: a row without a time (a note): the title alone.
    time: Option<(IDWriteTextLayout, f32, f32)>,
    title: Option<(IDWriteTextLayout, f32, f32)>,
    place: Option<(IDWriteTextLayout, f32, f32)>,
    now: bool,
    past: bool,
}

/// The brushes an agenda row is drawn with: the text color, the tertiary
/// one and the hint one (all the theme's: no colors of its own).
struct EventInk<'a> {
    on: &'a windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    dim: &'a windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    hint: &'a windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    /// The card (the theme's tile; clear in high contrast, outlined).
    card: &'a windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    /// The text and tertiary colors, for the glyph.
    on_c: D2D1_COLOR_F,
    dim_c: D2D1_COLOR_F,
    outline: bool,
    /// The card's corners (a row fill's: concentric to the menu's, DIP).
    card_r: f32,
}

/// Which of the theme's text colors an agenda card's line is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone3 {
    /// The text color.
    Text,
    /// The hint color (secondary text).
    Hint,
    /// The tertiary color.
    Dim,
}

/// Pure: an agenda card's time, title and place colors. The title is what
/// the card is for: the text color, on the first line. The time and the
/// place are its second line: the secondary color (it carries
/// information: 4.5:1 on the card, never the tertiary). All dim once the
/// event is over.
fn event_tones(past: bool) -> [Tone3; 3] {
    if past {
        [Tone3::Dim; 3]
    } else {
        [Tone3::Hint, Tone3::Text, Tone3::Hint]
    }
}

/// Draws one agenda row (the one place its look is decided): a two-line
/// row (3.3) on a card. The card is a vessel: on the vessel line (4 from
/// the menu's sides, the row fill's corners), 4 from the next one; what is
/// in it starts at the content line: a calendar glyph (24 in a 28 box),
/// then 12 on, the title (body strong) over the time and the place
/// (footnote, secondary). All dim once the event is over; while it is on,
/// a short bar in the text color before the glyph (no accent: the menus
/// are achromatic). A row without a time shows its text alone. `(ox, top,
/// rh)`: the rows' left edge, this row's top and height; `(w_px, s)`: the
/// menu's width and the scale. Right to left it is mirrored.
fn draw_event_row(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, ev: &EventText, ink: &EventInk, (ox, top, rh): (f32, f32, f32), (w_px, s): (f32, f32), rtl: bool) -> Result<()> {
    let opts = windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT;
    let mx = |x: f32, w: f32| crate::i18n::mirror::span(rtl, x, w, ox, ox + w_px);
    let brush = |t: Tone3| match t {
        Tone3::Text => ink.on,
        Tone3::Hint => ink.hint,
        Tone3::Dim => ink.dim,
    };
    let [time_c, title_c, place_c] = event_tones(ev.past).map(brush);
    let (ix, iy) = (ROW_INSET * s, EVENT_GAP / 2.0 * s);
    unsafe {
        let card = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: ox + ix, top: top + iy, right: ox + w_px - ix, bottom: top + rh - iy },
            radiusX: ink.card_r * s,
            radiusY: ink.card_r * s,
        };
        if ink.outline {
            let half = s.max(1.0) / 2.0;
            let inner = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: card.rect.left + half, top: card.rect.top + half, right: card.rect.right - half, bottom: card.rect.bottom - half },
                radiusX: (card.radiusX - half).max(0.0),
                radiusY: (card.radiusY - half).max(0.0),
            };
            dc.DrawRoundedRectangle(&inner, ink.on, s.max(1.0), None);
        } else {
            dc.FillRoundedRectangle(&card, ink.card);
        }
        let (bx, side) = (EVENT_BOX * s, EVENT_ICON * s);
        let icon_x = mx(ox + CONTENT_X * s, bx) + bx / 2.0;
        crate::gfx::draw_glyph(dc, Glyph::Calendar, icon_x, top + rh / 2.0, side, if ev.past { ink.dim_c } else { ink.on_c })?;
        if ev.now {
            // Between the card's edge and the content line.
            let (bw, bh) = (space::S1 * s, space::S5 * s);
            let x = mx(ox + (ROW_INSET + space::S2) * s, bw);
            let r = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: x, top: top + (rh - bh) / 2.0, right: x + bw, bottom: top + (rh + bh) / 2.0 },
                radiusX: bw / 2.0,
                radiusY: bw / 2.0,
            };
            dc.FillRoundedRectangle(&r, ink.on);
        }
        let text_x = ox + EVENT_TEXT_X * s;
        let Some((tl, tw, th)) = &ev.title else { return Ok(()) };
        let Some((ml, mw, mh)) = &ev.time else {
            // A note: one line in the middle, the secondary color.
            crate::gfx::draw_text(dc, Vector2 { X: mx(text_x, *tw), Y: (top + (rh - th) / 2.0).round() }, tl, if ev.past { ink.dim } else { ink.hint }, opts);
            return Ok(());
        };
        // Two lines, no gap between them (their line heights are it).
        let line2_h = ev.place.as_ref().map_or(*mh, |p| p.2.max(*mh));
        let y0 = (top + (rh - th - line2_h) / 2.0).round();
        crate::gfx::draw_text(dc, Vector2 { X: mx(text_x, *tw), Y: y0 }, tl, title_c, opts);
        let y2 = y0 + th;
        crate::gfx::draw_text(dc, Vector2 { X: mx(text_x, *mw), Y: y2 + (line2_h - mh) / 2.0 }, ml, time_c, opts);
        if let Some((l, w, h)) = &ev.place {
            crate::gfx::draw_text(dc, Vector2 { X: mx(text_x + mw + PLACE_GAP * s, *w), Y: y2 + (line2_h - h) / 2.0 }, l, place_c, opts);
        }
    }
    Ok(())
}

fn draw_tile(gfx: &Gfx, paint: &Paint, w_px: i32, content_px: i32, k: usize, s: f32) -> Result<SpriteVisual> {
    let (top, h) = tile_span(k, content_px);
    let surface = gfx.draw(w_px as u32, h as u32, |dc, ox, oy| unsafe {
        let clip = D2D_RECT_F { left: ox, top: oy, right: ox + w_px as f32, bottom: oy + h as f32 };
        dc.PushAxisAlignedClip(&clip, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
        let r = paint(dc, ox, oy - top as f32, top as f32, (top + h) as f32);
        dc.PopAxisAlignedClip();
        r
    })?;
    let c = &gfx.compositor;
    let v = c.CreateSpriteVisual()?;
    let brush = c.CreateSurfaceBrushWithSurface(&surface)?;
    brush.SetStretch(CompositionStretch::Fill)?;
    v.SetBrush(&brush)?;
    v.SetOffset(Vector3 { X: 0.0, Y: top as f32 / s, Z: 0.0 })?;
    v.SetSize(Vector2 { X: w_px as f32 / s, Y: h as f32 / s })?;
    Ok(v)
}

/// A slider's track: from the content line to its mirror, less in a
/// narrow menu so the track keeps at least SLIDER_MIN_TRACK (DIP).
const SLIDER_PAD: f32 = CONTENT_X;
/// The slider's track (a capsule well) and knob (DIP).
const SLIDER_TRACK_H: f32 = size::TRACK_H;
#[cfg(test)]
const SLIDER_KNOB: f32 = size::KNOB;

/// Pure: the side of the knob's visual (the knob and its shadow's room).
pub fn slider_knob_box() -> f32 {
    parts::knob_box()
}
const SLIDER_MIN_TRACK: f32 = 16.0;

/// Pure: a slider's track (start, length DIP) in a menu `width` wide: its
/// padding gives way on a narrow menu; never 0 or less long (the pointer
/// still maps to every value).
pub fn slider_track(width: f32) -> (f32, f32) {
    let pad = ((width - SLIDER_MIN_TRACK) / 2.0).clamp(0.0, SLIDER_PAD);
    (pad, (width - pad * 2.0).max(1.0))
}

/// Pure: a slider's value after key `vk` from `old`: 5 % a step (Right
/// grows it; right to left, Left does), Home / End to the ends.
pub fn slider_key(rtl: bool, vk: u16, old: f32) -> f32 {
    match vk {
        0x24 => 0.0,
        0x23 => 1.0,
        _ if crate::i18n::mirror::arrow(rtl, vk == VK_RIGHT.0) > 0 => (old + 0.05).min(1.0),
        _ => (old - 0.05).max(0.0),
    }
}

/// Pure: a slider's accent fill (x, width) on the track `x0..x0 + tw` at
/// `v`: from the left end, or (right to left) from the right end.
pub fn slider_fill(rtl: bool, v: f32, x0: f32, tw: f32) -> (f32, f32) {
    let w = tw * v.clamp(0.0, 1.0);
    (crate::i18n::mirror::span(rtl, x0, w, x0, x0 + tw), w)
}

/// Pure: which level's highlight is the focus, given each level's
/// highlighted (non-separator) row: the deepest that has one.
pub fn focus_level(hovers: &[Option<usize>]) -> Option<usize> {
    hovers.iter().rposition(|h| h.is_some())
}

/// The selectable row whose access key is virtual key `vk` (letters and
/// digits: their VK codes are the upper-case ASCII codes).
pub fn by_key(items: &[Item], vk: u16) -> Option<usize> {
    let c = char::from_u32(vk as u32).filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit())?;
    items.iter().position(|it| it.selectable() && it.key.is_some_and(|k| k.to_ascii_uppercase() == c))
}

fn screen_pt(hwnd: HWND, lp: LPARAM) -> POINT {
    let mut p = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
    unsafe {
        let _ = ClientToScreen(hwnd, &mut p);
    }
    p
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // A closed menu fading out lets the pointer through.
        WM_NCHITTEST if crate::motion::retired(hwnd) => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEMOVE => {
            let p = screen_pt(hwnd, lp);
            with_track(|t| t.on_move(p));
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let p = screen_pt(hwnd, lp);
            with_track(|t| t.on_button(p, true, msg == WM_LBUTTONDOWN));
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let p = screen_pt(hwnd, lp);
            with_track(|t| t.on_button(p, false, msg == WM_LBUTTONUP));
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            with_track(|t| t.on_key(wp.0 as u16));
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Screen coordinates already.
            let p = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
            with_track(|t| t.on_wheel(p, delta));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == SCROLL_TIMER => {
            with_track(|t| t.on_scroll_tick());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == SUBMENU_TIMER => {
            with_track(|t| t.on_sub_timer(hwnd));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CLOCK_TIMER => {
            with_track(|t| t.on_clock_tick(hwnd));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CROSSFADE_TIMER => {
            let _ = KillTimer(Some(hwnd), CROSSFADE_TIMER);
            with_track(|t| {
                if let Some(li) = t.levels.iter().position(|l| l.hwnd == hwnd) {
                    t.end_crossfade(li);
                }
            });
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            // Another window took the foreground (only level 0 is ever
            // active; our own submenus never activate).
            with_track(|t| {
                if t.levels.first().map(|l| l.hwnd) == Some(hwnd) {
                    t.done.get_or_insert(Outcome::Lost);
                }
            });
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            let now = GetCapture();
            with_track(|t| {
                if t.levels.first().map(|l| l.hwnd) == Some(hwnd) && now != hwnd {
                    t.done.get_or_insert(Outcome::Lost);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_GETOBJECT => {
            let answer = crate::a11y::on_getobject(hwnd, wp, lp, || {
                with_track(|t| t.publish_a11y());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == crate::a11y::wm_action() => {
            for (key, act) in crate::a11y::take(hwnd) {
                with_track(|t| {
                    if act == crate::a11y::Act::Refresh {
                        t.publish_a11y();
                    } else {
                        t.a11y_act(hwnd, &key, act);
                    }
                });
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

/// The fills, bands and small visuals of a level, and their rules.
#[path = "popup_parts.rs"]
pub(crate) mod parts;

/// Menus drawn off screen for the design audit (tests only).
#[cfg(test)]
#[path = "popup_harmony.rs"]
pub(crate) mod harmony;

/// chrome_at's own visuals read back for the design audit (tests only).
#[cfg(test)]
#[path = "popup_chrome_harmony.rs"]
pub(crate) mod chrome_harmony;

#[cfg(test)]
mod tests {
    #[test]
    fn companion_submenus_keep_the_outward_side_on_a_tight_screen() {
        use super::*;
        let work=RECT{left:0,top:0,right:800,bottom:600};
        for left in [false,true] {for scale in [1.,1.25,1.5,2.] {
            let at=POINT{x:if left{250}else{550},y:500};
            let gap=sub_gap(scale);
            let width=beside_width(outward_room(at.x,left,work),gap,true).min(240);
            let align=if left{Align::BottomRight}else{Align::BottomLeft};
            let (x,y)=level_origin(align,None,false,at,width,240,work,scale);
            let parent=RECT{left:x,top:y,right:x+width,bottom:y+240};
            let anchor=sub_anchor(parent,28.,left,scale);
            let sub_width=outward_room(anchor.x,left,work).min(240);
            let (sx,_)=level_origin(align,Some(parent),left,anchor,sub_width,200,work,scale);
            assert!(if left{sx+sub_width<=parent.left-gap}else{sx>=parent.right+gap});
            assert!(sx>=work.left && sx+sub_width<=work.right);
        }}
    }
    use super::*;

    /// Menus without a clock row.
    const NO_CLOCK: f32 = 0.0;

    /// Menus have no focus ring: the keyboard's place is the selected
    /// fill (gfx::parts: "the selected fill, no ring"). It moves on the
    /// navigation keys and on a row's access key (a native menu's: the
    /// letter picks, or opens the submenu with its first row selected);
    /// a media or volume key, or a letter that is no access key, moves
    /// nothing (crate::input).
    #[test]
    fn the_menus_cursor_moves_only_on_its_keys() {
        let m = vec![Item::new(1, "&Logs"), Item::new(2, "Bluetooth")];
        for vk in [crate::input::sample::MUTE, crate::input::sample::VOLUME_UP, crate::input::sample::PLAY_PAUSE, 0x70, b'Z' as u16] {
            assert_eq!(by_key(&m, vk), None, "{vk:#x}");
        }
        let src = include_str!("popup.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert_eq!(code.matches("by_key = true").count(), 3, "focus_row, focus_nav, back_to");
        assert!(include_str!("popup_parts.rs").contains("/// The keyboard's place: the selected fill, no ring."));
    }

    #[test]
    fn only_the_day_numbers_cross_fade_in_place() {
        let mut head = Item::grid(vec!["S".into(), "M".into()], None);
        head.abbrev = true;
        let items = vec![Item::clock(), Item::note("November 2026"), head, Item::grid(vec!["1".into(), "2".into()], Some(0)), Item::grid(vec!["8".into(), "9".into()], None), Item::new(1, "Next month")];
        let rows = vec![(4.0, 90.0), (94.0, 32.0), (126.0, 32.0), (158.0, 32.0), (190.0, 32.0), (222.0, 32.0)];
        // The weeks only: not the clock, the month's name, the weekday
        // names (the same every month) or the buttons.
        assert_eq!(day_band(&items, &rows), Some((158.0, 222.0)));
        assert_eq!(day_band(&items[..2], &rows[..2]), None);
        // A month of six weeks after one of five: one span over both.
        assert_eq!(join_bands(Some((158.0, 318.0)), Some((158.0, 350.0))), Some((158.0, 350.0)));
        assert_eq!(join_bands(None, Some((1.0, 2.0))), Some((1.0, 2.0)));
        assert_eq!(join_bands(None, None), None);
    }

    #[test]
    fn a_menu_grows_from_the_cell_that_opened_it() {
        // A menu 200 x 300 DIP hanging 6 px under a 32 px bar, from a cell
        // at x 100..160: it grows from its top edge at the cell's middle.
        let cell = RECT { left: 100, top: 0, right: 160, bottom: 32 };
        let at = POINT { x: 100, y: 38 };
        assert_eq!(open_origin(Some(cell), at, (100, 38), (200.0, 300.0), 1.0), (30.0, 0.0));
        // At 150 %: the same point in DIPs.
        let cell15 = RECT { left: 150, top: 0, right: 240, bottom: 48 };
        assert_eq!(open_origin(Some(cell15), POINT { x: 150, y: 57 }, (150, 57), (200.0, 300.0), 1.5), (30.0, 0.0));
        // A bar at the bottom: the menu is over the cell, its bottom edge.
        let low = RECT { left: 100, top: 1048, right: 160, bottom: 1080 };
        assert_eq!(open_origin(Some(low), POINT { x: 100, y: 1042 }, (100, 742), (200.0, 300.0), 1.0), (30.0, 300.0));
        // Pushed aside by the screen's end (the calendar at the work
        // area's right end, the clock further right): the nearest point of
        // its edge.
        let clock = RECT { left: 1800, top: 0, right: 1900, bottom: 32 };
        assert_eq!(open_origin(Some(clock), POINT { x: 1914, y: 38 }, (1594, 38), (320.0, 400.0), 1.0), (256.0, 0.0));
        // No cell (a menu at the pointer): where it was opened at, as before.
        assert_eq!(open_origin(None, POINT { x: 500, y: 300 }, (500, 300), (200.0, 300.0), 1.0), (0.0, 0.0));
        assert_eq!(open_origin(None, POINT { x: 600, y: 600 }, (500, 300), (200.0, 300.0), 2.0), (50.0, 150.0));
        // Scrubbing takes over at full size over SCRUB_MS (90; 60 reduced).
        assert_eq!(crate::motion::SCRUB_MS, crate::motion::FADE_IN_MS);
        assert_eq!(crate::motion::fade_ms(crate::motion::Mode::Reduced, crate::motion::SCRUB_MS), crate::motion::REDUCED_MS);
    }

    #[test]
    fn sliding_to_another_title_swaps_menus_at_once() {
        // Only a hand-over to another title of the bar swaps in place.
        assert!(swaps(Outcome::Switch(2)) && swaps(Outcome::Neighbor(-1)));
        for out in [Outcome::Picked(1), Outcome::Cancelled, Outcome::Lost, Outcome::Dragged(3)] {
            assert!(!swaps(out), "{out:?}: the menu fades out");
        }
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, IsWindow, WS_EX_TOOLWINDOW, WS_POPUP};
            let win = || CreateWindowExW(WS_EX_TOOLWINDOW, windows::core::w!("STATIC"), windows::core::w!(""), WS_POPUP, 0, 0, 10, 10, None, None, None, None).unwrap();
            // The old menu stays until the next is up, then goes at once.
            let (a, b) = (win(), win());
            hold_swap(vec![(a, None, Box::new(())), (b, None, Box::new(()))], None);
            assert!(swap_pending() && IsWindow(Some(a)).as_bool());
            assert!(drop_swap(None));
            assert!(!swap_pending() && !IsWindow(Some(a)).as_bool() && !IsWindow(Some(b)).as_bool());
            assert!(!drop_swap(None), "only once");
            // No next menu: it goes after SWAP_WAIT_MS by itself.
            let c = win();
            hold_swap(vec![(c, None, Box::new(()))], None);
            fade_swap();
            assert!(!swap_pending() && !IsWindow(Some(c)).as_bool());
        }
    }

    #[test]
    fn the_check_column_is_kept_only_where_a_row_uses_it() {
        let plain = |l: &str| Item { label: l.into(), enabled: true, ..Default::default() };
        // The calendar's actions: no marks, labels start near the edge.
        assert_eq!(gutter(&[plain("Previous month"), plain("Next month")]), PLAIN_LEFT);
        // Other-month days in a grid row are "checked" but draw no mark.
        let grid = Item { cells: vec!["30".into()], checked: true, ..Default::default() };
        assert_eq!(gutter(&[grid, plain("Today")]), PLAIN_LEFT);
        // One checked row (or an icon) keeps the column for every row.
        let on = Item { checked: true, ..plain("Show seconds") };
        assert_eq!(gutter(&[plain("A"), on]), CHECK_W);
        let pic = Item { icon: Some((16, vec![0; 16 * 16 * 4])), ..plain("App") };
        assert_eq!(gutter(&[pic]), CHECK_W);
    }

    #[test]
    fn trailing_glyphs_take_the_end_column() {
        use crate::glyphs::Glyph;
        let plain = Item::default();
        assert_eq!((trail_w(&plain), end_w(&plain, 30.0)), (0.0, 30.0));
        let wifi = Item { trail: vec![Glyph::Wifi(3), Glyph::Lock], ..Default::default() };
        assert_eq!(trail_w(&wifi), 2.0 * TRAIL_SIDE + TRAIL_GAP);
        assert_eq!(end_w(&wifi, 0.0), trail_w(&wifi));
        assert_eq!(end_w(&wifi, 30.0), trail_w(&wifi) + 2.0 * TRAIL_GAP + 30.0, "apart from a shortcut");
    }

    #[test]
    fn agenda_titles_read_on_their_card() {
        use crate::syslook::SysLook;
        use crate::theme::{contrast, over, Theme};
        // Time and title in the text color, the place in the hint color.
        assert_eq!(event_tones(false), [Tone3::Hint, Tone3::Text, Tone3::Hint]);
        assert_eq!(event_tones(true), [Tone3::Dim; 3], "over: all dim");
        // The card (the tile) on the opaque menu (transparency off, as the
        // review measured), light and dark: body text needs 4.5:1.
        for light in [true, false] {
            let t = Theme::graphite().resolve(&SysLook { light, transparency: false, ..Default::default() });
            let card = over(t.tile(), t.popup_tint());
            let ratio = contrast(t.text(&t.popup), card);
            assert!(ratio >= 4.5, "light {light}: titles {ratio:.2}:1");
        }
        // The hint color (the secondary text level) reads there too now:
        // it was about 4.2:1 on the light card before the text levels.
        let t = Theme::graphite().resolve(&SysLook { light: true, transparency: false, ..Default::default() });
        let hint = contrast(t.text_dim(false), over(t.tile(), t.popup_tint()));
        assert!(hint >= 4.5, "{hint:.2}");
    }

    #[test]
    fn the_calendar_clock_rests_while_nobody_can_see_it() {
        assert_eq!(clock_wait(false, 250), Some(765), "the next second");
        assert_eq!(clock_wait(false, 999), Some(16));
        assert_eq!(clock_wait(true, 250), None, "locked, asleep or dark: no timer");
        assert_eq!(clock_wait(true, 0), None);
        // A clock row is never picked, focused or a group's header.
        let c = Item::clock();
        assert!(c.clock && !c.selectable() && !c.focusable() && !c.header());
        // As tall as the kit's 100 DIP with Segoe UI at the default size
        // (lines of about 64 and 22 DIP: 1.33 em, whole px).
        let segoe = 64.0 + CLOCK_GAP + 22.0;
        let (rows, _) = row_layout(&[c.clone(), Item::sep(), Item::new(1, "x")], Density::Normal, segoe);
        assert_eq!(rows[0].1, 100.0);
        assert_eq!(row_layout(&[c], Density::Compact, segoe).0[0].1, 92.0);
    }

    #[test]
    fn the_calendar_clock_holds_its_lines_at_every_text_size() {
        let c = Item::clock();
        // Small, Default and Large.
        for scale in crate::fonts::TEXT_SCALES {
            // The lines as Segoe UI / Malgun Gothic draw them (1.33 em,
            // whole px), and the estimate used when they cannot be measured.
            let measured = (CLOCK_TIME * scale * 1.33).ceil() + CLOCK_GAP + (CLOCK_DATE * scale * 1.33).ceil();
            assert!(clock_text_h(scale) >= measured, "{scale}: the estimate is not short");
            for text_h in [measured, clock_text_h(scale)] {
                for d in [Density::Normal, Density::Compact] {
                    let h = row_layout(&[c.clone()], d, text_h).0[0].1;
                    // draw_clock centres the pair: room above and below.
                    assert!(h >= text_h + CLOCK_PAD_COMPACT, "{scale} {d:?}: {h} < {text_h}");
                }
                assert!(Density::Compact.clock_h(text_h) <= Density::Normal.clock_h(text_h), "compact is never taller");
            }
        }
        // Large in Segoe UI: the review's 95.64 DIP of text fits a compact row.
        let large = (CLOCK_TIME * 1.1 * 1.33).ceil() + CLOCK_GAP + (CLOCK_DATE * 1.1 * 1.33).ceil();
        assert!(Density::Compact.clock_h(large) >= 95.64);
        // fit() counts the same height (a menu that just fits stays normal).
        let menu = vec![c.clone(), Item::sep(), Item::new(1, "x")];
        let h = row_layout(&menu, Density::Normal, large).1;
        assert_eq!(fit(menu.clone(), h, large).1, Density::Normal);
        assert_eq!(fit(menu, h - 1.0, large).1, Density::Compact);
    }

    #[test]
    fn notes_wrap_within_the_width() {
        // Each character 1 wide.
        let w = |s: &str| s.chars().count() as f32;
        assert_eq!(wrap_text("Restart now? Unsaved work may be lost.", 14.0, &w), vec!["Restart now?", "Unsaved work", "may be lost."]);
        assert_eq!(wrap_text("short", 14.0, &w), vec!["short"]);
        // No spaces (Japanese, Chinese): broken by characters.
        assert_eq!(wrap_text("あいうえおかきくけこ", 4.0, &w), vec!["あいうえ", "おかきく", "けこ"]);
        // A long word between short ones.
        assert_eq!(wrap_text("a bbbbbbb c", 4.0, &w), vec!["a", "bbbb", "bbb", "c"]);
        assert_eq!(wrap_text("", 4.0, &w), vec![""]);
        for line in wrap_text("지금 시스템을 종료할까요? 열린 앱에서 저장하지 않은 작업은 사라질 수 있어요.", 12.0, &w) {
            assert!(w(&line) <= 12.0, "{line}");
        }
    }

    fn menu() -> Vec<Item> {
        vec![Item::new(1, "a"), Item::sep(), Item::new(2, "b").enabled(false), Item::new(3, "c")]
    }

    #[test]
    fn rows_are_as_tall_as_what_they_hold() {
        // Design system 3.3: 28 for text and a glyph, 32 for a 28 DIP
        // thing (a month's round buttons, a week's day circles, a
        // failure's button), 24 for a header and the weekday names, 48
        // for two lines (an agenda row).
        let mut names = Item::grid(vec!["M".into(); 7], None);
        names.abbrev = true;
        let items = vec![
            Item::new(1, "row"),
            Item::new(0, "head").head(),
            Item::month("November", MonthNav::default()),
            names,
            Item::grid(vec!["1".into(); 7], None),
            Item::event(2, EventRow { time: "10:00".into(), title: "x".into(), ..Default::default() }),
            Item::failed(3, "failed", "Try again"),
            Item::busy("loading"),
            Item::quiet("nothing"),
        ];
        let h = |d| row_layout(&items, d, NO_CLOCK).0.iter().map(|r| r.1).collect::<Vec<_>>();
        assert_eq!(h(Density::Normal), vec![28.0, 24.0, 32.0, 24.0, 32.0, 48.0, 32.0, 28.0, 28.0]);
        assert_eq!(h(Density::Compact), vec![24.0, 24.0, 28.0, 24.0, 28.0, 40.0, 28.0, 24.0, 24.0]);
        // The two lines of the content line and its mirror.
        assert_eq!((CONTENT_X, PLAIN_LEFT, RIGHT_PAD), (16.0, 16.0, 16.0));
        // With marks: a 16 box on the content line, 8, then the label.
        assert_eq!((CHECK_CENTER - CHECK_BOX / 2.0, CHECK_W), (16.0, 40.0));
        // What an empty list, the ring and a failure are: a line that is
        // never picked, and a row whose button is.
        assert!(!items[8].selectable() && !items[8].focusable() && !items[7].selectable());
        assert!(items[6].selectable() && items[6].retry.as_deref() == Some("Try again"));
    }

    #[test]
    fn a_paragraph_is_lines_apart_not_rows_apart() {
        // A note alone: a row for one line, 4 over and under more.
        assert_eq!(para_h(1, false, false, Density::Normal), 28.0);
        assert_eq!(para_h(1, false, false, Density::Compact), 24.0);
        assert_eq!(para_h(3, false, false, Density::Normal), 3.0 * 18.0 + 8.0);
        assert_eq!(para_top(3, false, false, 62.0), 4.0);
        assert_eq!(para_top(1, false, false, 28.0), 5.0);
        // A notice (11.4): 12 over the title (8 and the menu's 4), 4 to
        // its reason, 8 under it; lines 18 apart whatever the density.
        let notice = vec![Item::title("Could not open"), Item { lines: vec!["a".into(), "b".into()], ..Item::note("a b") }, Item::sep(), Item::new(1, "Try again")];
        assert!(after_title(&notice, 1) && !after_title(&notice, 0) && !after_title(&notice, 3));
        let (rows, h) = row_layout(&notice, Density::Normal, NO_CLOCK);
        assert_eq!(rows, vec![(4.0, 26.0), (30.0, 48.0), (78.0, 8.0), (86.0, 28.0)]);
        assert_eq!(h, 118.0);
        assert_eq!((para_top(1, true, false, 26.0), para_top(2, false, true, 48.0)), (8.0, 4.0));
        // A title of two lines grows by a line.
        assert_eq!(para_h(2, true, false, Density::Normal), 8.0 + 36.0);
        // A reason cut to three lines keeps the rest of its text for the
        // last one (cut with "…" there).
        let label = "one two  three four five";
        assert_eq!(rest_after(label, &["one two".into(), "three".into()]), "four five");
        assert_eq!(rest_after("あいうえお", &["あい".into()]), "うえお");
        assert_eq!(rest_after("a b", &["a".into(), "b".into()]), "");
        assert_eq!(NOTICE_W, 280.0);
    }

    #[test]
    fn a_submenu_sits_four_from_its_parent() {
        assert_eq!((sub_gap(1.0), sub_gap(1.5), sub_gap(2.0)), (4, 6, 8));
        let parent = RECT { left: 100, top: 50, right: 300, bottom: 400 };
        // Beside its row (the row's top less the menu's padding).
        assert_eq!(sub_anchor(parent, 60.0, false, 1.0), POINT { x: 304, y: 50 + 60 - 4 });
        assert_eq!(sub_anchor(parent, 60.0, true, 1.5), POINT { x: 94, y: 50 + 90 - 6 });
        // The 4 between them belongs to the menu: a press there does not
        // close it.
        let sub = RECT { left: 304, top: 106, right: 504, bottom: 300 };
        assert!(in_gap(&[parent, sub], POINT { x: 302, y: 150 }));
        assert!(!in_gap(&[parent, sub], POINT { x: 302, y: 20 }), "over both");
        assert!(!in_gap(&[parent, sub], POINT { x: 600, y: 150 }));
        assert!(!in_gap(&[parent], POINT { x: 302, y: 150 }), "no submenu");
        // A submenu flipped to the left.
        let left = RECT { left: 0, top: 106, right: 96, bottom: 300 };
        assert!(in_gap(&[parent, left], POINT { x: 98, y: 150 }));
        // It opens after the pointer rested SUBMENU_DELAY_MS (180).
        assert_eq!(crate::motion::SUBMENU_DELAY_MS, 180);
    }

    #[test]
    fn a_month_turns_like_a_page() {
        let nav = MonthNav { prev: 7, next: 8, today: 9, ..Default::default() };
        assert_eq!(month_turn(Some(&nav), 8), 1);
        assert_eq!(month_turn(Some(&nav), 7), -1);
        // Back to today, a day: in place.
        assert_eq!(month_turn(Some(&nav), 9), 0);
        assert_eq!(month_turn(Some(&nav), 30), 0);
        assert_eq!(month_turn(None, 8), 0);
        assert_eq!(month_turn(Some(&MonthNav::default()), 0), 0, "nothing to pick");
    }

    #[test]
    fn agenda_rows_cut_their_texts_to_the_menu() {
        // Both fit: whole.
        assert_eq!(event_split(100.0, 50.0, 200.0, 8.0, 40.0), (100.0, 50.0));
        // No place: the title takes the room, cut.
        assert_eq!(event_split(300.0, 0.0, 200.0, 8.0, 40.0), (200.0, 0.0));
        // A long title: the place keeps 40 % of the room.
        let (t, p) = event_split(400.0, 150.0, 200.0, 8.0, 40.0);
        assert_eq!((t, p), (112.0, 80.0));
        // A short title leaves the rest to the place.
        assert_eq!(event_split(50.0, 400.0, 200.0, 8.0, 40.0), (50.0, 142.0));
        // Too little room for a place: dropped.
        assert_eq!(event_split(400.0, 150.0, 60.0, 8.0, 40.0), (60.0, 0.0));
        // UIA reads the row as "time, title, place".
        let it = Item::event(3, EventRow { time: "09:30".into(), title: "Standup".into(), place: "Room 4".into(), ..Default::default() });
        assert_eq!(it.label, "09:30, Standup, Room 4");
        assert!(it.selectable() && !it.header());
        let bare = Item::event(1, EventRow { title: "Add".into(), past: true, ..Default::default() });
        assert_eq!(bare.label, "Add");
        assert_eq!(gutter(&[it, bare]), PLAIN_LEFT, "no check column for agenda rows");
    }

    #[test]
    fn grid_cells_are_picked_where_they_are_drawn() {
        // 7 columns of 36 between 12 DIP margins.
        let w = 7.0 * 36.0 + 24.0;
        assert_eq!(grid_cell(5.0, w, 7, false), None, "the margin");
        assert_eq!(grid_cell(12.0, w, 7, false), Some(0));
        assert_eq!(grid_cell(12.0 + 36.0 * 3.5, w, 7, false), Some(3));
        assert_eq!(grid_cell(w - 13.0, w, 7, false), Some(6));
        assert_eq!(grid_cell(w - 5.0, w, 7, false), None);
        // Right to left the first column is the rightmost.
        assert_eq!(grid_cell(w - 13.0, w, 7, true), Some(0));
        assert_eq!(grid_cell(13.0, w, 7, true), Some(6));
        assert_eq!(grid_cell(50.0, w, 0, false), None);
    }

    fn month_nav(today: u32) -> MonthNav {
        MonthNav { prev: 1, next: 2, today, prev_name: "Previous month".into(), next_name: "Next month".into(), today_name: "Today".into() }
    }

    #[test]
    fn a_month_header_has_its_buttons_over_the_last_two_columns() {
        // The calendar's 7 columns of 36 between 12 DIP margins.
        let w = 7.0 * 36.0 + 24.0;
        let at = |x: f32, rtl: bool| nav_part(x, w, rtl);
        assert_eq!(at(5.0, false), None, "the margin");
        assert_eq!(at(20.0, false), Some(NavPart::Title));
        assert_eq!(at(12.0 + 36.0 * 4.9, false), Some(NavPart::Title));
        assert_eq!(at(12.0 + 36.0 * 5.5, false), Some(NavPart::Prev));
        assert_eq!(at(12.0 + 36.0 * 6.5, false), Some(NavPart::Next));
        assert_eq!(at(w - 5.0, false), None);
        // Right to left they sit on the left, next outermost.
        assert_eq!(at(12.0 + 36.0 * 0.5, true), Some(NavPart::Next));
        assert_eq!(at(12.0 + 36.0 * 1.5, true), Some(NavPart::Prev));
        assert_eq!(at(w - 20.0, true), Some(NavPart::Title));
        // Drawn where they are hit, on the grid's own columns.
        for rtl in [false, true] {
            for p in [NavPart::Title, NavPart::Prev, NavPart::Next] {
                let (x, sw) = nav_span(p, w, rtl);
                assert_eq!(at(x + 1.0, rtl), Some(p), "{p:?} {rtl}");
                assert_eq!(at(x + sw - 1.0, rtl), Some(p), "{p:?} {rtl}");
            }
            let (x, _) = nav_span(NavPart::Next, w, rtl);
            assert_eq!(grid_cell(x + 18.0, w, 7, rtl), Some(6), "over Saturday's column");
        }
        assert_eq!(nav_span(NavPart::Prev, w, false), (12.0 + 36.0 * 5.0, 36.0));
        // A click picks the part's id; the title only when today is not shown.
        let n = month_nav(0);
        assert_eq!([NavPart::Prev, NavPart::Next, NavPart::Title].map(|p| n.id(p)), [1, 2, 0]);
        assert_eq!(month_nav(3).id(NavPart::Title), 3);
        // The row itself is never picked or focused: its parts are.
        let row = Item::month("November 2026", n);
        assert!(!row.selectable() && !row.focusable() && row.id == 0);
        assert_eq!(step(&[row.clone(), Item::new(9, "x")], None, 1), Some(1));
    }

    #[test]
    fn a_calendar_moves_its_months_from_the_keyboard() {
        const PGUP: u16 = 0x21;
        const PGDN: u16 = 0x22;
        const HOME: u16 = 0x24;
        let n = month_nav(3);
        assert_eq!(nav_key(&n, PGUP, false, false), Some(1));
        assert_eq!(nav_key(&n, PGDN, false, false), Some(2));
        assert_eq!(nav_key(&n, HOME, false, false), Some(3));
        assert_eq!(nav_key(&n, VK_LEFT.0, false, false), Some(1));
        assert_eq!(nav_key(&n, VK_RIGHT.0, false, false), Some(2));
        // Right to left, Left goes forward.
        assert_eq!(nav_key(&n, VK_LEFT.0, true, false), Some(2));
        assert_eq!(nav_key(&n, VK_RIGHT.0, true, false), Some(1));
        // On a row with a submenu (the focus timer) forward opens it; back
        // still moves the month.
        assert_eq!(nav_key(&n, VK_RIGHT.0, false, true), None);
        assert_eq!(nav_key(&n, VK_LEFT.0, false, true), Some(1));
        // Today showing: Home is the menu's own (the first row).
        assert_eq!(nav_key(&month_nav(0), HOME, false, false), None);
        // Other keys are the menu's.
        for vk in [VK_DOWN.0, VK_UP.0, VK_RETURN.0, VK_ESCAPE.0, 0x23] {
            assert_eq!(nav_key(&n, vk, false, false), None, "{vk}");
        }
    }

    #[test]
    fn screen_readers_get_a_month_header_s_buttons() {
        use crate::a11y::{Act, Role};
        let w = 7.0 * 36.0 + 24.0;
        let row = RECT { left: 100, top: 50, right: 100 + (w * 1.5) as i32, bottom: 80 };
        let nodes = nav_nodes(&month_nav(3), 2, row, w, 1.5, false);
        assert_eq!(nodes.iter().map(|n| (n.key.as_str(), n.name.as_str(), n.role)).collect::<Vec<_>>(), [("r2.prev", "Previous month", Role::Button), ("r2.next", "Next month", Role::Button)]);
        assert!(nodes.iter().all(|n| n.invoke && n.enabled));
        // Each on its column, inside the row.
        assert_eq!((nodes[0].rect.left, nodes[0].rect.right), (100 + (192.0 * 1.5) as i32, 100 + (228.0 * 1.5) as i32));
        assert_eq!((nodes[1].rect.right, nodes[1].rect.top, nodes[1].rect.bottom), (100 + (264.0 * 1.5) as i32, 50, 80));
        // Right to left: next leftmost.
        let rtl = nav_nodes(&month_nav(3), 2, row, w, 1.5, true);
        assert!(rtl[1].rect.left < rtl[0].rect.left);
        // Invoking picks what a click does.
        let n = month_nav(3);
        assert_eq!(nav_act(&n, Some("prev"), Act::Invoke), Some(1));
        assert_eq!(nav_act(&n, Some("next"), Act::Invoke), Some(2));
        assert_eq!(nav_act(&n, None, Act::Invoke), Some(3));
        assert_eq!(nav_act(&n, Some("x"), Act::Invoke), None);
        assert_eq!(nav_act(&n, Some("prev"), Act::Focus), None);
    }

    #[test]
    fn a_month_header_s_buttons_take_the_focus() {
        // SetFocus on "r2.next" names the part; the focus then reads back
        // as that node (GetFocus), and as the row once another row has it.
        assert_eq!(nav_key_part(Some("next")), Some(NavPart::Next));
        assert_eq!(nav_key_part(Some("prev")), Some(NavPart::Prev));
        assert_eq!(nav_key_part(None), Some(NavPart::Title));
        assert_eq!(nav_key_part(Some("x")), None);
        assert_eq!(focus_key(Some(2), Some((2, NavPart::Next))).as_deref(), Some("r2.next"));
        assert_eq!(focus_key(Some(2), Some((2, NavPart::Prev))).as_deref(), Some("r2.prev"));
        assert_eq!(focus_key(Some(2), Some((2, NavPart::Title))).as_deref(), Some("r2"));
        assert_eq!(focus_key(Some(5), Some((2, NavPart::Next))).as_deref(), Some("r5"), "another row");
        assert_eq!(focus_key(Some(5), None).as_deref(), Some("r5"));
        assert_eq!(focus_key(None, Some((2, NavPart::Next))), None);
        // Every node nav_nodes publishes can be focused by its own key.
        let w = 7.0 * 36.0 + 24.0;
        let row = RECT { left: 0, top: 0, right: w as i32, bottom: 30 };
        for node in nav_nodes(&month_nav(3), 2, row, w, 1.0, false) {
            let (r, part) = node.key.split_once('.').unwrap();
            assert_eq!(r, "r2");
            let p = nav_key_part(Some(part)).unwrap();
            assert_eq!(focus_key(Some(2), Some((2, p))).as_deref(), Some(node.key.as_str()));
        }
        // The source keeps the pieces wired: Focus reaches focus_nav, the
        // tree reads focus_key, Enter and Space pick the focused part, and
        // a month changed in place keeps it.
        let src = include_str!("popup.rs");
        let code = src.find("#[cfg(test)]").map_or(src, |i| &src[..i]);
        for wired in ["self.focus_nav(li, r, p);", "focus_key(l.hover, l.nav_focus)", "l.nav_focus.filter(|&(r, _)| cur == Some(r))", "self.focus_nav(0, r, p);"] {
            assert!(code.contains(wired), "{wired}");
        }
    }

    #[test]
    fn slider_and_grid_rows_are_not_picked() {
        let m = vec![Item::slider(0.5), Item::grid(vec!["1".into(); 7], Some(3)), Item::new(1, "x")];
        // The slider takes the keyboard focus (arrows move it), the grid not.
        assert_eq!(step(&m, None, 1), Some(0));
        assert_eq!(step(&m, Some(0), 1), Some(2));
        assert!(!m[0].selectable() && m[0].focusable());
    }

    #[test]
    fn one_level_has_the_focus() {
        assert_eq!(focus_level(&[]), None);
        assert_eq!(focus_level(&[None]), None);
        assert_eq!(focus_level(&[Some(2)]), Some(0));
        // A submenu with a highlight takes it from its parent row.
        assert_eq!(focus_level(&[Some(2), Some(0)]), Some(1));
        // Opened by the pointer, nothing highlighted yet: the parent keeps it.
        assert_eq!(focus_level(&[Some(2), None]), Some(0));
    }

    #[test]
    fn rows_stack_with_padding() {
        let (rows, h) = row_layout(&menu(), Density::Normal, NO_CLOCK);
        // Rows 28 (design system v2.1, D2), a separator's slot 8.
        assert_eq!(rows, vec![(4.0, 28.0), (32.0, 8.0), (40.0, 28.0), (68.0, 28.0)]);
        assert_eq!(h, 100.0);
        // Compact: 24 rows, 4 separators (all on the 4 DIP grid).
        let (rows, h) = row_layout(&menu(), Density::Compact, NO_CLOCK);
        assert_eq!(rows, vec![(4.0, 24.0), (28.0, 4.0), (32.0, 24.0), (56.0, 24.0)]);
        assert_eq!(h, 84.0);
    }

    #[test]
    fn keyboard_skips_separators_and_disabled() {
        let m = menu();
        assert_eq!(step(&m, None, 1), Some(0));
        assert_eq!(step(&m, Some(0), 1), Some(3));
        assert_eq!(step(&m, Some(3), 1), Some(0));
        assert_eq!(step(&m, None, -1), Some(3));
        assert_eq!(step(&[Item::sep()], None, 1), None);
    }

    #[test]
    fn access_keys_pick_enabled_rows() {
        let mut m = menu();
        m[0].key = Some('L');
        m[2].key = Some('B');
        m[3].key = Some('3');
        assert_eq!(by_key(&m, b'L' as u16), Some(0));
        assert_eq!(by_key(&m, b'B' as u16), None, "disabled");
        assert_eq!(by_key(&m, b'3' as u16), Some(3));
        assert_eq!(by_key(&m, b'X' as u16), None);
        assert_eq!(by_key(&m, 0x26), None, "arrow keys are not access keys");
    }

    fn ids(items: &[Item]) -> Vec<u32> {
        items.iter().map(|i| i.id).collect()
    }

    #[test]
    fn compact_rows_only_when_needed() {
        // 10 rows: 288 DIP at 28, 248 at 24.
        let ten: Vec<Item> = (1..=10).map(|i| Item::new(i, format!("{i}"))).collect();
        assert_eq!(row_layout(&ten, Density::Normal, NO_CLOCK).1, 288.0);
        assert_eq!(row_layout(&ten, Density::Compact, NO_CLOCK).1, 248.0);
        assert_eq!(fit(ten.clone(), 288.0, NO_CLOCK), (ten.clone(), Density::Normal), "just fits: as is");
        assert_eq!(fit(ten.clone(), 287.0, NO_CLOCK), (ten.clone(), Density::Compact), "a little short: compact");
        assert_eq!(fit(ten.clone(), 250.0, NO_CLOCK), (ten.clone(), Density::Compact));
        // Too short even compact, nothing foldable: everything kept (it scrolls).
        assert_eq!(fit(ten.clone(), 100.0, NO_CLOCK), (ten, Density::Compact));
    }

    /// A Steam-like menu: a jump list (a named group, then tasks), then the
    /// app's own rows (100+).
    fn jump_menu() -> Vec<Item> {
        let mut m = vec![Item::new(0, "최근 플레이").enabled(false).fold(2)];
        m.extend((1..=5).map(|i| Item::new(i, format!("game {i}")).fold(2)));
        m.push(Item::sep());
        m.extend((6..=12).map(|i| Item::new(i, format!("task {i}")).fold(1)));
        m.push(Item::sep());
        m.extend((100..=109).map(|i| Item::new(i, format!("own {i}"))));
        m.push(Item::sep());
        m.push(Item::new(200, "quit"));
        m
    }

    #[test]
    fn jump_lists_fold_before_the_apps_own_rows() {
        let _l = crate::i18n::test_lock("ko");
        let m = jump_menu();
        let own: Vec<u32> = (100..=109).chain([200]).collect();
        let has_own = |v: &[Item]| own.iter().all(|id| v.iter().any(|i| i.id == *id));
        // 24 rows + 3 separators: 704 at 28, 596 compact. It fits compact.
        assert_eq!(fit(m.clone(), 704.0, NO_CLOCK), (m.clone(), Density::Normal));
        assert_eq!(fit(m.clone(), 650.0, NO_CLOCK), (m.clone(), Density::Compact));

        // 560 (36 short: two rows): the recent group folds partly, two
        // games stay under the header, three go under "더 보기".
        let (v, d) = fit(m.clone(), 560.0, NO_CLOCK);
        assert_eq!(d, Density::Compact);
        assert!(row_layout(&v, d, NO_CLOCK).1 <= 560.0);
        assert!(has_own(&v));
        assert_eq!(v[0].label, "최근 플레이");
        assert_eq!(ids(&v[1..3]), vec![1, 2]);
        assert_eq!((v[3].label.as_str(), ids(&v[3].sub)), ("더 보기", vec![3, 4, 5]));
        assert!(v[3].sub.iter().all(|i| i.fold == 0), "folded rows do not fold again");
        assert!(v[4].separator);
        assert_eq!(ids(&v[5..12]), (6..=12).collect::<Vec<_>>(), "tasks untouched");

        // 470: the recent group folds wholly (named after its header), then
        // the tasks partly.
        let (v, d) = fit(m.clone(), 470.0, NO_CLOCK);
        assert!(row_layout(&v, d, NO_CLOCK).1 <= 470.0);
        assert!(has_own(&v));
        assert_eq!((v[0].label.as_str(), ids(&v[0].sub)), ("최근 플레이", vec![1, 2, 3, 4, 5]));
        assert!(v[0].enabled && v[0].id == 0);
        assert!(v[1].separator);
        let more = v.iter().find(|i| i.label == "더 보기").expect("tasks partly folded");
        let shown: Vec<u32> = v.iter().map(|i| i.id).filter(|id| (6..=12).contains(id)).collect();
        let mut all = shown.clone();
        all.extend(ids(&more.sub));
        assert_eq!(all, (6..=12).collect::<Vec<_>>(), "every task is somewhere, in order");
        assert!(!shown.is_empty());

        // 260: both folded wholly; the rest scrolls, nothing dropped.
        let (v, d) = fit(m, 260.0, NO_CLOCK);
        assert_eq!(d, Density::Compact);
        assert!(has_own(&v));
        assert_eq!((v[0].label.as_str(), v[0].sub.len()), ("최근 플레이", 5));
        assert_eq!((v[2].label.as_str(), v[2].sub.len()), ("더 보기", 7));
        assert_eq!(v.len(), 3 + 1 + 10 + 2, "two folded rows, separators, own rows, quit");
    }

    #[test]
    fn single_foldable_rows_stay() {
        let _l = crate::i18n::test_lock("ko");
        let mut m: Vec<Item> = (1..=20).map(|i| Item::new(i, format!("{i}"))).collect();
        m[0].fold = 1;
        let (v, _) = fit(m.clone(), 200.0, NO_CLOCK);
        assert_eq!(ids(&v), ids(&m), "one row: folding saves nothing");
        assert_eq!(v[0].fold, 0);
    }

    #[test]
    fn menus_past_the_surface_limit_keep_every_command() {
        // 300 %: an app with 200 windows, then Unpin and Close (the review's case).
        let mut m: Vec<Item> = (1..=300).map(|i| Item::new(i, format!("window {i}"))).collect();
        m.push(Item::sep());
        m.push(Item::new(500, "unpin"));
        m.push(Item::sep());
        m.push(Item::new(501, "close"));
        let s = 3.0;
        let (v, d) = fit(m.clone(), 700.0, NO_CLOCK);
        assert_eq!(ids(&v), ids(&m), "nothing dropped, nothing folded");
        let (rows, content) = row_layout(&v, d, NO_CLOCK);
        let content_px = (content * s).ceil() as i32;
        assert!(content_px > crate::gfx::MAX_SURFACE_SIDE as i32, "past one surface");
        // Drawn in tiles, each under the limit, covering every row.
        let n = tile_count(content_px);
        let mut covered = 0;
        for k in 0..n {
            let (top, h) = tile_span(k, content_px);
            assert_eq!(top, covered);
            assert!(h > 0 && h <= TILE_PX && (h as u32) <= crate::gfx::MAX_SURFACE_SIDE);
            covered += h;
        }
        assert_eq!(covered, content_px);
        // Scrolled to the end, Close is visible and its tile is drawn.
        let mut view = View::new(content, 700.0);
        let close = v.iter().position(|i| i.id == 501).unwrap();
        view.offset = view.reveal(rows[close]);
        assert!(view.visible(rows[close]).is_some());
        let need = tiles_needed(&view, s, content_px);
        assert_eq!(need.end, n, "the last tile");
        assert!(need.contains(&(((rows[close].0 * s) as i32 / TILE_PX) as usize)));
    }

    #[test]
    fn only_tiles_near_the_view_are_drawn() {
        // 1128 DIP of rows at 2x: 2256 px, three tiles; 300 DIP visible.
        let v = View::new(1128.0, 300.0);
        let px = 2256;
        assert_eq!(tile_count(px), 3);
        assert_eq!(tile_span(2, px), (2048, 208));
        // Top: tile 0 shows, tile 1 is the margin.
        assert_eq!(tiles_needed(&v, 2.0, px), 0..2);
        // Middle: all.
        assert_eq!(tiles_needed(&View { offset: 450.0, ..v }, 2.0, px), 0..3);
        // A short menu: its one tile.
        assert_eq!(tiles_needed(&View::new(113.0, 600.0), 2.0, 226), 0..1);
        // A long one scrolled far: only around the view.
        let long = View { offset: 5000.0, ..View::new(20000.0, 300.0) };
        assert_eq!(tiles_needed(&long, 1.0, 20000), 3..7);
    }

    fn hw(n: usize) -> HWND {
        HWND(n as *mut _)
    }

    #[test]
    fn keyboard_stops_hover_scrolling_until_the_pointer_moves() {
        let mut e = EdgeScroll::default();
        let at = POINT { x: 10, y: 290 };
        // Pointing at the bottom band starts it.
        assert_eq!(e.pointer(at, Some((hw(1), 1))), (None, Some(hw(1))));
        // Home: the keyboard stops it; the tick finds nothing to do.
        assert_eq!(e.keyboard(), Some(hw(1)));
        assert_eq!(e.on, None);
        assert_eq!(e.keyboard(), None);
        // Windows' synthesized move to the same place does not restart it.
        assert_eq!(e.pointer(at, Some((hw(1), 1))), (None, None));
        assert_eq!(e.on, None);
        // Really moving (still on the band) does.
        assert_eq!(e.pointer(POINT { x: 12, y: 290 }, Some((hw(1), 1))), (None, Some(hw(1))));
        // Off the band: stopped.
        assert_eq!(e.pointer(POINT { x: 12, y: 100 }, None), (Some(hw(1)), None));
        // Onto another level's band: the old one stops, the new one starts.
        e.pointer(POINT { x: 12, y: 291 }, Some((hw(1), 1)));
        assert_eq!(e.pointer(POINT { x: 300, y: 5 }, Some((hw(2), -1))), (Some(hw(1)), Some(hw(2))));
    }

    #[test]
    fn home_after_hover_scrolling_stays_in_view() {
        // Hover scrolled down to the end, then Home: the first row is shown
        // and, the hover scrolling stopped, stays shown.
        let items: Vec<Item> = (1..=40).map(|i| Item::new(i, format!("{i}"))).collect();
        let (rows, h) = row_layout(&items, Density::Compact, NO_CLOCK);
        let mut v = View::new(h, 300.0);
        let mut e = EdgeScroll::default();
        e.pointer(POINT { x: 1, y: 290 }, Some((hw(1), 1)));
        v.offset = v.max_offset();
        e.keyboard();
        let home = step(&items, None, 1).unwrap();
        v.offset = v.reveal(rows[home]);
        // Ticks scroll only while `on` is set.
        if let Some((_, dir)) = e.on {
            v.offset = v.clamp(v.offset + dir as f32 * 7.0);
        }
        assert_eq!(v.offset, 0.0);
        assert!(v.visible(rows[home]).is_some());
    }

    #[test]
    fn a_submenus_first_row_past_disabled_ones_is_scrolled_to() {
        // 11 disabled rows, then 2 enabled, in 280 DIP (the review's case,
        // at the rows' height now).
        let mut sub: Vec<Item> = (1..=11).map(|i| Item::new(i, format!("{i}")).enabled(false)).collect();
        sub.push(Item::new(12, "a"));
        sub.push(Item::new(13, "b"));
        let (sub, d) = fit(sub, 280.0, NO_CLOCK);
        let (rows, h) = row_layout(&sub, d, NO_CLOCK);
        let v = View::new(h, 280.0);
        let first = step(&sub, None, 1).unwrap();
        assert_eq!(first, 11);
        assert_eq!(v.visible(rows[first]), None, "not shown unscrolled");
        // focus_row: revealed (before its focus is announced).
        let shown = View { offset: v.reveal(rows[first]), ..v };
        let (top, bottom) = shown.visible(rows[first]).unwrap();
        assert_eq!(bottom - top, rows[first].1, "whole");
    }

    #[test]
    fn short_menus_do_not_scroll() {
        let v = View::new(100.0, 600.0);
        assert!(!v.scrolls());
        assert_eq!((v.height, v.max_offset(), v.shift()), (100.0, 0.0, 0.0));
        assert_eq!(v.bands(), (false, false), "nothing is cut off: no band");
        let (rows, _) = row_layout(&menu(), Density::Normal, NO_CLOCK);
        assert_eq!(v.at(&rows, 10.0), Spot::Row(Some(0)));
        assert_eq!(v.at(&rows, 2.0), Spot::Row(None), "padding");
        assert_eq!(v.visible(rows[3]), Some((68.0, 96.0)));
    }

    #[test]
    fn scroll_offset_is_clamped() {
        // 40 rows of 24: 968 content in 300.
        let v = View::new(968.0, 300.0);
        assert!(v.scrolls());
        assert_eq!(v.height, 300.0);
        // Rows show whole between two bands of 12.
        assert_eq!((v.band, v.page()), (12.0, 276.0));
        // At an end the rows sit as in any menu: what is hidden is all it
        // scrolls by.
        assert_eq!(v.max_offset(), 668.0);
        assert_eq!(v.clamp(-5.0), 0.0);
        assert_eq!(v.clamp(5000.0), 668.0);
        assert!(!v.can(-1) && v.can(1));
        // A band shows only where rows are cut off (2.5).
        assert_eq!(v.bands(), (false, true));
        let end = View { offset: 668.0, ..v };
        assert!(end.can(-1) && !end.can(1));
        assert_eq!(end.bands(), (true, false));
        assert_eq!(View { offset: 100.0, ..v }.bands(), (true, true));
        // Never taller than its room, however short (scroll_band).
        assert_eq!(View::new(1000.0, 10.0).height, 10.0);
    }

    #[test]
    fn keyboard_focus_scrolls_into_view() {
        let items: Vec<Item> = (1..=40).map(|i| Item::new(i, format!("{i}"))).collect();
        let (rows, h) = row_layout(&items, Density::Compact, NO_CLOCK);
        let v = View::new(h, 300.0);
        // Visible already: no move.
        assert_eq!(v.reveal(rows[0]), 0.0);
        assert_eq!(v.reveal(rows[10]), 0.0);
        // Row 11 (shown at 268..292) needs 4 more: it ends at the bottom
        // band, where rows fade.
        assert_eq!(v.reveal(rows[11]), 4.0);
        let to = View { offset: v.reveal(rows[11]), ..v };
        assert_eq!(to.visible(rows[11]), Some((264.0, 288.0)));
        // The last row: the end.
        assert_eq!(v.reveal(rows[39]), v.max_offset());
        // Back up: the row at the top band.
        let end = View { offset: v.max_offset(), ..v };
        assert_eq!(end.reveal(rows[0]), 0.0);
        assert_eq!(end.reveal(rows[20]), rows[20].0 - 12.0);
        assert_eq!(View { offset: end.reveal(rows[20]), ..v }.visible(rows[20]), Some((12.0, 36.0)));
    }

    #[test]
    fn hits_follow_the_scroll() {
        let items: Vec<Item> = (1..=40).map(|i| Item::new(i, format!("{i}"))).collect();
        let (rows, h) = row_layout(&items, Density::Compact, NO_CLOCK);
        let v = View::new(h, 300.0);
        // Unscrolled, the top is as in any menu (no band: nothing is cut
        // off there); the bottom band scrolls.
        assert_eq!(v.at(&rows, 5.0), Spot::Row(Some(0)));
        assert_eq!(v.at(&rows, 290.0), Spot::Down);
        assert_eq!(v.at(&rows, 16.0), Spot::Row(Some(0)));
        assert_eq!(v.at(&rows, 30.0), Spot::Row(Some(1)));
        // Scrolled by 10 rows: the top band is there, and the same spot
        // is row 10.
        let s = View { offset: 240.0, ..v };
        assert_eq!(s.at(&rows, 5.0), Spot::Up);
        assert_eq!(s.at(&rows, 16.0), Spot::Row(Some(10)));
        assert_eq!(s.at(&rows, 283.0), Spot::Row(Some(21)));
        // Scrolled-out rows are not visible; half-shown ones are cut at
        // the band.
        assert_eq!(s.visible(rows[0]), None);
        assert_eq!(s.visible(rows[9]), None);
        assert_eq!(View { offset: 250.0, ..v }.visible(rows[10]), Some((12.0, 18.0)));
        assert_eq!(s.visible(rows[30]), None);
        // At the end the last row is clear to the menu's padding.
        let end = View { offset: v.max_offset(), ..v };
        assert_eq!(end.at(&rows, 290.0), Spot::Row(Some(39)));
        assert_eq!(end.visible(rows[39]), Some((272.0, 296.0)));
    }

    #[test]
    fn page_keys_move_a_screenful() {
        let mut items: Vec<Item> = (1..=40).map(|i| Item::new(i, format!("{i}"))).collect();
        items[14].enabled = false;
        let (rows, _) = row_layout(&items, Density::Compact, NO_CLOCK);
        // 276 DIP: 11 rows of 24 down.
        assert_eq!(page_step(&items, &rows, Some(0), 1, 276.0), Some(11));
        assert_eq!(page_step(&items, &rows, Some(11), -1, 276.0), Some(0));
        // A disabled row is skipped to the one before.
        assert_eq!(page_step(&items, &rows, Some(3), 1, 276.0), Some(13));
        // Ends stop, no wrap.
        assert_eq!(page_step(&items, &rows, Some(35), 1, 276.0), Some(39));
        assert_eq!(page_step(&items, &rows, Some(39), 1, 276.0), Some(39));
        assert_eq!(page_step(&items, &rows, Some(0), -1, 276.0), Some(0));
        // Nothing highlighted: the first / last.
        assert_eq!(page_step(&items, &rows, None, 1, 276.0), Some(0));
        assert_eq!(page_step(&items, &rows, None, -1, 276.0), Some(39));
        // A page smaller than a row still moves one.
        assert_eq!(page_step(&items, &rows, Some(0), 1, 5.0), Some(1));
    }

    #[test]
    fn slider_fill_grows_from_the_reading_start() {
        assert_eq!(slider_fill(false, 0.25, 16.0, 200.0), (16.0, 50.0));
        assert_eq!(slider_fill(true, 0.25, 16.0, 200.0), (166.0, 50.0));
        assert_eq!(slider_fill(true, 1.0, 16.0, 200.0), (16.0, 200.0));
        assert_eq!(slider_fill(true, 0.0, 16.0, 200.0), (216.0, 0.0));
    }

    #[test]
    fn a_submenu_at_a_display_edge_flips_inside_its_parents_display() {
        // Two displays side by side; the menu hangs at the left one's right
        // edge. Measured against the parent's display (level_monitor), the
        // submenu flips to the parent's left instead of opening on the
        // right display.
        let left_wa = RECT { left: 0, top: 39, right: 2560, bottom: 1600 };
        let parent = RECT { left: 2300, top: 200, right: 2560, bottom: 500 };
        let gap = sub_gap(1.5);
        let at = POINT { x: parent.right + gap, y: 220 };
        let (x, _) = level_origin(Align::TopLeft, Some(parent), false, at, 240, 200, left_wa, 1.5);
        assert_eq!(x, parent.left - 240 - gap);
        assert!(x + 240 <= left_wa.right);
    }

    #[test]
    fn a_submenu_chain_flipped_at_the_edge_keeps_going_left() {
        // A top-bar menu at the display's right edge: its submenu flips to
        // the left, and that one's submenu keeps going left instead of
        // opening back over the first menu (it would fit there).
        let wa = RECT { left: 0, top: 32, right: 1920, bottom: 1080 };
        let gap = sub_gap(1.0);
        let root = RECT { left: 1620, top: 34, right: 1900, bottom: 300 };
        let place = |levels: &[RECT], li: usize| {
            let parent = levels[li];
            let left = sub_side(levels, li, false);
            let (x, y) = level_origin(Align::TopLeft, Some(parent), left, sub_anchor(parent, 0.0, left, 1.0), 300, 200, wa, 1.0);
            RECT { left: x, top: y, right: x + 300, bottom: y + 200 }
        };
        let first = place(&[root], 0);
        assert_eq!(first.left, root.left - 300 - gap);
        let second = place(&[root, first], 1);
        assert_eq!(second.right, first.left - gap);
        assert!(second.right <= root.left, "over the first menu: {second:?}");
        // With room on the right the chain goes right all the way.
        let root = RECT { left: 100, right: 380, ..root };
        let first = place(&[root], 0);
        let second = place(&[root, first], 1);
        assert_eq!((first.left, second.left), (root.right + gap, first.right + gap));
        // Right to left starts on the left; no room there turns it right
        // once, and the chain keeps going that way.
        assert!(sub_side(&[root], 0, true));
        assert!(!sub_side(&[root, first], 1, true));
    }

    #[test]
    fn menus_hang_down_from_a_top_bar_and_up_from_a_bottom_one() {
        assert_eq!(Align::hanging(false, false), Align::TopLeft);
        assert_eq!(Align::hanging(false, true), Align::TopRight);
        assert_eq!(Align::hanging(true, false), Align::BottomLeft);
        assert_eq!(Align::hanging(true, true), Align::BottomRight);
        // The bar at the bottom reserves its strip: the work area ends on it.
        let wa = RECT { left: 0, top: 0, right: 1920, bottom: 1048 };
        let at = POINT { x: 300, y: 1046 };
        // Up from the title's corner, its bottom on the anchor.
        assert_eq!(level_origin(Align::BottomLeft, None, false, at, 200, 400, wa, 1.0), (300, 646));
        assert_eq!(level_origin(Align::BottomRight, None, true, at, 200, 400, wa, 1.0), (100, 646));
        // Near the right edge: slides left, still above the bar.
        let near = POINT { x: 1900, y: 1046 };
        assert_eq!(level_origin(Align::BottomLeft, None, false, near, 200, 400, wa, 1.0), (1720, 646));
        // Right to left near the left edge: inside.
        let left = POINT { x: 50, y: 1046 };
        assert_eq!(level_origin(Align::BottomRight, None, true, left, 200, 400, wa, 1.0), (0, 646));
        // Taller than the room above: the top edge of the work area, and
        // never past its bottom (over the bar).
        let (x, y) = level_origin(Align::BottomLeft, None, false, at, 200, 2000, wa, 1.0);
        assert_eq!((x, y), (300, 0));
        for h in [10, 400, 1000, 1048, 1500] {
            let (_, y) = level_origin(Align::BottomLeft, None, false, at, 200, h, wa, 1.0);
            assert!(y >= wa.top && (y + h <= wa.bottom || y == wa.top), "{h}: {y}");
        }
        // The top bar's menus are as before.
        let top_wa = RECT { top: 32, bottom: 1080, ..wa };
        assert_eq!(level_origin(Align::TopLeft, None, false, POINT { x: 300, y: 34 }, 200, 400, top_wa, 1.0), (300, 34));
        assert_eq!(level_origin(Align::TopRight, None, true, POINT { x: 300, y: 34 }, 200, 400, top_wa, 1.0), (100, 34));
        // A top-bar menu running off the bottom moves up into the work area.
        assert_eq!(level_origin(Align::TopLeft, None, false, POINT { x: 300, y: 900 }, 200, 400, top_wa, 1.0), (300, 680));
        // A submenu of a menu opened upward: beside its row, pulled up to
        // stay above the bar.
        let row = RECT { left: 300, top: 1000, right: 500, bottom: 1032 };
        // (4 from its parent: sub_anchor.)
        let at = sub_anchor(row, 0.0, false, 1.0);
        assert_eq!(at, POINT { x: 504, y: 996 });
        let (x, y) = level_origin(Align::BottomLeft, Some(row), false, at, 200, 300, wa, 1.0);
        assert_eq!((x, y), (504, 748));
        // Right to left: to the row's left, flipped right when no room,
        // 4 from it either way.
        let narrow = RECT { left: 10, ..row };
        let at = sub_anchor(narrow, 0.0, true, 1.0);
        assert_eq!(at.x, 6);
        let (x, _) = level_origin(Align::BottomRight, Some(narrow), true, at, 200, 300, wa, 1.0);
        assert_eq!(x, 504);
        // No room on the right: flipped to the parent's left, 4 from it.
        let far = RECT { left: 1700, top: 100, right: 1900, bottom: 400 };
        let (x, _) = level_origin(Align::TopLeft, Some(far), false, sub_anchor(far, 0.0, false, 1.0), 200, 300, wa, 1.0);
        assert_eq!(x, 1700 - 200 - 4);
    }

    #[test]
    fn a_long_menu_over_a_bottom_bar_fits_the_room() {
        // The room over a bar at the bottom is the work area's height (the
        // bar reserves its strip), as under a bar on top: compact rows,
        // then scrolling, and the window stays inside.
        let items: Vec<Item> = (0..60).map(|i| Item::new(i + 1, format!("Item {i}"))).collect();
        let wa = RECT { left: 0, top: 0, right: 1366, bottom: 768 - 32 };
        let work_h = (wa.bottom - wa.top) as f32;
        let (fitted, density) = fit(items, work_h, NO_CLOCK);
        let (_, content) = row_layout(&fitted, density, NO_CLOCK);
        let view = View::new(content, work_h);
        assert!(view.height <= work_h + 0.5, "{} > {work_h}", view.height);
        let h = view.height.ceil() as i32;
        let (_, y) = level_origin(Align::BottomLeft, None, false, POINT { x: 10, y: wa.bottom - 2 }, 200, h, wa, 1.0);
        assert!(y >= wa.top && y + h <= wa.bottom, "{y} + {h}");
    }

    #[test]
    fn a_menu_hanging_from_the_bar_keeps_the_gap() {
        let gap = crate::placement::popup_gap(1.0);
        // A bar on top reserving 32 px: the calendar hangs `gap` under it.
        let wa = RECT { left: 0, top: 32, right: 1920, bottom: 1080 };
        let at = POINT { x: 300, y: 32 + gap };
        let room = level_room(Align::TopLeft, true, at, wa);
        assert_eq!(room, 1080 - 32 - gap);
        // The same room as every other panel's under that cell (and the
        // calendar's agenda is counted for it: topbar::hang_room_dip).
        let cell = RECT { left: 300, top: 0, right: 340, bottom: 32 };
        assert_eq!(at.y, crate::placement::hang_edge(cell, gap, false, wa));
        assert_eq!(room, crate::placement::hang_room(cell, gap, false, wa));
        // Laid out in that room it starts on the gap, however tall.
        for h in [100, 600, room] {
            assert_eq!(level_origin(Align::TopLeft, None, false, at, 300, h, wa, 1.0), (300, at.y), "{h}");
        }
        // As tall as the whole work area (what the popup used to allow),
        // it was moved up onto the bar: no gap.
        assert_eq!(level_origin(Align::TopLeft, None, false, at, 300, 1048, wa, 1.0).1, 32);
        // Over a bar at the bottom: the room over the gap.
        let wa_b = RECT { top: 0, bottom: 1048, ..wa };
        let up = POINT { x: 300, y: 1048 - gap };
        let room_b = level_room(Align::BottomLeft, true, up, wa_b);
        assert_eq!(room_b, 1048 - gap);
        let (_, y) = level_origin(Align::BottomLeft, None, false, up, 300, room_b, wa_b, 1.0);
        assert_eq!(y + room_b, up.y, "its bottom on the gap");
        // A menu at the pointer (the dock's tray menu, a launch notice)
        // keeps the whole work area: it slides.
        assert_eq!(level_room(Align::TopLeft, false, at, wa), 1048);
        assert_eq!(level_room(Align::Above, false, up, wa_b), 1048);
        assert_eq!(level_room(Align::RightOf, true, at, wa), 1048);
        // A bottom dock's menu keeps to the room over its anchor (off the
        // bar and a magnified icon): laid out there, it ends on the anchor.
        let dock = POINT { x: 300, y: 840 };
        let room_d = level_room(Align::Above, true, dock, wa_b);
        assert_eq!(room_d, 840);
        let (_, y) = level_origin(Align::Above, None, false, dock, 300, room_d, wa_b, 1.0);
        assert_eq!(y + room_d, dock.y, "never past the anchor");
    }

    #[test]
    fn a_side_dock_menu_keeps_to_the_room_beside_its_anchor() {
        // Review P2 #2: a side dock's menu or notice wider than the room
        // past its anchor would slide back over the gap; its width keeps
        // to that room instead (level_span), so it starts on the anchor.
        let wa = RECT { left: 0, top: 0, right: 1024, bottom: 768 };
        let at = POINT { x: 800, y: 300 };
        assert_eq!(level_span(Align::RightOf, true, at, wa), 224);
        assert_eq!(level_span(Align::LeftOf, true, POINT { x: 180, y: 300 }, wa), 180);
        // Not hanging, or not beside a dock: the whole width.
        assert_eq!(level_span(Align::RightOf, false, at, wa), 1024);
        assert_eq!(level_span(Align::Above, true, at, wa), 1024);
        assert_eq!(level_span(Align::TopLeft, true, at, wa), 1024);
        // Past the far edge: at least a pixel.
        assert_eq!(level_span(Align::RightOf, true, POINT { x: 1030, y: 0 }, wa), 1);
        for s in [1.0f32, 2.0] {
            let span = level_span(Align::RightOf, true, at, wa);
            let w = level_px(level_width(400.0, span, s), s, span);
            assert_eq!(level_origin(Align::RightOf, None, false, at, w, 200, wa, s).0, at.x, "{s}");
            let at_r = POINT { x: 180, y: 300 };
            let span = level_span(Align::LeftOf, true, at_r, wa);
            let w = level_px(level_width(400.0, span, s), s, span);
            let (x, _) = level_origin(Align::LeftOf, None, false, at_r, w, 200, wa, s);
            assert_eq!(x + w, at_r.x, "{s}");
        }
        // A bottom dock's notice (hangs, Above): the room over the anchor.
        let at_b = POINT { x: 512, y: 570 };
        let wa_b = RECT { left: 0, top: 64, right: 1024, bottom: 640 };
        let room = level_room(Align::Above, true, at_b, wa_b);
        let (_, y) = level_origin(Align::Above, None, false, at_b, 300, room, wa_b, 2.0);
        assert_eq!(y + room, at_b.y);
    }

    #[test]
    fn every_menu_off_the_bar_sits_the_popup_gap_from_it() {
        // The rule (placement::POPUP_GAP_DIP): the menu's own rect (the
        // shadow margin is outside it) exactly popup_gap px from the bar's
        // edge, under a bar on top and over one at the bottom, at every
        // scale, left to right and right to left.
        for (s, want) in [(1.0f32, 6), (1.25, 8), (1.5, 9), (2.0, 12)] {
            let gap = crate::placement::popup_gap(s);
            assert_eq!(gap, want, "{s}");
            let bar_h = (28.0 * s).round() as i32;
            for rtl in [false, true] {
                // Bar on top.
                let wa = RECT { left: 0, top: bar_h, right: 2560, bottom: 1440 };
                let cell = RECT { left: 400, top: 0, right: 460, bottom: bar_h };
                let at = POINT { x: if rtl { cell.right } else { cell.left }, y: crate::placement::hang_edge(cell, gap, false, wa) };
                let align = Align::hanging(false, rtl);
                let h = level_room(align, true, at, wa).min(400);
                let (_, y) = level_origin(align, None, rtl, at, 240, h, wa, s);
                assert_eq!(y - cell.bottom, gap, "top bar at {s}, rtl {rtl}");
                // Bar at the bottom.
                let wa_b = RECT { left: 0, top: 0, right: 2560, bottom: 1440 - bar_h };
                let cell_b = RECT { left: 400, top: 1440 - bar_h, right: 460, bottom: 1440 };
                let up = POINT { x: at.x, y: crate::placement::hang_edge(cell_b, gap, true, wa_b) };
                let align = Align::hanging(true, rtl);
                let h = level_room(align, true, up, wa_b).min(400);
                let (_, y) = level_origin(align, None, rtl, up, 240, h, wa_b, s);
                assert_eq!(cell_b.top - (y + h), gap, "bottom bar at {s}, rtl {rtl}");
            }
        }
    }

    #[test]
    fn a_tall_calendar_fits_under_the_gap_not_over_it() {
        // A short screen (a handheld): the calendar's rows at the normal
        // density are taller than the room under the bar's gap.
        let mut items = vec![Item::clock(), Item::sep(), Item::new(0, "November 2026").enabled(false)];
        items.extend((0..7).map(|_| Item::grid(vec!["1".into(); 7], None)));
        items.push(Item::sep());
        items.extend((0..6).map(|i| Item::new(i + 1, format!("Row {i}"))));
        let clock = clock_text_h(1.0);
        let wa = RECT { left: 0, top: 32, right: 1024, bottom: 32 + 520 };
        let at = POINT { x: 800, y: 32 + crate::placement::popup_gap(1.0) };
        let room = level_room(Align::TopLeft, true, at, wa) as f32;
        assert!(row_layout(&items, Density::Normal, clock).1 > room);
        // Fitted to that room: compact rows (or scrolling), never taller.
        let (fitted, d) = fit(items, room, clock);
        let view = View::new(row_layout(&fitted, d, clock).1, room);
        assert!(view.height <= room, "{} > {room}", view.height);
        let h = (view.height).ceil() as i32;
        assert_eq!(level_origin(Align::TopLeft, None, false, at, 300, h, wa, 1.0).1, at.y, "still on the gap");
    }

    #[test]
    fn the_calendar_is_wide_enough_for_its_grid_and_notes() {
        // 7 columns of 36 with 12 either side, as before...
        assert_eq!(grid_w(7, 20.0), 7.0 * 36.0 + 24.0);
        assert_eq!(grid_w(0, 20.0), 0.0, "no grid");
        // ...wider for a weekday name longer than a column (some languages'
        // short names): each column its text and 4 either side.
        assert_eq!(grid_w(7, 40.0), 7.0 * 48.0 + 24.0);
        // An agenda note is drawn on its card after the icon: the menu holds
        // its text whole (event_texts' room for it is exactly the text).
        let w = event_note_w(200.0);
        let room = w - EVENT_TEXT_X - RIGHT_PAD;
        assert_eq!(room, 200.0);
        assert!(w > PLAIN_LEFT + 200.0 + RIGHT_PAD, "more than a plain label needs");
    }
    #[test]
    fn a_short_room_scrolls_inside_it_with_thinner_bands() {
        // The review's case: a bar y=0..32, work area y=0..96, the gap's
        // edge at y=40: 56 DIP of room for a long hidden-window list.
        let wa = RECT { left: 0, top: 0, right: 1024, bottom: 96 };
        let at = POINT { x: 10, y: 40 };
        let room = level_room(Align::TopLeft, true, at, wa);
        assert_eq!(room, 56);
        let v = View::new(1000.0, room as f32);
        assert_eq!(v.height, 56.0, "not the old 64");
        assert!(v.scrolls());
        assert_eq!(v.band, 12.0);
        assert!(v.page() >= ROW_H, "a whole row still shows");
        let h = level_px(v.height, 1.0, room);
        assert_eq!(level_origin(Align::TopLeft, None, false, at, 200, h, wa, 1.0).1, at.y, "on the gap, not over it");
        // Bottom 88: 48 DIP: thinner bands, a row between them.
        let v = View::new(1000.0, 48.0);
        assert_eq!((v.height, v.band, v.page()), (48.0, 10.0, ROW_H));
        assert_eq!(View::new(1000.0, 44.0).band, SCROLL_BAND_MIN);
        // The same under a bar at the bottom.
        let up = POINT { x: 10, y: 56 };
        let room_b = level_room(Align::BottomLeft, true, up, wa);
        assert_eq!(room_b, 56);
        let h = level_px(View::new(1000.0, room_b as f32).height, 1.0, room_b);
        let (_, y) = level_origin(Align::BottomLeft, None, false, up, 200, h, wa, 1.0);
        assert_eq!(y + h, up.y, "its bottom on the gap");
        // Tinier rooms: bands at most a quarter each, rows between them;
        // the wheel and the keyboard still scroll to the last row.
        for room in [40.0f32, 32.0, 24.0, 16.0, 8.0] {
            let v = View::new(1000.0, room);
            assert!(v.height <= room, "{room}");
            assert!(v.scrolls(), "{room}");
            assert!(v.band <= SCROLL_BAND && v.band <= room / 4.0, "{room}");
            assert!(v.page() >= room / 2.0 - 0.001, "{room}");
            let end = View { offset: v.max_offset(), ..v };
            let last = (1000.0 - PAD_Y - ROW_H, ROW_H);
            let bottom = end.visible(last).map(|(_, b)| b);
            assert!(bottom.is_some_and(|b| (b - (v.height - PAD_Y)).abs() < 0.001), "{room}: at the end the last row sits as in any menu ({bottom:?})");
        }
        // A room tall enough keeps the full bands.
        assert_eq!(scroll_band(300.0), SCROLL_BAND);
        assert_eq!(scroll_band(64.0), SCROLL_BAND);
    }

    #[test]
    fn a_menu_at_175_percent_keeps_its_pixel_budget() {
        // The review's case: 175 %, a 32 DIP bar y=0..56, the gap 11 px
        // (6 DIP, 10.5 rounded), room y=67..1080 = 1013 px.
        let s = 1.75f32;
        let gap = crate::placement::popup_gap(s);
        assert_eq!(gap, 11);
        let wa = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let at = POINT { x: 100, y: 56 + gap };
        let room = level_room(Align::TopLeft, true, at, wa);
        assert_eq!(room, 1013);
        // Back to pixels the DIP may come out a hair over 1013 (rounded
        // up), but the menu stays 1013 tall and the gap 11.
        let v = View::new(5000.0, room as f32 / s);
        let h = level_px(v.height, s, room);
        assert_eq!(h, 1013);
        assert_eq!(level_origin(Align::TopLeft, None, false, at, 300, h, wa, s).1 - 56, gap);
        // The same over a bar at the bottom (y=1024..1080).
        let up = POINT { x: 100, y: 1024 - gap };
        let room_b = level_room(Align::BottomLeft, true, up, wa);
        let h = level_px(View::new(5000.0, room_b as f32 / s).height, s, room_b);
        let (_, y) = level_origin(Align::BottomLeft, None, false, up, 300, h, wa, s);
        assert_eq!(1024 - (y + h), gap);
        // Every room at every scale: never a pixel over (fitting content
        // too: content == room as DIP).
        for s in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 3.0] {
            for room in 1..1500 {
                let dip = room as f32 / s;
                assert!(level_px(View::new(dip * 3.0, dip).height, s, room) <= room, "{s} {room}");
                assert!(level_px(View::new(dip, dip).height, s, room) <= room, "{s} {room}");
            }
        }
        // Smaller than the room: rounded up as before.
        assert_eq!(level_px(100.2, 1.0, 1010), 101);
    }

    #[test]
    fn a_wide_calendar_is_capped_to_the_work_area_with_every_column() {
        // The review's case: 800 px at 200 % (400 DIP); a weekday 60 DIP
        // wide asks for 7 * (60 + 8) + 24 = 500 DIP.
        let want = grid_w(7, 60.0);
        assert_eq!(want, 500.0);
        let s = 2.0;
        let width = level_width(want, 800, s);
        assert_eq!(width, 400.0);
        assert_eq!(level_px(width, s, 800), 800, "the right edge on screen");
        let wa = RECT { left: 0, top: 0, right: 800, bottom: 600 };
        let (x, _) = level_origin(Align::TopRight, None, false, POINT { x: 792, y: 40 }, 800, 300, wa, s);
        assert_eq!(x, 0);
        // All seven columns, each with its own cell under the pointer.
        let col = (width - 24.0) / 7.0;
        for k in 0..7 {
            assert_eq!(grid_cell(12.0 + col * (k as f32 + 0.5), width, 7, false), Some(k));
        }
        // Their texts fit them: shortened, never dropped.
        let room = grid_text_room(width, 7);
        assert!((room - (376.0 / 7.0 - 8.0)).abs() < 0.001);
        let w = |t: &str| t.chars().count() as f32 * 12.0;
        let short = shorten("Donnerstag", room, &w);
        assert!(!short.is_empty() && w(&short) <= room && "Donnerstag".starts_with(&short), "{short}");
        // A menu that fits is untouched.
        assert_eq!(level_width(276.0, 800, s), 276.0);
        assert_eq!(level_width(276.0, 1920, 1.0), 276.0);
        assert_eq!(grid_text_room(400.0, 0), 0.0);
        // An agenda note wider than the screen: cut to the card's room
        // (event_split for the capped width), not past the edge.
        let note = event_note_w(350.0);
        assert!(note > 400.0);
        let card_room = width - EVENT_TEXT_X - RIGHT_PAD;
        assert_eq!(event_split(350.0, 0.0, card_room, PLACE_GAP, PLACE_MIN), (card_room, 0.0));
    }

    #[test]
    fn short_names_keep_their_start() {
        let w = |t: &str| t.chars().count() as f32 * 10.0;
        assert_eq!(shorten("Mon", 30.0, &w), "Mon", "fits: as it is");
        assert_eq!(shorten("Mon.", 30.0, &w), "Mon");
        assert_eq!(shorten("Mo. ", 30.0, &w), "Mo", "no dot or space left at the end");
        assert_eq!(shorten("Mo.", 15.0, &w), "M");
        assert_eq!(shorten("목요일", 25.0, &w), "목요");
        assert_eq!(shorten("Mon", 0.0, &w), "M", "at least the first letter");
        assert_eq!(shorten("", 0.0, &w), "");
    }

    #[test]
    fn a_narrow_calendar_draws_every_day_whole() {
        // The review's case: 640 px at 400 % (160 DIP), digits 7 DIP wide:
        // a cell's room is 11.43, so "10" was cut to "1" while it picked
        // the 10th.
        let width = level_width(500.0, 640, 4.0);
        assert_eq!(width, 160.0);
        let measure = |t: &str, size: f32| t.chars().count() as f32 * 7.0 * size / GRID_TEXT;
        let room = grid_text_room(width, 7);
        assert_eq!(shorten("10", room, &|t| measure(t, GRID_TEXT)), "1", "what the old cut drew");
        let mut head = Item::grid(["Sonntag", "Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag"].map(String::from).to_vec(), None);
        head.abbrev = true;
        // November 2026 as the calendar lays it out; a cell picks its day
        // (here the id is the day itself, like Action::Day's).
        let mut items = vec![head];
        for week in crate::topbar::panels::month_grid(2026, 11) {
            let mut row = Item::grid(week.iter().map(|&d| if d == 0 { String::new() } else { d.to_string() }).collect(), None);
            row.cell_ids = week.to_vec();
            items.push(row);
        }
        let drawn = grid_texts(&items, width, 7, &measure);
        let col = (width - 24.0) / 7.0;
        for (it, texts) in items.iter().zip(&drawn) {
            if it.abbrev {
                for (name, (t, _)) in it.cells.iter().zip(texts) {
                    assert!(!t.is_empty() && name.starts_with(t.as_str()) && measure(t, GRID_TEXT) <= room, "{name} -> {t}");
                }
                continue;
            }
            for (&id, (t, size)) in it.cell_ids.iter().zip(texts) {
                if id == 0 {
                    assert!(t.is_empty());
                    continue;
                }
                assert_eq!(t, &id.to_string(), "the day drawn is the day picked");
                assert!(measure(t, *size) <= col + 0.001, "{t} fits its column at {size}");
            }
        }
        // Narrower still: the days get smaller, never cut.
        let tiny = grid_texts(&items, 90.0, 7, &measure);
        let size = tiny[1].iter().find(|(t, _)| !t.is_empty()).map(|c| c.1).unwrap();
        assert!((GRID_TEXT_MIN..GRID_TEXT).contains(&size));
        assert!(tiny.iter().skip(1).flatten().zip(items.iter().skip(1).flat_map(|i| &i.cells)).all(|((t, _), c)| t == c));
        // Wide enough: all as they are.
        assert!(grid_texts(&items, 400.0, 7, &measure).iter().flatten().all(|(_, s)| *s == GRID_TEXT));
    }

    /// November 2026's day rows as the calendar lays them out (the id of a
    /// cell is its day).
    fn november_rows() -> Vec<Item> {
        crate::topbar::panels::month_grid(2026, 11)
            .into_iter()
            .map(|week| {
                let mut row = Item::grid(week.iter().map(|&d| if d == 0 { String::new() } else { d.to_string() }).collect(), None);
                row.cell_ids = week.to_vec();
                row
            })
            .collect()
    }

    #[test]
    fn narrow_calendars_never_draw_a_day_over_its_neighbour() {
        // Digits 7 DIP wide at 13 DIP (the reviews' model), and a font
        // whose width does not shrink in proportion (side bearings).
        let linear = |t: &str, size: f32| t.chars().count() as f32 * 7.0 * size / GRID_TEXT;
        let bearing = |t: &str, size: f32| if t.is_empty() { 0.0 } else { t.chars().count() as f32 * 7.0 * size / GRID_TEXT + 0.6 };
        type Measure<'a> = &'a dyn Fn(&str, f32) -> f32;
        let models: [Measure; 2] = [&linear, &bearing];
        let items = november_rows();
        let overlap = |a: (f32, f32), b: (f32, f32)| a.0.max(b.0) < a.1.min(b.1) - 1e-3;
        let inside = |a: (f32, f32), cell: (f32, f32)| a.0 >= cell.0 - 1e-3 && a.1 <= cell.1 + 1e-3;
        // From the 6 DIP floor's edge (two digits 14 * 6 / 13 = 6.46 DIP
        // need a 8.46 DIP column, a 83.2 DIP menu) to the review's 150 and
        // a roomy menu; 60 DIP is under the floor (fix-panels6 P3).
        for width in [60.0f32, 70.0, 80.0, 83.0, 83.3, 84.0, 90.0, 100.0, 120.0, 150.0, 160.0, 276.0] {
            let col = grid_col(width, 7);
            let (side, radius) = grid_mark(col);
            assert!(side <= 28.0 && (radius - side / 2.0).abs() < 1e-3, "a circle at {width}");
            for (m, measure) in models.iter().enumerate() {
                let drawn = grid_texts(&items, width, 7, measure);
                for rtl in [false, true] {
                    // Centres as the paint puts them (mirrored right to
                    // left); the cell a click there picks.
                    let centre = |k: usize| crate::i18n::mirror::span(rtl, 12.0 + col * (k as f32 + 0.5), 0.0, 0.0, width);
                    for (row, texts) in items.iter().zip(&drawn) {
                        let mut spans = Vec::new();
                        for (k, (t, size)) in texts.iter().enumerate() {
                            let c = centre(k);
                            assert_eq!(grid_cell(c, width, 7, rtl), Some(k));
                            assert_eq!(t, &row.cells[k], "every digit kept");
                            let cell = (c - col / 2.0, c + col / 2.0);
                            let tw = measure(t, *size);
                            let text = (c - tw / 2.0, c + tw / 2.0);
                            let mark = (c - side / 2.0, c + side / 2.0);
                            assert!(inside(text, cell), "width {width} model {m} rtl {rtl}: {t} at {size} is {tw} wide in a {col} column");
                            assert!(inside(mark, cell), "the square stays in its cell at {width}");
                            // A narrow column's square holds its number
                            // whole (its ink on the square's color).
                            if side < 28.0 && !t.is_empty() {
                                assert!(inside(text, mark), "width {width} model {m}: {t} {tw} in a {side} square");
                            }
                            spans.push((text, mark));
                        }
                        // Next to each other (either way round): no day's
                        // number under another's number or square.
                        for k in 1..spans.len() {
                            let (a, b) = (spans[k - 1], spans[k]);
                            assert!(!overlap(a.0, b.0), "numbers {k}-1/{k} meet at {width} rtl {rtl}");
                            assert!(!overlap(a.0, b.1) && !overlap(a.1, b.0), "a square over its neighbour's number at {width} rtl {rtl}");
                        }
                    }
                }
                // The 6 DIP floor holds wherever it fits (the linear model).
                let size = drawn[0].iter().find(|(t, _)| !t.is_empty()).map(|c| c.1).unwrap();
                if m == 0 && grid_day_room(col) >= linear("10", GRID_TEXT_MIN) {
                    assert!(size >= GRID_TEXT_MIN - 1e-4, "{size} at {width}");
                }
                if m == 0 && width <= 80.0 {
                    assert!(size < GRID_TEXT_MIN, "too narrow even for the floor: smaller still ({size} at {width})");
                }
            }
        }
        // The review's P2 case (150 DIP, an 18 DIP column): 13 DIP kept,
        // the square 16 DIP so the 20th's square leaves the 19th alone.
        let col = grid_col(150.0, 7);
        assert_eq!(col, 18.0);
        assert_eq!(grid_mark(col).0, 16.0);
        assert!(grid_texts(&items, 150.0, 7, &linear).iter().flatten().all(|(_, s)| *s == GRID_TEXT));
        // A roomy column keeps the usual circle.
        assert_eq!(grid_mark(36.0), (28.0, 14.0));
    }

    #[test]
    fn a_capped_menu_never_draws_a_label_over_its_shortcut() {
        // The review's case: 200 DIP, labels at 40, a 160 DIP hint and a
        // long label: they used to get 40..64 and 28..188.
        let (width, left) = (200.0f32, 40.0f32);
        let boxes = |label_w: f32, short_w: f32, sub: bool| {
            let (lr, sr) = row_columns(width, left, label_w, short_w, sub);
            let label = (left, left + label_w.min(lr));
            let right = width - RIGHT_PAD - if sub { ARROW_W } else { 0.0 };
            let short = (right - short_w.min(sr), right);
            (label, short, lr, sr)
        };
        let (label, short, lr, sr) = boxes(300.0, 160.0, false);
        assert!(lr >= LABEL_MIN && sr >= SHORTCUT_MIN, "both shown, the hint cut ({lr}, {sr})");
        for rtl in [false, true] {
            let (l0, l1) = crate::i18n::mirror::edges(rtl, label.0, label.1, 0.0, width);
            let (s0, s1) = crate::i18n::mirror::edges(rtl, short.0, short.1, 0.0, width);
            assert!(l1 + SHORTCUT_GAP <= s0 + 0.001 || s1 + SHORTCUT_GAP <= l0 + 0.001, "rtl {rtl}: {l0}..{l1} and {s0}..{s1}");
        }
        // Every case: within the menu, apart, the arrow's room kept.
        for w in [60.0f32, 120.0, 160.0, 200.0, 320.0] {
            for label_w in [0.0f32, 20.0, 80.0, 300.0] {
                for short_w in [0.0f32, 30.0, 160.0] {
                    for sub in [false, true] {
                        let (lr, sr) = row_columns(w, left, label_w, short_w, sub);
                        let used = left + lr + if sr > 0.0 { SHORTCUT_GAP + sr } else { 0.0 } + RIGHT_PAD + if sub { ARROW_W } else { 0.0 };
                        assert!(lr >= 0.0 && sr >= 0.0 && sr <= short_w, "{w} {label_w} {short_w} {sub}");
                        assert!(used <= w.max(left + RIGHT_PAD + if sub { ARROW_W } else { 0.0 }) + 0.001, "{w} {label_w} {short_w} {sub}: {used}");
                        assert!(sr == 0.0 || sr >= SHORTCUT_MIN.min(short_w), "a hint shown has room");
                        if sub {
                            assert_eq!(sr, 0.0, "the arrow, not a hint");
                        }
                    }
                }
            }
        }
        // Too narrow for both: the hint goes, the label keeps the row.
        assert_eq!(row_columns(120.0, left, 300.0, 160.0, false), (120.0 - left - RIGHT_PAD, 0.0));
        // Room for everything: as before.
        assert_eq!(row_columns(400.0, left, 100.0, 60.0, false), (400.0 - left - RIGHT_PAD - SHORTCUT_GAP - 60.0, 60.0));
    }

    #[test]
    fn a_row_taller_than_the_view_shows_its_text() {
        // The review's case: 8 DIP of room, compact 28 DIP rows from y=4,
        // text 18 tall: bands 2, a page of 4.
        let items: Vec<Item> = (1..=20).map(|i| Item::new(i, format!("row {i}"))).collect();
        let (rows, content) = row_layout(&items, Density::Compact, NO_CLOCK);
        let v = View::new(content, 8.0);
        assert_eq!((v.band, v.page()), (2.0, 4.0));
        for r in [0, 1, 10, 19] {
            let (y, h) = rows[r];
            let seen = View { offset: v.reveal(rows[r]), ..v };
            let text = (y + (h - 18.0) / 2.0, 18.0);
            let mid = y + h / 2.0 + seen.shift();
            assert!(seen.visible(text).is_some(), "row {r}: its text shows");
            assert!(mid >= seen.band && mid <= seen.height - seen.band, "row {r}: its middle in the view ({mid})");
        }
        // A page that holds the row: still revealed whole.
        let v = View::new(content, 120.0);
        let seen = View { offset: v.reveal(rows[19]), ..v };
        let (a, b) = seen.visible(rows[19]).unwrap();
        assert!((b - a - rows[19].1).abs() < 0.001);
    }

    #[test]
    fn a_narrow_slider_keeps_a_track_and_exact_key_steps() {
        // The review's case: 32 DIP wide, the track was 0 long and every
        // value mapped to 0.
        for width in [0.0f32, 1.0, 8.0, 16.0, 31.0, 32.0, 48.0, 200.0] {
            let (x0, tw) = slider_track(width);
            assert!(x0 >= 0.0 && tw > 0.0, "{width}: {x0} {tw}");
            if width >= 1.0 {
                assert!(x0 + tw <= width + 0.001, "{width}: inside the menu");
            }
            for rtl in [false, true] {
                for v in [0.0f32, 0.25, 0.55, 1.0] {
                    let x = crate::i18n::mirror::slider_x(rtl, v, x0, x0 + tw);
                    assert!((crate::i18n::mirror::slider_value(rtl, x, x0, x0 + tw) - v).abs() < 0.001, "{width} {rtl} {v}");
                }
            }
        }
        assert_eq!(slider_track(32.0), (8.0, 16.0));
        assert_eq!(slider_track(200.0), (16.0, 168.0), "a usual menu as before");
        // The keyboard's value is the step itself (set_slider), not one
        // read back from pixels.
        assert!((slider_key(false, VK_RIGHT.0, 0.5) - 0.55).abs() < 1e-6);
        assert!((slider_key(true, VK_LEFT.0, 0.5) - 0.55).abs() < 1e-6, "right to left, Left grows it");
        assert!((slider_key(false, VK_LEFT.0, 0.5) - 0.45).abs() < 1e-6);
        assert_eq!(slider_key(false, 0x24, 0.5), 0.0);
        assert_eq!(slider_key(false, 0x23, 0.5), 1.0);
        assert_eq!(slider_key(false, VK_RIGHT.0, 0.99), 1.0);
    }

    #[test]
    fn literal_labels_keep_their_ampersand_in_uia() {
        // A menu label's access key mark goes; a file name's & stays.
        assert_eq!(Item::new(1, "&Open").a11y_name(), "Open");
        assert_eq!(Item::new(1, "R&D.txt").literal().a11y_name(), "R&D.txt");
        assert_eq!(Item::new(1, "A&&B.txt").literal().a11y_name(), "A&&B.txt");
        assert_ne!(Item::new(1, "R&D.txt").literal().a11y_name(), Item::new(2, "RD.txt").literal().a11y_name());
        // A jump list group folded wholly keeps its header's name as is.
        let _l = crate::i18n::test_lock("en");
        let mut m = vec![Item::new(0, "Q&A").enabled(false).fold(2).literal()];
        m.extend((1..=5).map(|i| Item::new(i, format!("R&D {i}.txt")).fold(2).literal()));
        m.extend((100..=109).map(|i| Item::new(i, format!("own {i}"))));
        let (v, _) = fit(m, 330.0, NO_CLOCK);
        assert_eq!((v[0].label.as_str(), v[0].literal, v[0].a11y_name()), ("Q&A", true, "Q&A".to_string()));
        assert!(v[0].sub.iter().all(|i| i.literal));
    }

    #[test]
    fn hairlines_are_one_device_pixel_on_a_whole_pixel() {
        for s in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.25, 3.0] {
            for y in [0.0f32, 7.3, 12.5, 40.0, 101.7] {
                let (top, h) = hairline(y, s);
                let (tp, hp) = (top * s, h * s);
                assert!((tp - tp.round()).abs() < 1e-3, "{s} {y}: top {tp} px");
                assert!((hp - 1.0).abs() < 1e-4, "{s} {y}: {hp} px thick");
                assert!((top - y).abs() <= 0.5 / s + 1e-4, "{s} {y}: moved too far");
            }
            assert_eq!(hairline_px(10.4 * s, 0.0, s).1, 1.0);
            assert_eq!(hairline_px(10.6, 0.0, s).0, 11.0);
            // High contrast's 1 DIP lines: whole pixels, never thinner than one.
            let t = hairline_px(3.0, 1.0, s).1;
            assert!(t >= 1.0 && t == t.round(), "{s}: {t}");
        }
        assert_eq!(hairline(5.0, 0.0), (5.0, 1.0), "a broken scale falls back to 1");
    }

    #[test]
    fn menu_metrics_sit_on_the_grid_and_corners_are_concentric() {
        let grid = |v: f32| (v / 4.0).fract() == 0.0;
        for v in [ROW_H, ROW_H_COMPACT, SEP_H, SEP_H_COMPACT, HEADER_H, HEADER_H_COMPACT, PAD_Y, ROW_INSET, PLAIN_LEFT, RIGHT_PAD, CHECK_W, SLIDER_TRACK_H, SLIDER_KNOB] {
            assert!(grid(v), "{v} is off the 4 DIP grid");
        }
        assert_eq!(PLAIN_LEFT, RIGHT_PAD, "text as far in on both sides");
        // The menu's 12 and its rows' highlight 4 in: 8.
        let t = Theme::graphite();
        assert_eq!(t.radii().menu, 12.0);
        assert_eq!(crate::theme::concentric(t.radii().menu, ROW_INSET), 8.0);
        assert_eq!(crate::theme::capsule(SLIDER_TRACK_H), 2.0, "the track is a capsule");
        // The knob's room holds its E1 shadow (the further layer).
        assert!(parts::KNOB_M >= Elevation::E1.reach(), "the knob's shadow is not cut");
        // Held, it grows by KNOB_HELD inside the same room.
        assert!(parts::KNOB_M >= SLIDER_KNOB * (size::KNOB_HELD - 1.0) / 2.0 + 2.0, "nor the held knob");
        assert_eq!(SHORT_SIZE, crate::theme::ramp::FOOTNOTE.size);
        assert_eq!(HEAD_SIZE, crate::theme::ramp::FOOTNOTE.size);
        assert_eq!(FONT_SIZE, crate::theme::ramp::BODY.size);
        assert_eq!(CLOCK_DATE, crate::theme::ramp::HEADLINE.size, "the clock's date is on the ramp");
    }

    #[test]
    fn group_headers_are_short_rows_never_picked() {
        let m = vec![Item::new(0, "Recent").head(), Item::new(1, "a.txt"), Item::sep(), Item::new(2, "Quit")];
        assert!(m[0].head && !m[0].enabled && !m[0].selectable() && m[0].header());
        let (rows, h) = row_layout(&m, Density::Normal, NO_CLOCK);
        assert_eq!(rows[0].1, HEADER_H);
        assert_eq!(rows[1].1, ROW_H);
        assert_eq!(rows[2].1, SEP_H);
        assert_eq!(h, PAD_Y * 2.0 + HEADER_H + ROW_H * 2.0 + SEP_H);
        assert_eq!(row_layout(&m, Density::Compact, NO_CLOCK).0[0].1, HEADER_H_COMPACT);
        // The keyboard passes over it.
        assert_eq!(step(&m, None, 1), Some(1));
        assert_eq!(step(&m, Some(1), -1), Some(3), "wraps past the header");
        // A plain disabled info row is not a header.
        assert!(!Item::new(0, "No recent items").enabled(false).head);
    }

    #[test]
    fn trailing_glyphs_go_whole_when_the_end_column_is_cut() {
        // Room for everything: glyphs and the shortcut beside them.
        let p = end_plan(36.0 + TRAIL_GAP * 2.0 + 30.0, 36.0, 30.0);
        assert!(p.trail && (p.short - 30.0).abs() < 1e-4);
        // The review's case: row_columns left the end column nothing.
        assert_eq!(end_plan(0.0, 36.0, 0.0), EndPlan { trail: false, short: 0.0 });
        // Less than the glyphs: none of them (never over the label).
        let p = end_plan(20.0, 36.0, 0.0);
        assert!(!p.trail && p.short == 0.0);
        let p = end_plan(20.0, 36.0, 40.0);
        assert!(!p.trail && p.short == 20.0, "the shortcut gets the room");
        // No glyphs: the shortcut has the column.
        assert_eq!(end_plan(50.0, 0.0, 60.0), EndPlan { trail: false, short: 50.0 });
        // Narrow menus, both reading directions: what is drawn fits the
        // column row_columns gave (the label's room ends where it starts).
        for width in [120.0f32, 160.0, 240.0] {
            let (label_room, end_room) = row_columns(width, CHECK_W, 100.0, 36.0, false);
            let p = end_plan(end_room, 36.0, 0.0);
            let drawn = if p.trail { 36.0 } else { 0.0 };
            assert!(CHECK_W + label_room + drawn <= width - RIGHT_PAD + 1e-3, "{width}: label {label_room} + glyphs {drawn}");
        }
    }

    #[test]
    fn a_networks_glyphs_are_read_out() {
        {
            let _l = crate::i18n::test_lock("en");
            assert_eq!(trail_help(&[Glyph::Wifi(3), Glyph::Lock]), "Signal 3 of 4, Secured");
            assert_eq!(trail_help(&[Glyph::Wifi(1)]), "Signal 1 of 4, Open network");
            assert_eq!(trail_help(&[]), "");
        }
        let _k = crate::i18n::test_lock("ko");
        assert!(trail_help(&[Glyph::Wifi(4), Glyph::Lock]).contains("보안"));
    }

    #[test]
    fn the_menu_draws_no_opaque_dim_text() {
        // Design B2 review P2 #1: text_dim() is the secondary level mixed
        // onto the opaque tint, not onto what Glass really shows; every
        // menu text (rows, the clock's date, the scroll arrows) takes the
        // translucent levels the test below holds to 4.5:1.
        let src = include_str!("popup.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(!code.contains(".text_dim("), "the translucent text levels, not text_dim()");
        assert!(code.contains("parts::ink(self.theme.text_secondary())"));
    }

    #[test]
    fn menu_text_keeps_its_contrast_over_glass_on_any_wallpaper() {
        use crate::syslook::SysLook;
        use crate::theme::{contrast, over, Finish, Material, Rgba};
        // What the paint draws with (translucent levels) over the menu's
        // plate as text sees it (design B1 review P2 #1; glass v3 review
        // 3: Theme::plate_seen, the top light and the grain at their
        // bounds, every fallback level) over the greys and the full colors
        // (theme::BACKDROPS), on the menu and on its rows: rows and
        // shortcuts are body text, 4.5:1 on every finish (no large-text
        // exception); the faint headers 2.4:1 as before.
        let read = |c: Rgba, bg: Rgba| contrast(over(c, bg), bg);
        for finish in [Finish::Flat, Finish::Glass] {
            for light in [false, true] {
                let mut raw = Theme::graphite();
                raw.finish = finish;
                let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
                let rows = [State::Hover, State::Pressed, State::Selected].map(|s| t.fill_row(s));
                for (wp, need) in crate::theme::backdrops() {
                    for menu in t.plate_seen(Material::Menu, wp) {
                        let header = read(t.text_tertiary(), menu);
                        assert!(header >= crate::theme::HEADER_NEED, "{finish:?} light {light} wallpaper {wp:?}: header {header:.2}");
                        for bg in std::iter::once(menu).chain(rows.map(|r| over(r, menu))) {
                            for (what, c) in [("primary", t.text_primary()), ("shortcut", t.text_secondary())] {
                                let k = read(c, bg);
                                assert!(k >= need, "{finish:?} light {light} wallpaper {wp:?} on {bg:?}: {what} {k:.2}");
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod command_update_tests {
    use super::*;
    #[test]
    fn reserved_command_keeps_structure_and_only_changes_label_and_enabled() {
        let options = Item::new(1, "Options").submenu(vec![Item::new(2, "Child")]);
        let pending = Item::new(3, "Quit Completely").enabled(false);
        let mut rows = vec![options.clone(), Item::sep(), pending.clone()];
        for row in [Item::new(3, "Quit Completely"), Item::new(3, "Cannot Quit Completely").enabled(false), pending] {
            assert_eq!(command_update(&mut rows, &row), Some(2));
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0], options);
            assert!(rows[1].separator);
            assert_eq!(rows[2], row);
        }
        let before = rows.clone();
        assert_eq!(command_update(&mut rows, &Item::new(3, "Replacement").submenu(vec![Item::new(4, "Bad")])), None);
        assert_eq!(command_update(&mut rows, &Item::new(5, "New")), None);
        assert_eq!(rows, before);
    }
}
