//! Desktop-only widget editing. Registry contains owned paint data, never COM objects.
use super::{
    icons,
    widget::{self, Size},
    win,
};
use crate::{gfx::Gfx, theme::Theme};
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Instant,
};
use windows::{
    core::Result,
    Win32::{
        Foundation::*,
        UI::{
            Input::KeyboardAndMouse::*,
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
};

pub const COMMAND: u32 = WM_APP + 0x2d1;
pub const REFRESH: u32 = WM_APP + 0x2d2;
/// Posted to an edit overlay: the chosen card's size menu opens (outside
/// the pointer's borrow of the editor).
const SIZE_MENU: u32 = WM_APP + 0x2d3;
const HOLD: usize = 0x5745;
#[cfg(test)]
pub fn between(a: RECT, b: RECT, t: f32) -> RECT {
    let m = |x: i32, y: i32| x + ((y - x) as f32 * t).round() as i32;
    RECT { left: m(a.left, b.left), top: m(a.top, b.top), right: m(a.right, b.right), bottom: m(a.bottom, b.bottom) }
}

static DRAGGING: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
pub fn dragging(h: HWND) -> bool {
    DRAGGING.load(Ordering::Relaxed) == h.0 as isize
}
static ACTIVE: AtomicBool = AtomicBool::new(false);
static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static REQUESTS: Mutex<Vec<Request>> = Mutex::new(Vec::new());
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Battery,
    Note,
    Inbox,
}
impl Kind {
    pub const ALL: [Self; 3] = [Self::Battery, Self::Note, Self::Inbox];
    pub fn label(self) -> &'static str {
        match self {
            Self::Battery => crate::i18n::t("battery.title"),
            Self::Note => crate::i18n::t("widgets.notes"),
            Self::Inbox => crate::i18n::t("widgets.inbox"),
        }
    }
}
#[derive(Clone)]
pub enum Preview {
    Note(String, String),
    Inbox(Vec<crate::notes::inbox::Row>, Vec<usize>, usize),
    Battery(
        crate::device_battery::model::Desktop,
        Vec<crate::device_battery::model::Device>,
    ),
}
impl Preview {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Note(..) => Kind::Note,
            Self::Inbox(..) => Kind::Inbox,
            Self::Battery(..) => Kind::Battery,
        }
    }
    pub fn empty(kind: Kind) -> Self {
        match kind {
            Kind::Note => Self::Note(String::new(), String::new()),
            Kind::Inbox => Self::Inbox(vec![], vec![], 0),
            Kind::Battery => Self::Battery(Default::default(), vec![]),
        }
    }
}
#[derive(Clone)]
struct Entry {
    hwnd: isize,
    /// The widget, whatever its window (Key): what edit mode chooses.
    key: Key,
    /// Its size in places as saved (None: not known; read from its px).
    size: Option<Size>,
    preview: Preview,
}

/// A widget, the same across its windows: what edit mode's choice, a
/// resize and the size menu hold (a window may be made again).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Battery,
    Inbox,
    /// A note by its id.
    Note(String),
}

/// Pure: the card of widget `key` among `cards` (its window now).
fn find_key<'a>(cards: &'a [(Entry, RECT)], key: &Key) -> Option<&'a (Entry, RECT)> {
    cards.iter().find(|(e, _)| e.key == *key)
}

/// The widget card window `h` shows.
fn key_of(h: isize) -> Option<Key> {
    ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|e| e.hwnd == h).map(|e| e.key.clone())
}

/// The window widget `key` shows in now.
fn hwnd_of(key: &Key) -> Option<HWND> {
    ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|e| e.key == *key).map(|e| HWND(e.hwnd as *mut _))
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Idle,
    Editing,
    Dragging,
}
#[derive(Clone, Copy, Debug)]
pub enum Event {
    Enter,
    Hold(u64),
    Drag,
    Drop,
    Done,
    Escape,
    Blank,
    OtherApp,
    CancelDrag,
}
pub fn transition(mode: Mode, event: Event) -> Mode {
    match (mode, event) {
        (_, Event::Done | Event::Escape | Event::Blank | Event::OtherApp) => Mode::Idle,
        (Mode::Idle, Event::Enter) => Mode::Editing,
        (Mode::Idle, Event::Hold(ms)) if ms >= 500 => Mode::Editing,
        (Mode::Editing, Event::Drag) => Mode::Dragging,
        (Mode::Dragging, Event::Drop | Event::CancelDrag) => Mode::Editing,
        _ => mode,
    }
}
#[derive(Clone, Debug)]
pub enum Action {
    DisplaysChanged,
    Remove,
    Hide,
    Place(RECT),
    /// Resized by its handles (or its size menu): its new card rect
    /// (screen px) and size in places.
    Resize(RECT, Size),
    Add {
        kind: Kind,
        size: Size,
        monitor: usize,
        note: Option<String>,
    },
    Enter,
    Exit,
}
enum Request {
    Enter(Option<isize>),
    /// A long press on a card: its handles (entering quietly when not
    /// editing yet).
    Select(isize),
    Finish(Event),
    Refresh,
    Regrid,
}
static ACTIONS: Mutex<Vec<(isize, Action)>> = Mutex::new(Vec::new());
pub fn take_actions(h: HWND) -> Vec<Action> {
    let mut q = ACTIONS.lock().unwrap_or_else(|e| e.into_inner());
    let mut mine = vec![];
    q.retain(|(id, a)| {
        if *id == h.0 as isize {
            mine.push(a.clone());
            false
        } else {
            true
        }
    });
    drop(q);
    if mine.iter().any(|a| matches!(a, Action::DisplaysChanged)) {
        super::drag_hint::displays_changed();
        let _ = DRAGGING.compare_exchange(h.0 as isize, 0, Ordering::Relaxed, Ordering::Relaxed);
        if super::grab::cancel(h) {
            unsafe {
                if GetCapture() == h { let _ = ReleaseCapture(); }
            }
            super::drag_hint::hide();
        }
    }
    if mine.iter().any(|a| matches!(a, Action::Place(_))) {
        let _ = DRAGGING.compare_exchange(h.0 as isize, 0, Ordering::Relaxed, Ordering::Relaxed);
    }
    if mine.iter().any(|a| matches!(a, Action::Place(_) | Action::Resize(..))) {
        let mons = crate::monitors::all();
        mine.retain(|a| allow_geometry(Some(h), a, &mons));
    }
    mine
}

