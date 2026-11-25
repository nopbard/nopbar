//! The notes' own thread and its hidden window (the hub): the list of
//! notes, the layer they are on (crate::desklayer), saving, displays
//! changing, Explorer restarting, quitting and the session ending.
//!
//! A hidden top-level window (not a message-only one) so Windows'
//! broadcasts reach it: WM_DISPLAYCHANGE, WM_SETTINGCHANGE (work area),
//! TaskbarCreated, WM_QUERYENDSESSION / WM_ENDSESSION.
//!
//! Saving happens here, at once (crate::notes::store): 500 ms after typing
//! stops (at most 5 s while it goes on) each note whose editor differs
//! from its file is written; a failure keeps the text in a recovery copy
//! and tries again later. Deleting and restoring are done on the spot;
//! what they return is what the disk has. Quitting asks first, before the
//! dock goes (WM_PREPARE_QUIT): every note is saved, and NopBar's own quit
//! is called off, the notes in front, if a text could be kept nowhere.
//! Installers, updates the second time and Windows' own closing never
//! wait: everything is tried, then they go on.
//!
//! Virtual desktops (crate::notes::desks): each note belongs to one
//! desktop or to all of them. Notes of other desktops are hidden (their
//! windows kept, their text saved like any other) and shown again when
//! their desktop comes back (WM_DESKS, from crate::vdesk's registry watch:
//! no polling). Summoning shows this desktop's notes only. A note's menu
//! (right click) has one row: show it on all desktops.
//!
//! Costs nothing while nothing happens: the thread waits in GetMessage;
//! the save timer runs once after typing; the retry timer only while a
//! save fails; the foreground WinEvent hook is set only while there are
//! notes.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetMessageW, IsIconic, IsWindow, IsWindowVisible,
    KillTimer, PostQuitMessage, RegisterClassExW, SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage, MSG, SPI_SETWORKAREA,
    SW_HIDE, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DISPLAYCHANGE, WM_ENDSESSION, WM_QUERYENDSESSION,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::check;
use super::desks::{self, Desks, WinOp};
use super::note::{self, Ev};
use super::store::{self, Doc, RealDisk};
use crate::desklayer::layer::{self, Action, Event, Layer};
use crate::desklayer::place::{self, Place};
use crate::desklayer::{grid, icons};
use crate::desklayer::win as lw;

/// From a note (wParam: Ev, lParam: the note).
pub const WM_NOTE: u32 = WM_APP + 1;
/// The look changed (crate::notes::retheme).
pub const WM_LOOK: u32 = WM_APP + 2;
/// The shortcut was pressed (registered on the dock's window, which
/// passes it on): summon or send back.
pub const WM_TOGGLE: u32 = WM_APP + 3;
/// Sent by crate::notes::shutdown after the dock is gone: save what can
/// be, close, end the thread.
pub const WM_SHUTDOWN: u32 = WM_APP + 4;
/// The foreground WinEvent (wParam: the new foreground window).
const WM_FOREGROUND: u32 = WM_APP + 5;
/// Sent by crate::notes::prepare_quit before anything goes (wParam: the
/// kind, super::Quit as usize). Answers 1: go on; 0: stay (the notes come
/// to the front).
pub const WM_PREPARE_QUIT: u32 = WM_APP + 6;
/// Notes asked from outside (super::create_at).
pub const WM_NEW: u32 = WM_APP + 9;
pub const WM_MAKE: u32 = WM_APP + 7;
/// The virtual desktops changed (crate::vdesk), or the setting
/// (super::set_per_desktop).
pub const WM_DESKS: u32 = WM_APP + 8;
pub const WM_REMIND: u32 = WM_APP + 10;

const SAVE_TIMER: usize = 1;
const SAVE_MS: u32 = 500;
/// Typing that never pauses is still saved this often.
const SAVE_MAX: Duration = Duration::from_secs(5);
const DISPLAY_TIMER: usize = 2;
const DISPLAY_MS: u32 = 400;
const RETRY_TIMER: usize = 3;
/// Focus the note once Show desktop has settled (홈 화면으로 간 뒤).
const HOME_FOCUS_TIMER: usize = 4;
const HOME_FOCUS_MS: u32 = 220;
/// DIP between a note and the one + makes beside it.
const GAP: f32 = 12.0;
/// Windows' ENDSESSION_CLOSEAPP (Restart Manager: an installer closing
/// apps) and ENDSESSION_CRITICAL: never held up.
const ENDSESSION_CLOSEAPP: isize = 0x1;
const ENDSESSION_CRITICAL: isize = 0x4000_0000;

struct Item {
    size:Option<crate::desklayer::widget::Size>,
    hidden:bool,
    id: String,
    hwnd: HWND,
    place: Place,
    /// The text its file has (None: not known, or no file yet).
    saved: Option<String>,
    /// The text last kept in a recovery copy.
    kept: Option<String>,
    /// The last text read from its editor, or captured when its window
    /// was destroyed: what is saved while the editor cannot be read.
    last: String,
    /// The virtual desktop it belongs to (None: every desktop).
    desk: Option<String>,
}

/// A deleted note, for Ctrl+Z (this session).
struct Gone {
    id: String,
    md: String,
    place: Place,
    desk: Option<String>,
}

struct Hub {
    hwnd: HWND,
    /// Stacking order (first: the bottom one).
    notes: Vec<Item>,
    deleted: Vec<Gone>,
    /// Since when changes wait for the save timer.
    dirty_since: Option<Instant>,
    /// Saves failed in a row (the retry waits longer each time).
    fails: u32,
    /// The layout this thread last wrote to notes.json (or loaded): what a
    /// write compares with to know which fields it changed (store::merge),
    /// and its text once written whole.
    layout: store::Index,
    layout_text: String,
    last: String,
    /// The window to give the focus back to on dismiss.
    before: HWND,
    /// The last other app's window in front (from the hook).
    other: HWND,
    /// The desktop was in front (Show desktop) at the last foreground change.
    shell_front: bool,
    /// This summon showed the desktop itself (dismiss puts the apps back).
    homed: bool,
    /// Only shortcut-created scratch notes: dropped when sent back empty.
    fresh: Vec<String>,
    hook: isize,
    made: u32,
    /// A logoff is held up (ShutdownBlockReasonCreate).
    blocking: bool,
}

thread_local! {
    static HUB: RefCell<Option<Hub>> = const { RefCell::new(None) };
    static LAYER: Cell<Layer> = const { Cell::new(Layer::Desktop) };
    /// The virtual desktops as last read, and the setting.
    static DESKS: RefCell<Desks> = RefCell::new(Desks::default());
}

fn desks() -> Desks {
    DESKS.with(|d| d.borrow().clone())
}
pub(super) fn current_desktop()->Option<String>{desks().home_for_new()}
pub(super) fn shows_desktop(d:Option<&str>)->bool{desks().shows(d)}
pub(super) fn reveal(id:Option<&str>){if let Some(id)=id{with(|h|h.last=id.to_owned());}summon();}
pub(super) fn captured_note(text:&str)->Option<String>{
    let m=crate::monitors::under_cursor().or_else(crate::monitors::primary)?;let p=grid_spot(&m).unwrap_or_else(||place::first_on(&m,240.,200.,32.,0));
    let id=new_id();let hwnd=open(&id,p,text,None,false,desks().home_for_new())?;on_note(Ev::Moved,hwnd);schedule_save();Some(id)
}
pub(super) fn reminder_seed()->Vec<(String,String)>{docs().into_iter().map(|d|(d.id,d.text)).collect()}
pub(super) fn complete_line(id:&str,line:&str){
    let w=with(|h|h.notes.iter().find(|n|n.id==id).map(|n|n.hwnd)).flatten();
    if let Some(w)=w{if let Some(body)=note::markdown(w){let mut done=false;let body=body.lines().map(|s|if !done&&s==line{done=true;format!("- [x] {}",s.trim_start_matches("- [ ] "))}else{s.to_owned()}).collect::<Vec<_>>().join("\n");note::replace_markdown(w,&body);schedule_save();}}
}

