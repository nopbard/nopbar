//! One note: a card on the desktop, drawn on the GPU in one Composition
//! surface like every other widget (crate::desklayer::surface): a
//! `WS_EX_NOREDIRECTIONBITMAP` popup exactly as big as its card, its
//! shadow in a click-through window right under it
//! (crate::desklayer::shade): a click beside the card reaches the desktop.
//!
//! Typing: right on the card. A windowless RichEdit (crate::notes::textbox)
//! lies on the card's text area (under the header, PAD inside the sides
//! and the foot); the card's window hands it the keys, the IME and the
//! mouse, and the card's Composition surface draws its text (Direct2D)
//! and our caret over the card's own glass, static or live: typing looks
//! like the card, only the text changes. Its text is kept plain (the card
//! sets the formats), so typing, the Korean IME, Undo and the clipboard
//! are RichEdit's own; the text is saved as before.
//!
//! Where a windowless RichEdit cannot be made or drawn (logged once), the
//! old way is the fallback: an owned popup over the text area holding a
//! transparent RichEdit window, its background the card's pixels there
//! (on a live wallpaper: the card's cover as one solid colour).
//!
//! - Moved by dragging the card (the header and the edges around the text).
//! - Two hover buttons inside the card's top right: a new note beside
//!   it and its menu (…); the date shows there otherwise.
//! - A thin scroll mark at the right edge while typing and hovered,
//!   only when the text is taller than the editor (no scroll bar).
//! - Checklist lines (crate::notes::check): a click on the box toggles
//!   it, typed "- [ ] " becomes a box, Enter on an item starts the next.
//! - A right click (or the menu key) asks the hub for the note's menu.
//!
//! Every change of state that concerns the notes as a whole (typed,
//! moved, +, Esc, Ctrl+Z with nothing left to undo, lost focus) is posted
//! to the hub (crate::notes::hub), never handled inside a nested window
//! message.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRectRgn, DeleteObject, EndPaint, 
    GetDC, GetDeviceCaps, GetUpdateRgn, InvalidateRect, ReleaseDC, ScreenToClient, SelectClipRgn, ValidateRect,
    HDC, LOGPIXELSY, 
    PAINTSTRUCT, 
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::Ime::{ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, GCS_COMPSTR};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetForegroundWindow,
    GetWindowRect, IsWindow, IsWindowVisible, LoadCursorW, PostMessageW, RegisterClassExW, SendMessageW, SetCursor, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, ES_AUTOVSCROLL, ES_MULTILINE, ES_WANTRETURN, GWLP_WNDPROC, HMENU, IDC_ARROW, IDC_HAND,
    MA_NOACTIVATE, SC_MAXIMIZE, SC_MINIMIZE, SIZE_MINIMIZED, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE, WA_INACTIVE, WINDOWPOS, WINDOW_STYLE, WM_ACTIVATE, WM_APP,
    WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_KEYDOWN,
    WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT,
    WM_PASTE, WM_PRINTCLIENT, WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS, WM_SHOWWINDOW, WM_SIZE, WM_SYSCOMMAND, WM_TIMER, WM_VSCROLL,
    WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING, WNDCLASSEXW, WNDPROC, WS_CHILD, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    WS_POPUP, WS_VISIBLE, CS_DBLCLKS, IDC_IBEAM, HTCLIENT, KillTimer, SetTimer, GetCaretBlinkTime, SystemParametersInfoW,
    SYSTEM_PARAMETERS_INFO_ACTION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_SETTEXT,
};

use super::richedit as re;
use super::textbox::TextBox;
use super::{check, Look};
use crate::desklayer::{card, surface, win as layer_win};
use crate::theme::Rgba;

pub const CLASS: PCWSTR = w!("NopBar.Note");
pub const EDITOR_CLASS: PCWSTR = w!("NopBar.NoteEditor");

/// Smallest size (DIP).
pub const MIN_W: f32 = 140.0;
pub const MIN_H: f32 = 90.0;
/// Where the text starts under the header (DIP).
pub const STRIP: f32 = card::TITLE_TOP;

const PAD: f32 = crate::desklayer::widget::PAD;
/// Posted to a note: set its text in the card's formats again (an IME
/// composition just ended).
const WM_REFORMAT: u32 = WM_APP + 2;

const ID_EDIT: i32 = 1;

/// Posted to the note itself: undo a minimize (Show desktop's).
const WM_UNMINIMIZE: u32 = WM_APP + 1;
const WM_IME_STARTCOMPOSITION: u32 = 0x010D;
const WM_IME_ENDCOMPOSITION: u32 = 0x010E;
const WM_IME_COMPOSITION: u32 = 0x010F;
const WM_INPUTLANGCHANGE: u32 = 0x0051;
/// The IME's messages (and WM_IME_SETCONTEXT .. WM_IME_KEYUP: notify,
/// control, select, char, request, keydown, keyup), handed to the card's
/// RichEdit.
fn ime_message(m: u32) -> bool {
    matches!(m, WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION | WM_IME_COMPOSITION | WM_INPUTLANGCHANGE) || (0x0281..=0x0291).contains(&m)
}
/// Our caret's blink (the card window's timer).
const CARET_TIMER: usize = 0x4352;
/// Tests (another thread): sets a note's text as if typed (WM_SETTEXT to
/// its RichEdit); lParam: UTF-16, 0-terminated.
pub const WM_TYPE_TEST: u32 = WM_APP + 5;

/// What a note tells the hub (wParam of WM_NOTE; lParam: the note).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Ev {
    Changed = 1,
    Add,
    Delete,
    Escape,
    Undo,
    Moved,
    Deactivated,
    Lost,
    /// WM_CLOSE (Alt+F4): sent back to the desktop, never destroyed.
    Close,
    /// Right click, the menu key or …: the note's menu (at menu_at).
    Menu,
}

impl Ev {
    pub fn from(v: usize) -> Option<Ev> {
        [Ev::Changed, Ev::Add, Ev::Delete, Ev::Escape, Ev::Undo, Ev::Moved, Ev::Deactivated, Ev::Lost, Ev::Close, Ev::Menu].into_iter().find(|e| *e as usize == v)
    }
}

use crate::desklayer::field::Bg;

/// Where a note's text is typed.
#[derive(Clone)]
enum Edit {
    /// Right on the card (crate::notes::textbox): no window of its own.
    Inline(Rc<TextBox>),
    /// The fallback: a RichEdit window (`edit`) in an owned popup (`pop`)
    /// over the card's text area.
    Window { pop: HWND, edit: HWND },
}

impl Edit {
    /// The window holding the keyboard and the IME while typing.
    fn hwnd(&self) -> HWND {
        match self {
            Edit::Inline(t) => t.hwnd(),
            Edit::Window { edit, .. } => *edit,
        }
    }
    fn pop(&self) -> Option<HWND> {
        match self {
            Edit::Inline(_) => None,
            Edit::Window { pop, .. } => Some(*pop),
        }
    }
    fn inline(&self) -> Option<&Rc<TextBox>> {
        match self {
            Edit::Inline(t) => Some(t),
            Edit::Window { .. } => None,
        }
    }
    /// Its text can still be read (a fallback editor goes before its note).
    fn live(&self) -> bool {
        match self {
            Edit::Inline(_) => true,
            Edit::Window { edit, .. } => unsafe { IsWindow(Some(*edit)) }.as_bool(),
        }
    }
}

impl re::Target for Edit {
    fn send(&self, msg: u32, wp: usize, lp: isize) -> isize {
        match self {
            Edit::Inline(t) => re::Target::send(t.as_ref(), msg, wp, lp),
            Edit::Window { edit, .. } => re::Target::send(edit, msg, wp, lp),
        }
    }
}

struct Win {
    glass: Option<surface::Glass>,
    id: String,
    edit: Edit,
    /// The caret shows now (it blinks while typing goes on).
    blink: bool,
    /// When the caret last moved or the note took the keyboard: it stops
    /// blinking (and stays) a while after.
    blink_since: Option<Instant>,
    /// Where the click that opened typing was (client px): the caret goes
    /// there once typing opens.
    click: Option<POINT>,
    hover: bool,
    /// The editor is open (the note has the keyboard).
    active: bool,
    widget_editing: bool,
    /// Dragged (moved since the press): drawn lifted.
    lifted: bool,
    /// Our own edit (a toggle, a conversion, setting the text): not typing.
    busy: bool,
    /// The WM_CHAR that follows a key we handled (Enter, Esc) is dropped.
    skip_char: bool,
    /// Destroyed by us (deleted, quitting) rather than by someone else.
    closing: bool,
    /// An IME composition is open (WM_IME_STARTCOMPOSITION .. END).
    ime: bool,
    /// When its text last changed (unix seconds), shown on its face.
    date: Option<i64>,
    /// The editor's text as last seen (display form): what a destroyed
    /// note leaves (its owned editor goes first).
    text: String,
    bg: Option<Bg>,
    editor_light: Option<bool>,
}