/// Screen geometry may have changed since a drag reserved its landing rect.
pub fn allow_geometry(h: Option<HWND>, a: &Action, mons: &[crate::monitors::Monitor]) -> bool {
    let r = match a {
        Action::Place(r) | Action::Resize(r, _) => *r,
        _ => return true,
    };
    let allowed = super::place::geometry_allowed(
        h.and_then(icons::saved_place), mons, super::grid_state::display_settling(),
    ) && mons.iter().any(|m| {
        r.left >= m.rect.left && r.top >= m.rect.top && r.right <= m.rect.right && r.bottom <= m.rect.bottom
    });
    if !allowed {
        if let Some(h) = h {
            icons::rollback(h, r);
        }
    }
    allowed
}
pub fn send(h: HWND, action: Action) {
    ACTIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((h.0 as isize, action));
    unsafe {
        let _ = PostMessageW(Some(h), COMMAND, WPARAM(0), LPARAM(0));
    }
}
fn request(r: Request) {
    let mut requests = REQUESTS.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(r, Request::Regrid) && requests.iter().any(|r| matches!(r, Request::Regrid)) { return; }
    requests.push(r);
    drop(requests);
    super::host::wake_edit();
}
pub fn enter() {
    request(Request::Enter(None));
}
/// Card `h` was long pressed: its frame, handles and menu show.
pub fn select(h: HWND) {
    request(Request::Select(h.0 as isize));
}
pub fn refresh() {
    if active() {
        request(Request::Refresh);
    }
}
pub fn grid_changed() { request(Request::Regrid); }
pub fn displays_changed() {
    super::drag_clock::displays_changed();
    super::drag_hint::displays_changed();
    let hs: Vec<_> = ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|e| HWND(e.hwnd as *mut _)).collect();
    for h in hs { send(h, Action::DisplaysChanged); }
}
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}
pub fn foreground(h: HWND) {
    if active() && !is_home(h) {
        request(Request::Finish(Event::OtherApp));
    }
}
pub fn finish() {
    request(Request::Finish(Event::Done));
}
pub(super) fn escape_drag() {
    request(Request::Finish(Event::Escape));
}

pub(super) fn drag_tick(card: HWND, sample: super::drag_input::Sample) {
    let step = APP.with(|slot| {
        let mut slot = slot.try_borrow_mut().ok()?;
        let drag = slot.as_mut()?.drag.as_mut().filter(|d| d.entry.hwnd == card.0 as isize)?;
        Some(drag.step(sample))
    });
    if let Some(step) = step {
        super::drag_hint::update(card, step, sample.at);
    }
}
/// Card window `h` shows widget `key` (its saved size `size` when known)
/// with `preview`. Another window that showed the same widget is dropped.
pub fn remember(h: HWND, key: Key, size: Option<Size>, preview: Preview) {
    let mut entries = ENTRIES.lock().unwrap_or_else(|e| e.into_inner());
    entries.retain(|e| e.hwnd == h.0 as isize || e.key != key);
    if let Some(e) = entries.iter_mut().find(|e| e.hwnd == h.0 as isize) {
        e.preview = preview;
        e.key = key;
        if size.is_some() {
            e.size = size;
        }
    } else {
        entries.push(Entry {
            hwnd: h.0 as isize,
            key,
            size,
            preview,
        });
        drop(entries);
        unsafe {
            let _ = SetWindowSubclass(h, Some(subclass), HOLD, 0);
        }
        refresh();
    }
}
pub fn forget(h: HWND) {
    ACTIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(id, _)| *id != h.0 as isize);
    let _ = DRAGGING.compare_exchange(h.0 as isize, 0, Ordering::Relaxed, Ordering::Relaxed);
    ENTRIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|e| e.hwnd != h.0 as isize);
    refresh();
}
fn visible() -> Vec<(Entry, RECT)> {
    ENTRIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter_map(|e| {
            let h = HWND(e.hwnd as *mut _);
            icons::showing(h)
                .then(|| {
                    icons::committed(h)
                        .or_else(|| icons::card(h))
                        .map(|r| (e.clone(), r))
                })
                .flatten()
        })
        .collect()
}
thread_local! {
    static APP:RefCell<Option<Editor>>=const{RefCell::new(None)};
    static THEME:RefCell<Theme>=RefCell::new(Theme::graphite());
    static FRAME_PENDING:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};
    static PRESSED:RefCell<Option<(HWND,POINT,Instant)>>=const{RefCell::new(None)};
}
/// The look every widget thread draws a dragged card in (configure()).
static LOOK: Mutex<Option<Theme>> = Mutex::new(None);

/// The widgets' look (a dragged card is drawn in it on its own thread).
pub fn look() -> Theme {
    LOOK.lock().ok().and_then(|l| l.clone()).unwrap_or_else(Theme::graphite)
}

/// What card window `h` shows (its last remembered preview).
pub fn preview_of(h: HWND) -> Option<Preview> {
    ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|e| e.hwnd == h.0 as isize).map(|e| e.preview.clone())
}

pub fn configure(theme: &Theme) {
    if let Ok(mut l) = LOOK.lock() {
        *l = Some(theme.clone());
    }
    let changed = THEME.with(|t| {
        let mut t = t.borrow_mut();
        let changed = *t != *theme;
        *t = theme.clone();
        changed
    });
    if changed && active() {
        finish();
    } else {
        refresh();
    }
}
unsafe extern "system" fn subclass(
    h: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    _: usize,
    root: usize,
) -> LRESULT {
    let target = h;
    match m {
        WM_LBUTTONDOWN if !active() => {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let control = control_at(h,p);
            let rich_edit = crate::notes::editing_body(h,p);
            if hold_surface(control,rich_edit) {
                PRESSED.with(|v| *v.borrow_mut() = Some((h, p, Instant::now())));
                SetTimer(Some(h), HOLD, 500, None);
            }
        }
        WM_MOUSEMOVE => {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let cancel = PRESSED.with(|v| {
                v.borrow().as_ref().is_some_and(|(id, start, _)| {
                    *id == h && super::grab::beyond(*start, p)
                })
            });
            if cancel {
                PRESSED.with(|v| v.borrow_mut().take());
                let _ = KillTimer(Some(h), HOLD);
            }
        }
        WM_TIMER if w.0 == HOLD => {
            let _ = KillTimer(Some(h), HOLD);
            let mut p=POINT::default();let _=GetCursorPos(&mut p);
            let same=icons::card(target).is_some_and(|r|contains(r,p))
                && GetAncestor(WindowFromPoint(p),GA_ROOT)==target;
            let held = PRESSED.with(|v| v.borrow_mut().take().is_some_and(|(id,_,at)| {
                id==h && hold_fires(at.elapsed().as_millis() as u64,
                    GetAsyncKeyState(VK_LBUTTON.0 as i32)<0,same,is_home(GetForegroundWindow()))
            }));
            if held { let _=ReleaseCapture(); select(h); }
            return LRESULT(0);
        }
        message if hold_cancelled(message) => {
            PRESSED.with(|v| v.borrow_mut().take());
            let _ = KillTimer(Some(h), HOLD);
        }
        WM_KEYDOWN if local_escape(w.0 == VK_ESCAPE.0 as usize, GetAncestor(GetFocus(),GA_ROOT)==h) && active() => {
            finish();
            return LRESULT(0);
        }
        WM_KEYDOWN if w.0 == VK_ESCAPE.0 as usize && GetCapture() == h && super::grab::cancel(h) => {
            let _ = ReleaseCapture();
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            let _ = KillTimer(Some(h), HOLD);
            let _ = RemoveWindowSubclass(h, Some(subclass), HOLD);
            if root == 0 {
                forget(h);
            }
        }
        _ => {}
    }
    DefSubclassProc(h, m, w, l)
}
struct Overlay {
    window: super::edit_surface::Surface,
    monitor: crate::monitors::Monitor,
    index: usize,
}
struct Drag {
    size: Size,
    entry: Entry,
    origin: RECT,
    start: POINT,
    placement: super::grab::Placement,
    _grid: super::grid_state::Frozen,
}
impl Drag {
    fn step(&mut self, sample: super::drag_input::Sample) -> super::grab::Step {
        self.placement.step(self.origin, self.start, sample.point, self.size)
    }