/// Does note `n` show on this desktop?
fn shown(n: &Item) -> bool {
    !n.hidden && (super::headless() || place::connected(n.place.mon, &crate::monitors::all()).is_some()) && DESKS.with(|d| d.borrow().shows(n.desk.as_deref()))
}

/// The windows of the notes shown on this desktop.
fn shown_windows() -> Vec<HWND> {
    with(|h| h.notes.iter().filter(|n| shown(n)).map(|n| n.hwnd).collect()).unwrap_or_default()
}

/// The layer the notes are on (read by every note's pin).
pub fn layer() -> Layer {
    LAYER.with(|l| l.get())
}

fn with<R>(f: impl FnOnce(&mut Hub) -> R) -> Option<R> {
    HUB.with(|h| h.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Pure: the wait before retry `n` (1, 2, 4 ... seconds, at most 60).
pub fn backoff(n: u32) -> Duration {
    Duration::from_secs((1u64 << n.saturating_sub(1).min(6)).min(60))
}

/// The thread: makes the hub, publishes it, loads the notes, runs until
/// WM_SHUTDOWN. `ready` hears the hub (0: none); if nobody listens any
/// more (init gave up waiting) the hub still publishes itself and works.
pub fn run(ready: std::sync::mpsc::Sender<isize>) {
    unsafe {
        // RichEdit's drag and drop wants OLE on its thread.
        let _ = windows::Win32::System::Ole::OleInitialize(None);
        let _queue = if super::headless(){None}else{use windows::Win32::System::WinRT::*; CreateDispatcherQueueController(DispatcherQueueOptions{dwSize:std::mem::size_of::<DispatcherQueueOptions>() as u32,threadType:DQTYPE_THREAD_CURRENT,apartmentType:DQTAT_COM_STA}).ok()};
        let Ok(hinst) = GetModuleHandleW(None) else {
            let _ = ready.send(0);
            return;
        };
        let class = w!("NopBar.NotesHub");
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WINDOW_EX_STYLE(0), class, w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None);
        let Ok(hwnd) = hwnd else {
            crate::log!("notes: no hub window");
            let _ = ready.send(0);
            return;
        };
        HUB.with(|h| {
            *h.borrow_mut() = Some(Hub {
                hwnd,
                notes: Vec::new(),
                deleted: Vec::new(),
                dirty_since: None,
                fails: 0,
                layout: store::Index::default(),
                layout_text: String::new(),
                last: String::new(),
                before: HWND::default(),
                other: HWND::default(),
                shell_front: false,
                homed: false,
                fresh: Vec::new(),
                hook: 0,
                made: 0,
                blocking: false,
            })
        });
        // Notes report to this hub (same thread); other threads find it,
        // whether or not init still waits.
        note::set_hub(hwnd);
        super::publish(hwnd);
        let _ = ready.send(hwnd.0 as isize);
        if let Some(l) = super::take_look() {
            note::set_look(l);
        }
        // Told of every change from now on; read once here.
        crate::vdesk::listen(hwnd, WM_DESKS);
        // The watch's first reading (it may have just started), so the
        // notes do not all show for a moment.
        crate::vdesk::wait_first(Duration::from_millis(500));
        DESKS.with(|d| *d.borrow_mut() = read_desks());
        load();
        super::runtime::init(super::panel_theme());
        super::widgets_ready(hwnd);
        // The desktop's grid read once.
        if !super::headless() { icons::read(hwnd); }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        windows::Win32::System::Ole::OleUninitialize();
    }
}

/// The desktops as surely known now (crate::vdesk::sure), and the setting.
fn read_desks() -> Desks {
    Desks::of(crate::vdesk::sure().as_ref(), super::per_desktop())
}

fn load() {
    let mut loaded = store::load(&mut RealDisk::new(), store::now_ms());
    if !loaded.index.seeded {
        if let Some(m) = crate::monitors::primary() {
            let p = grid_spot(&m).unwrap_or_else(|| place::first_on(&m, 240., 200., 32., 0));
            if store::seed_first(&mut RealDisk::new(), p, store::now_ms()) {
                loaded = store::load(&mut RealDisk::new(), store::now_ms());
            }
        }
    }
    let mons = crate::monitors::all();
    let mut unplaced = 0;
    // Notes of desktops closed while NopBar was not running: every
    // desktop, before their windows are made (so they show), and written
    // at once (review 3).
    let mut homes: Vec<Option<String>> = loaded.notes.iter().map(|(e, _)| e.desktop.clone()).collect();
    let (rehomed, _) = desks().settle_loaded(&mut homes);
    for ((e, _), h) in loaded.notes.iter_mut().zip(homes) {
        e.desktop = h;
    }
    for (e, md) in &loaded.notes {
        let mut p = e.place;
        if p.mon == [0; 4] {
            // Not in the index (new, imported): top right, stepped.
            if let Some(m) = mons.first() {
                p = place::first_on(m, p.w, p.h, 32.0, unplaced);
            }
            unplaced += 1;
        }
        if let Some(w) = open_sized(&e.id, p, md, Some(md.clone()), false, e.desktop.clone(), e.size) {
            with(|h|if let Some(n)=h.notes.iter_mut().find(|n|n.hwnd==w){n.hidden=e.hidden;});
            if e.hidden {lw::placing(||unsafe{let _=ShowWindow(w,SW_HIDE);});}
            on_note(Ev::Moved, w);
        }
    }
    with(|h| {
        h.last = loaded.index.last.clone();
        h.layout = loaded.index.clone();
    });
    if rehomed {
        crate::log!("notes: notes of a closed desktop show on every desktop now");
        save();
    }
    apply_desks();
    publish_hidden();
    // The places of notes notes.json did not have.
    schedule_save();
    crate::log!("notes: {} loaded", with(|h| h.notes.len()).unwrap_or(0));
}

/// Makes the window of note `id` (of desktop `desk`) and puts it on the
/// current layer (hidden while its desktop is not this one).
fn open(id: &str, p: Place, md: &str, saved: Option<String>, focus: bool, desk: Option<String>) -> Option<HWND> {
    open_sized(id,p,md,saved,focus,desk,None)
}
fn open_sized(id: &str, p: Place, md: &str, saved: Option<String>, focus: bool, desk: Option<String>,size:Option<crate::desklayer::widget::Size>) -> Option<HWND> {
    let show = desks().shows(desk.as_deref()) && place::connected(p.mon, &crate::monitors::all()).is_some();
    let (hwnd,size) = window(id, p, md, focus, show,size)?;
    // Its face's date: when its file was last written.
    if let Some(t) = std::fs::metadata(store::dir().join(store::body_name(id))).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()) {
        note::set_date(hwnd, t.as_secs() as i64);
    }
    // A widget on the desktop's grid: its cells kept from the icons.
    if !super::headless() { icons::register(hwnd); }
    icons::remember_place(hwnd, p);
    with(|h| h.notes.push(Item { size, hidden:false, id: id.to_string(), hwnd, place: p, saved, kept: None, last: md.to_string(), desk }));
    watch_or_not();
    Some(hwnd)
}