thread_local! {
    static LOOK: RefCell<Look> = RefCell::new(Look::default());
    static WINS: RefCell<HashMap<isize, Win>> = RefCell::new(HashMap::new());
    /// The RichEdit's own window procedure (subclassed).
    static PREV: RefCell<HashMap<isize, isize>> = RefCell::new(HashMap::new());
    /// Editor popups and their RichEdits: the note they belong to.
    static OWNER: RefCell<HashMap<isize, isize>> = RefCell::new(HashMap::new());
    /// Notes destroyed by someone else: (id, Markdown), for the hub.
    static LOST: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// A move of ours: its DPI change keeps the size we gave.
    static OWN_MOVE: Cell<bool> = const { Cell::new(false) };
    /// The hub of this thread (set by it before any note exists).
    static HUB_HERE: Cell<isize> = const { Cell::new(0) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
    /// Where the last menu was asked for (screen px; None: by key).
    static MENU_AT: Cell<Option<(i32, i32)>> = const { Cell::new(None) };
    /// Full colour (summoned, or the desktop itself in front).
    static VIVID: Cell<bool> = const { Cell::new(false) };
    /// A windowless RichEdit could not be made or drawn here: notes use
    /// the editor window (logged once).
    static INLINE_BROKEN: Cell<bool> = const { Cell::new(false) };
}

/// Typing goes on the card itself (false: the fallback editor window).
fn inline_ok() -> bool {
    !INLINE_BROKEN.with(|b| b.get())
}

/// Typing on the card failed here (`why`): logged once, and every note
/// made from now on types in the editor window.
fn inline_broken(why: &str) {
    if !INLINE_BROKEN.with(|b| b.replace(true)) {
        crate::log!("notes: typing on the card unavailable ({why}); the editor window is used");
    }
}

/// Tests: notes made from now on type on the card (`on`) or in the
/// fallback editor window.
#[cfg(test)]
pub fn set_test_inline(on: bool) {
    INLINE_BROKEN.with(|b| b.set(!on));
    super::textbox::set_test_refuse(!on);
}

fn edit_of(hwnd: HWND) -> Option<Edit> {
    with(hwnd, |w| w.edit.clone())
}

/// Summoned or the desktop in front (`v`), or back behind other apps:
/// every face drawn again.
pub fn set_vivid(v: bool) {
    if VIVID.with(|x| x.replace(v)) == v {
        return;
    }
    let all: Vec<isize> = WINS.with(|w| w.try_borrow().map(|m| m.keys().copied().collect()).unwrap_or_default());
    for h in all {
        unsafe {
            let _ = InvalidateRect(Some(HWND(h as *mut _)), None, false);
        }
    }
}

/// When note `hwnd`'s text last changed (unix seconds; its file's time).
pub fn set_date(hwnd: HWND, unix: i64) {
    with(hwnd, |w| w.date = Some(unix));
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Pure: the civil date (year, month, day) of day `z` since 1970-01-01
/// (Howard Hinnant's days_from_civil, inverted).
pub fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Pure: the day since 1970-01-01 of a civil date (days_from_civil).
pub fn days(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `unix` (seconds) as a local SYSTEMTIME: the offset Windows applies now
/// (local minus UTC), then the civil date.
fn local_time(unix: i64) -> windows::Win32::Foundation::SYSTEMTIME {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let (l, u) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let mins = |t: &windows::Win32::Foundation::SYSTEMTIME| days(t.wYear as i64, t.wMonth as u32, t.wDay as u32) * 1440 + t.wHour as i64 * 60 + t.wMinute as i64;
    let off = mins(&l) - mins(&u);
    let t = unix + off.clamp(-1440, 1440) * 60;
    let (y, m, d) = civil(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    windows::Win32::Foundation::SYSTEMTIME { wYear: y as u16, wMonth: m as u16, wDay: d as u16, wHour: (s / 3600) as u16, wMinute: (s / 60 % 60) as u16, ..Default::default() }
}

/// A note's date as its face shows it: the time today, else the day.
fn date_text(unix: i64) -> String {
    let t = local_time(unix);
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    if (t.wYear, t.wMonth, t.wDay) == (now.wYear, now.wMonth, now.wDay) {
        crate::i18n::date::time(&t)
    } else {
        crate::i18n::date::month_day(&t)
    }
}

/// Pretends note `hwnd` has the keyboard (its editor opens, not shown),
/// for pictures.
#[cfg(test)]
pub fn set_test_active(hwnd: HWND, on: bool) {
    if on {
        open_editor(hwnd, false);
    } else {
        close_editor(hwnd);
    }
}

/// The face (the note drawn, not its editor) shows: nobody types in it.
fn shows_face(hwnd: HWND) -> bool {
    with(hwnd, |w| !w.active || w.widget_editing).unwrap_or(true)
}

/// Asks the hub for the note's menu at `at` (screen px; None: by key).
fn ask_menu(note: HWND, at: Option<POINT>) {
    MENU_AT.with(|m| m.set(at.map(|p| (p.x, p.y))));
    post(Ev::Menu, note);
}

/// WM_CONTEXTMENU's point (-1, -1: from the keyboard).
fn context_point(lp: LPARAM) -> Option<POINT> {
    let p = lparam_pt(lp);
    (p.x != -1 || p.y != -1).then_some(p)
}

/// Where the note's menu goes (screen px): where it was asked for, or
/// under the header's left end when asked by key.
pub fn menu_at(hwnd: HWND) -> POINT {
    if let Some((x, y)) = MENU_AT.with(|m| m.get()) {
        return POINT { x, y };
    }
    let r = card(hwnd);
    let s = scale(hwnd);
    POINT { x: r.left + px(PAD, s), y: r.top + px(STRIP, s) }
}

pub fn set_look(l: Look) {
    LOOK.with(|x| *x.borrow_mut() = l);
}

pub fn look() -> Look {
    LOOK.with(|x| x.borrow().clone())
}

fn with<R>(hwnd: HWND, f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    WINS.with(|w| w.try_borrow_mut().ok()?.get_mut(&(hwnd.0 as isize)).map(f))
}

pub fn is_note(hwnd: HWND) -> bool {
    WINS.with(|w| w.try_borrow().map(|m| m.contains_key(&(hwnd.0 as isize))).unwrap_or(false))
}

/// The note an editor popup or its RichEdit belongs to.
fn owner(h: HWND) -> HWND {
    HWND(OWNER.with(|o| o.borrow().get(&(h.0 as isize)).copied()).unwrap_or(0) as *mut _)
}

/// Runs `f` with DPI changes from it keeping the size it sets.
pub fn own_move<R>(f: impl FnOnce() -> R) -> R {
    let before = OWN_MOVE.with(|o| o.replace(true));
    let r = f();
    OWN_MOVE.with(|o| o.set(before));
    r
}

pub fn take_lost() -> Vec<(String, String)> {
    LOST.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

pub(super) fn colorref(c: Rgba) -> COLORREF {
    COLORREF(c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16)
}

/// Note windows of this thread are drawn at scale `s` (tests only).
#[cfg(test)]
pub fn set_test_scale(s: Option<f32>) {
    crate::desklayer::surface::set_test_scale(s);
}

pub fn scale(hwnd: HWND) -> f32 {
    surface::scale(hwnd)
}

pub(super) fn px(v: f32, s: f32) -> i32 {
    (v * s).round() as i32
}


/// Note `hwnd`'s card (screen px): its window.
pub fn card(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    r
}

/// Puts note `hwnd`'s card at `r` (screen px; its window around it).
pub fn set_card(hwnd: HWND, r: RECT) {
    own_move(|| unsafe {
        let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
    });
}

/// Pure: the editor's area on a card `w` x `h` DIP (left, top, right,
/// bottom): under the header, PAD inside the sides and the foot.
#[cfg(test)]
pub fn text_rect(w: f32, h: f32) -> (f32, f32, f32, f32) {
    (PAD, STRIP, (w - PAD).max(PAD + 1.), (h - PAD).max(STRIP + 1.))
}

/// Pure: text_rect() in whole pixels on a card `card` px big at `s`,
/// never outside the card (review 4: a small card at 400 % has no room;
/// the area is then empty).
pub fn text_px(card: (i32, i32), s: f32) -> RECT {
    let (w, h) = (card.0.max(0), card.1.max(0));
    let left = px(PAD, s).min(w);
    let top = (px(STRIP, s) - title_nudge(s)).clamp(0, h);
    RECT { left, top, right: (w - px(PAD, s)).clamp(left, w), bottom: (h - px(PAD, s)).clamp(top, h) }
}

/// Pure: how far up the editor sits (px) so its title lies on the card's:
/// RichEdit sets a line of exact height 2 to 4 px lower than DirectWrite
/// centres it (measured: graphite_snapshots::typing_title_lies_on_the_cards_title).
pub fn title_nudge(s: f32) -> i32 {
    ((1.5 * s).round() as i32).max(3)
}

/// Pure: a card `card` px big at `s` has room to type in (a line of body
/// text, a few characters wide); else its editor does not open.
pub fn editable(card: (i32, i32), s: f32) -> bool {
    let t = text_px(card, s);
    t.right - t.left >= px(24., s) && t.bottom - t.top >= px(card::BODY.height, s)
}

/// The note that window `h` is part of: the note itself, its editor or
/// the editor's RichEdit (review 6: the keyboard is in the editor while
/// typing, and that is still the note).
pub fn note_of(h: HWND) -> Option<HWND> {
    if h.is_invalid() {
        return None;
    }
    if is_note(h) {
        return Some(h);
    }
    let n = owner(h);
    is_note(n).then_some(n)
}

/// A note's fallback editor popup and its RichEdit (tests); None while it
/// types on the card.
#[cfg(test)]
pub fn editor_windows(h: HWND) -> Option<(HWND, HWND)> {
    with(h, |w| w.edit.pop().map(|p| (p, w.edit.hwnd()))).flatten()
}

/// Note `h` types right on its card (tests).
#[cfg(test)]
pub fn types_inline(h: HWND) -> bool {
    with(h, |w| w.edit.inline().is_some()).unwrap_or(false)
}

/// Pure: every corner of `r` lies inside the card `w` x `h` with corners
/// `radius` (all DIP).
#[cfg(test)]
pub fn inside_rounded(r: (f32, f32, f32, f32), w: f32, h: f32, radius: f32) -> bool {
    let corner_ok = |x: f32, y: f32| {
        if x < 0. || y < 0. || x > w || y > h {
            return false;
        }
        let cx = x.clamp(radius, w - radius);
        let cy = y.clamp(radius, h - radius);
        let (dx, dy) = (x - cx, y - cy);
        dx * dx + dy * dy <= radius * radius
    };
    corner_ok(r.0, r.1) && corner_ok(r.2, r.1) && corner_ok(r.0, r.3) && corner_ok(r.2, r.3)
}

/// Pure: the thin scroll mark (top, height) in a view `view` px tall
/// showing `doc` px scrolled by `at`, at least `min` tall. None when it
/// all fits.
pub fn scroll_mark(view: i32, doc: i32, at: i32, min: i32) -> Option<(i32, i32)> {
    if view <= 0 || doc <= view + 1 {
        return None;
    }
    let h = ((view as i64 * view as i64 / doc as i64) as i32).max(min).min(view);
    let travel = (view - h).max(0);
    let f = (at.max(0) as f32 / (doc - view) as f32).clamp(0.0, 1.0);
    Some(((f * travel as f32).round() as i32, h))
}

/// The hub that notes on this thread report to (review #11: its own
/// handle, not the one published to other threads).
pub fn set_hub(hub: HWND) {
    HUB_HERE.with(|h| h.set(hub.0 as isize));
}

fn post(ev: Ev, hwnd: HWND) {
    let hub = HWND(HUB_HERE.with(|h| h.get()) as *mut _);
    if !hub.is_invalid() {
        unsafe {
            let _ = PostMessageW(Some(hub), super::hub::WM_NOTE, WPARAM(ev as usize), LPARAM(hwnd.0 as isize));
        }
    }
}

fn register() {
    if REGISTERED.with(|r| r.get()) {
        return;
    }
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else { return };
        for (class, f) in [(CLASS, proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT), (EDITOR_CLASS, editor_proc)] {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                // A double click selects a word in the text typed on the card.
                style: if class == CLASS { CS_DBLCLKS } else { Default::default() },
                lpfnWndProc: Some(f),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        }
        // RichEdit 4.1+ (RICHEDIT50W); kept loaded.
        let _ = LoadLibraryW(w!("msftedit.dll"));
    }
    REGISTERED.with(|r| r.set(true));
}

/// The window alone, exactly its card at `r` (screen px): a popup drawn by
/// Composition only, no frame, no region, no child window, its shadow in a
/// click-through window of its own (crate::desklayer::shade).
pub fn create_frame(r: RECT) -> Option<HWND> {
    register();
    let wr = r;
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let title = HSTRING::from(crate::i18n::t("notes.window"));
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW,
            CLASS,
            &title,
            WS_POPUP,
            wr.left,
            wr.top,
            (wr.right - wr.left).max(1),
            (wr.bottom - wr.top).max(1),
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .ok()?;
        // Its shadow: a click-through window right under it.
        crate::desklayer::shade::attach(hwnd);
        Some(hwnd)
    }
}

/// A new note window (hidden), its card at `r`, with editor text `shown`
/// (check::to_display). The hub places and shows it.
pub fn create(id: &str, r: RECT, shown: &str) -> Option<HWND> {
    let hwnd = create_frame(r)?;
    let edit = if inline_ok() {
        match TextBox::new(hwnd, false, on_notify) {
            Ok(t) => Some(Edit::Inline(Rc::new(t))),
            Err(e) => {
                inline_broken(&e.0);
                None
            }
        }
    } else {
        None
    };
    let edit = match edit {
        Some(e) => e,
        None => match window_editor(hwnd) {
            Some(e) => e,
            None => {
                crate::log!("notes: cannot make a note's editor (RichEdit missing?)");
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return None;
            }
        },
    };
    WINS.with(|w| {
        w.borrow_mut().insert(
            hwnd.0 as isize,
            Win {
                glass: None,
                id: id.to_string(),
                edit: edit.clone(),
                blink: true,
                blink_since: None,
                click: None,
                hover: false,
                active: false,
                widget_editing: false,
                lifted: false,
                busy: true,
                skip_char: false,
                closing: false,
                ime: false,
                date: Some(now_unix()),
                text: shown.to_string(),
                bg: None,
                editor_light: None,
            },
        )
    });
    // Rich text so the title and the body each have the card's format;
    // the formats are ours alone (format_text), the text kept plain. Undo
    // with many levels, any script; text first, events after, so loading
    // is not typing.
    re::send(&edit, re::EM_SETTEXTMODE, (re::TM_RICHTEXT | re::TM_MULTILEVELUNDO | re::TM_MULTICODEPAGE) as usize, 0);
    re::send(&edit, re::EM_SETMARGINS, re::EC_LEFTMARGIN | re::EC_RIGHTMARGIN, 0);
    // The card's font for every script, never one RichEdit picks itself.
    let lang = re::send(&edit, re::EM_GETLANGOPTIONS, 0, 0) as u32;
    re::send(&edit, re::EM_SETLANGOPTIONS, 0, (lang & !(re::IMF_AUTOFONT | re::IMF_DUALFONT)) as isize);
    re::send(&edit, re::EM_SETUNDOLIMIT, 100, 0);
    style(hwnd);
    re::set_text(&edit, shown);
    re::send(&edit, re::EM_SETEVENTMASK, 0, (re::ENM_CHANGE | re::ENM_SCROLL) as isize);
    with(hwnd, |w| w.busy = false);
    format_text(hwnd);
    if let Edit::Window { edit, .. } = edit {
        unsafe {
            let prev = SetWindowLongPtrW(edit, GWLP_WNDPROC, child_proc as *const () as usize as isize);
            PREV.with(|p| p.borrow_mut().insert(edit.0 as isize, prev));
        }
    }
    place_editor(hwnd, false);
    crate::desklayer::hover::attach(hwnd);
    crate::desklayer::backdrop::listen(hwnd);
    if !super::headless() {
        remember(hwnd, crate::desklayer::edit::Preview::Note(shown.to_string(), String::new()));
    }
    Some(hwnd)
}

/// The fallback editor of note `hwnd`: an owned popup holding a
/// transparent RichEdit window, kept out of captures with its note.
fn window_editor(hwnd: HWND) -> Option<Edit> {
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let pop = CreateWindowExW(WS_EX_TOOLWINDOW, EDITOR_CLASS, w!(""), WS_POPUP, 0, 0, 1, 1, Some(hwnd), None, Some(hinst.into()), None).ok()?;
        let edit = CreateWindowExW(
            WS_EX_TRANSPARENT,
            re::CLASS,
            w!(""),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32),
            0,
            0,
            1,
            1,
            Some(pop),
            Some(HMENU(ID_EDIT as isize as *mut _)),
            Some(hinst.into()),
            None,
        );
        let Ok(edit) = edit else {
            let _ = DestroyWindow(pop);
            return None;
        };
        OWNER.with(|o| {
            let mut o = o.borrow_mut();
            o.insert(pop.0 as isize, hwnd.0 as isize);
            o.insert(edit.0 as isize, hwnd.0 as isize);
        });
        // The typed text is kept out of captures with its note (review 5).
        crate::desklayer::privacy::register(pop, false);
        Some(Edit::Window { pop, edit })
    }
}

/// Typing on note `hwnd`'s card could not be drawn: it types in the
/// fallback editor window from now on, its text, selection and keyboard
/// carried over (Undo starts again).
fn fall_back(hwnd: HWND) {
    let Some(Edit::Inline(old)) = edit_of(hwnd) else { return };
    let Some(edit) = window_editor(hwnd) else { return };
    let text = re::text(old.as_ref());
    let (a, b) = re::sel(old.as_ref());
    let had_focus = old.has_focus();
    old.set_focus(false);
    with(hwnd, |w| {
        w.edit = edit.clone();
        w.busy = true;
    });
    re::send(&edit, re::EM_SETTEXTMODE, (re::TM_RICHTEXT | re::TM_MULTILEVELUNDO | re::TM_MULTICODEPAGE) as usize, 0);
    re::send(&edit, re::EM_SETMARGINS, re::EC_LEFTMARGIN | re::EC_RIGHTMARGIN, 0);
    let lang = re::send(&edit, re::EM_GETLANGOPTIONS, 0, 0) as u32;
    re::send(&edit, re::EM_SETLANGOPTIONS, 0, (lang & !(re::IMF_AUTOFONT | re::IMF_DUALFONT)) as isize);
    re::send(&edit, re::EM_SETUNDOLIMIT, 100, 0);
    style(hwnd);
    re::set_text(&edit, &text);
    re::set_sel(&edit, a, b);
    re::send(&edit, re::EM_SETEVENTMASK, 0, (re::ENM_CHANGE | re::ENM_SCROLL) as isize);
    with(hwnd, |w| w.busy = false);
    format_text(hwnd);
    if let Edit::Window { edit: e, .. } = edit {
        unsafe {
            let prev = SetWindowLongPtrW(e, GWLP_WNDPROC, child_proc as *const () as usize as isize);
            PREV.with(|p| p.borrow_mut().insert(e.0 as isize, prev));
        }
    }
    // The text box goes once nothing of ours holds it (not inside its draw).
    drop(old);
    let open = with(hwnd, |w| std::mem::replace(&mut w.active, false)).unwrap_or(false);
    if open && open_editor(hwnd, true) && had_focus {
        unsafe {
            let _ = SetFocus(Some(edit.hwnd()));
        }
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// RichEdit's notifications from a note's text typed on its card.
fn on_notify(hwnd: HWND, code: u32) {
    if code == re::EN_CHANGE {
        on_change(hwnd);
    }
}

/// The note's id.
#[cfg(test)]
pub fn id_of(hwnd: HWND) -> Option<String> {
    with(hwnd, |w| w.id.clone())
}

/// The note's text as stored (Markdown, "\n" line breaks).
/// None: the note is gone: never an empty text standing in for it.
pub fn markdown(hwnd: HWND) -> Option<String> {
    let (edit, kept) = with(hwnd, |w| (w.edit.clone(), w.text.clone()))?;
    Some(check::to_markdown(&if edit.live() { re::text(&edit) } else { kept }))
}

pub fn replace_markdown(hwnd: HWND, text: &str) {
    if let Some(e) = edit_of(hwnd) {
        let shown = check::to_display(text);
        re::set_text(&e, &shown);
        with(hwnd, |w| w.text = shown);
        format_text(hwnd);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

/// Keyboard into the note's text (the caller made it the foreground):
/// typing opens on the card (or the fallback editor over it).
pub fn focus(hwnd: HWND) {
    if !open_editor(hwnd, true) {
        return;
    }
    let Some(e) = edit_of(hwnd) else { return };
    match &e {
        Edit::Inline(t) => unsafe {
            if windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() != hwnd {
                let _ = SetFocus(Some(hwnd));
            }
            t.set_focus(true);
            // The click that opened typing puts the caret where it was.
            if let Some(p) = with(hwnd, |w| w.click.take()).flatten() {
                let lp = LPARAM(((p.y as u16 as isize) << 16) | p.x as u16 as isize);
                t.send_raw(WM_LBUTTONDOWN, WPARAM(1), lp);
                t.send_raw(WM_LBUTTONUP, WPARAM(0), lp);
            }
            wake_caret(hwnd);
        },
        Edit::Window { edit, .. } => unsafe {
            let _ = SetFocus(Some(*edit));
        },
    }
}

/// Closes the note (deleted, or quitting): not "lost".
pub fn destroy(hwnd: HWND) {
    with(hwnd, |w| w.closing = true);
    crate::desklayer::icons::forget(hwnd);
    // Through the pin: its hide is ours.
    layer_win::placing(|| unsafe {
        let _ = DestroyWindow(hwnd);
    });
}

/// The look changed (theme, language): colors, font, names.
pub fn restyle(hwnd: HWND) {
    with(hwnd, |w| {
        w.glass = None;
        w.bg = None;
    });
    style(hwnd);
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(crate::i18n::t("notes.window")));
    }
    if !shows_face(hwnd) {
        open_editor(hwnd, true);
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn logpy() -> i32 {
    thread_local!(static L: Cell<i32> = const { Cell::new(0) });
    let v = L.with(|l| l.get());
    if v > 0 {
        return v;
    }
    let v = unsafe {
        let dc = GetDC(None);
        let v = GetDeviceCaps(Some(dc), LOGPIXELSY);
        ReleaseDC(None, dc);
        v
    };
    L.with(|l| l.set(v.max(96)));
    v.max(96)
}

/// The theme the cards are drawn in (the notes' look decides light/dark).
fn theme() -> crate::theme::Theme {
    let mut t = super::panel_theme();
    t.is_light = look().light;
    t
}

fn theme_at(hwnd: HWND) -> crate::theme::Theme {
    let mut theme = theme();
    let under = crate::desklayer::backdrop::under_for(hwnd, card(hwnd));
    theme.is_light = card::theme_for(&theme, under.as_ref()).is_light;
    theme
}

/// How a run of the editor's text is set: the card's title (20/700,
/// first grey) or body (14, second grey), in the card's font.
fn char_format(hwnd: HWND, title: bool) -> re::CharFormat2 {
    let dpi = (scale(hwnd) * 96.).round() as u32;
    let t = theme_at(hwnd);
    let c = card(hwnd);
    let l = if title { card::title_line((c.right - c.left) as f32 / scale(hwnd)) } else { card::BODY };
    let mut cf = re::CharFormat2 {
        // Italic, underline and strikeout off: a run never keeps them.
        mask: re::CFM_COLOR | re::CFM_FACE | re::CFM_SIZE | re::CFM_WEIGHT | re::CFM_BOLD | re::CFM_ITALIC | re::CFM_UNDERLINE | re::CFM_STRIKEOUT,
        effects: if title { re::CFE_BOLD } else { 0 },
        height: re::twips(l.size, dpi, logpy()),
        weight: l.weight.0 as u16,
        color: colorref(card::ink(&t, if title { 1 } else { 2 })).0,
        ..Default::default()
    };
    // The card's font (the built-in Pretendard added for GDI too).
    let family = crate::fonts::gdi_face(&crate::fonts::map(crate::fonts::THEME_DEFAULT));
    for (i, u) in family.encode_utf16().take(31).enumerate() {
        cf.face[i] = u;
    }
    cf
}

/// The title's and the body's lines: as tall as the card's (26 and 20),
/// 4 DIP between the title and the body (card::BODY_TOP).
fn para_format(hwnd: HWND, title: bool) -> re::ParaFormat2 {
    let dpi = (scale(hwnd) * 96.).round() as u32;
    let line = if title { card::TITLE_LINE } else { card::BODY.height };
    let after = if title { card::BODY_TOP - card::TITLE_TOP - line } else { 0. };
    re::ParaFormat2 {
        mask: re::PFM_LINESPACING | re::PFM_SPACEAFTER | re::PFM_SPACEBEFORE,
        line_spacing: re::twips(line, dpi, logpy()),
        line_spacing_rule: re::LINE_EXACT,
        space_after: re::twips(after, dpi, logpy()),
        ..Default::default()
    }
}

/// The editor's look: the body's format for new text, then the card's
/// formats on what is there.
fn style(hwnd: HWND) {
    let Some(edit) = edit_of(hwnd) else { return };
    let th = theme_at(hwnd);
    with(hwnd, |w| w.editor_light = Some(th.is_light));
    let cf = char_format(hwnd, false);
    if let Some(t) = edit.inline() {
        // The text's colour, the selection a grey band (no accent colour).
        t.set_colors(colorref(card::ink(&th, 2)), colorref(selection(&th)));
        t.set_default_format(&cf);
    }
    re::send(&edit, re::EM_SETCHARFORMAT, re::SCF_DEFAULT as usize, &cf as *const _ as isize);
    format_text(hwnd);
}

/// The selection's band on the card: the ink's grey, faint, over the
/// card's colour (achromatic).
fn selection(t: &crate::theme::Theme) -> Rgba {
    let base = card::surface(t.is_light);
    let ink = card::ink(t, 1);
    let mix = |a: u8, b: u8| (a as f32 * 0.72 + b as f32 * 0.28).round() as u8;
    Rgba { r: mix(base.r, ink.r), g: mix(base.g, ink.g), b: mix(base.b, ink.b), a: 255 }
}

/// Pure: where the title ends (UTF-16, its paragraph break included).
pub fn title_end(text: &[u16]) -> usize {
    text.iter().position(|&c| c == '\r' as u16 || c == '\n' as u16).map_or(text.len(), |i| i + 1)
}

/// The card's formats forced on the text: the first paragraph the title,
/// the rest the body (pasted or typed formats never stay). Out of the
/// Undo history and drawn once; never during an IME composition (it is
/// set again when the composition ends).
fn format_text(hwnd: HWND) {
    let Some((edit, ime, busy)) = with(hwnd, |w| (w.edit.clone(), w.ime, w.busy)) else { return };
    if ime || composing(&edit) {
        return;
    }
    with(hwnd, |w| w.busy = true);
    let text: Vec<u16> = re::text(&edit).encode_utf16().collect();
    let end = title_end(&text) as i32;
    let set = || {
        let (a, b) = re::sel(&edit);
        for (from, to, title) in [(0, end, true), (end, -1, false)] {
            if title || (end as usize) < text.len() || text.is_empty() {
                re::set_sel(&edit, from, to);
                let cf = char_format(hwnd, title);
                let pf = para_format(hwnd, title);
                re::send(&edit, re::EM_SETCHARFORMAT, re::SCF_SELECTION as usize, &cf as *const _ as isize);
                re::send(&edit, re::EM_SETPARAFORMAT, 0, &pf as *const _ as isize);
            }
        }
        re::set_sel(&edit, a, b);
    };
    match re::Tom::of(&edit) {
        Some(tom) => tom.quietly(set),
        None => set(),
    }
    with(hwnd, |w| w.busy = busy);
}

/// Pure: a key that pastes (Ctrl+V, Ctrl+Shift+V, Shift+Insert).
pub fn is_paste(vk: u32, ctrl: bool, shift: bool, alt: bool) -> bool {
    !alt && ((vk == 0x56 && ctrl) || (vk == 0x2D && shift && !ctrl))
}

/// Pastes the clipboard's text only (no formats, no pictures).
fn paste_text(edit: impl re::Target) {
    re::send(edit, re::EM_PASTESPECIAL, re::CF_UNICODETEXT, 0);
}

use crate::desklayer::surface::gfx;
#[cfg(test)]
pub(super) use crate::desklayer::surface::render;

/// What one note shows, for frame().
pub struct Frame<'a> {
    pub text: &'a str,
    /// Its date, shown in the header while no button shows.
    pub date: &'a str,
    /// Its editor is open: the header only, the editor shows the text.
    pub editing: bool,
    /// The text being typed on the card (drawn by its RichEdit).
    pub typing: Option<Typing<'a>>,
    /// The hover buttons' opacity.
    pub alpha: f32,
    /// The scroll mark (top, height) in DIP from the text's top.
    pub mark: Option<(f32, f32)>,
    /// The wallpaper under the card (crate::desklayer::backdrop).
    pub under: Option<&'a crate::desklayer::backdrop::Under>,
    /// Being dragged: a little larger (its deeper shadow is under it,
    /// crate::desklayer::shade).
    pub lift: bool,
    /// With its shadow (pictures); a live card's is its shade window.
    pub shadow: bool,
}

/// The text typed on a card, for frame().
pub struct Typing<'a> {
    text: &'a TextBox,
    /// The card's px per DIP.
    scale: f32,
    /// The caret shows in this frame (it blinks).
    caret: bool,
}

/// A note's card at (0, 0), `w` x `h` DIP: the live surface, the editor's
/// background and every picture draw it.
pub unsafe fn frame(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, gfx: &crate::gfx::Gfx, theme: &crate::theme::Theme, f: &Frame<'_>, w: f32, h: f32) -> windows::core::Result<()> {
    if f.lift {
        card::lifted(dc, w, h);
    }
    if f.shadow {
        card::plate(dc, theme, w, h, false, f.under)?;
    } else {
        card::body(dc, theme, w, h, f.under)?;
    }
    let theme = card::theme_for(theme, f.under);
    let date = if f.alpha > 0. { "" } else { f.date };
    if f.editing {
        card::header(dc, gfx, theme, card::Kind::Note, crate::i18n::t("widgets.notes"), date, w)?;
        if let Some(t) = &f.typing {
            typed(dc, theme, t)?;
        }
    } else {
        card::note(dc, gfx, theme, f.text, date, w, h)?;
    }
    card::controls(dc, theme, card::Kind::Note, w, f.alpha)?;
    if let Some((top, mh)) = f.mark {
        let c = card::ink(theme, 3);
        crate::companion::chat::paint::rounded(dc, crate::companion::chat::paint::rect(w - 8., STRIP + top, 3., mh), 1.5, card::rgba(c))?;
    }
    Ok(())
}

/// The typed text on the card's glass, clipped to its area, and the
/// caret: RichEdit draws in the card's px (the context's transform is
/// DIP, so it is scaled back for the call). A failed Direct2D draw is
/// remembered on the text box (redraw falls back), never the card's error.
unsafe fn typed(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, theme: &crate::theme::Theme, t: &Typing<'_>) -> windows::core::Result<()> {
    use windows::Win32::Graphics::Direct2D::{Common::D2D_RECT_F, ID2D1RenderTarget, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE};
    let s = t.scale.max(0.01);
    let r = t.text.rect();
    let clip = D2D_RECT_F { left: r.left as f32 / s, top: r.top as f32 / s, right: r.right as f32 / s, bottom: r.bottom as f32 / s };
    dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
    let mut m = windows_numerics::Matrix3x2::identity();
    dc.GetTransform(&mut m);
    let px = windows_numerics::Matrix3x2 { M11: m.M11 / s, M12: m.M12 / s, M21: m.M21 / s, M22: m.M22 / s, M31: m.M31, M32: m.M32 };
    dc.SetTransform(&px);
    let aa = dc.GetTextAntialiasMode();
    // Grey antialiasing as the card's own text (no ClearType on glass).
    dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    let rt: &ID2D1RenderTarget = dc;
    let drawn = t.text.draw(rt);
    dc.SetTextAntialiasMode(aa);
    let caret = match (t.caret, t.text.caret()) {
        (true, Some(c)) => {
            let ink = card::ink(theme, 1);
            let w = (c.w as f32).max(1.0f32.max(s.round()));
            crate::companion::chat::paint::rounded(dc, crate::companion::chat::paint::rect(c.x as f32, c.y as f32, w, c.h as f32), 0., card::rgba(ink))
        }
        _ => Ok(()),
    };
    dc.SetTransform(&m);
    dc.PopAxisAlignedClip();
    if let Err(e) = drawn {
        t.text.set_failed(&e);
    }
    caret
}

/// The view of the text being typed: where it starts in the RichEdit's
/// own coordinates (the card's px on the card, the window's 0, 0 in the
/// fallback) and how tall it is (px).
fn view(edit: &Edit) -> (POINT, i32) {
    match edit {
        Edit::Inline(t) => {
            let r = t.rect();
            (POINT { x: r.left, y: r.top }, r.bottom - r.top)
        }
        Edit::Window { edit, .. } => {
            let mut er = RECT::default();
            unsafe {
                let _ = GetClientRect(*edit, &mut er);
            }
            (POINT::default(), er.bottom)
        }
    }
}

/// The scroll mark of an open editor (DIP), while hovered.
fn mark_of(hwnd: HWND) -> Option<(f32, f32)> {
    let (edit, hover) = with(hwnd, |w| (w.edit.clone(), w.hover && w.active))?;
    if !hover {
        return None;
    }
    let s = scale(hwnd);
    let (origin, height) = view(&edit);
    let n = re::len(&edit);
    let last = re::pos_of(&edit, (n - 1).max(0));
    let at = re::scroll_pos(&edit).y;
    let doc = at + last.y - origin.y + (card::BODY.height * s).round() as i32;
    let (top, h) = scroll_mark(height, doc, at, px(16.0, s))?;
    Some((top as f32 / s, h as f32 / s))
}

/// The note's state, as frame() takes it.
struct Shown {
    lift: bool,
    text: String,
    date: String,
    editing: bool,
    typing: Option<(Rc<TextBox>, bool)>,
    alpha: f32,
    mark: Option<(f32, f32)>,
}

fn frame_of(hwnd: HWND) -> Option<Shown> {
    let (text, date, editing, lift, edit, blink) = with(hwnd, |w| (w.text.clone(), w.date.map(date_text).unwrap_or_default(), w.active && !w.widget_editing, w.lifted, w.edit.clone(), w.blink))?;
    let typing = edit.inline().filter(|_| editing).map(|t| (t.clone(), blink));
    Some(Shown { lift, text, date, editing, typing, alpha: crate::desklayer::hover::alpha(hwnd), mark: mark_of(hwnd) })
}

/// Edit mode learns what note `hwnd` shows: the note (its id, its saved
/// size) and `preview`.
fn remember(hwnd: HWND, preview: crate::desklayer::edit::Preview) {
    let Some(id) = with(hwnd, |w| w.id.clone()) else { return };
    crate::desklayer::edit::remember(hwnd, crate::desklayer::edit::Key::Note(id), super::hub::note_size(hwnd), preview);
}

/// Draws the note's card again in its window's surface.
fn redraw(hwnd: HWND) {
    if super::headless() {
        return;
    }
    let Some(gfx) = gfx() else { return };
    let Some(Shown { lift, text, date, editing, typing, alpha, mark }) = frame_of(hwnd) else { return };
    if lift { return; }
    if !editing {
        remember(hwnd, crate::desklayer::edit::Preview::Note(text.clone(), date.clone()));
    }
    let s = scale(hwnd);
    let c = card(hwnd);
    let size = (c.right - c.left, c.bottom - c.top);
    let theme = theme();
    let stale = with(hwnd, |w| w.glass.as_ref().is_none_or(|g| g.card != size || g.dpi != s)).unwrap_or(false);
    if stale {
        // One target per window: the old one goes first.
        with(hwnd, |w| w.glass = None);
        match surface::Glass::new(Some(hwnd), gfx, size, s) {
            Ok(g) => {
                with(hwnd, |w| w.glass = Some(g));
            }
            Err(e) => {
                crate::log!("notes: no card surface ({e})");
                return;
            }
        }
    }
    let under = crate::desklayer::backdrop::under_for(hwnd, c);
    let glass = card::glass_for(&theme, under.as_ref());
    let light = glass.map_or(theme.is_light, |g| g.light);
    if editing && with(hwnd, |w| w.editor_light != Some(light)).unwrap_or(false) {
        style(hwnd);
    }
    let typing_ref = typing.as_ref().map(|(t, caret)| Typing { text: t, scale: s, caret: *caret });
    let f = Frame { text: &text, date: &date, editing, typing: typing_ref, alpha, mark, under: under.as_ref(), lift: false, shadow: false };
    crate::desklayer::shade::set(hwnd, light, glass.is_some(), false);
    let (w, h) = (size.0 as f32 / s, size.1 as f32 / s);
    let drawn = with(hwnd, |win| win.glass.as_ref().map(|g| {
        g.draw(|dc, gfx| unsafe { frame(dc, gfx, &theme, &f, w, h) })?;
        g.root.SetIsVisible(true)
    }));
    if let Some(Some(Err(e))) = drawn {
        crate::log!("notes: card not drawn ({e})");
    }
    // RichEdit could not draw on the card: the fallback editor from now on.
    let failed = typing.as_ref().and_then(|(t, _)| t.failed());
    drop(f);
    drop(typing);
    if let Some(why) = failed {
        inline_broken(&why);
        fall_back(hwnd);
    }
}

/// The card's pixels under the fallback editor (the same glass, cut
/// out); on a live wallpaper (no picture to cut): the card's cover as one
/// solid colour.
fn editor_bg(hwnd: HWND) -> Option<Bg> {
    let s = scale(hwnd);
    let c = card(hwnd);
    let t = theme();
    let cut = text_px((c.right - c.left, c.bottom - c.top), s);
    if crate::desklayer::live::enabled() && !t.high_contrast && !t.transparency_off {
        let solid = cover_solid(t.is_light);
        let (w, h) = ((cut.right - cut.left).max(1), (cut.bottom - cut.top).max(1));
        let px: Vec<u8> = std::iter::repeat_n([solid.b, solid.g, solid.r, 255], (w * h) as usize).flatten().collect();
        return unsafe { crate::desklayer::field::dib(&px, w, h) }.map(|bmp| Bg { bmp, w, h });
    }
    let under = crate::desklayer::backdrop::under_for(hwnd, c);
    crate::desklayer::field::piece(c, s, &t, cut, under.as_ref())
}

/// Pure: the card's thickest cover (dark 55 %, light 72 %) as one solid
/// colour: its tint over the card's own colour.
pub fn cover_solid(light: bool) -> Rgba {
    let (_, a) = card::cover_range(light);
    let tint = card::tint(light);
    let base = card::surface(light);
    let mix = |t: u8, b: u8| (t as f32 * a + b as f32 * (1. - a)).round() as u8;
    Rgba { r: mix(tint.r, base.r), g: mix(tint.g, base.g), b: mix(tint.b, base.b), a: 255 }
}

/// Paints the fallback editor's background (note `note`) on `dc` at its
/// origin.
unsafe fn paint_bg(note: HWND, dc: HDC) {
    let bg = with(note, |w| w.bg.as_ref().map(|b| (b.bmp, b.w, b.h))).flatten();
    let light = with(note, |w| w.editor_light).flatten().unwrap_or_else(|| look().light);
    crate::desklayer::field::paint(dc, bg, light);
}

/// Opens typing on the card (`show`: on screen; pictures keep a fallback
/// editor hidden). False: it cannot open (edit mode, no room).
fn open_editor(hwnd: HWND, show: bool) -> bool {
    let Some((open, editing, has_bg, inline)) = with(hwnd, |w| (w.active, w.widget_editing, w.bg.is_some(), w.edit.inline().is_some())) else { return false };
    if editing {
        return false;
    }
    // No room to type (a tiny card on a dense display): the face stays.
    let c = card(hwnd);
    if !editable((c.right - c.left, c.bottom - c.top), scale(hwnd)) {
        return false;
    }
    let light = theme_at(hwnd).is_light;
    if with(hwnd, |w| w.editor_light != Some(light)).unwrap_or(false) {
        style(hwnd);
    }
    if !inline && (!has_bg || !open) {
        let bg = editor_bg(hwnd);
        with(hwnd, |w| w.bg = bg);
    }
    with(hwnd, |w| w.active = true);
    place_editor(hwnd, show);
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    true
}

/// Closes typing: the face shows again.
fn close_editor(hwnd: HWND) {
    let Some((edit, was)) = with(hwnd, |w| (w.edit.clone(), std::mem::replace(&mut w.active, false))) else { return };
    if was {
        let text = re::text(&edit);
        with(hwnd, |w| w.text = text);
    }
    match &edit {
        Edit::Inline(t) => {
            t.set_focus(false);
            unsafe {
                let _ = KillTimer(Some(hwnd), CARET_TIMER);
            }
        }
        Edit::Window { pop, .. } => layer_win::placing(|| unsafe {
            if IsWindowVisible(*pop).as_bool() {
                let _ = ShowWindow(*pop, SW_HIDE);
            }
        }),
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Typing's area: the card's text area. The fallback editor goes over it,
/// right above the note in the z-order (never above other programs), its
/// RichEdit filling it.
fn place_editor(hwnd: HWND, show: bool) {
    let Some((edit, open)) = with(hwnd, |w| (w.edit.clone(), w.active)) else { return };
    let s = scale(hwnd);
    let c = card(hwnd);
    let t = text_px((c.right - c.left, c.bottom - c.top), s);
    let (pop, edit) = match edit {
        Edit::Inline(tb) => {
            tb.set_rect(inline_rect(t, s));
            return;
        }
        Edit::Window { pop, edit } => (pop, edit),
    };
    if !open {
        return;
    }
    let (x, y, w, h) = (c.left + t.left, c.top + t.top, t.right - t.left, t.bottom - t.top);
    let visible = show || unsafe { IsWindowVisible(pop) }.as_bool();
    layer_win::placing(|| unsafe {
        let _ = SetWindowPos(edit, None, 0, 0, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        let flags = SWP_NOACTIVATE | SWP_NOOWNERZORDER | if visible { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
        let _ = SetWindowPos(pop, None, x, y, w, h, flags | SWP_NOZORDER);
        if visible {
            above(hwnd, pop);
        }
    });
}

/// Puts editor `pop` right above note `note`.
fn above(note: HWND, pop: HWND) {
    crate::desklayer::field::above(note, pop);
}

/// A picture of note `hwnd`'s card (no shadow): (w, h, premultiplied
/// BGRA), the text being typed included while typing is open. For tests.
#[cfg(test)]
pub fn picture(hwnd: HWND) -> Option<(i32, i32, Vec<u8>)> {
    let Shown { text, date, editing, typing, .. } = frame_of(hwnd)?;
    let s = scale(hwnd);
    let c = card(hwnd);
    let size = (c.right - c.left, c.bottom - c.top);
    let theme = theme();
    let under = crate::desklayer::backdrop::under_for(hwnd, c);
    let typing_ref = typing.as_ref().map(|(t, caret)| Typing { text: t, scale: s, caret: *caret });
    let f = Frame { text: &text, date: &date, editing, typing: typing_ref, alpha: 0., mark: None, under: under.as_ref(), lift: false, shadow: false };
    let (w, h) = (size.0 as f32 / s, size.1 as f32 / s);
    let mut pixels = render(size.0, size.1, s, |dc, gfx| unsafe { frame(dc, gfx, &theme, &f, w, h) })?;
    if let (true, Some(Edit::Window { pop, .. })) = (editing, edit_of(hwnd)) {
        let t = text_px(size, s);
        let typed = print(pop, t.right - t.left, t.bottom - t.top)?;
        for y in 0..(t.bottom - t.top) {
            for x in 0..(t.right - t.left) {
                let from = ((y * (t.right - t.left) + x) * 4) as usize;
                let to = (((t.top + y) * size.0 + t.left + x) * 4) as usize;
                pixels[to..to + 3].copy_from_slice(&typed[from..from + 3]);
                pixels[to + 3] = 255;
            }
        }
    }
    Some((size.0, size.1, pixels))
}

/// WM_PRINT of window `h` (its children too) into `w` x `h` BGRA.
#[cfg(test)]
fn print(h: HWND, w: i32, ht: i32) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, CreateDIBSection, DeleteDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
    use windows::Win32::UI::WindowsAndMessaging::{PRF_CHILDREN, PRF_CLIENT, PRF_ERASEBKGND, WM_PRINT};
    unsafe {
        let dc = CreateCompatibleDC(None);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -ht, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let old = SelectObject(dc, bmp.into());
        SendMessageW(h, WM_PRINT, Some(WPARAM(dc.0 as usize)), Some(LPARAM((PRF_CLIENT | PRF_CHILDREN | PRF_ERASEBKGND) as isize)));
        let out = std::slice::from_raw_parts(bits as *const u8, (w * ht * 4) as usize).to_vec();
        SelectObject(dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(dc);
        Some(out)
    }
}

/// Pure: where the RichEdit typed on the card lays its text (card px):
/// the text area as the fallback editor window had it (the same GDI
/// layout, its title nudge included), so typing starts where the face's
/// text lies.
pub fn inline_rect(t: RECT, _s: f32) -> RECT {
    t
}

/// The caret moved or typing just opened: it shows, and blinks again for
/// a while.
fn wake_caret(hwnd: HWND) {
    with(hwnd, |w| {
        w.blink = true;
        w.blink_since = Some(Instant::now());
    });
    let ms = unsafe { GetCaretBlinkTime() };
    if ms != 0 && ms != u32::MAX {
        unsafe {
            SetTimer(Some(hwnd), CARET_TIMER, ms, None);
        }
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// How long the caret blinks after the last key or click (Windows' caret
/// timeout, 5 s by default); then it stays shown.
fn caret_timeout() -> std::time::Duration {
    const SPI_GETCARETTIMEOUT: u32 = 0x2022;
    let mut ms: u32 = 0;
    let ok = unsafe { SystemParametersInfoW(SYSTEM_PARAMETERS_INFO_ACTION(SPI_GETCARETTIMEOUT), 0, Some(&mut ms as *mut u32 as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) }.is_ok();
    std::time::Duration::from_millis(if ok && ms > 0 { ms as u64 } else { 5000 })
}

/// The caret's blink timer: shown / hidden, until the timeout (then it
/// stays and the timer stops).
fn blink(hwnd: HWND) {
    let done = with(hwnd, |w| {
        let over = w.blink_since.is_none_or(|t| t.elapsed() >= caret_timeout());
        w.blink = over || !w.blink;
        over || !w.active
    })
    .unwrap_or(true);
    if done {
        unsafe {
            let _ = KillTimer(Some(hwnd), CARET_TIMER);
        }
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn track_leave(h: HWND) {
    let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
    unsafe {
        let _ = TrackMouseEvent(&mut t);
    }
}

fn hover_in(note: HWND, under: HWND) {
    let changed = with(note, |w| !std::mem::replace(&mut w.hover, true)).unwrap_or(false);
    if !crate::desklayer::grab::moving(note) {
        track_leave(under);
    }
    if changed {
        unsafe {
            let _ = InvalidateRect(Some(note), None, false);
        }
    }
}

fn hover_out(note: HWND) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    let r = card(note);
    if p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom {
        return;
    }
    if with(note, |w| std::mem::replace(&mut w.hover, false)).unwrap_or(false) {
        unsafe {
            let _ = InvalidateRect(Some(note), None, false);
        }
    }
}

/// An IME composition is open in the note's editor (review #8): its
/// keys are the IME's.
fn composing_in(note: HWND, edit: &Edit) -> bool {
    with(note, |w| w.ime).unwrap_or(false) || composing(edit)
}

fn composing(edit: &Edit) -> bool {
    let h = edit.hwnd();
    if h.is_invalid() {
        return false;
    }
    unsafe {
        let himc = ImmGetContext(h);
        if himc.is_invalid() {
            return false;
        }
        let n = ImmGetCompositionStringW(himc, GCS_COMPSTR, None, 0);
        let _ = ImmReleaseContext(h, himc);
        n > 0
    }
}

/// The text changed: tell the hub; typed Markdown at the caret's line
/// start becomes a box.
fn on_change(hwnd: HWND) {
    let Some((edit, busy)) = with(hwnd, |w| (w.edit.clone(), w.busy)) else { return };
    let edit = &edit;
    if busy {
        return;
    }
    post(Ev::Changed, hwnd);
    with(hwnd, |w| w.date = Some(now_unix()));
    if !composing_in(hwnd, edit) {
        let (a, b) = re::sel(edit);
        if a == b {
            let (start, text) = para(edit, a);
            if let Some((s, e, rep)) = check::autoconvert(&text) {
                let caret = (a - start) as usize;
                if caret >= e {
                    with(hwnd, |w| w.busy = true);
                    re::replace(edit, start + s as i32, start + e as i32, &rep);
                    let after = a - (e - s) as i32 + rep.encode_utf16().count() as i32;
                    re::set_sel(edit, after, after);
                    with(hwnd, |w| w.busy = false);
                }
            }
        }
    }
    format_text(hwnd);
    let text = re::text(edit);
    with(hwnd, |w| w.text = text);
    if crate::a11y::wanted(hwnd) {
        publish(hwnd);
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn line_px(hwnd: HWND) -> i32 {
    (card::BODY.height * scale(hwnd)).round() as i32
}

/// The logical line (paragraph, up to its break) holding position `cp`
/// and where it starts: wrapping on screen never splits a list item
/// (review #9).
fn para(edit: &Edit, cp: i32) -> (i32, String) {
    let all: Vec<u16> = re::text(edit).encode_utf16().collect();
    let (s, e) = check::paragraph(&all, cp.max(0) as usize);
    (s as i32, String::from_utf16_lossy(&all[s..e]))
}

/// The box under edit-client point `p`: (its position, checked).
fn box_at(note: HWND, edit: &Edit, p: POINT) -> Option<(i32, bool)> {
    let cp = re::char_at(edit, p);
    let (start, text) = para(edit, cp);
    let (col, checked) = check::marker(&text)?;
    let g = start + col as i32;
    let p0 = re::pos_of(edit, g);
    let p1 = re::pos_of(edit, g + 1);
    let lh = line_px(note);
    let x1 = if p1.y == p0.y && p1.x > p0.x { p1.x } else { p0.x + lh * 2 / 3 };
    let slack = (2.0 * scale(note)).round() as i32;
    (p.x >= p0.x - slack && p.x <= x1 + slack && p.y >= p0.y && p.y < p0.y + lh).then_some((g, checked))
}

fn toggle_box(note: HWND, edit: &Edit, g: i32, checked: bool) {
    let (sa, sb) = re::sel(edit);
    with(note, |w| w.busy = true);
    re::replace(edit, g, g + 1, &check::glyph(!checked).to_string());
    re::set_sel(edit, sa, sb);
    with(note, |w| w.busy = false);
    let text = re::text(edit);
    with(note, |w| w.text = text);
    format_text(note);
    post(Ev::Changed, note);
}

fn lparam_pt(lp: LPARAM) -> POINT {
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
}

fn ctrl_only() -> bool {
    unsafe { GetKeyState(0x11) < 0 && GetKeyState(0x10) >= 0 && GetKeyState(0x12) >= 0 }
}

fn no_mods() -> bool {
    unsafe { GetKeyState(0x11) >= 0 && GetKeyState(0x10) >= 0 && GetKeyState(0x12) >= 0 }
}

/// What a key does in the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// RichEdit (and the IME) have it.
    Pass,
    /// Back to the desktop (summoned) or done typing.
    Escape,
    /// Ctrl+Z with nothing left to undo here: a deleted note comes back.
    Undo,
    /// Enter: the checklist may continue or end.
    Enter,
}

/// Pure: who takes key `vk`. While an IME composition is open nothing is
/// taken: Esc cancels the composition, Enter commits it (review #8).
pub fn key_action(vk: u32, composing: bool, ctrl_only: bool, no_mods: bool, can_undo: bool) -> Key {
    if composing {
        return Key::Pass;
    }
    match vk {
        0x1B if no_mods => Key::Escape,
        0x5A if ctrl_only && !can_undo => Key::Undo,
        0x0D if no_mods => Key::Enter,
        _ => Key::Pass,
    }
}

/// Keys the note handles before RichEdit: Some(answer) when handled.
fn edit_key(note: HWND, edit: &Edit, vk: u32) -> Option<LRESULT> {
    if !matches!(vk, 0x1B | 0x5A | 0x0D) {
        return None;
    }
    let can_undo = vk == 0x5A && re::send(edit, re::EM_CANUNDO, 0, 0) != 0;
    match key_action(vk, composing_in(note, edit), ctrl_only(), no_mods(), can_undo) {
        Key::Pass => None,
        Key::Escape => {
            with(note, |w| w.skip_char = true);
            post(Ev::Escape, note);
            Some(LRESULT(0))
        }
        Key::Undo => {
            post(Ev::Undo, note);
            Some(LRESULT(0))
        }
        Key::Enter => {
            let (a, b) = re::sel(edit);
            if a != b {
                return None;
            }
            let (start, text) = para(edit, a);
            match check::on_enter(&text, (a - start).max(0) as usize) {
                check::Enter::Normal => None,
                check::Enter::Continue(prefix) => {
                    with(note, |w| w.skip_char = true);
                    re::replace(edit, a, a, &format!("\r{prefix}"));
                    Some(LRESULT(0))
                }
                check::Enter::EndList { start: s, end: e } => {
                    with(note, |w| w.skip_char = true);
                    re::replace(edit, start + s as i32, start + e as i32, "");
                    Some(LRESULT(0))
                }
            }
        }
    }
}

/// Paints the editor's background over the part of RichEdit `h` about
/// to be painted (its text is drawn transparent over it).
unsafe fn erase_update(note: HWND, h: HWND) {
    let rgn = CreateRectRgn(0, 0, 0, 0);
    if GetUpdateRgn(h, rgn, false).0 > 1 {
        let dc = GetDC(Some(h));
        SelectClipRgn(dc, Some(rgn));
        paint_bg(note, dc);
        ReleaseDC(Some(h), dc);
    }
    let _ = DeleteObject(rgn.into());
}

/// The editor's RichEdit: hover for the note, its own keys and clicks,
/// the card's pixels under its transparent text.
unsafe extern "system" fn child_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let prev = PREV.with(|p| p.borrow().get(&(h.0 as isize)).copied()).unwrap_or(0);
    let call = || {
        // SAFETY: `prev` is the window procedure SetWindowLongPtrW returned.
        let f: WNDPROC = std::mem::transmute::<isize, WNDPROC>(prev);
        CallWindowProcW(f, h, msg, wp, lp)
    };
    if prev == 0 {
        return DefWindowProcW(h, msg, wp, lp);
    }
    let note = owner(h);
    let edit = edit_of(note).unwrap_or(Edit::Window { pop: HWND::default(), edit: h });
    let edit = &edit;
    match msg {
        WM_ERASEBKGND => {
            paint_bg(note, HDC(wp.0 as *mut _));
            LRESULT(1)
        }
        WM_PAINT => {
            erase_update(note, h);
            call()
        }
        WM_PRINTCLIENT => {
            paint_bg(note, HDC(wp.0 as *mut _));
            call()
        }
        WM_MOUSEMOVE => {
            hover_in(note, h);
            call()
        }
        WM_MOUSELEAVE => {
            hover_out(note);
            // The card's hover fade (crate::desklayer::hover) too.
            SendMessageW(note, WM_MOUSELEAVE, None, None);
            call()
        }
        WM_NCDESTROY => {
            PREV.with(|p| p.borrow_mut().remove(&(h.0 as isize)));
            call()
        }
        WM_RBUTTONUP => {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            ask_menu(note, Some(p));
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            ask_menu(note, context_point(lp));
            LRESULT(0)
        }
        WM_IME_STARTCOMPOSITION => {
            with(note, |w| w.ime = true);
            call()
        }
        WM_IME_ENDCOMPOSITION => {
            with(note, |w| w.ime = false);
            let r = call();
            // The composed text in the card's format, once it is in.
            let _ = PostMessageW(Some(note), WM_REFORMAT, WPARAM(0), LPARAM(0));
            r
        }
        // Text only: what is pasted takes the card's format.
        WM_PASTE => {
            paste_text(h);
            LRESULT(0)
        }
        WM_KEYDOWN if is_paste(wp.0 as u32, GetKeyState(0x11) < 0, GetKeyState(0x10) < 0, GetKeyState(0x12) < 0) => {
            paste_text(h);
            LRESULT(0)
        }
        WM_KEYDOWN => match edit_key(note, edit, wp.0 as u32) {
            Some(r) => r,
            None => {
                let r = call();
                if with(note, |w| w.hover).unwrap_or(false) {
                    let _ = InvalidateRect(Some(note), None, false);
                }
                r
            }
        },
        WM_CHAR if (wp.0 == 0x0D || wp.0 == 0x1B) && with(note, |w| std::mem::replace(&mut w.skip_char, false)).unwrap_or(false) => LRESULT(0),
        WM_LBUTTONDOWN if composing_in(note, edit) => call(),
        WM_LBUTTONDOWN => match box_at(note, edit, lparam_pt(lp)) {
            Some((g, checked)) => {
                toggle_box(note, edit, g, checked);
                LRESULT(0)
            }
            None => call(),
        },
        WM_SETCURSOR => {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(h, &mut p);
            if box_at(note, edit, p).is_some() {
                SetCursor(LoadCursorW(None, IDC_HAND).ok());
                LRESULT(1)
            } else {
                call()
            }
        }
        // Scrolled: the background is not shifted with the text.
        WM_MOUSEWHEEL | WM_VSCROLL => {
            let r = call();
            let _ = InvalidateRect(Some(h), None, true);
            let _ = InvalidateRect(Some(note), None, false);
            r
        }
        _ => call(),
    }
}

/// The editor popup: its RichEdit's notifications go to the note; it
/// closes when the keyboard goes to anything but its note.
unsafe extern "system" fn editor_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let note = owner(h);
    match msg {
        WM_COMMAND if !note.is_invalid() => SendMessageW(note, WM_COMMAND, Some(wp), Some(lp)),
        WM_ERASEBKGND => {
            paint_bg(note, HDC(wp.0 as *mut _));
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(h, &mut ps);
            paint_bg(note, dc);
            let _ = EndPaint(h, &ps);
            LRESULT(0)
        }
        WM_PRINTCLIENT => {
            paint_bg(note, HDC(wp.0 as *mut _));
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let inactive = (wp.0 & 0xFFFF) as u32 == WA_INACTIVE;
            if inactive && HWND(lp.0 as *mut _) != note {
                // The keyboard went elsewhere: done typing.
                close_editor(note);
                post(Ev::Deactivated, note);
            } else if !inactive {
                if let Some(e) = edit_of(note).map(|e| e.hwnd()) {
                    let _ = SetFocus(Some(e));
                }
            }
            LRESULT(0)
        }
        WM_SETFOCUS => {
            if let Some(e) = edit_of(note).map(|e| e.hwnd()) {
                let _ = SetFocus(Some(e));
            }
            LRESULT(0)
        }
        // Right above its note: an activation's "to the top" never takes
        // it above other programs.
        WM_WINDOWPOSCHANGING => {
            if super::hub::layer() == crate::desklayer::layer::Layer::Desktop {
                crate::desklayer::field::pin(note, h, &mut *(lp.0 as *mut WINDOWPOS));
            }
            DefWindowProcW(h, msg, wp, lp)
        }
        WM_CLOSE => {
            post(Ev::Close, note);
            LRESULT(0)
        }
        WM_DESTROY => {
            OWNER.with(|o| o.borrow_mut().remove(&(h.0 as isize)));
            crate::desklayer::privacy::forget(h);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

/// The point `lp` (screen) in card DIP.
fn card_point(hwnd: HWND, p: POINT) -> (f32, f32) {
    let r = card(hwnd);
    let s = scale(hwnd);
    ((p.x - r.left) as f32 / s, (p.y - r.top) as f32 / s)
}

/// The hover button under the cursor, while the buttons show.
fn button_under_cursor(hwnd: HWND) -> Option<card::Btn> {
    if crate::desklayer::hover::alpha(hwnd) < 0.5 {
        return None;
    }
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    let (x, y) = card_point(hwnd, p);
    let r = card(hwnd);
    card::button_at(card::Kind::Note, (r.right - r.left) as f32 / scale(hwnd), x, y)
}

/// Client point `p` lies on the card's text area.
fn in_text(hwnd: HWND, p: POINT) -> bool {
    let c = card(hwnd);
    let t = text_px((c.right - c.left, c.bottom - c.top), scale(hwnd));
    p.x >= t.left && p.x < t.right && p.y >= t.top && p.y < t.bottom
}

/// The window's message for the text typed on its card: Some when it is
/// that text's (the keys, the IME, the mouse on the text, RichEdit's
/// timers, our caret's).
unsafe fn typed_message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if msg == WM_TIMER && wp.0 == CARET_TIMER {
        blink(hwnd);
        return Some(LRESULT(0));
    }
    let edit = edit_of(hwnd)?;
    let t = edit.inline()?.clone();
    let forward = || {
        let (handled, r) = t.send_raw(msg, wp, lp);
        if handled {
            r
        } else {
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    };
    match msg {
        WM_TIMER if wp.0 >= super::textbox::TIMER_BASE => {
            t.send_raw(msg, WPARAM(wp.0 - super::textbox::TIMER_BASE), lp);
            return Some(LRESULT(0));
        }
        WM_KILLFOCUS => {
            t.set_focus(false);
            let _ = KillTimer(Some(hwnd), CARET_TIMER);
            return None;
        }
        WM_CAPTURECHANGED if t.captured() => {
            t.lose_capture();
            t.send_raw(msg, wp, lp);
            return Some(LRESULT(0));
        }
        _ => {}
    }
    if shows_face(hwnd) {
        return None;
    }
    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let p = lparam_pt(lp);
            if !in_text(hwnd, p) {
                return None;
            }
            if !composing_in(hwnd, &edit) {
                if let Some((g, checked)) = box_at(hwnd, &edit, p) {
                    toggle_box(hwnd, &edit, g, checked);
                    return Some(LRESULT(0));
                }
            }
            wake_caret(hwnd);
            t.send_raw(msg, wp, lp);
            Some(LRESULT(0))
        }
        // A selection being dragged.
        WM_MOUSEMOVE if t.captured() => {
            hover_in(hwnd, hwnd);
            t.send_raw(msg, wp, lp);
            Some(LRESULT(0))
        }
        WM_LBUTTONUP if t.captured() => {
            t.send_raw(msg, wp, lp);
            Some(LRESULT(0))
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(hwnd, &mut p);
            if !in_text(hwnd, p) {
                return None;
            }
            let shape = if box_at(hwnd, &edit, p).is_some() { IDC_HAND } else { IDC_IBEAM };
            SetCursor(LoadCursorW(None, shape).ok());
            Some(LRESULT(1))
        }
        // Scrolled: the scroll mark follows.
        WM_MOUSEWHEEL => {
            let r = forward();
            let _ = InvalidateRect(Some(hwnd), None, false);
            Some(r)
        }
        WM_KEYDOWN => {
            wake_caret(hwnd);
            let vk = wp.0 as u32;
            if is_paste(vk, GetKeyState(0x11) < 0, GetKeyState(0x10) < 0, GetKeyState(0x12) < 0) {
                paste_text(&edit);
                return Some(LRESULT(0));
            }
            Some(edit_key(hwnd, &edit, vk).unwrap_or_else(forward))
        }
        WM_KEYUP => Some(forward()),
        WM_CHAR => {
            if (wp.0 == 0x0D || wp.0 == 0x1B) && with(hwnd, |w| std::mem::replace(&mut w.skip_char, false)).unwrap_or(false) {
                return Some(LRESULT(0));
            }
            wake_caret(hwnd);
            Some(forward())
        }
        // Text only: what is pasted takes the card's format.
        WM_PASTE => {
            paste_text(&edit);
            Some(LRESULT(0))
        }
        m if ime_message(m) => {
            if m == WM_IME_STARTCOMPOSITION {
                with(hwnd, |w| w.ime = true);
            }
            let r = forward();
            if m == WM_IME_ENDCOMPOSITION {
                with(hwnd, |w| w.ime = false);
                // The composed text in the card's format, once it is in.
                let _ = PostMessageW(Some(hwnd), WM_REFORMAT, WPARAM(0), LPARAM(0));
            }
            if m == WM_IME_COMPOSITION {
                wake_caret(hwnd);
            }
            Some(r)
        }
        _ => None,
    }
}

/// What a screen reader reads of note `hwnd`: one text field holding all
/// of its text (its value), focused while it is typed in.
fn publish(hwnd: HWND) {
    let Some((edit, kept, typing)) = with(hwnd, |w| (w.edit.clone(), w.text.clone(), w.active && !w.widget_editing)) else { return };
    let text = if edit.live() { re::text(&edit) } else { kept };
    let text = text.replace('\r', "\n");
    let c = card(hwnd);
    let t = text_px((c.right - c.left, c.bottom - c.top), scale(hwnd));
    let rect = RECT { left: c.left + t.left, top: c.top + t.top, right: c.left + t.right, bottom: c.top + t.bottom };
    let name = crate::i18n::t("notes.window").to_string();
    let node = crate::a11y::Node::new("text", crate::a11y::Role::Edit, name.clone(), rect).value(text);
    crate::a11y::publish(
        hwnd,
        crate::a11y::Tree { role: crate::a11y::Role::Group, name, nodes: vec![node], focus: typing.then(|| "text".to_string()), focusable: true, active: typing, focus_in: None },
    );
}

/// What a screen reader asked of note `hwnd`: its text set (as one
/// change, saved like typing), the keyboard into it.
fn a11y_actions(hwnd: HWND) {
    for (key, act) in crate::a11y::take(hwnd) {
        match act {
            crate::a11y::Act::SetValue => {
                if let (Some(v), Some(edit)) = (crate::a11y::take_value(hwnd, &key), edit_of(hwnd)) {
                    let all = re::len(&edit);
                    re::replace(&edit, 0, all, &v.replace("\r\n", "\r").replace('\n', "\r"));
                    format_text(hwnd);
                }
            }
            crate::a11y::Act::Focus => unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                focus(hwnd);
            },
            _ => {}
        }
        publish(hwnd);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(r) = typed_message(hwnd, msg, wp, lp) {
        return r;
    }
    match msg {
        WM_WINDOWPOSCHANGING => {
            layer_win::pin(super::hub::layer(), hwnd, &mut *(lp.0 as *mut WINDOWPOS));
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_WINDOWPOSCHANGED => {
            let p = &*(lp.0 as *const WINDOWPOS);
            if !p.flags.contains(SWP_NOMOVE) || !p.flags.contains(SWP_NOSIZE) || !p.flags.contains(SWP_NOZORDER) {
                place_editor(hwnd, false);
            }
            // Moved or resized: another piece of the wallpaper under it.
            if !p.flags.contains(SWP_NOSIZE) || !p.flags.contains(SWP_NOMOVE) {
                let _ = InvalidateRect(Some(hwnd), None, false);
                // The fallback editor's background: the piece under it now.
                if !crate::desklayer::grab::moving(hwnd) && with(hwnd, |w| w.active).unwrap_or(false) {
                    refresh_editor_bg(hwnd);
                }
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SHOWWINDOW if wp.0 == 0 => {
            close_editor(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SIZE => {
            if wp.0 as u32 == SIZE_MINIMIZED {
                // Show desktop / minimize-all: a note stays on the desktop.
                let _ = PostMessageW(Some(hwnd), WM_UNMINIMIZE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_UNMINIMIZE => {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            LRESULT(0)
        }
        WM_SYSCOMMAND if matches!((wp.0 as u32) & 0xFFF0, SC_MINIMIZE | SC_MAXIMIZE) => LRESULT(0),
        // A press on + or … does not take the keyboard; nor one on the face
        // (it may be a drag): its click does, when let go.
        WM_MOUSEACTIVATE if button_under_cursor(hwnd).is_some() || shows_face(hwnd) => LRESULT(MA_NOACTIVATE as isize),
        WM_ACTIVATE => {
            let active = (wp.0 & 0xFFFF) as u32 != WA_INACTIVE;
            let pop = edit_of(hwnd).and_then(|e| e.pop());
            if active {
                focus(hwnd);
            } else if pop != Some(HWND(lp.0 as *mut _)) {
                close_editor(hwnd);
                post(Ev::Deactivated, hwnd);
            }
            LRESULT(0)
        }
        WM_SETFOCUS => {
            if !shows_face(hwnd) {
                focus(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            hover_in(hwnd, hwnd);
            crate::desklayer::grab::moved(hwnd);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            hover_out(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            match button_under_cursor(hwnd) {
                Some(card::Btn::Add) => post(Ev::Add, hwnd),
                Some(card::Btn::More) => {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    ask_menu(hwnd, Some(p));
                }
                // Anywhere else on the card (the text being typed is the
                // text's): a click, or a drag past 6 px.
                // Its saved size, fixed for the drag (read from its px
                // once when it has none).
                None => crate::desklayer::grab::press(hwnd, super::hub::note_size(hwnd)),
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            match crate::desklayer::grab::release(hwnd) {
                crate::desklayer::grab::Up::Dropped => {}
                // A click (no move): typing starts, as before; on the text,
                // the caret goes where it was clicked.
                crate::desklayer::grab::Up::Click => {
                    let p = lparam_pt(lp);
                    if shows_face(hwnd) && in_text(hwnd, p) {
                        with(hwnd, |w| w.click = Some(p));
                    }
                    if GetForegroundWindow() == hwnd {
                        focus(hwnd);
                    } else {
                        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                    }
                }
                crate::desklayer::grab::Up::None | crate::desklayer::grab::Up::Suppressed => {}
            }
            LRESULT(0)
        }
        crate::desklayer::drag_hint::LIFTED => {
            with(hwnd, |w| w.lifted = true);
            close_editor(hwnd);
            with(hwnd, |w| {
                if let Some(glass) = &w.glass {
                    let _ = glass.root.SetIsVisible(false);
                }
            });
            crate::desklayer::shade::set(hwnd, false, false, true);
            if let Some(gfx) = gfx() {
                crate::desklayer::drag_hint::hidden(hwnd, wp, &gfx.compositor);
            }
            LRESULT(0)
        }
        crate::desklayer::drag_hint::LANDED => {
            with(hwnd, |w| w.lifted = false);
            redraw(hwnd);
            if let Some(gfx) = gfx() {
                crate::desklayer::drag_hint::presented(hwnd, wp, &gfx.compositor);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            crate::desklayer::grab::cancel(hwnd);
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            post(Ev::Moved, hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as i32;
            let code = ((wp.0 >> 16) & 0xFFFF) as u32;
            match (id, code) {
                (ID_EDIT, re::EN_CHANGE) => on_change(hwnd),
                (ID_EDIT, re::EN_VSCROLL) => {
                    if let Some(e) = edit_of(hwnd).filter(|e| e.pop().is_some()) {
                        let _ = InvalidateRect(Some(e.hwnd()), None, true);
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                _ => {}
            }
            LRESULT(0)
        }
        crate::desklayer::hover::REDRAW => LRESULT(0),
        WM_REFORMAT => {
            format_text(hwnd);
            LRESULT(0)
        }
        // A new picture of the wallpaper: the card and its editor again.
        crate::desklayer::backdrop::WM_BACKDROP => {
            with(hwnd, |w| w.bg = None);
            if !shows_face(hwnd) {
                open_editor(hwnd, false);
                if let Some(e) = edit_of(hwnd).filter(|e| e.pop().is_some()) {
                    let _ = InvalidateRect(Some(e.hwnd()), None, true);
                }
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let _ = ValidateRect(Some(hwnd), None);
            redraw(hwnd);
            LRESULT(0)
        }
        m if m == crate::desklayer::edit::COMMAND => {
            super::hub::widget_commands(hwnd);
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            ask_menu(hwnd, context_point(lp));
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // Dragged: its size is the one fixed at the press, set where it
            // lands (crate::desklayer::grab), never Windows' suggestion.
            if !OWN_MOVE.with(|o| o.get()) && !crate::desklayer::grab::moving(hwnd) {
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            with(hwnd, |w| w.bg = None);
            style(hwnd);
            place_editor(hwnd, false);
            if !shows_face(hwnd) {
                open_editor(hwnd, false);
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        // Alt+F4 or anyone's WM_CLOSE: a note is not closed, it goes back
        // to the desktop (a note goes only by its menu's delete).
        WM_CLOSE => {
            post(Ev::Close, hwnd);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_GETOBJECT => {
            let answer = crate::a11y::on_getobject(hwnd, wp, lp, || publish(hwnd));
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == crate::a11y::wm_action() => {
            a11y_actions(hwnd);
            LRESULT(0)
        }
        WM_TYPE_TEST => {
            if let Some(e) = edit_of(hwnd) {
                re::send(&e, WM_SETTEXT, 0, lp.0);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            crate::desklayer::backdrop::unlisten(hwnd);
            crate::a11y::forget(hwnd);
            let _ = KillTimer(Some(hwnd), CARET_TIMER);
            let gone = WINS.with(|w| w.try_borrow_mut().ok().and_then(|mut m| m.remove(&(hwnd.0 as isize))));
            OWNER.with(|o| o.borrow_mut().retain(|_, n| *n != hwnd.0 as isize));
            if let Some(w) = gone {
                if !w.closing {
                    // Not ours (someone else destroyed it): the hub makes
                    // it again from this text (a fallback editor went first).
                    let text = if w.edit.live() { re::text(&w.edit) } else { w.text.clone() };
                    LOST.with(|l| l.borrow_mut().push((w.id.clone(), check::to_markdown(&text))));
                    post(Ev::Lost, hwnd);
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The fallback editor's background again (the card moved or the
/// wallpaper changed under it); nothing to do while typing on the card.
fn refresh_editor_bg(hwnd: HWND) {
    let light = theme_at(hwnd).is_light;
    if with(hwnd, |w| w.editor_light != Some(light)).unwrap_or(false) {
        style(hwnd);
    }
    let Some(Edit::Window { edit, .. }) = edit_of(hwnd) else { return };
    let bg = editor_bg(hwnd);
    with(hwnd, |w| w.bg = bg);
    unsafe {
        let _ = InvalidateRect(Some(edit), None, true);
    }
}

/// Edit mode on (`on`): the editor closes, the face shows; off: typing
/// goes on if the note still has the keyboard.
pub fn edit_mode(hwnd: HWND, on: bool) {
    with(hwnd, |w| w.widget_editing = on);
    if on {
        close_editor(hwnd);
        unsafe {
            let _ = SetFocus(None);
        }
    } else {
        let fg = unsafe { GetForegroundWindow() };
        let pop = edit_of(hwnd).and_then(|e| e.pop());
        if fg == hwnd || Some(fg) == pop {
            focus(hwnd);
        }
    }
    if let Some((text, date)) = with(hwnd, |w| (w.text.clone(), w.date)) {
        remember(hwnd, crate::desklayer::edit::Preview::Note(text, date.map(date_text).unwrap_or_default()));
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// `p` (screen) is on the open editor's text.
pub(super) fn editing_body(hwnd: HWND, p: POINT) -> bool {
    if shows_face(hwnd) {
        return false;
    }
    let c = card(hwnd);
    let t = text_px((c.right - c.left, c.bottom - c.top), scale(hwnd));
    let (x, y) = (p.x - c.left, p.y - c.top);
    x >= t.left && x < t.right && y >= t.top && y < t.bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindow, GetWindowLongPtrW, GW_CHILD, GWL_EXSTYLE, GWL_STYLE, HTCLIENT, WM_NCHITTEST, WS_THICKFRAME};

    #[test]
    fn the_scroll_mark_shows_only_when_needed() {
        assert_eq!(scroll_mark(100, 100, 0, 16), None);
        assert_eq!(scroll_mark(100, 200, 0, 16), Some((0, 50)));
        assert_eq!(scroll_mark(100, 200, 100, 16), Some((50, 50)));
        // Long text: never thinner than the minimum, never past the end.
        assert_eq!(scroll_mark(100, 10_000, 99_999, 16), Some((84, 16)));
    }

    /// Review #8: during an IME composition the editor keeps Esc, Enter
    /// and Ctrl+Z (the composition is the IME's to cancel or commit).
    #[test]
    fn keys_go_to_the_ime_while_composing() {
        for vk in [0x1B, 0x0D, 0x5A] {
            assert_eq!(key_action(vk, true, vk == 0x5A, vk != 0x5A, false), Key::Pass, "{vk:#x}");
        }
        assert_eq!(key_action(0x1B, false, false, true, false), Key::Escape);
        assert_eq!(key_action(0x0D, false, false, true, false), Key::Enter);
        assert_eq!(key_action(0x5A, false, true, false, false), Key::Undo);
        // Something left to undo: RichEdit's own Undo.
        assert_eq!(key_action(0x5A, false, true, false, true), Key::Pass);
        // Shift+Enter is a plain line break.
        assert_eq!(key_action(0x0D, false, false, false, false), Key::Pass);
    }

    #[test]
    fn events_round_trip() {
        for e in [Ev::Changed, Ev::Add, Ev::Delete, Ev::Escape, Ev::Undo, Ev::Moved, Ev::Deactivated, Ev::Lost, Ev::Close, Ev::Menu] {
            assert_eq!(Ev::from(e as usize), Some(e));
        }
        assert_eq!(Ev::from(0), None);
    }

    /// The 0.1.91 lesson (owner screenshots): a note window is one GPU
    /// surface like the battery's. No redirection bitmap, no sizing frame,
    /// no window region (whose corners missed the card's), no GDI child
    /// (the old + and × buttons). The window is the card's rectangle
    /// exactly: a click beside it goes to the desktop (review: the 32 DIP
    /// shadow margin kept desktop icons from their clicks).
    #[test]
    fn a_note_window_is_one_composition_card() {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 288, bottom: -32000 + 176 };
        let hwnd = create_frame(r).expect("note frame");
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            assert!(ex & WS_EX_NOREDIRECTIONBITMAP.0 != 0, "no redirection bitmap");
            assert!(style & WS_THICKFRAME.0 == 0, "no sizing frame");
            let rgn = CreateRectRgn(0, 0, 0, 0);
            assert_eq!(windows::Win32::Graphics::Gdi::GetWindowRgn(hwnd, rgn).0, 0, "no window region (ERROR: none set)");
            let _ = DeleteObject(rgn.into());
            assert!(GetWindow(hwnd, GW_CHILD).map_or(true, |c| c.is_invalid()), "no child window");
            assert!(!IsWindowVisible(hwnd).as_bool());
            // The window is the card where it was asked for, all of it.
            let mut wr = RECT::default();
            let _ = GetWindowRect(hwnd, &mut wr);
            assert_eq!(wr, r);
            assert_eq!(card(hwnd), r);
            let hit = |x: i32, y: i32| SendMessageW(hwnd, WM_NCHITTEST, None, Some(LPARAM(((y as u16 as isize) << 16) | x as u16 as isize))).0;
            assert_eq!(hit(r.left + 40, r.top + 40), HTCLIENT as isize);
            let _ = DestroyWindow(hwnd);
        }
    }

    /// The editor covers the text area only: a rectangle inside the card's
    /// rounded corners at every size and scale, so no corner of it shows.
    #[test]
    fn the_editor_sits_inside_the_rounded_card() {
        use crate::desklayer::widget::{Size, RADIUS};
        let g = crate::desklayer::grid::Grid { ox: 0, oy: 0, cw: 76, ch: 96, cols: 20, rows: 12, ..Default::default() };
        for s in [1., 1.25, 1.5, 2.] {
            let g = crate::desklayer::grid::Grid { cw: (76. * s) as i32, ch: (96. * s) as i32, ..g };
            for size in Size::ALL {
                let p = size.pixels(&g, s);
                let (w, h) = (p.0 as f32 / s, p.1 as f32 / s);
                assert!(inside_rounded(text_rect(w, h), w, h, RADIUS), "{size:?} at {s}");
                let t = text_px(p, s);
                let dip = (t.left as f32 / s, t.top as f32 / s, t.right as f32 / s, t.bottom as f32 / s);
                assert!(inside_rounded(dip, w, h, RADIUS), "{size:?} at {s}: {t:?}");
                assert!(t.top as f32 / s >= 32., "under the header");
            }
        }
        // A rectangle reaching into a corner is caught.
        assert!(!inside_rounded((2., 2., 100., 100.), 136., 176., RADIUS));
    }

    /// Typing looks like the card (review): the first paragraph is the
    /// title (20/700, first grey), the rest the body (14 on 20, second
    /// grey); typed and pasted text takes them; Undo undoes the typing
    /// only (setting the formats is not recorded); a composition is left
    /// alone until it ends.
    #[test]
    fn typing_keeps_the_cards_formats_out_of_undo() {
        for inline in [true, false] {
            set_test_inline(inline);
            formats_out_of_undo(inline);
        }
        set_test_inline(true);
    }

    fn formats_out_of_undo(inline: bool) {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 288, bottom: -32000 + 176 };
        let hwnd = create("fmt", r, "빌드 전에\n서버 주소").expect("note window");
        assert_eq!(types_inline(hwnd), inline);
        let edit = edit_of(hwnd).unwrap();
        let edit = &edit;
        let fmt = |at: i32| {
            re::set_sel(edit, at, at + 1);
            let mut cf = re::CharFormat2 { mask: re::CFM_SIZE | re::CFM_BOLD | re::CFM_COLOR, ..Default::default() };
            re::send(edit, re::EM_GETCHARFORMAT, re::SCF_SELECTION as usize, &mut cf as *mut _ as isize);
            let mut pf = re::ParaFormat2 { mask: re::PFM_LINESPACING, ..Default::default() };
            re::send(edit, re::EM_GETPARAFORMAT, 0, &mut pf as *mut _ as isize);
            (cf.height, cf.effects & re::CFE_BOLD, cf.color, pf.line_spacing, pf.line_spacing_rule)
        };
        let (title, body) = (fmt(0), fmt(7));
        assert_eq!(title.1, re::CFE_BOLD, "{title:?} {body:?}");
        assert_eq!(body.1, 0);
        assert!(title.0 > body.0, "title bigger: {title:?} {body:?}");
        assert_ne!(title.2, body.2, "first and second grey");
        assert_eq!((title.4, body.4), (re::LINE_EXACT, re::LINE_EXACT));
        assert!(title.3 > body.3);
        // Typing at the end of the title, then a new line: each keeps its
        // place's format; one Undo takes the typing back, nothing else.
        let before = re::text(edit);
        re::set_sel(edit, 5, 5);
        re::replace(edit, 5, 5, "\r다음 줄");
        assert_eq!(fmt(7), body, "a new line under the title is body");
        assert_eq!(fmt(0), title);
        re::send(edit, re::EM_UNDO, 0, 0);
        assert_eq!(re::text(edit), before);
        assert_eq!(re::send(edit, re::EM_CANUNDO, 0, 0), 0, "formats never in Undo");
        // While composing nothing is set; after, it is.
        with(hwnd, |w| w.ime = true);
        re::replace(edit, 0, 0, "새 ");
        with(hwnd, |w| w.ime = false);
        format_text(hwnd);
        assert_eq!(fmt(0), title);
        assert_eq!(markdown(hwnd).as_deref(), Some("새 빌드 전에\n서버 주소"), "saved as plain text");
        destroy(hwnd);
    }

    #[test]
    fn only_text_is_pasted() {
        assert!(is_paste(0x56, true, false, false));
        assert!(is_paste(0x56, true, true, false));
        assert!(is_paste(0x2D, false, true, false));
        assert!(!is_paste(0x56, false, false, false));
        assert!(!is_paste(0x56, true, false, true));
        assert!(!is_paste(0x2D, true, true, false));
        assert_eq!(title_end(&"a\rb".encode_utf16().collect::<Vec<_>>()), 2);
        assert_eq!(title_end(&"abc".encode_utf16().collect::<Vec<_>>()), 3);
    }

    /// Review 4 [P1]: a small card on a 400 % display with Explorer's
    /// 76 x 96 px cells is 88 x 128 px; its text area (16 DIP = 64 px in
    /// from each side) has no room. The area stays inside the card, and
    /// the editor does not open (no out-of-range cut, no panic in a window
    /// message).
    #[test]
    fn review_04_the_editor_area_never_leaves_the_card() {
        for (card, s) in [((88, 128), 4.), ((40, 40), 2.), ((288, 176), 1.), ((10, 300), 1.5)] {
            let t = text_px(card, s);
            assert!(t.left >= 0 && t.top >= 0 && t.right <= card.0 && t.bottom <= card.1, "{card:?} at {s}: {t:?}");
        }
        assert!(!editable((88, 128), 4.));
        assert!(editable((288, 176), 1.));
        crate::desklayer::surface::set_test_scale(Some(4.));
        let r = RECT { left: -32000, top: -32000, right: -32000 + 88, bottom: -32000 + 128 };
        let hwnd = create("tiny", r, "작은 메모").expect("note window");
        set_test_active(hwnd, true);
        assert!(shows_face(hwnd), "no room: the editor stays closed");
        destroy(hwnd);
        crate::desklayer::surface::set_test_scale(None);
    }

    /// Review 5 [P1]: the editor (an owned popup holding the typed text) is
    /// kept out of captures like its note, and forgotten with it.
    #[test]
    fn review_05_the_editor_is_kept_out_of_captures() {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 288, bottom: -32000 + 176 };
        // Typed on the card: no window but the card (kept out with it).
        let hwnd = create("cap", r, "secret").expect("note window");
        assert!(types_inline(hwnd));
        assert_eq!(editor_windows(hwnd), None);
        destroy(hwnd);
        // The fallback editor: its popup is kept out too.
        set_test_inline(false);
        let hwnd = create("cap", r, "secret").expect("note window");
        let (pop, _) = editor_windows(hwnd).expect("the fallback editor");
        assert!(crate::desklayer::privacy::registered(pop));
        destroy(hwnd);
        assert!(!crate::desklayer::privacy::registered(pop));
        set_test_inline(true);
    }

    /// Review 6: the note, its editor and the editor's RichEdit are the
    /// same note.
    #[test]
    fn review_06_editor_windows_are_their_note() {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 288, bottom: -32000 + 176 };
        let hwnd = create("own", r, "").expect("note window");
        assert_eq!(note_of(hwnd), Some(hwnd));
        assert_eq!(note_of(HWND(std::ptr::null_mut())), None);
        destroy(hwnd);
        set_test_inline(false);
        let hwnd = create("own", r, "").expect("note window");
        let (pop, edit) = editor_windows(hwnd).unwrap();
        for h in [hwnd, pop, edit] {
            assert_eq!(note_of(h), Some(hwnd));
        }
        destroy(hwnd);
        set_test_inline(true);
    }

    /// A full note (its editor and RichEdit) loads its text as given,
    /// Hangul and boxes included, and gives it back as Markdown.
    #[test]
    fn a_note_window_keeps_its_text() {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 240, bottom: -32000 + 200 };
        let md = "제목\n- [ ] 우유\n- [x] 빵";
        let Some(hwnd) = create("t1", r, &check::to_display(md)) else {
            panic!("note window");
        };
        assert_eq!(markdown(hwnd).as_deref(), Some(md));
        // T1: WM_CLOSE (Alt+F4) never destroys a note.
        unsafe {
            SendMessageW(hwnd, WM_CLOSE, None, None);
        }
        assert!(unsafe { IsWindow(Some(hwnd)) }.as_bool());
        assert_eq!(markdown(hwnd).as_deref(), Some(md));
        assert_eq!(id_of(hwnd).as_deref(), Some("t1"));
        // Typed on the card: no window of its own.
        assert!(types_inline(hwnd));
        assert!(unsafe { GetWindow(hwnd, GW_CHILD) }.map_or(true, |c| c.is_invalid()));
        destroy(hwnd);
        // The fallback editor is an owned popup, not a child of the note.
        set_test_inline(false);
        let hwnd2 = create("t1b", r, &check::to_display(md)).expect("note window");
        assert_eq!(markdown(hwnd2).as_deref(), Some(md));
        let (pop, _) = editor_windows(hwnd2).unwrap();
        assert!(unsafe { GetWindow(hwnd2, GW_CHILD) }.map_or(true, |c| c.is_invalid()));
        assert_eq!(unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindow(pop, windows::Win32::UI::WindowsAndMessaging::GW_OWNER) }.ok(), Some(hwnd2));
        destroy(hwnd2);
        set_test_inline(true);
        assert!(take_lost().is_empty(), "closed by us is not lost");
        assert!(!is_note(hwnd));
        assert_eq!(markdown(hwnd), None, "a gone editor reads as nothing, not as empty");
    }

    /// Someone else destroys a note: its text survives (its editor is
    /// destroyed before it, so the kept copy is what the hub gets).
    #[test]
    fn a_note_destroyed_by_someone_else_leaves_its_text() {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 240, bottom: -32000 + 200 };
        let md = "남길 글\n- [ ] 하나";
        let hwnd = create("t2", r, &check::to_display(md)).expect("note window");
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        let lost = take_lost();
        assert_eq!(lost, vec![("t2".to_string(), md.to_string())]);
    }

    fn send(hwnd: HWND, m: u32, wp: usize, lp: isize) -> isize {
        unsafe { SendMessageW(hwnd, m, Some(WPARAM(wp)), Some(LPARAM(lp))) }.0
    }

    fn type_text(hwnd: HWND, s: &str) {
        for c in s.encode_utf16() {
            send(hwnd, WM_CHAR, c as usize, 1);
        }
    }

    /// Typing right on the card: keys and characters go through the card
    /// window into its RichEdit (no window of its own); Enter, the
    /// checklist conversion, a selection, Undo, the IME's composition
    /// flags and the card's formats all work as in the editor window, and
    /// the text is saved as Markdown.
    #[test]
    fn typing_on_the_card_types_selects_and_undoes() {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 288, bottom: -32000 + 176 };
        let hwnd = create("type", r, "").expect("note window");
        assert!(types_inline(hwnd));
        set_test_active(hwnd, true);
        assert!(!shows_face(hwnd), "typing is open");
        type_text(hwnd, "할 일");
        send(hwnd, WM_KEYDOWN, 0x0D, 0);
        send(hwnd, WM_CHAR, 0x0D, 1);
        type_text(hwnd, "- [ ] 우유");
        assert_eq!(markdown(hwnd).as_deref(), Some("할 일\n- [ ] 우유"));
        assert!(with(hwnd, |w| w.text.contains('\u{2610}')).unwrap(), "typed Markdown became a box");
        let edit = edit_of(hwnd).unwrap();
        let edit = &edit;
        // The card's formats: the title bold, the rest not.
        let bold = |at: i32| {
            let (a, b) = re::sel(edit);
            re::set_sel(edit, at, at + 1);
            let mut cf = re::CharFormat2 { mask: re::CFM_BOLD | re::CFM_SIZE, ..Default::default() };
            re::send(edit, re::EM_GETCHARFORMAT, re::SCF_SELECTION as usize, &mut cf as *mut _ as isize);
            re::set_sel(edit, a, b);
            (cf.effects & re::CFE_BOLD != 0, cf.height)
        };
        assert!(bold(0).0 && !bold(5).0, "title {:?} body {:?}", bold(0), bold(5));
        // A selection is replaced by what is typed; Undo takes it back.
        let before = re::text(edit);
        re::set_sel(edit, 0, 1);
        type_text(hwnd, "X");
        assert!(re::text(edit).starts_with("X 일"), "{:?}", re::text(edit));
        re::send(edit, re::EM_UNDO, 0, 0);
        assert_eq!(re::text(edit), before);
        // Formats never in Undo: the typing's own steps only.
        // A double click on a word selects it (the card is CS_DBLCLKS).
        let p = re::pos_of(edit, 6);
        let lp = ((p.y + 4) as isize) << 16 | (p.x + 2) as isize;
        send(hwnd, WM_LBUTTONDOWN, 1, lp);
        send(hwnd, WM_LBUTTONUP, 0, lp);
        send(hwnd, WM_LBUTTONDBLCLK, 1, lp);
        send(hwnd, WM_LBUTTONUP, 0, lp);
        let (a, b) = re::sel(edit);
        assert!(b > a, "a word selected: {a}..{b}");
        // A pasted run's formats never stay (the card's are set again):
        // RTF in place of the selection, as a rich paste would bring.
        re::set_sel(edit, 0, 0);
        let rtf = b"{\\rtf1\\ansi {\\b0\\i\\fs72 BIG }}\0";
        let st = re::TwoFlags { flags: 2 /* ST_SELECTION */ | 1 /* ST_KEEPUNDO */, codepage: 0 };
        re::send(edit, re::EM_SETTEXTEX, &st as *const _ as usize, rtf.as_ptr() as isize);
        assert!(re::text(edit).starts_with("BIG "), "{:?}", re::text(edit));
        let (b0, h0) = bold(0);
        assert!(b0, "the pasted run took the title's format");
        let mut cf = re::CharFormat2 { mask: 0x0000_0002 /* CFM_ITALIC */, ..Default::default() };
        re::set_sel(edit, 0, 1);
        re::send(edit, re::EM_GETCHARFORMAT, re::SCF_SELECTION as usize, &mut cf as *mut _ as isize);
        assert_eq!(cf.effects & 2, 0, "no italic left");
        assert_eq!(h0, char_format(hwnd, true).height, "the title's size");
        // The IME: its composition holds the card's formats back, then the
        // card sets them again.
        send(hwnd, WM_IME_STARTCOMPOSITION, 0, 0);
        assert!(with(hwnd, |w| w.ime).unwrap());
        send(hwnd, WM_IME_ENDCOMPOSITION, 0, 0);
        assert!(!with(hwnd, |w| w.ime).unwrap());
        // Esc is the card's (back to the desktop), never typed.
        let n = re::len(edit);
        send(hwnd, WM_KEYDOWN, 0x1B, 0);
        send(hwnd, WM_CHAR, 0x1B, 1);
        assert_eq!(re::len(edit), n);
        // Saved and loaded again: the same text.
        let md = markdown(hwnd).unwrap();
        replace_markdown(hwnd, &md);
        assert_eq!(markdown(hwnd).as_deref(), Some(md.as_str()));
        set_test_active(hwnd, false);
        assert!(shows_face(hwnd));
        destroy(hwnd);
    }

    /// The text typed from another thread (as the hub test does).
    #[test]
    fn typing_test_message_sets_the_text() {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 288, bottom: -32000 + 176 };
        let hwnd = create("tt", r, "before").expect("note window");
        let text: Vec<u16> = "after \u{2610} 한글".encode_utf16().chain(std::iter::once(0)).collect();
        send(hwnd, WM_TYPE_TEST, 0, text.as_ptr() as isize);
        assert_eq!(markdown(hwnd).as_deref(), Some("after \u{2610} 한글"));
        destroy(hwnd);
    }

    /// Windowless RichEdit refused: the note types in the fallback editor
    /// window (and says so once); on a live wallpaper its background is
    /// the card's thickest cover as one solid colour.
    #[test]
    fn without_typing_on_the_card_the_editor_window_is_used() {
        set_test_inline(false);
        let r = RECT { left: -32000, top: -32000, right: -32000 + 288, bottom: -32000 + 176 };
        let hwnd = create("fb", r, "x").expect("note window");
        assert!(!types_inline(hwnd));
        assert!(editor_windows(hwnd).is_some());
        destroy(hwnd);
        set_test_inline(true);
        // Dark 55 %, light 72 % of the tint over the card's colour.
        let dark = cover_solid(false);
        let light = cover_solid(true);
        assert!(dark.r < 0x1C && dark.a == 255, "{dark:?}");
        assert!(light.r > 0xF2 && light.a == 255, "{light:?}");
    }

    /// A text box that cannot draw on the card makes its note fall back to
    /// the editor window, its text and selection kept.
    #[test]
    fn a_failed_draw_falls_back_with_the_text() {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 288, bottom: -32000 + 176 };
        let hwnd = create("fd", r, "keep me").expect("note window");
        assert!(types_inline(hwnd));
        let edit = edit_of(hwnd).unwrap();
        re::set_sel(&edit, 2, 4);
        drop(edit);
        fall_back(hwnd);
        assert!(!types_inline(hwnd));
        let edit = edit_of(hwnd).unwrap();
        assert_eq!(re::sel(&edit), (2, 4));
        assert_eq!(markdown(hwnd).as_deref(), Some("keep me"));
        destroy(hwnd);
        set_test_inline(true);
    }

    /// The ink box (left, top, right, bottom) brighter than `level` in
    /// `band` of a `w` px wide picture.
    fn ink_box(px: &[u8], w: i32, band: RECT, level: i32) -> (i32, i32, i32, i32) {
        let (mut l, mut t, mut r, mut b) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for y in band.top..band.bottom {
            for x in band.left..band.right {
                let i = ((y * w + x) * 4) as usize;
                if (px[i] as i32 + px[i + 1] as i32 + px[i + 2] as i32) / 3 > level {
                    l = l.min(x);
                    r = r.max(x);
                    t = t.min(y);
                    b = b.max(y);
                }
            }
        }
        (l, t, r, b)
    }

    /// Pictures of a note at rest and while typed in on the card (dark),
    /// at scale `s`, on the static glass or the live one (the host's
    /// backdrop under a translucent card).
    fn rest_and_typing(s: f32, live: bool) -> ((i32, i32), Vec<u8>, Vec<u8>) {
        let mut t = crate::theme::Theme::graphite();
        t.is_light = false;
        set_look(Look::of(&t));
        set_test_scale(Some(s));
        let (w, h) = ((288. * s) as i32, (176. * s) as i32);
        let r = RECT { left: -32000, top: -32000, right: -32000 + w, bottom: -32000 + h };
        let hwnd = create("ov", r, "빌드 전에 확인\n서버 주소 바꾸기\n로그 보기").expect("note window");
        let stage = crate::gfx::promo_kit::stage();
        let parent = stage.gfx.compositor.CreateContainerVisual().unwrap();
        let layer = crate::desklayer::live::Layer::new(&stage.gfx.compositor, &parent, None).unwrap();
        let _ground = crate::glass::StandIn::new(Rgba { r: 90, g: 110, b: 130, a: 255 });
        let _mode = live.then(|| crate::desklayer::live::TestMode::new(false));
        let shot = |typing: bool| {
            set_test_active(hwnd, typing);
            assert_eq!(!shows_face(hwnd), typing);
            if live {
                let mut out = None;
                layer.draw((0., 0.), || {
                    out = picture(hwnd);
                    Ok(())
                })
                .unwrap();
                out.unwrap().2
            } else {
                picture(hwnd).unwrap().2
            }
        };
        let rest = shot(false);
        let typing = shot(true);
        set_test_active(hwnd, false);
        destroy(hwnd);
        set_test_scale(None);
        ((w, h), rest, typing)
    }

    /// The brief's check: the card being typed in and the card at rest,
    /// laid over each other, keep the title and the first body line within
    /// 2 px (where they start, their top and foot; the line's end within
    /// 4), on the static glass and the live one, at 100 % and 200 %.
    #[test]
    fn typing_on_the_card_lies_on_the_face() {
        for live in [false, true] {
            for s in [1., 2.] {
                let ((w, _), rest, typing) = rest_and_typing(s, live);
                let px = |v: f32| (v * s) as i32;
                let title = RECT { left: px(12.), top: px(36.), right: w - px(12.), bottom: px(72.) };
                let body = RECT { left: px(12.), top: px(74.), right: w - px(12.), bottom: px(94.) };
                for (what, band, level) in [("title", title, 190), ("body", body, 150)] {
                    let a = ink_box(&rest, w, band, level);
                    let b = ink_box(&typing, w, band, level);
                    assert!(a.0 < a.2 && b.0 < b.2, "{what} at {s} live {live}: rest {a:?} typing {b:?}");
                    // Where it starts, its top and its foot (the baseline).
                    let worst = [a.0 - b.0, a.1 - b.1, a.3 - b.3].iter().map(|v| v.abs()).max().unwrap();
                    assert!(worst <= 2, "{what} at {s} live {live}: rest {a:?} typing {b:?}");
                    // The line's end: GDI sets each glyph on whole pixels
                    // (measured 3 px at 100 %, 4 px = 2 DIP at 200 %).
                    assert!((a.2 - b.2).abs() <= 4, "{what} end at {s} live {live}: rest {a:?} typing {b:?}");
                }
            }
        }
    }

    /// Pictures to look at: the card at rest and typed in, static and
    /// live, 100 % and 200 % (target/ui-snapshots/memo-inline).
    #[test]
    #[ignore = "offscreen PNG inspection"]
    fn snapshots_typing_on_the_card() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/memo-inline");
        for live in [false, true] {
            for s in [1., 2.] {
                let ((w, h), rest, typing) = rest_and_typing(s, live);
                let name = format!("{}-{}x", if live { "live" } else { "static" }, s as i32);
                crate::gfx::promo_kit::save_png(&rest, w as u32, h as u32, &dir.join(format!("{name}-rest.png")));
                crate::gfx::promo_kit::save_png(&typing, w as u32, h as u32, &dir.join(format!("{name}-typing.png")));
            }
        }
        // Light, a word selected, the caret: as typed with the keyboard.
        for light in [false, true] {
            let mut t = crate::theme::Theme::graphite();
            t.is_light = light;
            set_look(Look::of(&t));
            set_test_scale(Some(2.));
            let r = RECT { left: -32000, top: -32000, right: -32000 + 576, bottom: -32000 + 352 };
            let hwnd = create("sel", r, "빌드 전에 확인\n서버 주소 바꾸기").expect("note window");
            set_test_active(hwnd, true);
            let edit = edit_of(hwnd).unwrap();
            edit.inline().unwrap().set_focus(true);
            re::set_sel(&edit, 9, 11);
            let (w, h, px) = picture(hwnd).unwrap();
            let name = if light { "light" } else { "dark" };
            crate::gfx::promo_kit::save_png(&px, w as u32, h as u32, &dir.join(format!("{name}-2x-selection.png")));
            re::set_sel(&edit, 4, 4);
            let (w, h, px) = picture(hwnd).unwrap();
            crate::gfx::promo_kit::save_png(&px, w as u32, h as u32, &dir.join(format!("{name}-2x-caret.png")));
            edit.inline().unwrap().set_focus(false);
            destroy(hwnd);
            set_test_scale(None);
        }
    }
}