    fn sample(&mut self, at: f64, p: POINT) {
        let card = HWND(self.entry.hwnd as *mut _);
        if let Some(sample) = super::drag_hint::sample(card, at, p) {
            super::drag_hint::update(card, self.step(sample), sample.at);
        }
    }
}
/// A press on a card in edit mode: a click chooses it, a move past 6 px
/// drags it.
struct Pending {
    entry: Entry,
    rect: RECT,
    start: POINT,
}
/// A side of the chosen card being dragged.
struct Resizing {
    key: Key,
    kind: Kind,
    edge: widget::Edge,
    origin: RECT,
    start: POINT,
    /// Where it would be let go now (screen px) and its size.
    result: (RECT, Size),
    _grid: super::grid_state::Frozen,
}
struct Editor {
    gfx: std::rc::Rc<Gfx>,
    theme: Theme,
    overlays: Vec<Overlay>,
    mode: Mode,
    gallery: Option<usize>,
    kind: Kind,
    size: Size,
    note: Option<String>,
    scroll: usize,
    drag: Option<Drag>,
    /// The widget whose handles show (by its key: its window may be made
    /// again, a resize must not lose it).
    chosen: Option<Key>,
    /// Entered by a long press (no badges, no buttons).
    quiet: bool,
    pending: Option<Pending>,
    resizing: Option<Resizing>,
    /// The size menu asked for: the card and where its menu item is.
    size_menu: Option<(Key, Kind, POINT)>,
}
/// Calculate and reserve the batch before queuing any owner-thread changes.
fn regrid() {
    if super::grid_state::display_settling() { return; }
    if super::grid_state::held() { super::grid_state::defer(); return; }
    let entries = ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mons = super::grid_state::monitors();
    let desk = widget::desk(1.);
    let mut widgets = Vec::new();
    for e in entries {
        let h = HWND(e.hwnd as *mut _);
        // A screen rectangle can overlap a different display after a dock change.
        // Only the owner's persisted monitor key may authorize a reflow.
        let Some(place) = icons::saved_place(h) else { continue };
        if super::place::connected(place.mon, &mons).is_none() { continue; }
        if !icons::showing(h) && !dragging(h) { continue; }
        let Some(r) = super::place::to_px(&place, &mons, (1., 1.)) else { continue };
        let size = super::grab::fixed(e.size, r, &mons, |_| desk.clone());
        widgets.push((h, r, size));
    }
    let before: Vec<_> = widgets.iter().map(|(h, r, _)| icons::committed(*h).unwrap_or(*r)).collect();
    let Some(next) = icons::realign_batch(&desk, &mons, &widgets) else {
        crate::log!("widget-grid: changed grid has no room for the complete layout");
        return;
    };
    for (((h, _, size), old), r) in widgets.into_iter().zip(before).zip(next) {
        if r != old { send(h, Action::Resize(r, size)); }
    }
    refresh();
}
pub fn process() {
    let requests = std::mem::take(&mut *REQUESTS.lock().unwrap_or_else(|e| e.into_inner()));
    for r in requests {
        match r {
            Request::Regrid => regrid(),
            Request::Enter(target) => {
                if active() {
                    if target.is_some() {
                        APP.with(|a|if let Some(e)=a.borrow_mut().as_mut(){e.gallery=gallery_monitor(&super::grid_state::monitors(),target);});
                        redraw();
                    }
                    continue;
                }
                let made = unsafe { Editor::new(target) };
                match made {
                    Ok(e) => {
                        for action in entry_actions(true) {
                            for (entry,_) in visible(){send(HWND(entry.hwnd as *mut _),action.clone());}
                        }
                        ACTIVE.store(true, Ordering::Relaxed);
                        APP.with(|a| *a.borrow_mut() = Some(e));
                        redraw();
                    }
                    Err(e) => {
                        debug_assert!(entry_actions(false).is_empty());
                        crate::log!("widget editor: {e}");
                    },
                }
            }
            Request::Select(h) => {
                let Some(h) = key_of(h) else { continue };
                if active() {
                    APP.with(|a| {
                        if let Some(e) = a.borrow_mut().as_mut() {
                            e.chosen = Some(h);
                        }
                    });
                    redraw();
                    continue;
                }
                match unsafe { Editor::new(None) } {
                    Ok(mut e) => {
                        e.quiet = true;
                        e.chosen = Some(h);
                        for (entry, _) in visible() {
                            send(HWND(entry.hwnd as *mut _), Action::Enter);
                        }
                        ACTIVE.store(true, Ordering::Relaxed);
                        APP.with(|a| *a.borrow_mut() = Some(e));
                        redraw();
                    }
                    Err(e) => crate::log!("widget editor: {e}"),
                }
            }
            Request::Finish(event) => {
                APP.with(|a| {
                    if let Some(e) = a.borrow_mut().as_mut() {
                        e.mode = transition(e.mode, event);
                    }
                });
                ACTIVE.store(false, Ordering::Relaxed);
                let entries = ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).clone();
                for e in entries {
                    send(HWND(e.hwnd as *mut _), Action::Exit);
                }
                APP.with(|a| {
                    let e = a.borrow_mut().take();
                    drop(e);
                });
            }
            Request::Refresh => redraw(),
        }
    }
}
fn redraw() {
    APP.with(|a| {
        if let Ok(mut a) = a.try_borrow_mut() {
            if let Some(e) = a.as_mut() {
                if let Err(err) = unsafe { e.draw() } {
                    crate::log!("widget editor paint: {err}");
                }
            }
        }
    });
}
impl Editor {
    unsafe fn new(target:Option<isize>) -> Result<Self> {
        let gfx = std::rc::Rc::new(Gfx::new()?);
        let mut overlays = vec![];
        let cards = visible();
        let monitors=super::grid_state::monitors();
        let gallery=gallery_monitor(&monitors,target);
        for (index, m) in monitors.into_iter().enumerate() {
            if !needs_overlay(cards.iter().any(|(_,r)|widget::intersects(*r,m.work)),index,gallery) {
                continue;
            }
            let window = super::edit_surface::Surface::new(
                gfx.clone(),
                &THEME.with(|t| t.borrow().clone()),
                m,
                Some(proc),
            )?;
            super::privacy::register(window.hwnd, false);
            overlays.push(Overlay {
                window,
                monitor: m,
                index,
            });
        }
        if overlays.is_empty() {
            return Err(windows::core::Error::from_hresult(E_FAIL));
        }
        Ok(Self {
            gfx,
            theme: THEME.with(|t| t.borrow().clone()),
            overlays,
            mode: transition(Mode::Idle, Event::Enter),
            gallery,
            kind: Kind::Battery,
            size: Size::SMALL,
            note: None,
            scroll: 0,
            drag: None,
            chosen: None,
            quiet: false,
            pending: None,
            resizing: None,
            size_menu: None,
        })
    }
    unsafe fn draw(&mut self) -> Result<()> {
        self.theme = THEME.with(|t| t.borrow().clone());
        let cards = visible();
        for (index, m) in super::grid_state::monitors().into_iter().enumerate() {
            let occupied = cards.iter().any(|(_, r)| widget::intersects(*r, m.work));
            if needs_overlay(occupied,index,self.gallery) && !self.overlays.iter().any(|o| o.index == index) {
                self.overlays.push(Overlay {
                    window: super::edit_surface::Surface::new(
                        self.gfx.clone(),
                        &self.theme,
                        m,
                        Some(proc),
                    )?,
                    monitor: m,
                    index,
                });
            }
        }
        let collection = crate::notes::hidden_widgets();
        self.scroll = self.scroll.min(collection.len().saturating_sub(3));
        if self
            .note
            .as_ref()
            .is_some_and(|id| !collection.iter().any(|(key, _)| key == id))
        {
            self.note = None;
        }
        for o in &mut self.overlays {
            let m = o.monitor;
            let frames: Vec<_> = cards
                .iter()
                .filter(|(_, r)| widget::intersects(*r, m.work))
                .map(|(e, r)| (e.hwnd, *r))
                .collect();
            let (preview, preview_hwnd) = if self.kind == Kind::Note {
                let preview = self.note
                    .as_deref()
                    .and_then(crate::notes::hidden_widget_preview)
                    .unwrap_or_else(|| Preview::empty(Kind::Note));
                let hwnd = self.note.as_ref().and_then(|id| hwnd_of(&Key::Note(id.clone())));
                (preview, hwnd)
            } else {
                ENTRIES
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .find(|e| e.preview.kind() == self.kind)
                    .map(|e| (e.preview.clone(), Some(HWND(e.hwnd as *mut _))))
                    .unwrap_or_else(|| (Preview::empty(self.kind), None))
            };
            // The chosen card on this display (gone: none), and the free
            // places round it.
            let chosen = self.chosen.as_ref().and_then(|c| find_key(&cards, c)).map(|(_, r)| *r).filter(|r| widget::intersects(*r, m.work));
            let places = if chosen.is_some() { slots_on(m, None) } else { Vec::new() };
            let ghost = self.resizing.as_ref().filter(|r| widget::intersects(r.origin, m.work)).map(|r| r.result.0);
            let mut scene = super::edit_view::Scene {
                monitor: m,
                frames: &frames,
                slots: &[],
                ghost,
                lift: None,
                gallery: self.gallery == Some(o.index),
                kind: self.kind,
                size: self.size,
                preview: &preview,
                preview_hwnd: preview_hwnd.unwrap_or(o.window.hwnd),
                notes: &collection[self.scroll..],
                selected: self.note.as_deref(),
                chosen,
                quiet: self.quiet,
                places: &places,
            };
            o.window.draw(&self.theme, &mut scene)?;
            o.window.show();
        }
        // Reinsert cards beneath the editor, exclusively through the desktop-layer API.
        for (e, _) in cards {
            let h = HWND(e.hwnd as *mut _);
            win::to_desktop(h, win::shell_host());
        }
        Ok(())
    }
    unsafe fn end_drag(&mut self, commit: bool) {
        let released = super::drag_metrics::now();
        if let Some(d) = self.drag.take() {
            let h = HWND(d.entry.hwnd as *mut _);
            let r = if commit {
                let target = super::drag_hint::release_point(h)
                    .and_then(|p| d.placement.drop_at(d.origin, d.start, p, d.size));
                widget::landing(target, d.origin)
            } else {
                d.origin
            };
            // The owner thread applies geometry (including DPI) and persists it.
            // Keep its committed reservation while the queued drop is being processed.
            if IsWindow(Some(h)).as_bool() {
                icons::commit(h, r);
                super::drag_hint::settle(h, r, released);
            }
            self.mode = transition(
                self.mode,
                if commit {
                    Event::Drop
                } else {
                    Event::CancelDrag
                },
            );
            let _ = ReleaseCapture();
        }
    }
    unsafe fn pointer(&mut self, h: HWND, m: u32, p: POINT) {
        if m == WM_MOUSEMOVE {
            if let Some(drag) = &mut self.drag {
                drag.sample(super::drag_metrics::now(), p);
                return;
            }
        }
        let Some(oi) = self.overlays.iter().position(|o| o.window.hwnd == h) else {
            return;
        };
        let mon = self.overlays[oi].monitor;
        let mi = self.overlays[oi].index;
        let dpi = mon.scale();
        let local = (
            (p.x - mon.work.left) as f32 / dpi,
            (p.y - mon.work.top) as f32 / dpi,
        );
        if m == WM_MOUSEMOVE {
            let started = super::drag_metrics::now();
            // A press on a card moved past 6 px: it is dragged.
            if self.pending.as_ref().is_some_and(|q| super::grab::beyond(q.start, p)) {
                if let Some(q) = self.pending.take() {
                    self.start_drag(h, q.entry, q.rect, q.start, started);
                }
            }
            if let Some(r) = &mut self.resizing {
                r.result = resize_to(r, p);
                return;
            }
            if let Some(d) = &mut self.drag {
                d.sample(started, p);
            }
            return;
        }
        if m == WM_LBUTTONUP {
            if let Some(r) = self.resizing.take() {
                let _ = ReleaseCapture();
                let (to, size) = resize_to(&r, p);
                if let Some(hc) = hwnd_of(&r.key).filter(|_| to != r.origin) {
                    icons::commit(hc, to);
                    send(hc, Action::Resize(to, size));
                }
                return;
            }
            // A click on a card: it is chosen (its handles show).
            if let Some(q) = self.pending.take() {
                let _ = ReleaseCapture();
                self.chosen = Some(q.entry.key);
                return;
            }
            self.end_drag(true);
            return;
        }
        let cards = visible();
        if m == WM_RBUTTONUP {
            if let Some((e, _)) = cards.iter().find(|(_, r)| contains(*r, p)) {
                let _ = PostMessageW(
                    Some(HWND(e.hwnd as *mut _)),
                    WM_CONTEXTMENU,
                    WPARAM(e.hwnd as usize),
                    LPARAM(((p.y as u16 as usize) << 16 | p.x as u16 as usize) as isize),
                );
            }
            return;
        }
        if m != WM_LBUTTONDOWN {
            return;
        }
        // The chosen card's menu and handles come first.
        if let Some((ch, cr)) = self.chosen.as_ref().and_then(|c| find_key(&cards, c)).map(|(e, r)| (e.clone(), *r)) {
            if widget::intersects(cr, mon.work) {
                let c = super::edit_view::local(mon, cr);
                let (_, items) = super::edit_view::menu(&self.gfx, mon, c);
                if let Some((item, _)) = items.iter().find(|(_, r)| inside(*r, local)) {
                    self.menu_item(h, *item, ch, p);
                    return;
                }
                if let Some(edge) = super::edit_view::handle_at(c, local) {
                    let kind = ch.preview.kind();
                    let size = ch.size.unwrap_or_else(|| size_of(cr, mon));
                    self.resizing = Some(Resizing { key: ch.key, kind, edge, origin: cr, start: p, result: (cr, size), _grid: crate::desklayer::grid_state::Frozen::new() });
                    SetCapture(h);
                    return;
                }
            }
        }
        let collection = crate::notes::hidden_widgets();
        let hit = super::edit_view::hit(
            mon,
            local,
            self.gallery == Some(mi),
            self.kind,
            &collection[self.scroll.min(collection.len())..],
        );
        // Entered by a long press: no "+ widget", no "done" there.
        let hit = if self.quiet && matches!(hit, super::edit_view::Hit::Done | super::edit_view::Hit::Gallery) { super::edit_view::Hit::None } else { hit };
        match hit {
            super::edit_view::Hit::Done => {
                self.mode = transition(self.mode, Event::Done);
                finish();
                return;
            }
            super::edit_view::Hit::Gallery => {
                self.gallery = Some(mi);
                return;
            }
            super::edit_view::Hit::Kind(k) => {
                self.kind = k;
                self.note = None;
                self.scroll = 0;
                return;
            }
            super::edit_view::Hit::Collection(step) => {
                self.scroll = if step > 0 {
                    self.scroll.saturating_add(3)
                } else {
                    self.scroll.saturating_sub(3)
                };
                return;
            }
            super::edit_view::Hit::Page(step) => {
                let i = Size::ALL.iter().position(|s| *s == self.size).unwrap_or(0) as i32;
                self.size = Size::ALL[(i + step).rem_euclid(3) as usize];
                return;
            }
            super::edit_view::Hit::Note(id) => {
                self.note = Some(id);
                return;
            }
            super::edit_view::Hit::NewNote => {
                self.note = None;
                return;
            }
            super::edit_view::Hit::Add => {
                let kind = self.kind;
                let existing = cards
                    .iter()
                    .find(|(e, _)| kind != Kind::Note && e.preview.kind() == kind)
                    .map(|(e, _)| HWND(e.hwnd as *mut _));
                if widget::auto(
                    &widget::desk(dpi),
                    &super::grid_state::monitors(),
                    mi,
                    self.size,
                    &icons::widget_rects(existing),
                )
                .is_none()
                {
                    return;
                }
                crate::dock::set_widget_enabled(kind, true);
                let action = Action::Add {
                    kind,
                    size: self.size,
                    monitor: mi,
                    note: self.note.clone(),
                };
                match kind {
                    Kind::Battery => crate::device_battery::runtime::widget_action(action),
                    _ => crate::notes::widget_action(action),
                }
                return;
            }
            super::edit_view::Hit::Panel => return,
            super::edit_view::Hit::None => {}
        }
        if let Some((e, _)) = cards.iter().filter(|_| !self.quiet).find(|(_, r)| {
            let (bx, by) = super::edit_view::badge_center(*r, mon);
            let dx = local.0 - bx;
            let dy = local.1 - by;
            dx * dx + dy * dy <= 121.
        }) {
            match e.preview.kind() {
                Kind::Note => send(HWND(e.hwnd as *mut _), Action::Remove),
                k => crate::dock::set_widget_enabled(k, false),
            };
            refresh();
            return;
        }
        if let Some((e, r)) = cards.into_iter().find(|(_, r)| contains(*r, p)) {
            // A click chooses it; a move past 6 px drags it.
            self.pending = Some(Pending { entry: e, rect: r, start: p });
            SetCapture(h);
        } else {
            self.mode = transition(self.mode, Event::Blank);
            finish();
        }
    }
}
#[cfg(test)]
impl Editor {
    /// An editor with no overlay (tests of its message paths).
    fn bare() -> Self {
        Self {
            gfx: crate::gfx::promo_kit::stage().gfx.clone(),
            theme: Theme::graphite(),
            overlays: Vec::new(),
            mode: Mode::Editing,
            gallery: None,
            kind: Kind::Battery,
            size: Size::SMALL,
            note: None,
            scroll: 0,
            drag: None,
            chosen: None,
            quiet: true,
            pending: None,
            resizing: None,
            size_menu: None,
        }
    }
}
impl Editor {
    unsafe fn start_drag(&mut self, h: HWND, e: Entry, r: RECT, start: POINT, started: f64) {
        let hcard = HWND(e.hwnd as *mut _);
        let frozen = super::grid_state::Frozen::new();
        let placement = super::grab::Placement::new(hcard);
        // Its saved size; read from its px (once) only when not known.
        let size = super::grab::fixed(e.size, r, &placement.mons, widget::desk);
        if !super::drag_hint::begin(hcard, r, size, true, started, &placement) {
            let _ = ReleaseCapture();
            return;
        }
        DRAGGING.store(hcard.0 as isize, Ordering::Relaxed);
        self.chosen = None;
        self.drag = Some(Drag { size, entry: e, origin: r, start, placement, _grid: frozen });
        self.mode = transition(self.mode, Event::Drag);
        refresh();
        SetCapture(h);
    }
    /// An item of the chosen card's menu.
    unsafe fn menu_item(&mut self, h: HWND, item: super::edit_view::Item, e: Entry, p: POINT) {
        match item {
            super::edit_view::Item::Size => {
                self.size_menu = Some((e.key, e.preview.kind(), p));
                let _ = PostMessageW(Some(h), SIZE_MENU, WPARAM(0), LPARAM(0));
            }
            super::edit_view::Item::Settings => {
                crate::settings::launch_page("general");
                finish();
            }
            super::edit_view::Item::Remove => {
                match e.preview.kind() {
                    // A note is hidden (its text kept), as its badge does.
                    Kind::Note => send(HWND(e.hwnd as *mut _), Action::Remove),
                    k => crate::dock::set_widget_enabled(k, false),
                }
                self.chosen = None;
                if self.quiet {
                    finish();
                }
            }
        }
    }
}