/// A note's window (made, placed, and shown when `show`: its desktop is
/// this one).
fn window(id: &str, p: Place, md: &str, focus: bool, show: bool,size:Option<crate::desklayer::widget::Size>) -> Option<(HWND,Option<crate::desklayer::widget::Size>)> {
    let mons = crate::monitors::all();
    let connected = place::connected(p.mon, &mons).is_some();
    // Keep an offline note in the model and a hidden placeholder window. Its
    // persisted place is never replaced by this temporary geometry.
    let rect = windows::Win32::Foundation::RECT { left:p.mon[0], top:p.mon[1], right:p.mon[2], bottom:p.mon[3] };
    let offline = crate::monitors::Monitor { handle:0, rect, work:rect, primary:false, dpi:96 };
    let min = if size.is_some() { (1., 1.) } else { (note::MIN_W, note::MIN_H) };
    let r = place::to_px(&p, if connected { &mons } else { std::slice::from_ref(&offline) }, min)?;
    let mut chosen_size=size;
    let reservation=if super::headless() || !connected {None}else{icons::reserve(None,|occupied|{
        let (placed,chosen)=startup_rect(r,size,icons::cached().as_ref(),&mons,occupied);chosen_size=chosen;Some(placed)
    })};
    let r=reservation.as_ref().map_or(r,|p|p.rect);
    let hwnd = note::own_move(|| note::create(id, r, &check::to_display(md)))?;
    if !super::headless(){icons::register_card(hwnd,0);icons::commit(hwnd,r);}
    // Home screen only: never above other programs (owner 0.1.89).
    lw::to_desktop(hwnd, lw::shell_host());
    if show && connected && !super::headless() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            if focus {
                let _ = SetForegroundWindow(hwnd);
                note::focus(hwnd);
            }
        }
    }
    Some((hwnd,chosen_size))
}

/// Notes let go of before the desktop's grid was read: snapped once it is.
#[derive(Default, Debug, PartialEq)]
struct Pending(Vec<isize>);

impl Pending {
    /// Pure: `w` waits (once).
    fn add(&mut self, w: isize) {
        if !self.0.contains(&w) {
            self.0.push(w);
        }
    }

    /// Pure: the grid came (`read`: it could be read): the notes still
    /// there (`alive`) to snap now; none when it could not (they stay as
    /// they are until let go of again, no asking in a loop).
    fn take(&mut self, read: bool, alive: impl Fn(isize) -> bool) -> Vec<isize> {
        let all = std::mem::take(&mut self.0);
        if read {
            all.into_iter().filter(|w| alive(*w)).collect()
        } else {
            Vec::new()
        }
    }
}

thread_local! {
    static PENDING: RefCell<Pending> = RefCell::new(Pending::default());
}

/// Note window `w`'s saved size in places (None: not saved, or the hub
/// is busy).
pub fn note_size(w: HWND) -> Option<crate::desklayer::widget::Size> {
    with(|h| h.notes.iter().find(|n| n.hwnd == w).and_then(|n| n.size)).flatten()
}

/// Note window `w` was let go of (moved, resized, just made): on the
/// desktop's cells, whole cells big, and (auto-arrange off) the icons
/// under it moved aside. The grid not read yet: asked for, and the note
/// snapped when it comes (on_grid).
fn snap_note(w: HWND) -> bool {
    if super::headless() { return true; }
    if crate::desklayer::grid_state::display_settling() || icons::saved_place(w).is_some_and(|p| place::connected(p.mon, &crate::monitors::all()).is_none()) { return false; }
    let r = note::card(w);
    let Some(desk) = icons::cached() else {
        PENDING.with(|p| p.borrow_mut().add(w.0 as isize));
        if let Some(h) = with(|h| h.hwnd) {
            icons::read(h);
        }
        return false;
    };
    let previous = icons::committed(w).unwrap_or(r);
    let size=with(|h|h.notes.iter().find(|n|n.hwnd==w).and_then(|n|n.size)).flatten();
    let reservation=icons::reserve(Some(w),|occupied|Some(crate::desklayer::widget::snap(&desk,&crate::monitors::all(),r,size,occupied).unwrap_or(previous)));
    let nr=reservation.as_ref().map_or(previous,|p|p.rect);
    icons::commit(w, nr);
    {
        if nr != r {
            note::set_card(w, nr);
        }
    }
    icons::push(w, w);
    true
}

/// The desktop's grid was read (the hub asked): notes waiting for it snap.
fn on_grid() {
    let read = icons::cached().is_some();
    let ours = |w: isize| with(|h| h.notes.iter().any(|n| n.hwnd.0 as isize == w)).unwrap_or(false);
    for w in PENDING.with(|p| p.borrow_mut().take(read, ours)) {
        on_note(Ev::Moved, HWND(w as *mut _));
    }
}

/// A new note's place on display `m` on the desktop's grid: its free
/// cells at the top right (None: the grid not read yet).
fn grid_spot(m: &crate::monitors::Monitor) -> Option<Place> {
    let desk = icons::cached()?;
    let mons = crate::monitors::all();
    let mi = mons.iter().position(|x| x.rect == m.rect)?;
    let r = crate::desklayer::widget::auto(&desk, &mons, mi, NEW_SIZE, &icons::widget_rects(None))?;
    place::from_px(r, &mons)
}

/// The foreground hook only while there are notes.
fn watch_or_not() {
    if super::headless() { return; }
    let Some((hwnd, any, hook)) = with(|h| (h.hwnd, !h.notes.is_empty(), h.hook)) else { return };
    if any && hook == 0 {
        let hk = lw::watch_foreground(hwnd, WM_FOREGROUND);
        with(|h| h.hook = hk);
    } else if !any && hook != 0 {
        lw::unwatch_foreground(hook);
        with(|h| h.hook = 0);
    }
}

/// Saves soon: 500 ms after the last change, but no later than SAVE_MAX
/// after the first unsaved one.
fn schedule_save() {
    let Some((hwnd, since)) = with(|h| (h.hwnd, *h.dirty_since.get_or_insert_with(Instant::now))) else { return };
    if since.elapsed() >= SAVE_MAX {
        save();
    } else {
        unsafe {
            SetTimer(Some(hwnd), SAVE_TIMER, SAVE_MS, None);
        }
    }
}

fn layout_of(h: &Hub) -> store::Index {
    store::Index { version: 1, seeded: h.layout.seeded || !h.notes.is_empty(), notes: h.notes.iter().map(|n| store::Entry { size:n.size, hidden:n.hidden, id: n.id.clone(), place: n.place, desktop: n.desk.clone() }).collect(), last: h.last.clone() }
}

/// Texts captured from windows destroyed by someone else: taken in at
/// once (a quit may come before their Lost message).
fn absorb_lost() {
    for (id, md) in note::take_lost() {
        with(|h| {
            if let Some(n) = h.notes.iter_mut().find(|n| n.id == id) {
                n.last = md;
            }
        });
    }
}

/// Every note as it is now: its editor's text (or, if it cannot be read,
/// the last text known) and what is known of its file.
fn docs() -> Vec<Doc> {
    absorb_lost();
    let hwnds: Vec<HWND> = with(|h| h.notes.iter().map(|n| n.hwnd).collect()).unwrap_or_default();
    // Read first (editor messages), then record (no borrow across them).
    let read: Vec<Option<String>> = hwnds.into_iter().map(note::markdown).collect();
    with(|h| {
        h.notes
            .iter_mut()
            .zip(read)
            .map(|(n, r)| Doc { id: n.id.clone(), text: store::text_now(r, &mut n.last), saved: n.saved.clone(), kept: n.kept.clone() })
            .collect()
    })
    .unwrap_or_default()
}

/// The note's text now (its editor, else the last text known).
fn text_of(i: usize, w: HWND) -> String {
    let read = note::markdown(w);
    with(|h| h.notes.get_mut(i).map(|n| store::text_now(read, &mut n.last))).flatten().unwrap_or_default()
}

/// Saves every note whose editor differs from its file, now, and the
/// layout until it is written whole. The ids whose text could be kept
/// nowhere, and store::LAYOUT while the layout is not written: both keep
/// the retry going and count when quitting.
fn settle_now() -> Vec<String> {
    let mut docs = docs();
    let changed:Vec<_>=docs.iter().filter(|d|d.dirty()).map(|d|d.id.clone()).collect();
    let mut disk = RealDisk::new();
    // The layout, when it is not on disk whole yet (layout_text is set
    // only once it is).
    let layout = with(|h| {
        h.dirty_since = None;
        let l = layout_of(h);
        let text = store::index_text(&l);
        (text != h.layout_text).then(|| (l, text, h.layout.clone()))
    })
    .flatten();
    let s = store::settle_with_layout(&mut disk, &mut docs, layout.as_ref().map(|(l, _, old)| (l, old)), store::now_ms());
    let all_saved = docs.iter().all(|d| !d.dirty()) && s.layout_done;
    with(|h| {
        for d in &docs {
            if let Some(n) = h.notes.iter_mut().find(|n| n.id == d.id) {
                n.saved = d.saved.clone();
                n.kept = d.kept.clone();
            }
        }
        // Written (whole or not): this thread's fields are on disk now.
        if s.index.is_some() {
            if let Some((l, _, _)) = &layout {
                h.layout = l.clone();
            }
        }
        if s.layout_done {
            if let Some((_, text, _)) = &layout {
                h.layout_text = text.clone();
            }
        }
    });
    for d in &docs {if changed.contains(&d.id) && !d.dirty(){let desk=with(|h|h.notes.iter().find(|n|n.id==d.id).and_then(|n|n.desk.clone())).flatten();super::runtime::note_saved(&d.id,&d.text,desk);}}
    let mut lost = s.lost;
    super::runtime::retain_notes(&docs.iter().map(|d|d.id.clone()).collect::<Vec<_>>());
    if !super::runtime::save(){lost.push("inbox".into());}
    // Not all in their files: again later, a little later each time.
    let Some(hwnd) = with(|h| h.hwnd) else { return lost };
    unsafe {
        let _ = KillTimer(Some(hwnd), SAVE_TIMER);
    }
    if all_saved {
        with(|h| h.fails = 0);
        unsafe {
            let _ = KillTimer(Some(hwnd), RETRY_TIMER);
        }
    } else {
        let n = with(|h| {
            h.fails += 1;
            h.fails
        })
        .unwrap_or(1);
        crate::log!("notes: a save failed; trying again in {} s", backoff(n).as_secs());
        unsafe {
            SetTimer(Some(hwnd), RETRY_TIMER, backoff(n).as_millis() as u32, None);
        }
    }
    if !lost.is_empty() {
        crate::log!("notes: {} text(s) could be kept nowhere yet", lost.len());
    }
    lost
}

fn save() {
    settle_now();
}

fn item_of(hwnd: HWND) -> Option<usize> {
    with(|h| h.notes.iter().position(|n| n.hwnd == hwnd)).flatten()
}

/// `w` is one of the notes: a note, its editor or the editor's RichEdit.
fn is_ours(w: HWND) -> bool {
    note::note_of(w).is_some()
}

/// Runs a layer event.
fn on(ev: Event) {
    let (next, act) = layer::next(layer(), ev);
    match act {
        Action::Nothing => {}
        Action::Summon => summon(),
        Action::Dismiss { restore } => dismiss(restore),
        Action::Release => release(),
    }
    LAYER.with(|l| l.set(next));
}

/// The notes to the front (quitting called off, a logoff held up).
fn show_all() {
    if layer() != Layer::Summoned {
        on(Event::Hotkey);
    }
}

/// A new note's size: medium, wide (owner 0.1.91: a new note came up
/// large and tall).
const NEW_SIZE: crate::desklayer::widget::Size = crate::desklayer::widget::Size::MEDIUM;

/// A new, empty note (its file is written with the next save).
fn new_note(p: Place, focus: bool, temporary: bool) -> Option<String> {
    let id = new_id();
    let hwnd = open_sized(&id, p, "", None, focus, desks().home_for_new(), Some(NEW_SIZE))?;
    on_note(Ev::Moved, hwnd);
    with(|h| {
        h.last = id.clone();
        if temporary {
            h.fresh.push(id.clone());
        }
    });
    schedule_save();
    Some(id)
}

/// Notes asked from outside, each with its text, centered at a point.
fn make_asked() {
    for (text, (x, y), still) in super::take_made() {
        if !still() {
            continue;
        }
        let at = windows::Win32::Foundation::POINT { x, y };
        let hmon = unsafe { windows::Win32::Graphics::Gdi::MonitorFromPoint(at, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST) };
        let Some(m) = crate::monitors::info(hmon.0 as isize).or_else(crate::monitors::primary) else { continue };
        let (w, h) = ((240.0 * m.scale()) as i32, (200.0 * m.scale()) as i32);
        let r = place::clamp(windows::Win32::Foundation::RECT { left: x - w / 2, top: y - h / 2, right: x - w / 2 + w, bottom: y - h / 2 + h }, m.work);
        let Some(p) = place::from_px(r, &crate::monitors::all()) else { continue };
        let id = new_id();
        if let Some(hwnd) = open_sized(&id, p, &text, None, false, desks().home_for_new(), Some(NEW_SIZE)) {
            on_note(Ev::Moved, hwnd);
            with(|h| h.last = id.clone());
            schedule_save();
        }
    }
}

fn create_and_summon() {
    let Some(m) = crate::monitors::under_cursor().or_else(crate::monitors::primary) else { return };
    let p = grid_spot(&m).unwrap_or_else(|| place::first_on(&m, 240., 200., 32., 0));
    if new_note(p, false, false).is_some() {
        summon();
        save();
    }
}

fn summon() {
    super::runtime::layer(true);
    let fg = unsafe { GetForegroundWindow() };
    if layer() != Layer::Summoned {
        with(|h| h.before = if is_ours(fg) { h.other } else { fg });
    }
    LAYER.with(|l| l.set(Layer::Summoned));
    note::set_vivid(true);
    // This desktop's notes (and those of every desktop) only.
    if shown_windows().is_empty() {
        if let Some(m) = crate::monitors::under_cursor().or_else(crate::monitors::primary) {
            new_note(grid_spot(&m).unwrap_or_else(|| place::first_on(&m, 240.0, 200.0, 32.0, 0)), false, true);
        }
    }
    if super::headless() {
        return;
    }
    // Home screen, not an overlay (owner 0.1.89): notes never come above
    // other programs. Show the desktop they live on, then focus the note.
    let home = crate::desklayer::win::is_shell(fg) || is_ours(fg);
    if !home && show_desktop() {
        with(|h| h.homed = true);
        let hub = with(|h| h.hwnd).unwrap_or_default();
        unsafe {
            SetTimer(Some(hub), HOME_FOCUS_TIMER, HOME_FOCUS_MS, None);
        }
    } else {
        focus_last();
    }
}