/// Pure: the size (in places) of card `r` on monitor `m`'s grid.
fn size_of(r: RECT, m: crate::monitors::Monitor) -> Size {
    let dpi = m.scale();
    super::grid::Grid::widget(&m, widget::desk(dpi).cell).map_or(Size::SMALL, |g| Size::nearest((r.right - r.left, r.bottom - r.top), &g, dpi))
}

/// Where the chosen card's dragged side puts it with the pointer at `p`.
fn resize_to(r: &Resizing, p: POINT) -> (RECT, Size) {
    let mons = super::grid_state::monitors();
    let Some(mi) = super::grid::monitor_of(r.origin, &mons) else { return (r.origin, Size::SMALL) };
    let dpi = mons[mi].scale();
    let desk = widget::desk(dpi);
    let Some(g) = super::grid::Grid::widget(&mons[mi], desk.cell) else { return (r.origin, size_of(r.origin, mons[mi])) };
    let Some(span) = g.touched(r.origin) else { return (r.origin, size_of(r.origin, mons[mi])) };
    let (d, place) = match r.edge {
        widget::Edge::Left | widget::Edge::Right => (p.x - r.start.x, widget::SLOT * g.cw),
        widget::Edge::Top | widget::Edge::Bottom => (p.y - r.start.y, widget::SLOT * g.ch),
    };
    let others = widget::obstacles(&desk, &icons::widget_rects(hwnd_of(&r.key)));
    let (span, size) = widget::resized(&g, span, r.edge, widget::steps(d, place), widget::min_size(r.kind), 0, &others);
    (g.rect(span, 0), size)
}