/// The note last used on this desktop gets the focus.
fn focus_last() {
    let target = with(|h| {
        let here = || h.notes.iter().filter(|n| shown(n));
        here().find(|n| n.id == h.last).or(here().next_back()).map(|n| n.hwnd)
    })
    .flatten();
    if let Some(t) = target.filter(|_| !super::headless()) {
        unsafe {
            let _ = SetForegroundWindow(t);
        }
        note::focus(t);
    }
}

/// Explorer's own Show desktop toggle (the taskbar button). Works with the
/// hotkey's modifiers still down, unlike sending Win+D.
fn show_desktop() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_COMMAND};
    unsafe {
        if let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), None) {
            if !tray.is_invalid() && PostMessageW(Some(tray), WM_COMMAND, WPARAM(407), LPARAM(0)).is_ok() {
                return true;
            }
        }
    }
    matches!(crate::keys::unmixed(&crate::keys::SHOW_DESKTOP), crate::keys::Sent::Ok)
}

/// The one rule that drops a note by itself: made during this summon and
/// still empty when sent back.
fn dismiss(restore: bool) {
    super::runtime::layer(false);
    LAYER.with(|l| l.set(Layer::Desktop));
    note::set_vivid(with(|h| h.shell_front).unwrap_or(false));
    let fresh = with(|h| std::mem::take(&mut h.fresh)).unwrap_or_default();
    for id in fresh {
        let w = with(|h| h.notes.iter().find(|n| n.id == id).map(|n| n.hwnd)).flatten();
        if let Some(w) = w {
            // Unreadable: not known to be empty, so kept.
            if note::markdown(w).is_some_and(|t| t.trim().is_empty()) {
                remove(w, false);
            }
        }
    }
    // Every window down, shown or not (one may have been in front for a
    // desktop that is no longer this one: review 5).
    let host = lw::shell_host();
    for w in with(|h| h.notes.iter().map(|n| n.hwnd).collect::<Vec<_>>()).unwrap_or_default() {
        lw::to_desktop(w, host);
    }
    let homed = with(|h| std::mem::take(&mut h.homed)).unwrap_or(false);
    if restore {
        let fg = unsafe { GetForegroundWindow() };
        let still_home = crate::desklayer::win::is_shell(fg) || is_ours(fg);
        if homed && still_home && !super::headless() {
            // Back from the home screen: the same toggle puts the apps back.
            show_desktop();
            with(|h| h.before = HWND::default());
        } else {
            give_focus_back(host);
        }
    }
}

/// Esc on the desktop layer: done typing; the focus goes back.
fn release() {
    give_focus_back(lw::shell_host());
}

fn give_focus_back(host: Option<HWND>) {
    if super::headless() { return; }
    let before = with(|h| if h.before.is_invalid() { h.other } else { h.before }).unwrap_or_default();
    unsafe {
        if !before.is_invalid() && IsWindow(Some(before)).as_bool() && IsWindowVisible(before).as_bool() && !is_ours(before) {
            let _ = SetForegroundWindow(before);
        } else if let Some(d) = host {
            let _ = SetForegroundWindow(d);
        }
    }
    with(|h| h.before = HWND::default());
}

fn new_id() -> String {
    let n = with(|h| {
        h.made = h.made.wrapping_add(1);
        h.made
    })
    .unwrap_or(0);
    store::new_id(store::now_ms(), n)
}

/// Deletes note `w` now: its latest text saved (or kept), its file moved
/// to the trash. If that cannot be done the note simply stays.
/// `undoable`: Ctrl+Z can bring it back this session.
fn remove(w: HWND, undoable: bool) -> bool {
    let Some(i) = item_of(w) else { return false };
    let Some((id, place, saved, kept, desk)) = with(|h| h.notes.get(i).map(|n| (n.id.clone(), n.place, n.saved.clone(), n.kept.clone(), n.desk.clone()))).flatten() else { return false };
    let mut doc = Doc { id: id.clone(), text: text_of(i, w), saved, kept };
    let done = store::delete(&mut RealDisk::new(), &mut doc, store::now_ms());
    with(|h| {
        if let Some(n) = h.notes.get_mut(i) {
            n.saved = doc.saved.clone();
            n.kept = doc.kept.clone();
        }
    });
    if !done {
        crate::log!("notes: {id} could not be deleted; it stays");
        return false;
    }
    with(|h| {
        h.notes.remove(i);
        h.fresh.retain(|f| *f != id);
        if undoable {
            h.deleted.push(Gone { id: id.clone(), md: doc.text.clone(), place, desk });
        }
    });
    note::destroy(w);
    // The layout without it.
    save();
    watch_or_not();
    true
}

fn on_note(ev: Ev, w: HWND) {
    match ev {
        Ev::Changed => {
            if let Some(id) = with(|h| h.notes.iter().find(|n| n.hwnd == w).map(|n| n.id.clone())).flatten() {
                with(|h| h.last = id);
            }
            schedule_save();
        }
        Ev::Moved => {
            let settled=snap_note(w);
            if !can_save_geometry(settled,super::headless()){return;}
            let r = note::card(w);
            if let Some(p) = place::from_px(r, &crate::monitors::all()) {
                with(|h| {
                    if let Some(n) = h.notes.iter_mut().find(|n| n.hwnd == w) {
                        n.place = p;
                        icons::remember_place(w, p);
                    }
                });
                schedule_save();
            }
        }
        Ev::Add => {
            let r = note::card(w);
            let mons = crate::monitors::all();
            let Some(p0) = place::from_px(r, &mons) else { return };
            let Some(m) = place::pick(p0.mon, &mons) else { return };
            let nr = place::beside(r, m.work, (GAP * m.scale()).round() as i32);
            if let Some(p) = place::from_px(nr, &mons) {
                new_note(p, true, false);
            }
        }
        Ev::Delete => {
            // Among this desktop's notes.
            let i = shown_windows().iter().position(|x| *x == w).unwrap_or(0);
            if remove(w, true) && layer() == Layer::Summoned {
                // The next one keeps the keyboard; none left: back down.
                let here = shown_windows();
                let next = here.get(i.min(here.len().saturating_sub(1))).copied();
                match next {
                    Some(n) => unsafe {
                        let _ = SetForegroundWindow(n);
                        note::focus(n);
                    },
                    None => on(Event::Escape),
                }
            }
        }
        Ev::Undo => {
            // The last note deleted that would show here (Ctrl+Z on this
            // desktop never brings back another desktop's note unseen).
            let d = desks();
            let Some(g) = with(|h| h.deleted.iter().rposition(|g| d.shows(g.desk.as_deref())).map(|k| h.deleted.remove(k))).flatten() else { return };
            match store::restore(&mut RealDisk::new(), &g.id, &g.md, store::now_ms()) {
                Some(doc) => {
                    if open(&g.id, g.place, &g.md, doc.saved.clone(), true, g.desk.clone()).is_some() {
                        with(|h| {
                            if let Some(n) = h.notes.iter_mut().find(|n| n.id == g.id) {
                                n.kept = doc.kept.clone();
                            }
                            h.last = g.id.clone();
                        });
                        save();
                    }
                }
                None => {
                    crate::log!("notes: {} could not be restored; still in the trash", g.id);
                    with(|h| h.deleted.push(g));
                }
            }
        }
        Ev::Escape => on(Event::Escape),
        Ev::Deactivated => {
            let fg = unsafe { GetForegroundWindow() };
            if layer() == Layer::Summoned && !is_ours(fg) {
                on(Event::FocusLost);
            }
        }
        Ev::Lost => {
            // The text is in the note's `last` (absorbed); the note stays in
            // the list (saved like any other) whether or not a new window
            // can be made for it.
            absorb_lost();
            let gone: Vec<(String, Place, String, bool)> =
                with(|h| h.notes.iter().filter(|n| !note::is_note(n.hwnd)).map(|n| (n.id.clone(), n.place, n.last.clone(), shown(n))).collect()).unwrap_or_default();
            for (id, p, md, show) in gone {
                crate::log!("notes: a note window was destroyed by someone else; made again");
                match window(&id, p, &md, false, show,with(|h|h.notes.iter().find(|n|n.id==id).and_then(|n|n.size)).flatten()) {
                    Some((hwnd,size)) => with(|h| {
                        if let Some(n) = h.notes.iter_mut().find(|n| n.id == id) {
                            n.hwnd = hwnd;n.size=size;
                        }
                    }),
                    None => {
                        crate::log!("notes: no new window for {id}; its text is still saved");
                        None
                    }
                };
            }
            schedule_save();
        }
        Ev::Close => {
            if layer() == Layer::Summoned {
                on(Event::Escape);
            } else {
                release();
            }
        }
        Ev::Menu => menu(w),
    }
}

/// The note's menu (right click): "Show on all desktops", only when
/// desktops are told apart. Picking it moves the note between this
/// desktop and every desktop; notes.json is written at once.
fn menu(w: HWND) {
    let Some(home)=with(|h|h.notes.iter().find(|n|n.hwnd==w).map(|n|n.desk.clone())).flatten() else {return;};
    let mut extra=if desks().menu() {vec![crate::popup::Item::new(30,crate::i18n::t("notes.all_desktops")).checked(home.is_none())]} else {vec![]};
    // The note's delete (its × button before 0.1.92): Ctrl+Z brings it back.
    extra.push(crate::popup::Item::new(31,crate::i18n::t("notes.delete")));
    let result=crate::desklayer::widget::menu(&super::panel_theme(),note::menu_at(w),unsafe{windows::Win32::UI::HiDpi::GetDpiForWindow(w)}.max(96) as f32/96.,extra);
    if let Some(id @ 10..=12)=result {
        resize_widget(w,crate::desklayer::widget::Size::ALL[(id-10) as usize]);
        return;
    }
    if result==Some(31) {on_note(Ev::Delete,w);return;}
    if result!=Some(30) {return;}
    // Read again: the menu waited, and the note or the desktops may have
    // changed meanwhile.
    let d = desks();
    let Some(next) = with(|h| h.notes.iter().find(|n| n.hwnd == w).map(|n| d.toggled(n.desk.as_deref()))).flatten().flatten() else { return };
    with(|h| {
        if let Some(n) = h.notes.iter_mut().find(|n| n.hwnd == w) {
            n.desk = next;
        }
    });
    apply_desks();
    save();
}

pub(crate) fn resize_widget(w:HWND,size:crate::desklayer::widget::Size) {
    let Some(r)=icons::card(w) else {return;};
    let mons=crate::monitors::all();let desk=crate::desklayer::widget::desk(note::scale(w));
    if let Some(nr)=crate::desklayer::widget::snap(&desk,&mons,r,Some(size),&icons::widget_rects(Some(w))) {
        with(|h|if let Some(n)=h.notes.iter_mut().find(|n|n.hwnd==w){n.size=Some(size);});
        icons::commit(w,nr);
        note::set_card(w,nr);
        on_note(Ev::Moved,w);
        schedule_save();
        crate::desklayer::edit::refresh();
    }
}

/// Notes (and deleted notes kept for Ctrl+Z) of desktops that no longer
/// exist belong to every desktop from now on. True: some did.
fn rehome_gone() -> bool {
    let d = desks();
    with(|h| d.rehome(h.notes.iter_mut().map(|n| &mut n.desk).chain(h.deleted.iter_mut().map(|g| &mut g.desk)))).unwrap_or(false)
}