/// The chosen card's size menu (Request posted as SIZE_MENU): every size
/// it may take, the one it has checked; a pick resizes it where it is.
unsafe fn size_menu(h: HWND) {
    let Some((gfx, theme, (card, kind, at))) = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().and_then(|e| e.size_menu.take().map(|m| (e.gfx.clone(), e.theme.clone(), m))))) else { return };
    let Some(hc) = hwnd_of(&card) else { return };
    let Some(r) = icons::committed(hc).or_else(|| icons::card(hc)) else { return };
    let mons = super::grid_state::monitors();
    let Some(mi) = super::grid::monitor_of(r, &mons) else { return };
    let now = ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|e| e.key == card).and_then(|e| e.size).unwrap_or_else(|| size_of(r, mons[mi]));
    let min = widget::min_size(kind);
    let sizes: Vec<Size> = Size::every().filter(|s| s.w >= min.w && s.h >= min.h).collect();
    let items = sizes.iter().enumerate().map(|(i, s)| crate::popup::Item::new(100 + i as u32, format!("{}×{}", s.w, s.h)).literal().checked(*s == now)).collect();
    let picked = crate::popup::track(crate::popup::Request {
        gfx: &gfx,
        theme: &theme,
        items,
        at,
        align: crate::popup::Align::TopLeft,
        scale: mons[mi].scale(),
        titles: Default::default(),
        on_slide: None,
        drag_ids: 0..0,
        on_space: None,
        on_pick: None,
        hangs: false,
    });
    let crate::popup::Outcome::Picked(id) = picked else { return };
    let Some(size) = sizes.get(id.wrapping_sub(100) as usize).copied() else { return };
    let dpi = mons[mi].scale();
    if let Some(to) = widget::snap(&widget::desk(dpi), &mons, r, Some(size), &icons::widget_rects(Some(hc))) {
        icons::commit(hc, to);
        send(hc, Action::Resize(to, size));
    }
    let _ = h;
    refresh();
}