/// Shows the notes of this desktop, hides the others' (through the pin:
/// the hide is ours).
fn apply_desks() {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOPMOST};
    let all: Vec<(HWND, bool)> = with(|h| h.notes.iter().map(|n| (n.hwnd, shown(n)&&!crate::desklayer::edit::dragging(n.hwnd))).collect()).unwrap_or_default();
    let host = lw::shell_host();
    let summoned = layer() == Layer::Summoned;
    for (w, show) in all {
        let visible = unsafe { IsWindowVisible(w).as_bool() };
        let topmost = unsafe { GetWindowLongPtrW(w, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 != 0;
        match desks::window_op(show, visible, topmost, summoned) {
            WinOp::Show { front } => {
                let _ = front;
                lw::to_desktop(w, host);
                if !super::headless() {
                    unsafe {
                        let _ = ShowWindow(w, SW_SHOWNOACTIVATE);
                    }
                }
            }
            WinOp::Hide => lw::placing(|| unsafe {
                let _ = ShowWindow(w, SW_HIDE);
            }),
            WinOp::Lower => lw::to_desktop(w, host),
            WinOp::Raise => lw::to_desktop(w, host),
            WinOp::Keep => {}
        }
    }
}

/// The desktops changed (a switch, one made or closed) or the setting.
fn desks_changed() {
    if crate::desklayer::edit::active(){crate::desklayer::edit::finish();}
    let now = read_desks();
    let before = desks();
    if before == now {
        return;
    }
    // Summoned on the desktop just left (or closed under them): sent back
    // first, while the reading that brought them up still holds (an empty
    // new note goes as usual; the focus stays where Windows put it).
    if desks::send_back_first(&before, &now, layer() == Layer::Summoned) {
        on(Event::FocusLost);
    }
    DESKS.with(|d| *d.borrow_mut() = now);
    // A closed desktop's notes: every desktop, written at once.
    if rehome_gone() {
        crate::log!("notes: a desktop was closed; its notes show on every desktop now");
        save();
    }
    apply_desks();
    super::runtime::refresh();
}

/// Another app's window came to the front (the WinEvent).
fn on_foreground(fg: HWND) {
    if super::runtime::owns(fg){return;}
    if super::headless() { return; }
    let shell = lw::is_shell(fg);
    let was_shell = with(|h| {
        h.other = fg;
        std::mem::replace(&mut h.shell_front, shell)
    })
    .unwrap_or(false);
    // The desktop itself in front: the faces in full; another app: quiet.
    if layer() == Layer::Desktop {
        note::set_vivid(shell);
    }
    match layer() {
        // Queued before the summon? Only a foreground that is still not
        // ours sends the notes back.
        Layer::Summoned if !is_ours(unsafe { GetForegroundWindow() }) => on(Event::FocusLost),
        Layer::Summoned => {}
        Layer::Desktop if shell || was_shell => {
            // Show desktop came (the desktop is in front: the notes over it)
            // or went (back down to just above it); shown even if the shell
            // hid or minimized them.
            let host = lw::shell_host();
            for w in shown_windows() {
                unsafe {
                    if !IsWindowVisible(w).as_bool() || IsIconic(w).as_bool() {
                        let _ = ShowWindow(w, SW_SHOWNOACTIVATE);
                    }
                }
                lw::to_desktop(w, host);
            }
        }
        Layer::Desktop => {}
    }
}

/// Displays changed: every note back to its place, kept on screen (the
/// saved place stays, so a display that comes back gets its notes back).
fn displays_changed() {
    if crate::desklayer::grid_state::display_settling() { apply_desks(); return; }
    let mons = crate::monitors::all();
    for (w, p) in with(|h| h.notes.iter().map(|n| (n.hwnd, n.place)).collect::<Vec<_>>()).unwrap_or_default() {
        if let Some(r) = place::to_px(&p, &mons, (1., 1.)) {
            note::set_card(w, r);
            icons::commit(w, r);
        }
    }
    apply_desks();
    crate::desklayer::edit::grid_changed();
}

/// Explorer (re)started: the desktop window is a new one.
fn reattach() {
    // Explorer restarted: its grid read anew.
    if let Some(h) = with(|h| h.hwnd) {
        icons::read(h);
    }
    if layer() != Layer::Desktop {
        return;
    }
    let host = lw::shell_host();
    for w in with(|h| h.notes.iter().map(|n| n.hwnd).collect::<Vec<_>>()).unwrap_or_default() {
        lw::to_desktop(w, host);
    }
}

/// Closes everything and ends the thread.
fn teardown(hwnd: HWND) {
    super::runtime::shutdown();
    crate::vdesk::unlisten(hwnd);
    if let Some(hook) = with(|h| h.hook) {
        lw::unwatch_foreground(hook);
    }
    for w in with(|h| h.notes.drain(..).map(|n| n.hwnd).collect::<Vec<_>>()).unwrap_or_default() {
        note::destroy(w);
    }
    unsafe {
        let _ = DestroyWindow(hwnd);
        PostQuitMessage(0);
    }
}

/// A held-up logoff is let go (the texts are safe now, or it was called off).
fn unblock(hwnd: HWND) {
    if with(|h| std::mem::replace(&mut h.blocking, false)).unwrap_or(false) {
        unsafe {
            let _ = windows::Win32::System::Shutdown::ShutdownBlockReasonDestroy(hwnd);
        }
    }
}

/// Windows asks whether the session may end. Every note is saved now;
/// only a user's logoff is held up, and only while a text could be kept
/// nowhere (with the reason; the user can still end it). Installers
/// (Restart Manager) and critical ends never are.
fn query_end(hwnd: HWND, flags: isize) -> bool {
    let lost = settle_now();
    let user = flags & (ENDSESSION_CLOSEAPP | ENDSESSION_CRITICAL) == 0;
    if lost.is_empty() || !user {
        unblock(hwnd);
        return true;
    }
    unsafe {
        let _ = windows::Win32::System::Shutdown::ShutdownBlockReasonCreate(hwnd, &windows::core::HSTRING::from(crate::i18n::t("notes.save_failed")));
    }
    with(|h| h.blocking = true);
    show_all();
    false
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NOTE => {
            if let Some(ev) = Ev::from(wp.0) {
                on_note(ev, HWND(lp.0 as *mut _));
            }
            LRESULT(0)
        }
        WM_TOGGLE => {
            if super::runtime::hotkey(){on(Event::Hotkey);}
            LRESULT(0)
        }
        WM_REMIND => {super::runtime::tick(); LRESULT(0)}
        WM_TIMER if wp.0 == super::runtime::TIMER => {super::runtime::tick(); LRESULT(0)}
        WM_NEW => {
            create_and_summon();
            LRESULT(0)
        }
        m if m==crate::desklayer::edit::COMMAND=>{widget_commands(hwnd);LRESULT(0)},
        WM_MAKE => {
            make_asked();
            LRESULT(0)
        }
        WM_DESKS => {
            desks_changed();
            LRESULT(0)
        }
        WM_FOREGROUND => {
            on_foreground(HWND(wp.0 as *mut _));
            LRESULT(0)
        }
        WM_LOOK => {
            super::runtime::retheme(super::panel_theme());
            if let Some(l) = super::take_look() {
                note::set_look(l);
                for w in with(|h| h.notes.iter().map(|n| n.hwnd).collect::<Vec<_>>()).unwrap_or_default() {
                    note::restyle(w);
                }
            }
            LRESULT(0)
        }
        WM_PREPARE_QUIT => {
            let lost = settle_now();
            let go = super::may_go(super::Quit::from(wp.0), lost.len());
            if !go {
                crate::log!("notes: quitting called off: {} text(s) could be kept nowhere", lost.len());
                show_all();
            }
            LRESULT(go as isize)
        }
        WM_SHUTDOWN => {
            settle_now();
            teardown(hwnd);
            LRESULT(1)
        }
        WM_TIMER if wp.0 == SAVE_TIMER || wp.0 == RETRY_TIMER => {
            if settle_now().is_empty() {
                unblock(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == HOME_FOCUS_TIMER => {
            let _ = KillTimer(Some(hwnd), HOME_FOCUS_TIMER);
            if layer() == Layer::Summoned {
                focus_last();
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == DISPLAY_TIMER => {
            if crate::desklayer::grid_state::display_settling() { return LRESULT(0); }
            let _ = KillTimer(Some(hwnd), DISPLAY_TIMER);
            displays_changed();
            // The grid on the displays as they are now.
            if !super::headless() { icons::read(hwnd); }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            crate::desklayer::grid_state::begin_display_change();
            apply_desks();
            SetTimer(Some(hwnd), DISPLAY_TIMER, DISPLAY_MS, None);
            LRESULT(0)
        }
        WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETWORKAREA.0 => {
            crate::desklayer::grid_state::begin_display_change();
            SetTimer(Some(hwnd), DISPLAY_TIMER, DISPLAY_MS, None);
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(query_end(hwnd, lp.0) as isize),
        WM_ENDSESSION => {
            if wp.0 != 0 {
                settle_now();
            } else {
                unblock(hwnd);
            }
            LRESULT(0)
        }
        // Restart Manager closing apps: everything saved, then it may go
        // on (the hub itself stays until the process ends).
        WM_CLOSE => {
            settle_now();
            LRESULT(0)
        }
        m if m == lw::taskbar_created() => {
            reattach();
            LRESULT(0)
        }
        m if m == icons::wm_grid() => {
            on_grid();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_let_go_of_before_the_grid_snap_when_it_comes_once() {
        let mut p = Pending::default();
        p.add(1);
        p.add(2);
        p.add(1);
        assert_eq!(p, Pending(vec![1, 2]), "each once");
        // The grid came: the notes still there snap, the list empties.
        assert_eq!(p.take(true, |w| w != 2), vec![1]);
        assert_eq!(p.take(true, |_| true), Vec::<isize>::new());
        // Explorer did not answer: nothing snaps and nothing waits on (no
        // asking again in a loop).
        p.add(3);
        assert!(p.take(false, |_| true).is_empty());
        assert_eq!(p, Pending::default());
    }

    #[test]
    fn backoff_grows_and_stops_at_a_minute() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(50), Duration::from_secs(60));
    }

    #[test]
    fn installers_and_critical_ends_are_never_held_up() {
        assert_ne!(ENDSESSION_CLOSEAPP & (ENDSESSION_CLOSEAPP | ENDSESSION_CRITICAL), 0);
        assert_ne!(ENDSESSION_CRITICAL & (ENDSESSION_CLOSEAPP | ENDSESSION_CRITICAL), 0);
        assert_eq!(0x8000_0000isize & (ENDSESSION_CLOSEAPP | ENDSESSION_CRITICAL), 0, "a plain logoff (ENDSESSION_LOGOFF)");
    }
}

fn publish_hidden(){
    *super::HIDDEN_PREVIEWS.lock().unwrap_or_else(|e|e.into_inner())=with(|h|h.notes.iter().filter(|n|n.hidden).map(|n|(n.id.clone(),n.last.clone())).collect()).unwrap_or_default();
    let list=with(|h|h.notes.iter().filter(|n|n.hidden).map(|n|(n.id.clone(),n.last.lines().next().filter(|s|!s.is_empty()).unwrap_or(crate::i18n::t("notes.new")).to_string())).collect()).unwrap_or_default();
    *super::HIDDEN_WIDGETS.lock().unwrap_or_else(|e|e.into_inner())=list;
}
pub(crate) fn widget_commands(w:HWND){
    use crate::desklayer::{edit::{self,Action,Kind},widget};
    for action in edit::take_actions(w){let visibility=action.clone();match action {
        Action::DisplaysChanged=>{displays_changed();},
        Action::Hide=>{},
        Action::Remove=>{
            // Save native RichEdit first; hiding never deletes or moves the body file.
            save();with(|h|if let Some(n)=h.notes.iter_mut().find(|n|n.hwnd==w){n.hidden=true;});apply_desks();save();publish_hidden();
        },
        Action::Place(r)=>{
            let mons=crate::monitors::all();
            if !edit::allow_geometry(Some(w), &visibility, &mons) { continue; }
            if let Some(p)=place::from_px(r,&mons){with(|h|if let Some(n)=h.notes.iter_mut().find(|n|n.hwnd==w){n.place=p;});icons::remember_place(w,p);note::set_card(w,r);icons::commit(w,r);apply_desks();save();}
        },
        Action::Resize(r,size)=>{
            let mons=crate::monitors::all();
            if !edit::allow_geometry(Some(w), &visibility, &mons) { continue; }
            // Its handles (or its size menu): the new size where it lies now.
            if let Some(p)=place::from_px(r,&mons){with(|h|if let Some(n)=h.notes.iter_mut().find(|n|n.hwnd==w){n.place=p;n.size=Some(size);});icons::remember_place(w,p);note::set_card(w,r);icons::commit(w,r);apply_desks();save();}
        },
        Action::Exit=>{note::edit_mode(w,false);},
        Action::Enter=>{note::edit_mode(w,true);note::own_move(||unsafe{let _=windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();});},
        Action::Add{kind:Kind::Note,size,monitor,note:id}=>{
            let mons=crate::monitors::all();let Some(m)=mons.get(monitor)else{continue;};
            let Some(r)=widget::auto(&widget::desk(m.scale()),&mons,monitor,size,&icons::widget_rects(None))else{continue;};
            let Some(p)=place::from_px(r,&mons)else{continue;};
            let existing=id.and_then(|id|with(|h|h.notes.iter().find(|n|n.id==id&&n.hidden).map(|n|n.hwnd)).flatten());
            if let Some(w)=existing {
                with(|h|if let Some(n)=h.notes.iter_mut().find(|n|n.hwnd==w){n.hidden=false;n.place=p;n.size=Some(size);n.desk=current_desktop();});
                icons::remember_place(w,p);note::set_card(w,r);icons::commit(w,r);apply_desks();
            }else{let id=new_id();if let Some(w)=open_sized(&id,p,"",None,false,desks().home_for_new(),Some(size)){on_note(Ev::Moved,w);}}
            save();publish_hidden();
        },
        other=>super::runtime::widget_action(other),
    }edit::apply_owner_visibility(w,&visibility);}
    edit::refresh();
}


#[cfg(test)] mod review_tests {
 use super::*;
 use crate::desklayer::widget::Size;
 /// Review 6: the editor popup and its RichEdit are ours (the keyboard
 /// is in them while typing).
 #[test] fn review_06_the_editor_is_ours(){
  let r=RECT{left:-32000,top:-32000,right:-32000+288,bottom:-32000+176};
  let hwnd=note::create("ours",r,"").unwrap();
  assert!(is_ours(hwnd));
  // Typed on the card: no window of its own.
  assert_eq!(note::editor_windows(hwnd),None);
  note::destroy(hwnd);
  // The fallback editor's popup and RichEdit are ours too.
  note::set_test_inline(false);
  let hwnd=note::create("ours",r,"").unwrap();
  let (pop,edit)=note::editor_windows(hwnd).unwrap();
  assert!(is_ours(hwnd)&&is_ours(pop)&&is_ours(edit));
  note::destroy(hwnd);
  note::set_test_inline(true);
 }
 #[test] fn a_new_note_is_medium_and_wide(){
  assert_eq!(NEW_SIZE,Size::MEDIUM);
  let (w,h)=NEW_SIZE.cells();assert!(w>h);
 }
 #[test] fn review_02_large_survives_150_percent_startup_and_grid_arrival(){
  let bounds=RECT{left:0,top:0,right:1920,bottom:1440};
  let mons=[crate::monitors::Monitor{handle:0,rect:bounds,work:bounds,primary:true,dpi:144}];
  let stored=RECT{left:120,top:120,right:552,bottom:672};
  let registry=grid::Desk{cell:(76,96),spacing_dpi:96,..Default::default()};
  let (temporary,size)=startup_rect_on(stored,Some(Size::LARGE),&registry,&mons,&[]);
  assert_eq!(size,Some(Size::LARGE));assert_eq!(grid::Grid::widget(&mons[0], registry.cell).unwrap().touched(temporary).map(|s| (s.w, s.h)), Some(Size::LARGE.cells()));
  assert!(!can_save_geometry(false,false));assert!(can_save_geometry(true,false));
  let grid=grid::Desk{cell:(138,144),..Default::default()};
  let (ready,size)=startup_rect(temporary,size,Some(&grid),&mons,&[]);
  assert_eq!(size,Some(Size::LARGE));assert_eq!(grid::Grid::widget(&mons[0], grid.cell).unwrap().touched(ready).map(|s| (s.w, s.h)), Some(Size::LARGE.cells()));
 }
}

fn can_save_geometry(grid_ready:bool,headless:bool)->bool {grid_ready||headless}
fn startup_rect(r:RECT,size:Option<crate::desklayer::widget::Size>,cached:Option<&grid::Desk>,mons:&[crate::monitors::Monitor],occupied:&[RECT])->(RECT,Option<crate::desklayer::widget::Size>){
    let fallback;
    let desk=match cached { Some(desk)=>desk, None=>{fallback=crate::desklayer::spacing::fallback(1.);&fallback} };
    startup_rect_on(r,size,desk,mons,occupied)
}
fn startup_rect_on(r:RECT,size:Option<crate::desklayer::widget::Size>,desk:&grid::Desk,mons:&[crate::monitors::Monitor],occupied:&[RECT])->(RECT,Option<crate::desklayer::widget::Size>){
    use crate::desklayer::widget::{self,Size};
    let Some(mi)=grid::monitor_of(r,mons)else{return(r,size);};let dpi=mons[mi].scale();
    let chosen=size.or_else(||grid::grids(desk,mons)[mi].map(|g|Size::nearest((r.right-r.left,r.bottom-r.top),&g,dpi)));
    (widget::snap(desk,mons,r,chosen,occupied).unwrap_or(r),chosen)
}