fn inside(r: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F, p: (f32, f32)) -> bool {
    p.0 >= r.left && p.0 < r.right && p.1 >= r.top && p.1 < r.bottom
}

impl Drop for Editor {
    fn drop(&mut self) {
        FRAME_PENDING.with(|p| p.set(false));
        unsafe {
            self.end_drag(false);
            for o in &self.overlays {
                super::privacy::forget(o.window.hwnd);
            }
        }
    }
}
/// The free places of monitor `m` (`but`'s own place counts as free).
pub fn slots_on(m: crate::monitors::Monitor, but: Option<HWND>) -> Vec<RECT> {
    let dpi = m.scale();
    let desk = widget::desk(dpi);
    super::grid::Grid::widget(&m, desk.cell).map_or_else(Vec::new, |g| widget::free_slots(&g, 0, &widget::obstacles(&desk, &icons::widget_rects(but))))
}
fn contains(r: RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}
/// The pointer moved over overlay `h` (after Editor::pointer): while
/// something follows it, one frame is asked for (at most one pending).
unsafe fn after_move(h: HWND) {
    let follows = APP.with(|a| a.try_borrow().ok().is_some_and(|a| a.as_ref().is_some_and(|e| e.resizing.is_some())));
    if follows && !FRAME_PENDING.with(|p| p.replace(true)) {
        SetTimer(Some(h), HOLD, 16, None);
    }
}
unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if m == super::backdrop::WM_BACKDROP {
        redraw();
        return LRESULT(0);
    }
    if m == SIZE_MENU {
        size_menu(h);
        return LRESULT(0);
    }
    if m == WM_WINDOWPOSCHANGING {
        win::pin(
            super::layer::Layer::Desktop,
            h,
            &mut *(l.0 as *mut WINDOWPOS),
        );
    }
    if m == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    if m == WM_KEYDOWN && local_escape(w.0 == VK_ESCAPE.0 as usize, GetAncestor(GetFocus(),GA_ROOT)==h) {
        request(Request::Finish(Event::Escape));
        return LRESULT(0);
    }
    if matches!(
        m,
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE | WM_RBUTTONUP
    ) {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        APP.with(|a| {
            if let Ok(mut a) = a.try_borrow_mut() {
                if let Some(e) = a.as_mut() {
                    e.pointer(h, m, p);
                }
            }
        });
        if m == WM_MOUSEMOVE {
            after_move(h);
        } else {
            refresh();
        }
        return LRESULT(0);
    }
    if m == WM_MOUSEWHEEL {
        APP.with(|a| {
            if let Ok(mut a) = a.try_borrow_mut() {
                if let Some(e) = a.as_mut() {
                    if e.gallery.is_some() && e.kind == Kind::Note {
                        if ((w.0 >> 16) as i16) < 0 {
                            e.scroll = e.scroll.saturating_add(1);
                        } else {
                            e.scroll = e.scroll.saturating_sub(1);
                        }
                    }
                }
            }
        });
        refresh();
        return LRESULT(0);
    }
    if m == WM_CAPTURECHANGED {
        APP.with(|a| {
            if let Ok(mut a) = a.try_borrow_mut() {
                if let Some(e) = a.as_mut() {
                    e.pending = None;
                    e.resizing = None;
                    e.end_drag(false);
                }
            }
        });
        refresh();
    }
    if m == WM_TIMER && w.0 == HOLD {
        FRAME_PENDING.with(|p| p.set(false));
        let _ = KillTimer(Some(h), HOLD);
        refresh();
        return LRESULT(0);
    }
    DefWindowProcW(h, m, w, l)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entry_long_press_threshold_and_drag_transitions() {
        assert_eq!(transition(Mode::Idle, Event::Hold(499)), Mode::Idle);
        assert_eq!(transition(Mode::Idle, Event::Hold(500)), Mode::Editing);
        assert_eq!(transition(Mode::Idle, Event::Drag), Mode::Idle);
        assert_eq!(transition(Mode::Editing, Event::Drag), Mode::Dragging);
        assert_eq!(transition(Mode::Dragging, Event::Drop), Mode::Editing);
        assert_eq!(transition(Mode::Dragging, Event::CancelDrag), Mode::Editing);
    }
    /// A dropped card glides to its place in 120 ms, eased out (fast first),
    /// its lift going from 1 to 0.
    #[test]
    fn every_exit_ends_drag_and_edit() {
        for mode in [Mode::Editing, Mode::Dragging] {
            for event in [Event::Escape, Event::Done, Event::Blank, Event::OtherApp] {
                assert_eq!(transition(mode, event), Mode::Idle);
            }
        }
    }

    unsafe extern "system" fn quiet(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        DefWindowProcW(h, m, w, l)
    }

    /// Review P1: a battery (or the inbox) resized twice and more by its
    /// handles keeps its window: the command's handler goes on with a live
    /// window, and the chosen widget (by its key) still finds its card, so
    /// the handles stay.
    #[test]
    fn a_card_resized_again_and_again_keeps_its_window_and_handles() {
        use crate::device_battery::view::{remake, Remake, View};
        let stage = crate::gfx::promo_kit::stage();
        let theme = Theme::graphite();
        let mut v = unsafe { View::sized(stage.gfx.clone(), &theme, Size::SMALL, (152., 152.), 1., Some(quiet)).unwrap() };
        let h = v.hwnd;
        let key = Key::Note("review-p1-resize".into());
        remember(h, key.clone(), Some(Size::SMALL), Preview::empty(Kind::Battery));
        let chosen = Some(key.clone());
        for (size, dims, dpi) in [(Size::MEDIUM, (320., 152.), 1.), (Size::LARGE, (320., 320.), 1.), (Size::new(3, 2), (488., 320.), 1.5), (Size::SMALL, (152., 152.), 1.5)] {
            assert_eq!(remake(Some(v.shape()), (size, dpi, dims)), Remake::Resize, "{size:?}");
            v.resize(size, dims, dpi).unwrap();
            assert_eq!(remake(Some(v.shape()), (size, dpi, dims)), Remake::Keep);
            assert_eq!(v.hwnd, h, "the same window");
            assert!(unsafe { IsWindow(Some(v.hwnd)) }.as_bool());
            let px = ((dims.0 * dpi).round() as i32, (dims.1 * dpi).round() as i32);
            assert_eq!((v.glass.card, v.glass.dpi), (px, dpi));
            remember(v.hwnd, key.clone(), Some(size), Preview::empty(Kind::Battery));
            // What the command handler does next: no panic on its window.
            apply_owner_visibility(h, &Action::Resize(RECT::default(), size));
            assert_eq!(hwnd_of(chosen.as_ref().unwrap()), Some(h), "still chosen");
        }
        assert_eq!(remake(None, (Size::SMALL, 1., (152., 152.))), Remake::Make);
        drop(v);
        forget(h);
    }

    /// Review P3: the pointer moving a handle (edit mode's `resizing`, no
    /// card dragged) asks for a frame, as a dragged card does: the result's
    /// outline follows. Nothing held: no frame.
    #[test]
    fn moving_a_handle_asks_for_a_frame() {
        let h = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), windows::core::w!("STATIC"), None, WS_POPUP, 0, 0, 10, 10, None, None, None, None).unwrap() };
        let frame_comes = || {
            let until = Instant::now() + std::time::Duration::from_millis(500);
            let mut msg = MSG::default();
            while Instant::now() < until {
                if unsafe { PeekMessageW(&mut msg, Some(h), WM_TIMER, WM_TIMER, PM_REMOVE) }.as_bool() && msg.wParam.0 == HOLD {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            false
        };
        let mut e = Editor::bare();
        e.resizing = Some(Resizing { key: Key::Battery, kind: Kind::Battery, edge: widget::Edge::Right, origin: RECT { left: 0, top: 0, right: 152, bottom: 152 }, start: POINT::default(), result: (RECT::default(), Size::SMALL), _grid: crate::desklayer::grid_state::Frozen::new() });
        APP.with(|a| *a.borrow_mut() = Some(e));
        FRAME_PENDING.with(|p| p.set(false));
        unsafe { after_move(h) };
        assert!(FRAME_PENDING.with(|p| p.get()), "a frame is asked for");
        assert!(frame_comes(), "and comes");
        // Nothing held: no frame.
        APP.with(|a| a.borrow_mut().as_mut().unwrap().resizing = None);
        FRAME_PENDING.with(|p| p.set(false));
        unsafe {
            let _ = KillTimer(Some(h), HOLD);
            after_move(h);
        }
        assert!(!FRAME_PENDING.with(|p| p.get()));
        assert!(!frame_comes());
        APP.with(|a| a.borrow_mut().take());
        unsafe {
            let _ = DestroyWindow(h);
        }
    }

    /// Review P1: should a widget's window still be made again, the choice
    /// follows the widget, not the old handle; and a command's handler never
    /// asserts on a window gone meanwhile.
    #[test]
    fn a_choice_follows_the_widget_to_its_new_window() {
        let key = Key::Note("review-p1-new-window".into());
        let (old, new) = (HWND(0x7_0001 as *mut _), HWND(0x7_0002 as *mut _));
        remember(old, key.clone(), Some(Size::MEDIUM), Preview::empty(Kind::Note));
        assert_eq!(hwnd_of(&key), Some(old));
        remember(new, key.clone(), None, Preview::empty(Kind::Note));
        assert_eq!(hwnd_of(&key), Some(new));
        assert_eq!(key_of(old.0 as isize), None, "the old window is no longer the widget's");
        let r = RECT { left: 10, top: 10, right: 330, bottom: 162 };
        let e = |hwnd: HWND| Entry { hwnd: hwnd.0 as isize, key: key.clone(), size: Some(Size::MEDIUM), preview: Preview::empty(Kind::Note) };
        let cards = vec![(e(new), r)];
        assert_eq!(find_key(&cards, &key).map(|(e, r)| (e.hwnd, *r)), Some((new.0 as isize, r)));
        forget(new);
        // A window destroyed under a command: nothing happens, no assert.
        let gone = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), windows::core::w!("STATIC"), None, WS_POPUP, 0, 0, 10, 10, None, None, None, None).unwrap() };
        unsafe {
            let _ = DestroyWindow(gone);
        }
        for a in [Action::Resize(r, Size::LARGE), Action::Hide, Action::Place(r), Action::Enter] {
            apply_owner_visibility(gone, &a);
        }
    }
}


// Input decisions are kept independent of HWNDs for regression tests.
fn hold_surface(control: bool, rich_edit: bool) -> bool { !control && !rich_edit }
fn hold_fires(elapsed: u64, down: bool, same_card: bool, home: bool) -> bool {
    transition(Mode::Idle,Event::Hold(elapsed))==Mode::Editing && down && same_card && home
}
fn hold_cancelled(message:u32)->bool { matches!(message,WM_LBUTTONUP|WM_CANCELMODE|WM_CAPTURECHANGED|WM_ENTERMENULOOP) }
fn home_foreground(shell:bool,own:bool)->bool { shell || own }
fn local_escape(key_message:bool,focused:bool)->bool { key_message && focused }
fn entry_actions(ready:bool)->Vec<Action> { if ready {vec![Action::Enter]} else {vec![]} }
#[cfg(test)]
mod review_tests {
 use super::*;
 #[test] fn review_01_menu_release_and_capture_cancel_hold(){
  assert!(!hold_surface(true,false));
  for m in [WM_ENTERMENULOOP,WM_CAPTURECHANGED,WM_CANCELMODE,WM_LBUTTONUP] {assert!(hold_cancelled(m));}
  assert!(!hold_fires(600,false,true,true));assert!(!hold_fires(600,true,false,true));
  assert!(hold_fires(500,true,true,true));assert!(!hold_fires(499,true,true,true));
 }
 #[test] fn review_04_rich_edit_never_arms_hold(){assert!(!hold_surface(false,true));assert!(hold_surface(false,false));}
 #[test] fn review_05_other_app_keeps_escape_and_blocks_hold(){
  assert!(!home_foreground(false,false));assert!(home_foreground(true,false));assert!(home_foreground(false,true));
  assert!(!hold_fires(600,true,true,false));assert!(!local_escape(true,false));assert!(local_escape(true,true));assert!(!local_escape(false,true));
 }
 #[test] fn review_06_failed_construction_sends_no_enter(){assert!(entry_actions(false).is_empty());assert!(matches!(&entry_actions(true)[..],[Action::Enter]));}
}


#[cfg(test)] mod drag_review_tests {
 use super::*;
 #[test] fn review_03_note_and_inbox_originals_hide_then_restore_on_owner(){
  for _kind in [Kind::Note,Kind::Inbox] {
   assert_eq!(original_visibility(&Action::Hide),Some(false));
   assert_eq!(original_visibility(&Action::Place(RECT::default())),Some(true));
   assert_eq!(original_visibility(&Action::Enter),None);
   std::thread::spawn(|| {
    let mut position=WINDOWPOS{flags:SWP_NOZORDER|SWP_HIDEWINDOW,..Default::default()};
    win::pin(super::super::layer::Layer::Desktop,HWND::default(),&mut position);
    assert!(!position.flags.contains(SWP_HIDEWINDOW),"an unowned hide is rejected");
    position.flags|=SWP_HIDEWINDOW;
    win::placing(||win::pin(super::super::layer::Layer::Desktop,HWND::default(),&mut position));
    assert!(position.flags.contains(SWP_HIDEWINDOW),"the owner thread preserves its hide");
   }).join().unwrap();
  }
 }
}

pub(super) fn is_home(h:HWND)->bool {
    let mut pid=0;unsafe{GetWindowThreadProcessId(h,Some(&mut pid));}
    home_foreground(win::is_shell(h),pid!=0 && pid==unsafe{windows::Win32::System::Threading::GetCurrentProcessId()})
}
pub fn control_at(h:HWND,p:POINT)->bool {
    let Some(r)=icons::card(h)else{return false;};
    let dpi=unsafe{windows::Win32::UI::HiDpi::GetDpiForWindow(h)}.max(96) as f32/96.;
    let x=(p.x-r.left) as f32/dpi;let y=(p.y-r.top) as f32/dpi;let width=(r.right-r.left) as f32/dpi;
    let entries=ENTRIES.lock().unwrap_or_else(|e|e.into_inner());
    let Some(e)=entries.iter().find(|e|e.hwnd==h.0 as isize)else{return false;};
    let kind=match &e.preview {Preview::Note(..)=>super::card::Kind::Note,Preview::Battery(..)=>super::card::Kind::Battery,Preview::Inbox(..)=>super::card::Kind::Inbox};
    super::card::button_at(kind,width,x,y).is_some()
}
fn original_visibility(action:&Action)->Option<bool> {
    match action {Action::Hide=>Some(false),Action::Place(_)=>Some(true),_=>None}
}
/// Called only by the target window's message handler: PLACING is thread local.
/// Only what shows or hides the card touches it, and only a live window
/// (a handler may have closed its card meanwhile).
pub fn apply_owner_visibility(h:HWND,action:&Action) {
    if matches!(action, Action::DisplaysChanged) && icons::saved_place(h).is_some_and(|p| super::place::connected(p.mon, &crate::monitors::all()).is_none()) {
        win::placing(||unsafe{let _=ShowWindow(h,SW_HIDE);});
        return;
    }
    let Some(show)=original_visibility(action) else {return};
    if show && (super::grid_state::display_settling() || icons::saved_place(h).is_some_and(|p| super::place::connected(p.mon, &crate::monitors::all()).is_none())) { return; }
    if !unsafe{IsWindow(Some(h))}.as_bool() {return;}
    debug_assert_eq!(unsafe{GetWindowThreadProcessId(h,None)},unsafe{windows::Win32::System::Threading::GetCurrentThreadId()});
    win::placing(||unsafe{let _=ShowWindow(h,if show{SW_SHOWNOACTIVATE}else{SW_HIDE});});
}

fn gallery_monitor(mons:&[crate::monitors::Monitor],target:Option<isize>)->Option<usize>{target.and_then(|target|mons.iter().position(|m|m.handle==target)
    .or_else(||mons.iter().position(|m|m.primary)).or_else(||(!mons.is_empty()).then_some(0)))}
fn needs_overlay(occupied:bool,index:usize,gallery:Option<usize>)->bool{occupied || gallery==Some(index)}
#[cfg(test)] mod shell_review_tests {
 use super::*;
 #[test] fn review_13_gallery_opens_on_empty_cursor_monitor_and_survives_missing_display(){
  let r=RECT{left:-1920,top:0,right:0,bottom:1080};
  let a=crate::monitors::Monitor{handle:10,rect:r,work:r,primary:false,dpi:144};
  let b=crate::monitors::Monitor{handle:20,primary:true,..a};
  assert_eq!(gallery_monitor(&[a,b],Some(10)),Some(0));assert!(needs_overlay(false,0,Some(0)));
  assert!(!needs_overlay(false,1,Some(0)));assert!(needs_overlay(true,1,Some(0)));
  assert_eq!(gallery_monitor(&[a,b],Some(99)),Some(1));assert_eq!(gallery_monitor(&[],Some(10)),None);
  assert_eq!(gallery_monitor(&[a,b],None),None);
 }
}

pub fn open_gallery() {
    if let Some(m)=crate::monitors::under_cursor().or_else(crate::monitors::primary) {
        request(Request::Enter(Some(m.handle)));
    }
}
