//! Windows-style search: a text field in a panel a quarter of the way
//! down the active monitor, with apps, Settings pages, a calculator, files
//! and a web search below it. Opened from the top bar's magnifier or the
//! global hotkey (shortcut "search" in crate::shortcuts, Alt+Space by default); the hotkey again,
//! Esc or a click outside closes it.
//!
//! Like the Control Center: one borderless window with the popup material
//! (popup::chrome), drawn immediate-mode into one D2D surface, modal in a
//! nested loop. The text field is our own: WM_CHAR for typing, the IME's
//! composition string (WM_IME_COMPOSITION) drawn inline so Korean works.
//!
//! Every source runs on a worker thread that lives only while the panel is
//! open (quick: apps, Settings, calculator; files: the search index; icons),
//! posting WM_SEARCH_REMOTE as results arrive. Nothing runs while it is
//! closed except the registered hotkey; only the app list is kept between
//! opens (and refreshed in the background on each open).

mod calc;
pub(crate) mod dates;
pub(crate) mod edit;
mod files;
pub mod hotkey;
pub(crate) mod matching;
pub(crate) mod sources;
mod units;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, Interface, Result, BOOL, HSTRING};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionStretch, ContainerVisual, SpriteVisual, Visual};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_HIT_TEST_METRICS};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{ClientToScreen, MonitorFromPoint, MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmNotifyIME, ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow, CANDIDATEFORM,
    CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM, CPS_CANCEL, GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR, IME_COMPOSITION_STRING, ISC_SHOWUICOMPOSITIONWINDOW,
    NI_COMPOSITIONSTR,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT,
    VK_PROCESSKEY, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use crate::a11y::{self, Act, Node, Role as Ax, Tree};
use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::i18n::t;
use crate::icons::Icon;
use crate::theme::metrics::{size, space};
use crate::theme::ramp::{self, TextStyle};
use crate::theme::{Level, Role, State, Theme};
use crate::wash::{raised_state, Lit, Shape};
use edit::Field;
use sources::{App, Hit, Kind};

/// Results (or the app list) changed: take them from the shared state.
const WM_SEARCH_REMOTE: u32 = WM_APP + 1;
const CARET_TIMER: usize = 1;
/// A query's answers are late (design system 11.3: nothing is shown for
/// what ends within TIP_DELAY_MS): the progress ring comes on.
const WAIT_TIMER: usize = 2;
const WAIT_MS: u32 = crate::motion::TIP_DELAY_MS as u32;

// ---- metrics (DIP; design system 3.2, 3.3) -------------------------------------
/// Panel width, unless the work area is narrower.
const W: f32 = 640.0;
/// The field: the list's fixed header (2.5).
const FIELD_H: f32 = 56.0;
/// The panel's two lines: rows (their fills) on the container line, what
/// is read (glyphs, icons, headings) on the content line; mirrored on the
/// right.
const VESSEL: f32 = size::PANEL_PAD;
const LINE: f32 = size::PANEL_PAD + size::CARD_PAD;
/// Above the first group, under the field.
const LIST_TOP: f32 = space::S3;
const HEAD_H: f32 = size::HEAD_H;
/// A result: two lines (its title over where it is).
const ROW_H: f32 = size::ROW_2XL;
/// The file search's note: one line with a 28 DIP button in it.
const NOTE_H: f32 = size::ROW_L;
/// "No results": the quiet block (11.1).
const QUIET_H: f32 = size::QUIET_MIN_H;
/// Before the web row (it has no heading).
const WEB_GAP: f32 = space::S3;
const PAD_BOTTOM: f32 = size::PANEL_PAD;
const SHADOW_M: f32 = crate::theme::metrics::SHADOW_M;
/// A two-line row's icon slot (an app's icon fills it, a glyph is GLYPH
/// in its middle); the note's warning is NOTE_GLYPH there.
const ICON: f32 = 28.0;
const GLYPH: f32 = size::ICON_L;
const NOTE_GLYPH: f32 = size::ICON_S;
/// The field's text: the ramp's Title (20 / 600, the Display instance).
const T_FIELD: TextStyle = ramp::TITLE;
const T_TITLE: TextStyle = ramp::BODY;
const T_STRONG: TextStyle = ramp::BODY_STRONG;
/// Second lines, hints and group heads.
const T_SMALL: TextStyle = ramp::FOOTNOTE;
/// A result's text: after its icon slot and 12. The field's text starts
/// on the same line (its magnifier sits in the same slot).
const TEXT_X: f32 = LINE + ICON + space::S4;
const FIELD_X0: f32 = TEXT_X;
/// How far an empty field's hint starts past the text's start: the
/// caret's width and a gap (states-issues: the caret stood on the hint's
/// first glyph).
const PLACEHOLDER_IN: f32 = crate::gfx::CARET_W + crate::theme::metrics::space::S1;
/// Kept free at the field's end for the progress ring: the ring and 8.
const FIELD_END: f32 = LINE + size::SPINNER + space::S3;
/// The panel to the work area's edges.
const EDGE: f32 = size::EDGE_GAP;
/// One wheel notch scrolls the list by a row.
const WHEEL_ROW: f32 = ROW_H;

/// Files asked of the index per query.
const FILES_TOP: usize = 20;
/// Index answers slower than this are dropped (and said to be too slow);
/// after "search again" the wait is FILES_PATIENT.
const FILES_LIMIT: Duration = Duration::from_millis(1500);
const FILES_PATIENT: Duration = Duration::from_millis(6500);
/// What the index itself is given (ADO's CommandTimeout, whole seconds):
/// normally, and after "search again".
const INDEX_SECS: i32 = 2;
const INDEX_SECS_PATIENT: i32 = 6;

/// Pure: an index query that failed after `took` with `secs` allowed ran
/// out of time (said as "too slow"), rather than the index being unusable
/// (recent items instead).
fn index_timed_out(took: Duration, secs: i32) -> bool {
    took + Duration::from_millis(300) >= Duration::from_secs(secs.max(1) as u64)
}
/// Typing pauses this long before the index is asked.
const FILES_DEBOUNCE: Duration = Duration::from_millis(150);
/// The app list is read for at most this long.
const APPS_LIMIT: Duration = Duration::from_secs(10);

// Glyphs (crate::glyphs).
const G_SEARCH: Glyph = Glyph::Search;
const G_SETTINGS: Glyph = Glyph::Gear;
const G_CALC: Glyph = Glyph::Calculator;
const G_GLOBE: Glyph = Glyph::Globe;
const G_APP: Glyph = Glyph::Window;
const G_FILE: Glyph = Glyph::Document;
const G_INFO: Glyph = Glyph::Info;

// ---- the app list (kept for the session) ------------------------------------------

static APPS: Mutex<Option<Arc<Vec<App>>>> = Mutex::new(None);
/// Bumped (under APPS) each time a reading's outcome is published: the list
/// and its state go out together (apps_snapshot).
static APPS_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static APPS_REFRESHING: AtomicBool = AtomicBool::new(false);
/// How the last reading of the app list went (AppsState as u8).
static APPS_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Where the app list stands, for Launchpad's empty screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppsState {
    /// Being read (or not read yet).
    Loading,
    Loaded,
    /// Read, and Windows listed no apps.
    Empty,
    /// Some apps, but the reading stopped part way (an error, out of
    /// time): the list may lack apps; reading again may complete it.
    Partial,
    /// Could not be read (nothing at all).
    Failed,
}

pub(crate) fn apps_state() -> AppsState {
    match APPS_STATE.load(Ordering::SeqCst) {
        1 => AppsState::Loaded,
        2 => AppsState::Empty,
        3 => AppsState::Failed,
        4 => AppsState::Partial,
        _ => AppsState::Loading,
    }
}

fn set_apps_state(st: AppsState) {
    let v = match st {
        AppsState::Loading => 0,
        AppsState::Loaded => 1,
        AppsState::Empty => 2,
        AppsState::Failed => 3,
        AppsState::Partial => 4,
    };
    APPS_STATE.store(v, Ordering::SeqCst);
}

/// Pure: after a reading of `n` apps (`complete`: read to the end; None:
/// it could not start), with `before` the state of the list kept so far.
/// Returns the new state and whether the new list replaces the old one.
/// A complete list from before is never replaced by a partial one.
fn next_apps_state(read: Option<(usize, bool)>, before: AppsState) -> (AppsState, bool) {
    let had_full = before == AppsState::Loaded;
    let had_some = matches!(before, AppsState::Loaded | AppsState::Partial);
    match read {
        Some((n, true)) if n > 0 => (AppsState::Loaded, true),
        Some((0, true)) if had_some => (before, false),
        Some((0, true)) => (AppsState::Empty, false),
        Some((n, false)) if n > 0 && !had_full => (AppsState::Partial, true),
        _ if had_some => (before, false),
        _ => (AppsState::Failed, false),
    }
}
/// The open panel's window (0 = none), for the app list's "refreshed".
static OPEN_HWND: AtomicIsize = AtomicIsize::new(0);
/// What was typed last time (shown, selected, on the next open).
static LAST_QUERY: Mutex<String> = Mutex::new(String::new());

pub(crate) fn app_list() -> Arc<Vec<App>> {
    APPS.lock().ok().and_then(|a| a.clone()).unwrap_or_default()
}

/// The app list with the state of the reading that published it and that
/// publication's number, taken as one (under the list's lock): a list is
/// never paired with another reading's state (Launchpad lets it be
/// arranged only when the list it shows is complete).
#[derive(Clone, Debug)]
pub(crate) struct AppsSnapshot {
    pub list: Arc<Vec<App>>,
    pub state: AppsState,
    pub gen: u64,
}

impl AppsSnapshot {
    /// The whole list (read to the end, not empty).
    pub fn complete(&self) -> bool {
        self.state == AppsState::Loaded && !self.list.is_empty()
    }
}

pub(crate) fn apps_snapshot() -> AppsSnapshot {
    let g = APPS.lock();
    let list = g.as_ref().ok().and_then(|a| (**a).clone()).unwrap_or_default();
    // (State and number are only changed under this lock: see publish_apps.)
    AppsSnapshot { list, state: apps_state(), gen: APPS_GEN.load(Ordering::SeqCst) }
}

/// Publishes a reading's outcome: the list (if it replaces the one kept),
/// its state and a new number, all under the list's lock.
fn publish_apps(list: Option<Vec<App>>, st: AppsState) {
    let mut g = APPS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(l) = list {
        *g = Some(Arc::new(l));
    }
    set_apps_state(st);
    APPS_GEN.fetch_add(1, Ordering::SeqCst);
}

/// Reads shell:AppsFolder again on a thread of its own (one at a time);
/// the open panel (and Launchpad) search again when it is done.
pub(crate) fn refresh_apps() {
    if APPS_REFRESHING.swap(true, Ordering::SeqCst) {
        return;
    }
    // A failed or empty reading is tried again: "loading" meanwhile.
    let before = apps_state();
    if matches!(before, AppsState::Failed | AppsState::Empty) {
        publish_apps(None, AppsState::Loading);
    }
    let started = std::thread::Builder::new().name("search-apps".into()).spawn(move || {
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let t0 = Instant::now();
        let (read, list) = match sources::list_apps(&|| t0.elapsed() > APPS_LIMIT) {
            Ok((list, ended)) => {
                match &ended {
                    sources::Ended::Done => crate::log!("search: {} apps in {} ms", list.len(), t0.elapsed().as_millis()),
                    sources::Ended::Stopped => crate::log!("search: app list stopped after {} ms ({} apps)", t0.elapsed().as_millis(), list.len()),
                    sources::Ended::Error(e) => crate::log!("search: app list failed part way ({e}; {} apps)", list.len()),
                }
                (Some((list.len(), ended == sources::Ended::Done)), list)
            }
            Err(e) => {
                crate::log!("search: app list: {e}");
                (None, Vec::new())
            }
        };
        let (st, replace) = next_apps_state(read, before);
        publish_apps(replace.then_some(list), st);
        if com {
            unsafe { CoUninitialize() };
        }
        APPS_REFRESHING.store(false, Ordering::SeqCst);
        crate::launchpad::apps_refreshed();
        let h = OPEN_HWND.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_SEARCH_REMOTE, WPARAM(1), LPARAM(0));
            }
        }
    });
    if started.is_err() {
        APPS_REFRESHING.store(false, Ordering::SeqCst);
    }
}

// ---- shared with the workers ---------------------------------------------------------

struct Shared {
    /// The panel window (0 once it is gone).
    hwnd: AtomicIsize,
    quit: AtomicBool,
    /// Results for query number .0, taken by the panel.
    quick: Mutex<Option<(u64, Vec<Hit>)>>,
    files: Mutex<Option<(u64, Files)>>,
    /// Ask the search index again (it could not be used before).
    retry_index: AtomicBool,
    /// Wait longer for the index (the user asked to search again).
    patient: AtomicBool,
    /// Icons by shell item (None: it has none). Freed with the panel.
    icons: Mutex<HashMap<String, Option<Arc<Icon>>>>,
}

impl Shared {
    fn new() -> Self {
        Shared {
            hwnd: AtomicIsize::new(0),
            quit: AtomicBool::new(false),
            quick: Mutex::new(None),
            files: Mutex::new(None),
            retry_index: AtomicBool::new(false),
            patient: AtomicBool::new(false),
            icons: Mutex::new(HashMap::new()),
        }
    }
    fn quitting(&self) -> bool {
        self.quit.load(Ordering::SeqCst)
    }
    fn post(&self) {
        let h = self.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_SEARCH_REMOTE, WPARAM(0), LPARAM(0));
            }
        }
    }
    fn put<T>(&self, slot: &Mutex<Option<(u64, T)>>, seq: u64, hits: T) {
        if self.quitting() {
            return;
        }
        if let Ok(mut s) = slot.lock() {
            *s = Some((seq, hits));
        }
        self.post();
    }
    fn stop(&self) {
        self.hwnd.store(0, Ordering::SeqCst);
        self.quit.store(true, Ordering::SeqCst);
    }
}

enum Msg<T> {
    Job(T),
    Quit,
}

struct Workers {
    quick: Sender<Msg<(u64, String)>>,
    files: Sender<Msg<(u64, String)>>,
    icons: Sender<Msg<(u32, Vec<String>)>>,
}

impl Workers {
    fn start(shared: &Arc<Shared>) -> Self {
        Workers {
            quick: spawn("search-quick", false, &QUICK_SLOT, shared, quick_worker),
            files: spawn("search-files", true, &FILES_SLOT, shared, files_worker),
            icons: spawn("search-icons", true, &ICONS_SLOT, shared, icons_worker),
        }
    }
    fn quit(&self, shared: &Shared) {
        shared.stop();
        let _ = self.quick.send(Msg::Quit);
        let _ = self.files.send(Msg::Quit);
        let _ = self.icons.send(Msg::Quit);
    }
}

/// A panel's job for one kind of worker.
struct Job<T> {
    shared: Arc<Shared>,
    rx: Receiver<Msg<T>>,
}

/// At most one worker thread per kind in the process (as the Control
/// Center's). A panel opened while the last one's worker is still stuck in
/// a slow call (the index, a shell icon extension) queues its job on that
/// thread instead of starting another; a newer queued job replaces an older
/// one, so threads and memory never pile up however often search opens.
struct Slot<T> {
    alive: bool,
    next: Option<Job<T>>,
}

type Query = (u64, String);
static QUICK_SLOT: Mutex<Slot<Query>> = Mutex::new(Slot { alive: false, next: None });
static FILES_SLOT: Mutex<Slot<Query>> = Mutex::new(Slot { alive: false, next: None });
static ICONS_SLOT: Mutex<Slot<(u32, Vec<String>)>> = Mutex::new(Slot { alive: false, next: None });

/// Starts (or queues) this panel's worker of one kind and returns its
/// command channel. The job ends on Quit (or when its panel is gone); the
/// thread then takes the queued job, if any, else ends.
fn spawn<T: Send + 'static>(name: &str, com: bool, slot: &'static Mutex<Slot<T>>, shared: &Arc<Shared>, f: fn(&Shared, Receiver<Msg<T>>)) -> Sender<Msg<T>> {
    let (tx, rx) = channel();
    let job = Job { shared: shared.clone(), rx };
    let Ok(mut s) = slot.lock() else { return tx };
    if s.alive {
        s.next = Some(job);
        return tx;
    }
    s.alive = true;
    drop(s);
    let r = std::thread::Builder::new().name(name.into()).spawn(move || {
        let inited = com && unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let mut job = Some(job);
        while let Some(j) = job.take() {
            if !j.shared.quitting() {
                f(&j.shared, j.rx);
            }
            job = match slot.lock() {
                Ok(mut s) => {
                    let n = s.next.take();
                    if n.is_none() {
                        s.alive = false;
                    }
                    n
                }
                Err(_) => None,
            };
        }
        if inited {
            unsafe { CoUninitialize() };
        }
    });
    if let Err(e) = r {
        crate::log!("search: {name} not started: {e}");
        if let Ok(mut s) = slot.lock() {
            s.alive = false;
        }
    }
    tx
}

/// Apps, Settings, calculator and the web row: fast, but off the UI thread.
fn quick_worker(shared: &Shared, rx: Receiver<Msg<(u64, String)>>) {
    while let Ok(Msg::Job((mut seq, mut text))) = rx.recv() {
        // Only the newest query matters.
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Job(j) => (seq, text) = j,
                Msg::Quit => return,
            }
        }
        if shared.quitting() {
            return;
        }
        let hits = sources::quick(&text, &app_list());
        shared.put(&shared.quick, seq, hits);
    }
}

/// Where the files of a query came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileSource {
    /// Windows' search index (the indexed locations).
    Index,
    /// Recent items only: the index could not be used.
    Recent,
}

/// One query's files, and how the search went.
struct Files {
    hits: Vec<Hit>,
    from: FileSource,
    /// Took longer than allowed: the files were dropped.
    timed_out: bool,
}

/// How the file search for the text shown stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilesState {
    /// No text.
    Idle,
    /// Asked (the progress ring shows once it has taken WAIT_MS).
    Waiting,
    Done(FileSource),
    TimedOut(FileSource),
}

/// Pure: the note row for a file search state (None: nothing to say; a
/// search still under way says nothing either: the ring does). Something
/// went wrong (design system 11.4): one line and a "Try again" button
/// (target "retry"; Enter on the row too, Ctrl+Enter goes on in Windows).
fn files_note(state: FilesState) -> Option<Hit> {
    let note = |title: &str| Hit { kind: Kind::Note, title: title.into(), detail: String::new(), target: "retry".into(), icon: None, score: 0 };
    match state {
        FilesState::Idle | FilesState::Waiting | FilesState::Done(FileSource::Index) => None,
        FilesState::TimedOut(_) => Some(note(t("search.files.timeout"))),
        FilesState::Done(FileSource::Recent) => Some(note(t("search.files.recent_only"))),
    }
}

/// Pure: something for the text shown is still on its way: its quick
/// results (`stale`) or its files.
fn waiting(text_empty: bool, stale: bool, files: FilesState) -> bool {
    !text_empty && (stale || files == FilesState::Waiting)
}

/// Pure: there is nothing to show for the text but the web row, and
/// nothing more is coming: "No results" (design system 11.1).
/// What the ring's wait does on a change (see Panel::sync_wait).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    /// A wait began: the ring is due WAIT_MS from now.
    Arm,
    /// Still waiting (armed, or the ring is up): nothing moves.
    Keep,
    /// Nothing is late any more: the timer and the ring go.
    Over,
}

/// Pure: `waiting` (answers are late), `armed` (the ring is due), `ring`
/// (it shows).
fn wait_step(waiting: bool, armed: bool, ring: bool) -> Wait {
    match waiting {
        false => Wait::Over,
        true if armed || ring => Wait::Keep,
        true => Wait::Arm,
    }
}

fn nothing_found(text_empty: bool, stale: bool, files: FilesState, kinds: &[Kind]) -> bool {
    !text_empty && !waiting(text_empty, stale, files) && kinds.iter().all(|k| *k == Kind::Web)
}

/// Pure: File Explorer's search for `q` (a search-ms: link).
fn explorer_search(q: &str) -> String {
    let mut out = String::from("search-ms:query=");
    for b in q.trim().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Files from the search index (recent items if it cannot be used).
fn files_worker(shared: &Shared, rx: Receiver<Msg<(u64, String)>>) {
    let mut index = None;
    let mut tried = false;
    while let Ok(Msg::Job((mut seq, mut text))) = rx.recv() {
        // Wait for a pause in the typing; a newer query replaces this one.
        loop {
            match rx.recv_timeout(FILES_DEBOUNCE) {
                Ok(Msg::Job(j)) => (seq, text) = j,
                Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        if shared.quitting() {
            return;
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            shared.put(&shared.files, seq, Files { hits: Vec::new(), from: FileSource::Index, timed_out: false });
            continue;
        }
        if shared.retry_index.swap(false, Ordering::SeqCst) {
            tried = false;
            index = None;
        }
        if !tried {
            tried = true;
            match files::Index::open() {
                Ok(i) => index = Some(i),
                Err(e) => crate::log!("search: files: index not available ({e}); using recent items"),
            }
        }
        let t0 = Instant::now();
        let q = matching::Query::new(&text);
        let mut from = FileSource::Index;
        let patient = shared.patient.load(Ordering::SeqCst);
        let secs = if patient { INDEX_SECS_PATIENT } else { INDEX_SECS };
        let answer = index.as_ref().map(|i| i.query(&text, FILES_TOP, secs));
        if let Some(Err(e)) = &answer {
            if index_timed_out(t0.elapsed(), secs) {
                crate::log!("search: files: the index ran out of time ({e})");
                shared.put(&shared.files, seq, Files { hits: Vec::new(), from, timed_out: true });
                continue;
            }
        }
        let hits = match answer {
            Some(Ok(rows)) => {
                let mut hits: Vec<Hit> = rows.into_iter().map(|(path, name)| {
                    let score = q.score(&name).unwrap_or(0);
                    sources::file_hit(&path, name, score)
                }).collect();
                // Our name match first, the index's rank among equals.
                hits.sort_by(|a, b| b.score.cmp(&a.score));
                hits
            }
            Some(Err(e)) => {
                crate::log!("search: files: {e}; using recent items");
                from = FileSource::Recent;
                sources::recent(&text, FILES_TOP, &|| shared.quitting())
            }
            None => {
                from = FileSource::Recent;
                sources::recent(&text, FILES_TOP, &|| shared.quitting())
            }
        };
        let took = t0.elapsed();
        let limit = if patient { FILES_PATIENT } else { FILES_LIMIT };
        if took > limit {
            // Said so (with "search again"), not just dropped.
            crate::log!("search: files took {} ms; dropped", took.as_millis());
            shared.put(&shared.files, seq, Files { hits: Vec::new(), from, timed_out: true });
            continue;
        }
        shared.put(&shared.files, seq, Files { hits, from, timed_out: false });
    }
}

/// An app's or file's icon (apps through the dock's disk cache; Launchpad
/// loads through here too).
pub(crate) fn load_icon(key: &str, px: u32) -> Option<Icon> {
    let size = px as i32;
    if crate::icons::is_own_app_icon(key) {
        return crate::icons::own_app_icon(px);
    }
    if key.to_lowercase().starts_with("shell:appsfolder\\") {
        let path = crate::icons::cache_path(key, &size.to_string());
        if let Some(p) = &path {
            if crate::icons::cache_fresh(key, p) {
                if let Some(i) = crate::icons::read_cache(p) {
                    return Some(i);
                }
            }
        }
        let icon = crate::icons::load(key, size)?;
        if let Some(p) = &path {
            crate::icons::write_cache(p, &icon);
        }
        return Some(icon);
    }
    crate::icons::load(key, size)
}

fn icons_worker(shared: &Shared, rx: Receiver<Msg<(u32, Vec<String>)>>) {
    while let Ok(Msg::Job((px, keys))) = rx.recv() {
        for k in keys {
            if shared.quitting() {
                return;
            }
            if shared.icons.lock().map(|m| m.contains_key(&k)).unwrap_or(true) {
                continue;
            }
            let icon = load_icon(&k, px).map(Arc::new);
            if let Ok(mut m) = shared.icons.lock() {
                m.insert(k, icon);
            }
            shared.post();
        }
    }
}

// ---- layout -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    Head(Kind),
    /// Index into the hits.
    Item(usize),
    /// "No results": the quiet block.
    Quiet,
}

/// A row's height by what it holds (design system 3.3): two lines, or the
/// note's one line with its 28 DIP button.
fn row_h(k: Kind) -> f32 {
    if k == Kind::Note { NOTE_H } else { ROW_H }
}

/// Lines (what, top, height) under the field and the height of it all,
/// DIP (`quiet`: "No results" first). Every hit has its row: a list taller
/// than its room scrolls under the field (list_view).
fn layout(kinds: &[Kind], quiet: bool) -> (Vec<(Line, f32, f32)>, f32) {
    let mut lines = Vec::new();
    if kinds.is_empty() && !quiet {
        return (lines, FIELD_H);
    }
    let mut y = FIELD_H + LIST_TOP;
    if quiet {
        lines.push((Line::Quiet, y, QUIET_H));
        y += QUIET_H;
    }
    let mut last: Option<Kind> = None;
    for (i, &k) in kinds.iter().enumerate() {
        // The file search's note sits in the Files group (under its
        // heading, which it brings along when no file was found).
        let group = if k == Kind::Note { Kind::File } else { k };
        if last != Some(group) {
            if group == Kind::Web {
                // (Under the quiet block the web row follows at once.)
                if !quiet || last.is_some() {
                    y += WEB_GAP;
                }
            } else {
                lines.push((Line::Head(group), y, HEAD_H));
                y += HEAD_H;
            }
            last = Some(group);
        }
        lines.push((Line::Item(i), y, row_h(k)));
        y += row_h(k);
    }
    (lines, y + PAD_BOTTOM)
}

/// Pure: the panel's height for a list `content` tall in the room `max_h`,
/// and how far the list can scroll.
fn list_view(content: f32, max_h: f32) -> (f32, f32) {
    let h = content.min(max_h.max(FIELD_H));
    (h, (content - h).max(0.0))
}

/// Pure: the scroll that shows the row at `y`, `h` tall (as laid out),
/// from scroll `off` in a panel `view` tall: clear of the band under the
/// field where the list fades (2.5) and of the panel's foot; the first
/// rows bring their heading back (0).
fn reveal(off: f32, (y, h): (f32, f32), view: f32, most: f32) -> f32 {
    let top = FIELD_H + size::SCROLL_FADE;
    let mut to = off;
    if y - to < top {
        to = y - top;
        // Nothing above it but a heading and the list's top: all of it.
        if to <= LIST_TOP + HEAD_H {
            to = 0.0;
        }
    } else if y + h - to > view - PAD_BOTTOM {
        to = y + h + PAD_BOTTOM - view;
    }
    to.clamp(0.0, most.max(0.0))
}

/// The quick results with the files put in (each group at most its limit),
/// in display order.
fn merge(quick: &[Hit], files: &[Hit]) -> Vec<Hit> {
    let mut v: Vec<Hit> = quick.to_vec();
    v.extend(files.iter().take(Kind::File.limit()).cloned());
    // Stable: each group keeps its own order.
    v.sort_by_key(|h| h.kind);
    v
}

/// The panel's width (DIP): W, or the work area less a margin when that
/// is narrower (a portrait monitor at a high scale).
fn panel_width(work: RECT, scale: f32) -> f32 {
    let room = (work.right - work.left) as f32 / scale - 2.0 * EDGE;
    W.min(room).max(160.0)
}

/// Where the panel goes (screen px, without the shadow margin): centered
/// on the work area, a quarter of the way down.
fn place(work: RECT, scale: f32, width: f32, height: f32) -> RECT {
    let w_px = (width * scale).ceil() as i32;
    let h_px = (height * scale).ceil() as i32;
    let x = work.left + ((work.right - work.left) - w_px) / 2;
    let y = work.top + (work.bottom - work.top) / 4;
    RECT { left: x.max(work.left), top: y, right: x.max(work.left) + w_px, bottom: y + h_px }
}

/// Room for the panel below its top (DIP).
fn max_height(work: RECT, scale: f32) -> f32 {
    let top = work.top + (work.bottom - work.top) / 4;
    (((work.bottom - top) as f32 - EDGE * scale) / scale).max(FIELD_H)
}

// ---- drawing helpers ---------------------------------------------------------------------

fn rgba(c: crate::theme::Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn contains(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// UTF-16 length of chars (DirectWrite positions are UTF-16).
pub(crate) fn utf16_len(cs: &[char]) -> u32 {
    cs.iter().map(|c| c.len_utf16() as u32).sum()
}

/// Char index of a UTF-16 position.
pub(crate) fn char_index(cs: &[char], pos16: u32) -> usize {
    let mut n = 0u32;
    for (i, c) in cs.iter().enumerate() {
        if n >= pos16 {
            return i;
        }
        n += c.len_utf16() as u32;
    }
    cs.len()
}

/// The window at `p` (screen px) is an IME's (candidate list, composition
/// window): its class says so.
unsafe fn ime_window(p: POINT) -> bool {
    let w = WindowFromPoint(p);
    if w.is_invalid() {
        return false;
    }
    let root = GetAncestor(w, GA_ROOT);
    [w, root].iter().any(|&h| {
        let mut buf = [0u16; 128];
        let n = GetClassNameW(h, &mut buf).max(0) as usize;
        is_ime_class(&String::from_utf16_lossy(&buf[..n]))
    })
}

fn is_ime_class(class: &str) -> bool {
    let c = class.to_ascii_lowercase();
    c == "ime" || c.starts_with("msctfime") || c.contains("candidate")
}

// ---- clipboard ------------------------------------------------------------------------------

/// The most text a paste takes (UTF-16 units), read before allocating.
const MAX_PASTE: usize = 4096;

/// The clipboard's text, at most MAX_PASTE units (a surrogate pair is not
/// cut in half); only that much is ever copied out of it.
pub(crate) unsafe fn clipboard_text(owner: HWND) -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    OpenClipboard(Some(owner)).ok()?;
    let read = || -> Option<String> {
        // CF_UNICODETEXT.
        let h = GetClipboardData(13).ok()?;
        let mem = HGLOBAL(h.0);
        let p = GlobalLock(mem) as *const u16;
        if p.is_null() {
            return None;
        }
        let units = (GlobalSize(mem) / 2).min(MAX_PASTE);
        let s = std::slice::from_raw_parts(p, units);
        let end = paste_end(s);
        let text = String::from_utf16_lossy(&s[..end]);
        let _ = GlobalUnlock(mem);
        Some(text)
    };
    let text = read();
    let _ = CloseClipboard();
    text
}

/// Where pasted text ends in `s` (already capped): at its terminator, and
/// never between the halves of a surrogate pair.
fn paste_end(s: &[u16]) -> usize {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    if end > 0 && (0xD800..=0xDBFF).contains(&s[end - 1]) {
        end - 1
    } else {
        end
    }
}

/// Puts text on the clipboard, owned by `owner` (the panel: with no owner,
/// EmptyClipboard leaves nobody to hand the data to and the copy fails).
/// False if it did not land; the old contents may then be gone already if
/// emptying worked but setting did not.
pub(crate) unsafe fn copy_text(owner: HWND, text: &str) -> bool {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) else { return false };
    let p = GlobalLock(mem) as *mut u16;
    if p.is_null() {
        let _ = GlobalFree(Some(mem));
        return false;
    }
    std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
    let _ = GlobalUnlock(mem);
    if OpenClipboard(Some(owner)).is_err() {
        let _ = GlobalFree(Some(mem));
        return false;
    }
    let ok = EmptyClipboard().is_ok() && SetClipboardData(13, Some(HANDLE(mem.0))).is_ok();
    if !ok {
        let _ = GlobalFree(Some(mem));
    }
    let _ = CloseClipboard();
    ok
}

/// Picking the calculator row: its result goes on the clipboard through
/// `put` as it was worked out (a plain number, or "2026-12-25" for a date),
/// never as shown on the row. True if it landed.
fn copy_result(hit: &Hit, put: impl FnOnce(&str) -> bool) -> bool {
    hit.kind == Kind::Calc && !hit.target.is_empty() && put(&hit.target)
}

/// Text at the right end of a row (a locale key): a failed copy on the
/// calculator row, and Ctrl+Enter on a selected file. The calculator row
/// says nothing about copying (a quiet row, no explanations).
fn row_hint(kind: Kind, selected: bool, copy_failed: bool) -> Option<&'static str> {
    match kind {
        Kind::Calc if copy_failed => Some("search.copy_failed"),
        Kind::File if selected => Some("search.hint.reveal"),
        _ => None,
    }
}

// ---- the panel -------------------------------------------------------------------------------

/// What the pointer holds down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
    /// A result (index into the hits).
    Row(usize),
    /// The note's "Try again".
    Retry,
}

/// What the last paint drew that answers the pointer (DIP, as laid out:
/// not mirrored), where the list is scrolled to.
#[derive(Default)]
struct Painted {
    rows: Vec<(D2D_RECT_F, usize)>,
    /// The note's button.
    retry: Option<D2D_RECT_F>,
}

/// What the rows show: one emphasis (design system 6.2: the later input
/// wins; a list whose selection stays has it follow the cursor).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Emphasis {
    /// The selected fill, there and gone at once: the keyboard moved last.
    mark: Option<usize>,
    /// The pointer's fill (90 on, 120 off), held down or not: the pointer
    /// moved last.
    lit: Option<(usize, bool)>,
    /// The note's button is under the pointer, and the pointer moved last.
    button: bool,
}

/// Pure: the emphasis for the selection `sel` (what Enter opens: the row
/// the later input chose), `by_key` (the keyboard moved last), what the
/// pointer holds down, the pointer on the note's button (`on_button`) and
/// the note's row (`note`). A press shows where it is; the keyboard's
/// selection is the selected fill; the pointer's is its fill on the row it
/// chose (that stays when the pointer goes on to the field: it is still
/// what Enter opens). The note's row answers the keyboard only: under the
/// pointer its button shows instead.
fn emphasis(sel: Option<usize>, by_key: bool, pressed: Option<Press>, on_button: bool, note: Option<usize>) -> Emphasis {
    match pressed {
        Some(Press::Row(i)) => Emphasis { lit: Some((i, true)), ..Default::default() },
        Some(Press::Retry) => Emphasis { button: true, ..Default::default() },
        None if by_key => Emphasis { mark: sel, ..Default::default() },
        None if on_button => Emphasis { button: true, ..Default::default() },
        None => Emphasis { lit: sel.filter(|&i| Some(i) != note).map(|i| (i, false)), ..Default::default() },
    }
}

/// Pure: the fills fade under the field (their band, 2.5) when the list
/// is scrolled, as gfx::scroll_fade fades the rows' picture; high contrast
/// has the picture's line instead, and no band.
fn banded(list_off: f32, hc: bool) -> bool {
    list_off > 0.0 && !hc
}

/// Pure: row `i`'s state for the theme's colors (high contrast draws from
/// it) under emphasis `e`.
fn row_state(i: usize, e: Emphasis) -> State {
    match e.lit {
        Some((t, true)) if t == i => State::Pressed,
        Some((t, false)) if t == i => State::Hover,
        _ if e.mark == Some(i) => State::Cursor,
        _ => State::Rest,
    }
}

/// Run once the panel is gone (the opened app gets the foreground).
#[derive(Clone, Debug, PartialEq, Eq)]
enum After {
    Open(String),
    /// Show a file in its folder.
    Reveal(String),
}

pub struct Request<'a> {
    pub gfx: &'a Rc<Gfx>,
    pub theme: &'a Theme,
    /// The monitor to open on (HMONITOR); None: the foreground window's.
    pub monitor: Option<isize>,
}

struct Panel {
    hwnd: HWND,
    gfx: Rc<Gfx>,
    theme: Theme,
    scale: f32,
    _target: DesktopWindowTarget,
    outer: ContainerVisual,
    content: SpriteVisual,
    /// Under the picture, cut at the field's foot: the rows' fills
    /// (crate::wash): the selection, the pointer's, the note's button.
    under: ContainerVisual,
    /// `under` seen through the list's scroll edge (2.5: the fills fade
    /// under the field with the picture's rows); None if that could not
    /// be had (then `under` is cut at the field's foot as it is).
    edges: Option<crate::popup::parts::Edges>,
    /// The band shows (scrolled; not in high contrast, where the picture
    /// has its line).
    banded: bool,
    mark: crate::wash::Mark,
    wash: crate::wash::Hover<usize>,
    retry: crate::wash::Raised,
    /// Over the picture: the progress ring at the field's end.
    over: ContainerVisual,
    ring: Option<crate::wash::Ring>,
    work: RECT,
    /// Panel width (DIP): W, or less on a narrow work area.
    w: f32,
    /// On screen (px), without the shadow margin.
    rect: RECT,
    height: f32,
    shared: Arc<Shared>,
    workers: Workers,
    field: Field,
    /// The IME's composition, shown at the caret until it is committed.
    comp: Vec<char>,
    /// First half of a surrogate pair from WM_CHAR.
    high: Option<u16>,
    /// Query number: results for older ones are dropped.
    seq: u64,
    quick: Vec<Hit>,
    files: Vec<Hit>,
    hits: Vec<Hit>,
    lines: Vec<(Line, f32, f32)>,
    /// The selection: what Enter opens. It follows the cursor of the input
    /// that moved last (design system 6.2): the keyboard's, or the row the
    /// pointer moved onto.
    sel: Option<usize>,
    /// The keyboard moved last (or the list is new): the selection shows
    /// as the selected fill; else as the pointer's fill.
    by_key: bool,
    /// The row under the pointer (never the note: only its button answers).
    hover: Option<usize>,
    /// The pointer is on the note's button.
    hot_retry: bool,
    pressed: Option<Press>,
    /// What the last paint drew.
    painted: Painted,
    /// The list's height as laid out, and how far it is scrolled.
    content_h: f32,
    list_off: f32,
    /// "No results" shows.
    quiet: bool,
    /// The ring shows (answers are WAIT_MS late).
    ring_on: bool,
    /// When the ring is due (WAIT_TIMER runs): set as a wait begins, kept
    /// while it lasts.
    wait_due: Option<Instant>,
    /// Field text scroll and the caret's x in the text (DIP).
    scroll: f32,
    caret_x: f32,
    caret_on: bool,
    /// Icon keys already sent to the icon worker.
    asked: HashSet<String>,
    last_mouse: (i32, i32),
    done: bool,
    after: Option<After>,
    /// Query number the quick results are for: when it is not `seq`, every
    /// row is from an older query and nothing may be opened (no selection).
    quick_seq: u64,
    /// Enter was pressed before the current query's results came: open the
    /// top row as soon as they do (Some(ctrl)).
    pending: Option<bool>,
    /// The calculator result could not be copied (shown on its row).
    copy_failed: bool,
    /// How the file search for the text shown stands (a note row).
    files_state: FilesState,
    /// Caret inside the composition (chars from its start).
    comp_cursor: usize,
    /// IME composition or candidate list on screen: the mouse is not
    /// captured (clicks reach the candidate window).
    ime_ui: bool,
    candidates: bool,
    /// Right-to-left language: the panel is laid out as usual and drawn
    /// through a mirroring transform (magnifier and icons on the right);
    /// text, glyphs and icons are drawn unmirrored at their mirrored
    /// places, the field's text is right-aligned and pointer x is mirrored
    /// back for the rows (see dip).
    rtl: bool,
}

thread_local! {
    static SP: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

fn with_panel<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    SP.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// The panel is open right now.
pub fn is_open() -> bool {
    SP.with(|c| c.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

/// Close the panel (hotkey again, display change). Safe from inside the
/// nested loop's handlers.
pub fn cancel() {
    let h = with_panel(|p| {
        p.done = true;
        p.hwnd
    });
    if let Some(h) = h {
        unsafe {
            let _ = PostMessageW(Some(h), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

/// The theme was re-resolved.
pub fn retheme(theme: &Theme) {
    with_panel(|p| {
        if p.theme != *theme {
            p.theme = theme.clone();
            if let Err(e) = p.build_chrome() {
                crate::log!("search: retheme failed: {e}");
            }
            p.redraw();
        }
    });
}

/// The hotkey was pressed: close if open, else open on the foreground
/// window's monitor.
pub fn toggle(gfx: &Rc<Gfx>, theme: &Theme) {
    if is_open() {
        cancel();
    } else {
        show(Request { gfx, theme, monitor: None });
    }
}

/// Shows the panel until it closes. Must not be called while the caller
/// holds its own RefCell borrow (the nested loop dispatches to it).
pub fn show(req: Request) {
    let _companion_pause = crate::companion::pause_ui();
    if is_open() || crate::popup::is_tracking() || crate::topbar::control::is_open() || crate::launchpad::is_open() {
        return;
    }
    // "Search with" another launcher (Settings > General): it opens instead.
    if crate::launchers::redirect_search() {
        return;
    }
    unsafe {
        register_class();
        let panel = match Panel::new(&req) {
            Ok(p) => p,
            Err(e) => {
                crate::log!("search: cannot open: {e}");
                return;
            }
        };
        let hwnd = panel.hwnd;
        SP.with(|c| *c.borrow_mut() = Some(panel));
        OPEN_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        refresh_apps();
        with_panel(|p| {
            // Last time's query, selected: typing replaces it.
            let last = LAST_QUERY.lock().map(|q| q.clone()).unwrap_or_default();
            if !last.is_empty() {
                p.field.insert(&last);
                p.field.select_all();
                p.changed();
            }
            p.redraw();
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        SetCapture(hwnd);
        let blink = GetCaretBlinkTime();
        if blink != 0 && blink != u32::MAX {
            SetTimer(Some(hwnd), CARET_TIMER, blink, None);
        }
        if GetForegroundWindow() != hwnd || GetCapture() != hwnd {
            crate::log!("search: could not take foreground/capture; not opening");
        } else {
            let mut msg = MSG::default();
            loop {
                if with_panel(|p| p.done).unwrap_or(true) {
                    break;
                }
                let r = GetMessageW(&mut msg, None, 0, 0);
                if r.0 == 0 || r.0 == -1 {
                    // WM_QUIT: close and let the outer loop see it.
                    PostQuitMessage(msg.wParam.0 as i32);
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = KillTimer(Some(hwnd), CARET_TIMER);
        let _ = KillTimer(Some(hwnd), WAIT_TIMER);
        let _ = ReleaseCapture();
        OPEN_HWND.store(0, Ordering::SeqCst);
        let mut after = None;
        if let Some(p) = SP.with(|c| c.borrow_mut().take()) {
            if let Ok(mut q) = LAST_QUERY.lock() {
                *q = p.field.text();
            }
            after = p.after.clone();
            p.close();
        }
        match after {
            Some(After::Open(target)) => {
                // Failing (a deleted file, a broken shortcut) is told; the
                // query is shown again on the next open (LAST_QUERY).
                let name = app_list().iter().find(|a| a.target() == target).map(|a| a.name.clone()).unwrap_or_default();
                crate::launch::open(&crate::launch::Target::new(&target, &name, crate::launch::Source::Search));
            }
            Some(After::Reveal(path)) => {
                let args = HSTRING::from(format!("/select,\"{path}\""));
                crate::launch::open_soon(crate::launch::Target::new("explorer.exe", "Explorer", crate::launch::Source::Dock).args(&args.to_string(), ""));
            }
            None => {}
        }
    }
}

unsafe fn register_class() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarSearch"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    // Fails harmlessly once registered.
    RegisterClassExW(&wc);
}

impl Panel {
    unsafe fn new(req: &Request) -> Result<Self> {
        let hmon = match req.monitor {
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
        Self::make(req.gfx, req.theme, mon.scale(), mon.work, Workers::start)
    }

    /// The (hidden) panel for the work area `work` at scale `s`, its
    /// workers from `workers` (Workers::start).
    unsafe fn make(gfx: &Rc<Gfx>, theme: &Theme, s: f32, work: RECT, workers: fn(&Arc<Shared>) -> Workers) -> Result<Self> {
        let height = FIELD_H;
        let width = panel_width(work, s);
        let rect = place(work, s, width, height);
        let m_px = (SHADOW_M * s).ceil() as i32;
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("NopBarSearch"),
            &HSTRING::from(t("search.title")),
            WS_POPUP,
            rect.left - m_px,
            rect.top - m_px,
            rect.right - rect.left + m_px * 2,
            rect.bottom - rect.top + m_px * 2,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        let shared = Arc::new(Shared::new());
        let built = (|| -> Result<Self> {
            let enable = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &enable as *const _ as *const _, 4);
            let c = gfx.compositor.clone();
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let outer = c.CreateContainerVisual()?;
            outer.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
            outer.SetOffset(Vector3 { X: m_px as f32, Y: m_px as f32, Z: 0.0 })?;
            target.SetRoot(&outer)?;
            let content = c.CreateSpriteVisual()?;
            let under = c.CreateContainerVisual()?;
            let (mark, wash, retry) = (crate::wash::Mark::new(&c, &under)?, crate::wash::Hover::new(&c, &under, false)?, crate::wash::Raised::new(&c, &under)?);
            let over = c.CreateContainerVisual()?;
            shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
            let mut p = Panel {
                hwnd,
                gfx: gfx.clone(),
                theme: theme.clone(),
                scale: s,
                _target: target,
                outer,
                content,
                under,
                edges: None,
                banded: false,
                mark,
                wash,
                retry,
                over,
                ring: None,
                work,
                w: width,
                rect,
                height,
                workers: workers(&shared),
                shared: shared.clone(),
                field: Field::default(),
                comp: Vec::new(),
                high: None,
                seq: 0,
                quick: Vec::new(),
                files: Vec::new(),
                hits: Vec::new(),
                lines: Vec::new(),
                sel: None,
                hover: None,
                by_key: true,
                hot_retry: false,
                pressed: None,
                painted: Painted::default(),
                content_h: height,
                list_off: 0.0,
                quiet: false,
                ring_on: false,
                wait_due: None,
                scroll: 0.0,
                caret_x: 0.0,
                caret_on: true,
                asked: HashSet::new(),
                last_mouse: (i32::MIN, i32::MIN),
                done: false,
                after: None,
                quick_seq: 0,
                pending: None,
                copy_failed: false,
                files_state: FilesState::Idle,
                comp_cursor: 0,
                ime_ui: false,
                candidates: false,
                rtl: crate::i18n::rtl(),
            };
            if let Err(e) = p.build_chrome() {
                p.workers.quit(&p.shared);
                return Err(e);
            }
            // It opens as panels do (crate::motion::open: it grows on
            // GLIDE, its light comes up after it; a short fade only with
            // reduced motion). Nothing on screen opened it (a shortcut):
            // it grows from its middle (7.4).
            let _ = crate::motion::open(&c, &p.outer, (width, height), crate::motion::origin((0.0, 0.0, width, height), None), crate::motion::Kind::Panel, s);
            Ok(p)
        })();
        if built.is_err() {
            shared.stop();
            let _ = DestroyWindow(hwnd);
        }
        built
    }

    fn build_chrome(&mut self) -> Result<()> {
        for v in [self.content.cast::<Visual>()?, self.under.cast()?, self.over.cast()?] {
            if let Ok(parent) = v.Parent() {
                let _ = parent.Children().and_then(|c| c.Remove(&v));
            }
        }
        self.outer.Children()?.RemoveAll()?;
        let (root, border) = crate::popup::chrome(&self.gfx, &self.theme, &self.outer, self.w, self.height, self.scale, self.theme.radii().panel)?;
        self.content.SetSize(Vector2 { X: self.w, Y: self.height })?;
        root.Children()?.InsertBelow(&self.content, &border)?;
        root.Children()?.InsertAbove(&self.over, &self.content)?;
        // The rows' fills never show under the field (the list scrolls
        // under it), and once it is scrolled they fade out over the band
        // under it as the rows' picture does (gfx::scroll_fade): one layer
        // of content (2.5), seen through the menus' scroll edges.
        self.under.SetSize(Vector2 { X: self.w, Y: self.height })?;
        self.edges = None;
        self.banded = false;
        let list_h = self.height - FIELD_H;
        let edged = (list_h > 2.0 * size::SCROLL_FADE)
            .then(|| -> Result<crate::popup::parts::Edges> {
                let view = self.gfx.compositor.CreateContainerVisual()?;
                view.SetSize(Vector2 { X: self.w, Y: list_h })?;
                self.under.SetClip(None)?;
                self.under.SetOffset(Vector3 { X: 0.0, Y: -FIELD_H, Z: 0.0 })?;
                view.Children()?.InsertAtTop(&self.under)?;
                let e = crate::popup::parts::Edges::new(&self.gfx, &self.theme, &view, (self.w, list_h, size::SCROLL_FADE), self.scale, (false, false))?;
                e.stage.SetOffset(Vector3 { X: 0.0, Y: FIELD_H, Z: 0.0 })?;
                Ok(e)
            })
            .and_then(|r| r.map_err(|e| crate::log!("search: no scroll edge for the fills: {e}")).ok());
        let fills: Visual = match &edged {
            Some(e) => e.stage.cast()?,
            None => {
                if let Ok(parent) = self.under.Parent() {
                    let _ = parent.Children().and_then(|c| c.Remove(&self.under));
                }
                self.under.SetOffset(Vector3 { X: 0.0, Y: 0.0, Z: 0.0 })?;
                self.under.SetClip(&self.gfx.compositor.CreateInsetClipWithInsets(0.0, FIELD_H, 0.0, 0.0)?)?;
                self.under.cast()?
            }
        };
        self.edges = edged;
        root.Children()?.InsertBelow(&fills, &self.content)?;
        // In the theme's colors again.
        self.mark.forget();
        self.wash.clear();
        self.retry.forget();
        self.over.Children()?.RemoveAll()?;
        self.ring = crate::wash::Ring::new(&self.gfx, &self.theme, self.scale, &self.over).ok();
        if let Some(r) = &mut self.ring {
            let cx = self.w - LINE - size::SPINNER / 2.0;
            r.place(crate::i18n::mirror::x(self.rtl, cx, 0.0, self.w), FIELD_H / 2.0);
            let c = self.gfx.compositor.clone();
            r.show(&c, self.ring_on);
        }
        Ok(())
    }

    /// The text shown has answers still on their way.
    fn waiting(&self) -> bool {
        waiting(self.query().trim().is_empty(), self.stale(), self.files_state)
    }

    /// The ring follows the wait: armed once when one begins (it shows
    /// WAIT_MS after that), gone at once when it is over.
    fn sync_wait(&mut self) {
        match wait_step(self.waiting(), self.wait_due.is_some(), self.ring_on) {
            Wait::Arm => {
                self.wait_due = Some(Instant::now() + Duration::from_millis(WAIT_MS as u64));
                unsafe {
                    SetTimer(Some(self.hwnd), WAIT_TIMER, WAIT_MS, None);
                }
            }
            // (Not armed again by every answer or key: the wait began with
            // the first answer still missing.)
            Wait::Keep => {}
            Wait::Over => {
                self.wait_due = None;
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), WAIT_TIMER);
                }
                self.set_ring(false);
            }
        }
    }

    /// WAIT_TIMER: the wait has lasted WAIT_MS; the ring comes on if it
    /// still goes on.
    fn wait_over(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), WAIT_TIMER);
        }
        self.wait_due = None;
        if self.waiting() {
            self.set_ring(true);
        }
    }

    fn set_ring(&mut self, on: bool) {
        self.ring_on = on;
        let c = self.gfx.compositor.clone();
        if let Some(r) = &mut self.ring {
            r.follow_motion(&c);
            r.show(&c, on);
        }
    }

    /// Reduced motion switched on while the ring turns: it stops (12.2).
    fn follow_motion(&mut self) {
        let c = self.gfx.compositor.clone();
        if let Some(r) = &mut self.ring {
            r.follow_motion(&c);
        }
    }

    fn close(self) {
        unsafe {
            // A composition still open must not end up in another window.
            let himc = ImmGetContext(self.hwnd);
            if !himc.is_invalid() {
                let _ = ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                let _ = ImmReleaseContext(self.hwnd, himc);
            }
        }
        self.workers.quit(&self.shared);
        // It fades out as it faded in (crate::motion::retire: click-through
        // meanwhile, then destroyed with everything that draws it).
        let (hwnd, c, outer) = (self.hwnd, self.gfx.compositor.clone(), self.outer.clone());
        crate::motion::retire(hwnd, &c, &outer, Box::new(self));
        // The icons go with the last reference to `shared` (the workers
        // drop theirs as they end).
    }

    // ---- queries and results ----

    /// The text (with any composition) changed: ask the sources again.
    fn changed(&mut self) {
        self.seq += 1;
        self.caret_on = true;
        unsafe {
            let blink = GetCaretBlinkTime();
            if blink != 0 && blink != u32::MAX {
                SetTimer(Some(self.hwnd), CARET_TIMER, blink, None);
            }
        }
        self.copy_failed = false;
        self.pending = None;
        let text = self.query();
        if text.trim().is_empty() {
            self.quick.clear();
            self.files.clear();
            self.files_state = FilesState::Idle;
            self.quick_seq = self.seq;
            self.merge(false);
        } else {
            self.files_state = FilesState::Waiting;
            // Everything shown is now from an older query: no selection,
            // nothing opens until this query's results come (stale() ).
            // Until the index answers, keep the files that still match.
            let q = matching::Query::new(&text);
            self.files.retain(|h| q.score(&h.title).is_some());
            self.merge(false);
            let _ = self.workers.files.send(Msg::Job((self.seq, text.clone())));
        }
        let _ = self.workers.quick.send(Msg::Job((self.seq, text)));
        self.sync_wait();
        self.redraw();
    }

    fn query(&self) -> String {
        self.field.with_composition(&self.comp).0.into_iter().collect()
    }

    /// Results or icons arrived (`apps`: the app list was refreshed).
    fn sync(&mut self, apps: bool) {
        if apps {
            let text = self.query();
            if !text.trim().is_empty() {
                let _ = self.workers.quick.send(Msg::Job((self.seq, text)));
            }
            return;
        }
        // Results of an older query (a later answer for text no longer
        // there) are dropped by their query number.
        let take = |slot: &Mutex<Option<(u64, Vec<Hit>)>>| slot.lock().ok().and_then(|mut s| s.take());
        let mut changed = false;
        let mut fresh = false;
        if let Some((seq, hits)) = take(&self.shared.quick) {
            if seq == self.seq {
                fresh = self.quick_seq != seq;
                self.quick = hits;
                self.quick_seq = seq;
                changed = true;
            }
        }
        if let Some((seq, got)) = self.shared.files.lock().ok().and_then(|mut s| s.take()) {
            if seq == self.seq {
                self.files = got.hits;
                self.files_state = if got.timed_out { FilesState::TimedOut(got.from) } else { FilesState::Done(got.from) };
                changed = true;
            }
        }
        if changed && !self.query().trim().is_empty() {
            // The first answer for a query starts at the top row.
            self.merge(!fresh);
            if fresh {
                if let (Some(ctrl), Some(i)) = (self.pending.take(), self.sel) {
                    self.activate(i, ctrl);
                    return;
                }
            }
        }
        self.sync_wait();
        self.redraw();
    }

    /// The rows shown are from an older query than the text.
    fn stale(&self) -> bool {
        self.quick_seq != self.seq
    }

    /// Rebuilds the rows; `keep`: the selected row stays selected if it is
    /// still there (else the top one is).
    fn merge(&mut self, keep: bool) {
        let was = if keep { self.sel.and_then(|i| self.hits.get(i)).map(|h| (h.kind, h.target.clone())) } else { None };
        self.hits = merge(&self.quick, &self.files);
        if let Some(n) = files_note(self.files_state) {
            self.hits.push(n);
            self.hits.sort_by_key(|h| h.kind);
        }
        if self.query().trim().is_empty() {
            self.hits.clear();
        }
        self.sel = match was.and_then(|(k, t)| self.hits.iter().position(|h| h.kind == k && h.target == t)) {
            _ if self.stale() => None,
            Some(i) => Some(i),
            None if self.hits.is_empty() => None,
            None => Some(0),
        };
        // The rows changed under the pointer: its fill goes until it moves
        // (the selection shows as the keyboard's).
        self.pressed = None;
        self.hover = None;
        self.hot_retry = false;
        self.by_key = true;
        self.wash.clear();
        let px = (ICON * self.scale).round() as u32;
        let want: Vec<String> = self.hits.iter().filter_map(|h| h.icon.clone()).filter(|k| !self.asked.contains(k)).collect();
        if !want.is_empty() {
            self.asked.extend(want.iter().cloned());
            let _ = self.workers.icons.send(Msg::Job((px, want)));
        }
        self.relayout();
    }

    /// Lines for the hits, and the window resized to them (a list taller
    /// than its room scrolls under the field).
    fn relayout(&mut self) {
        let kinds: Vec<Kind> = self.hits.iter().map(|h| h.kind).collect();
        self.quiet = nothing_found(self.query().trim().is_empty(), self.stale(), self.files_state, &kinds);
        let (lines, content) = layout(&kinds, self.quiet);
        self.lines = lines;
        self.content_h = content;
        let (h, most) = list_view(content, max_height(self.work, self.scale));
        let visible = self.visible();
        if self.sel.is_some_and(|s| !visible.contains(&s)) {
            self.sel = if self.stale() { None } else { visible.first().copied() };
        }
        // The selected row stays in view (a new list starts at its top).
        self.list_off = match self.sel.and_then(|s| self.line_of(s)) {
            Some(row) => reveal(self.list_off.min(most), row, h, most),
            None => 0.0,
        };
        if h != self.height {
            self.height = h;
            self.rect = place(self.work, self.scale, self.w, h);
            let m_px = (SHADOW_M * self.scale).ceil() as i32;
            let r = self.rect;
            unsafe {
                let _ = SetWindowPos(self.hwnd, None, r.left - m_px, r.top - m_px, r.right - r.left + m_px * 2, r.bottom - r.top + m_px * 2, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            if let Err(e) = self.build_chrome() {
                crate::log!("search: resize failed: {e}");
            }
        }
    }

    /// Hit indexes in the list, top to bottom (the keyboard's order).
    fn visible(&self) -> Vec<usize> {
        self.lines.iter().filter_map(|(l, _, _)| if let Line::Item(i) = l { Some(*i) } else { None }).collect()
    }

    /// Hit `i`'s row as laid out: (top, height).
    fn line_of(&self, i: usize) -> Option<(f32, f32)> {
        self.lines.iter().find(|(l, _, _)| *l == Line::Item(i)).map(|&(_, y, h)| (y, h))
    }

    /// How far the list can scroll.
    fn most(&self) -> f32 {
        (self.content_h - self.height).max(0.0)
    }

    /// Scrolls the list to `off` (kept in range); true when it moved.
    fn scroll_list(&mut self, off: f32) -> bool {
        let to = off.clamp(0.0, self.most());
        let moved = to != self.list_off;
        self.list_off = to;
        moved
    }

    // ---- colors ----

    fn field_x1(&self) -> f32 {
        self.w - FIELD_END
    }

    /// Text of `level` on something in state `s` (theme::text_of: the
    /// translucent levels, which keep their contrast on the panel's
    /// see-through surface; the system's colors in high contrast).
    fn ink_of(&self, level: Level, s: State) -> D2D1_COLOR_F {
        rgba(self.theme.text_of(level, s))
    }
    fn ink(&self) -> D2D1_COLOR_F {
        self.ink_of(Level::Primary, State::Rest)
    }
    fn dim(&self) -> D2D1_COLOR_F {
        self.ink_of(Level::Secondary, State::Rest)
    }
    fn faint(&self) -> D2D1_COLOR_F {
        self.ink_of(Level::Tertiary, State::Rest)
    }

    // ---- drawing ----

    /// `text` in ramp style `st` (its optical-size instance and tracking).
    fn layout_text(&self, text: &str, st: TextStyle) -> Option<(IDWriteTextLayout, f32, f32)> {
        self.gfx.text_style(text, &self.theme.font.family, st, 1.0).ok()
    }

    /// Text cut with "…" to fit `maxw` DIP.
    fn fit_text(&self, text: &str, st: TextStyle, maxw: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
        let l = self.layout_text(text, st)?;
        if l.1 <= maxw {
            return Some(l);
        }
        let chars: Vec<char> = text.chars().collect();
        let cut = |n: usize| format!("{}…", chars[..n].iter().collect::<String>().trim_end());
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.layout_text(&cut(mid), st).is_some_and(|l| l.1 <= maxw) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        self.layout_text(&cut(lo), st)
    }

    /// A one-line layout's width (DIP), whichever way it reads.
    fn width_of(l: &IDWriteTextLayout) -> f32 {
        let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = l.GetMetrics(&mut m);
        }
        m.widthIncludingTrailingWhitespace
    }

    /// x of UTF-16 position `pos` in a layout (DIP).
    fn x_at(l: &IDWriteTextLayout, pos: u32) -> f32 {
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = l.HitTestTextPosition(pos, false, &mut x, &mut y, &mut m);
        }
        x
    }

    /// The field's layout: (text shown, layout, height); None when empty.
    fn field_layout(&self) -> Option<(Vec<char>, usize, IDWriteTextLayout, f32)> {
        let (shown, comp_at) = self.field.with_composition(&self.comp);
        if shown.is_empty() {
            return None;
        }
        let s: String = shown.iter().collect();
        let (l, _, h) = self.layout_text(&s, T_FIELD)?;
        Some((shown, comp_at, l, h))
    }

    /// Where the caret is (chars into the shown text).
    fn caret_index(&self, comp_at: usize) -> usize {
        if self.comp.is_empty() { self.field.caret() } else { comp_at + self.comp_cursor.min(self.comp.len()) }
    }

    /// Keeps the caret inside the field (horizontal scroll).
    fn update_scroll(&mut self) {
        let Some((shown, comp_at, l, _)) = self.field_layout() else {
            self.scroll = 0.0;
            self.caret_x = 0.0;
            return;
        };
        let x = Self::x_at(&l, utf16_len(&shown[..self.caret_index(comp_at)]));
        let width = self.field_x1() - FIELD_X0;
        let total = Self::width_of(&l);
        if total <= width {
            self.scroll = 0.0;
        } else if x - self.scroll > width - 2.0 {
            self.scroll = x - width + 2.0;
        } else if x < self.scroll {
            self.scroll = x;
        }
        self.scroll = self.scroll.clamp(0.0, (total - width + 2.0).max(0.0));
        self.caret_x = x;
    }

    fn redraw(&mut self) {
        self.update_scroll();
        let s = self.scale;
        let (pw, ph) = ((self.w * s).ceil() as u32, (self.height * s).ceil() as u32);
        let mut painted = Painted::default();
        let me = &*self;
        let surface = self.gfx.draw(pw, ph, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let plain = Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy };
            // Right to left: x reflected across the panel's width.
            dc.SetTransform(&if me.rtl { Matrix3x2 { M11: -s, M31: ox + me.w * s, ..plain } } else { plain });
            let r = me.paint(dc, &mut painted);
            dc.SetTransform(&Matrix3x2::identity());
            r
        });
        self.painted = painted;
        match surface {
            Ok(surface) => {
                if let Ok(brush) = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) {
                    let _ = brush.SetStretch(CompositionStretch::Fill);
                    let _ = self.content.SetBrush(&brush);
                }
            }
            Err(e) => crate::log!("search: draw failed: {e}"),
        }
        self.sync_fills();
        self.place_ime();
        self.publish();
    }

    /// Group `k`'s heading. Files: where they were looked for (the
    /// index's locations, or recent items only).
    fn heading(&self, k: Kind) -> Option<&'static str> {
        match (k, self.files_state) {
            (Kind::File, FilesState::Done(FileSource::Recent) | FilesState::TimedOut(FileSource::Recent)) => Some(t("search.group.recent_files")),
            (Kind::File, _) => Some(t("search.group.files_indexed")),
            _ => k.heading(),
        }
    }

    /// Screen readers: the tree, while one reads this window.
    fn publish(&self) {
        if a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.tree());
        }
    }

    /// The field (its text the value; the keyboard stays in it), the
    /// ring while answers are late, then the list as drawn: headings and
    /// "No results" as text, each result an item that is the selection or
    /// not (Enter opens the selection), the file search's note as text and
    /// its "Try again" button. Rows scrolled under the field are off
    /// screen.
    fn tree(&self) -> Tree {
        let (ds, origin) = (self.scale, (self.rect.left, self.rect.top));
        let at = |x: f32, y: f32, w: f32, h: f32| a11y::px_rect(origin, ds, crate::i18n::mirror::span(self.rtl, x, w, 0.0, self.w), y, w, h);
        let mut nodes = vec![Node::new("field", Ax::Edit, t("search.title"), at(LINE, 0.0, self.w - 2.0 * LINE, FIELD_H)).value(self.query()).help(t("search.placeholder"))];
        if self.ring_on {
            let side = size::SPINNER;
            let mut n = Node::new("busy", Ax::Text, t("search.files.searching"), at(self.w - LINE - side, (FIELD_H - side) / 2.0, side, side));
            n.unfocusable = true;
            nodes.push(n);
        }
        let sel = self.sel.filter(|_| !self.stale());
        for &(line, y, h) in &self.lines {
            let y = y - self.list_off;
            let hidden = y + h <= FIELD_H || y >= self.height;
            let r = at(VESSEL, y, self.w - 2.0 * VESSEL, h);
            let mut n = match line {
                Line::Head(k) => match self.heading(k) {
                    Some(name) => Node::new(format!("head:{k:?}"), Ax::Text, name, r),
                    None => continue,
                },
                Line::Quiet => Node::new("none", Ax::Text, t("search.none"), r),
                Line::Item(i) => {
                    let Some(hit) = self.hits.get(i) else { continue };
                    if hit.kind == Kind::Note {
                        let mut note = Node::new(format!("row:{i}"), Ax::Text, hit.title.clone(), r);
                        note.hidden = hidden;
                        nodes.push(note);
                        if let Some(b) = self.painted.retry.filter(|_| !hidden) {
                            nodes.push(Node::new("retry", Ax::Button, t("launch.action.retry"), at(b.left, b.top, b.right - b.left, b.bottom - b.top)).invoke());
                        }
                        continue;
                    }
                    let mut item = Node::new(format!("row:{i}"), Ax::MenuItem, hit.title.clone(), r).help(hit.detail.clone()).invoke();
                    item.selected = Some(sel == Some(i));
                    item.scroll = true;
                    item
                }
            };
            n.hidden = hidden;
            nodes.push(n);
        }
        Tree { role: Ax::Group, name: t("search.title").into(), nodes, focus: Some("field".into()), focusable: true, active: true, focus_in: None }
    }

    /// What a screen reader asked of child `key`.
    fn asked(&mut self, key: &str, act: Act) {
        let row = key.strip_prefix("row:").and_then(|i| i.parse::<usize>().ok());
        match (key, row, act) {
            (_, _, Act::Refresh) => self.publish(),
            ("retry", _, Act::Invoke) => self.retry_files(),
            ("field", _, Act::SetValue) => {
                if let Some(text) = a11y::take_value(self.hwnd, "field") {
                    self.field.select_all();
                    self.field.insert(&text);
                    self.changed();
                }
            }
            (_, Some(i), Act::Invoke) => self.activate(i, false),
            (_, Some(i), Act::Select | Act::ScrollIntoView) if !self.stale() && self.visible().contains(&i) => {
                if act == Act::Select {
                    self.sel = Some(i);
                    self.by_key = true;
                }
                if let Some(row) = self.line_of(i) {
                    let to = reveal(self.list_off, row, self.height, self.most());
                    self.scroll_list(to);
                }
                self.redraw();
            }
            _ => {}
        }
    }

    /// A drawn row's shape for its fill (the rows sit on the container
    /// line: the panel's corner less that, design system 1.2).
    fn row_shape(&self, i: usize) -> Option<Shape> {
        let rad = crate::theme::concentric(self.theme.radii().panel, VESSEL);
        self.painted.rows.iter().find(|(_, k)| *k == i).map(|(r, _)| Shape::new(r.left, r.top, r.right - r.left, r.bottom - r.top, rad))
    }

    /// The note's row (file search gone wrong), if it is there.
    fn note(&self) -> Option<usize> {
        self.hits.iter().position(|h| h.kind == Kind::Note)
    }

    /// The one emphasis the rows show now (see `emphasis`); rows of an
    /// older query show none.
    fn emphasis(&self) -> Emphasis {
        let sel = self.sel.filter(|_| !self.stale());
        emphasis(sel, self.by_key, self.pressed, self.hot_retry, self.note())
    }

    /// The note's button's state, and its row's under it.
    fn button_states(&self, e: Emphasis) -> (State, State) {
        let ground = self.note().map_or(State::Rest, |n| row_state(n, e));
        (raised_state(e.button && self.hot_retry, self.pressed == Some(Press::Retry)), ground)
    }

    /// The fills under the picture follow the state (crate::wash): the
    /// keyboard's selection under its row at once, the pointer's fill
    /// fading on and off, a press at once (one of them: `emphasis`); the
    /// note's button. High contrast: the picture has them (paint).
    fn sync_fills(&mut self) {
        let c = self.gfx.compositor.clone();
        // The fills' band with the picture's (at once, as it is painted).
        let hc = self.theme.high_contrast;
        let band = banded(self.list_off, hc);
        if let Some(edges) = self.edges.as_mut().filter(|_| band != self.banded) {
            edges.jump((band, false), hc);
            self.banded = band;
        }
        let e = self.emphasis();
        self.mark.show(&self.theme, e.mark.and_then(|i| self.row_shape(i)));
        let lit = e.lit.and_then(|(t, down)| self.row_shape(t).map(|shape| Lit { t, down, shape }));
        self.wash.show(&c, &self.theme, lit);
        // (Its fill is the same on any row outside high contrast, where
        // the picture draws it.)
        let (state, _) = self.button_states(e);
        let button = self.painted.retry.map(|r| {
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let x = crate::i18n::mirror::span(self.rtl, r.left, w, 0.0, self.w);
            (Shape::new(x, r.top, w, h, self.theme.radii().control), state)
        });
        self.retry.show(&c, &self.theme, button);
    }

    /// A fill and, in high contrast, an outline for something of `role`
    /// in state `s` lying on something in state `ground` (a button on its
    /// row; theme::fill_on / outline_on: the outline inside its edge).
    unsafe fn plate(&self, dc: &ID2D1DeviceContext, role: Role, s: State, ground: State, r: D2D_RECT_F, rad: f32) -> Result<()> {
        self.fill(dc, rgba(self.theme.fill_on(role, s, ground)), r, rad)?;
        if let Some((c, wd)) = self.theme.outline_on(role, s, ground) {
            let h = wd / 2.0;
            let inner = D2D1_ROUNDED_RECT { rect: rect(r.left + h, r.top + h, r.right - h, r.bottom - h), radiusX: (rad - h).max(0.0), radiusY: (rad - h).max(0.0) };
            dc.DrawRoundedRectangle(&inner, &dc.CreateSolidColorBrush(&rgba(c), None)?, wd, None);
        }
        Ok(())
    }

    unsafe fn fill(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32) -> Result<()> {
        if c.a > 0.0 {
            let b = dc.CreateSolidColorBrush(&c, None)?;
            if rad > 0.0 {
                dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &b);
            } else {
                dc.FillRectangle(&r, &b);
            }
        }
        Ok(())
    }

    /// Text at (`x`, `y`): unmirrored right to left.
    unsafe fn draw_text(&self, dc: &ID2D1DeviceContext, l: &IDWriteTextLayout, x: f32, y: f32, c: D2D1_COLOR_F) -> Result<()> {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        if self.rtl {
            // Readable: unmirrored, where the mirrored box is (the layout
            // is exactly its text's width, gfx::read_rtl).
            let w = l.GetMaxWidth();
            self.unmirrored(dc, || crate::gfx::draw_text(dc, Vector2 { X: self.w - x - w, Y: y }, l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE));
        } else {
            crate::gfx::draw_text(dc, Vector2 { X: x, Y: y }, l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        Ok(())
    }

    /// Runs `f` with the panel's transform without its mirroring (right to
    /// left), in panel DIPs.
    unsafe fn unmirrored<R>(&self, dc: &ID2D1DeviceContext, f: impl FnOnce() -> R) -> R {
        let mut m = Matrix3x2::default();
        dc.GetTransform(&mut m);
        if m.M11 >= 0.0 {
            return f();
        }
        dc.SetTransform(&Matrix3x2 { M11: -m.M11, M31: m.M31 + m.M11 * self.w, ..m });
        let r = f();
        dc.SetTransform(&m);
        r
    }

    /// Where the field's text starts (DIP, unmirrored): after the
    /// magnifier, scrolled; right to left, ending at the field's right
    /// edge while it fits.
    fn text_x(&self, total: f32) -> f32 {
        let (a, b) = (FIELD_X0, self.field_x1());
        if !self.rtl {
            return a - self.scroll;
        }
        let (a, b) = crate::i18n::mirror::edges(true, a, b, 0.0, self.w);
        if total <= b - a { b - total } else { a - self.scroll }
    }

    unsafe fn glyph(&self, dc: &ID2D1DeviceContext, g: Glyph, size: f32, cx: f32, cy: f32, c: D2D1_COLOR_F) -> Result<()> {
        crate::gfx::draw_glyph(dc, g, cx, cy, size, c)
    }

    unsafe fn icon(&self, dc: &ID2D1DeviceContext, icon: &Icon, r: D2D_RECT_F) -> Result<()> {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let bmp = dc.CreateBitmap(D2D_SIZE_U { width: icon.width, height: icon.height }, Some(icon.pixels.as_ptr() as _), icon.width * 4, &props)?;
        if self.rtl {
            // App icons are never flipped.
            let (l, rr) = crate::i18n::mirror::edges(true, r.left, r.right, 0.0, self.w);
            let r = rect(l, r.top, rr, r.bottom);
            self.unmirrored(dc, || dc.DrawBitmap(&bmp, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None));
        } else {
            dc.DrawBitmap(&bmp, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
        }
        Ok(())
    }

    unsafe fn paint(&self, dc: &ID2D1DeviceContext, out: &mut Painted) -> Result<()> {
        // The field: the magnifier in the rows' icon slot, its text on
        // the rows' text line.
        let mid = FIELD_H / 2.0;
        self.glyph(dc, G_SEARCH, GLYPH, LINE + ICON / 2.0, mid, self.dim())?;
        dc.PushAxisAlignedClip(&rect(FIELD_X0 - 2.0, 0.0, self.field_x1() + if self.rtl { 2.0 } else { 0.0 }, FIELD_H), D2D1_ANTIALIAS_MODE_ALIASED);
        // The field draws unmirrored (its caret and selection follow the
        // text's own positions).
        let drawn = self.unmirrored(dc, || self.paint_field(dc));
        dc.PopAxisAlignedClip();
        drawn?;
        if self.lines.is_empty() {
            return Ok(());
        }
        // The list scrolls under the field (design system 2.5): cut at
        // the field's foot, and, once scrolled, fading in over the 12 DIP
        // under it. No line between the two: the band does that job (high
        // contrast: a line instead). The list ends at the panel's foot.
        let view = rect(0.0, FIELD_H, self.w, self.height);
        crate::gfx::scroll_fade(dc, view, self.list_off > 0.0, false, &self.theme, 1.0, || self.paint_list(dc, out))
    }

    /// The results as scrolled (paint's, inside its cut).
    unsafe fn paint_list(&self, dc: &ID2D1DeviceContext, out: &mut Painted) -> Result<()> {
        let icons = self.shared.icons.lock().map(|m| m.clone()).unwrap_or_default();
        let end = self.w - LINE;
        let sel = self.sel.filter(|_| !self.stale());
        let e = self.emphasis();
        for &(line, y, h) in &self.lines {
            let y = y - self.list_off;
            if y + h <= FIELD_H || y >= self.height {
                continue;
            }
            match line {
                Line::Head(k) => {
                    // Files: where they were looked for (the index's
                    // locations, or recent items only).
                    let head = self.heading(k);
                    if let Some((l, _, lh)) = head.and_then(|t| self.fit_text(t, T_SMALL, end - LINE)) {
                        self.draw_text(dc, &l, LINE, (y + (h - lh) / 2.0).round(), self.faint())?;
                    }
                }
                Line::Quiet => {
                    // The block reads the same both ways: unmirrored.
                    let r = rect(0.0, y, self.w, y + h);
                    self.unmirrored(dc, || self.gfx.quiet(dc, r, crate::gfx::Quiet::Empty, G_SEARCH, t("search.none"), &self.theme, 1.0, 0.0))?;
                }
                Line::Item(i) => {
                    let Some(hit) = self.hits.get(i) else { continue };
                    let r = rect(VESSEL, y, self.w - VESSEL, y + h);
                    let (cx, cy) = (LINE + ICON / 2.0, y + h / 2.0);
                    let selected = sel == Some(i);
                    if hit.kind == Kind::Note {
                        // Something went wrong (11.4): the warning, one
                        // line, "Try again" at the end. The row answers the
                        // keyboard's cursor only; the pointer has the button.
                        out.rows.push((r, i));
                        let s = row_state(i, e);
                        let rad = crate::theme::concentric(self.theme.radii().panel, VESSEL);
                        if self.theme.hc.is_some() {
                            self.plate(dc, Role::Row, s, State::Rest, r, rad)?;
                        }
                        self.glyph(dc, Glyph::Warning, NOTE_GLYPH, cx, cy, self.ink_of(Level::Secondary, s))?;
                        let mut right = end;
                        if let Some((l, bw, bh)) = self.layout_text(t("launch.action.retry"), T_TITLE) {
                            let w = bw + 2.0 * size::BUTTON_PAD_X;
                            let b = rect(end - w, cy - size::CONTROL_H / 2.0, end, cy + size::CONTROL_H / 2.0);
                            // On the row's cursor it answers on that (high
                            // contrast: the Highlight fill under it).
                            let (bs, ground) = self.button_states(e);
                            if self.theme.hc.is_some() {
                                self.plate(dc, Role::Raised, bs, ground, b, self.theme.radii().control)?;
                            }
                            self.draw_text(dc, &l, b.left + size::BUTTON_PAD_X, (cy - bh / 2.0).round(), rgba(self.theme.text_on(Level::Primary, bs, ground)))?;
                            out.retry = Some(b);
                            right = b.left - space::S4;
                        }
                        if let Some((l, _, th)) = self.fit_text(&hit.title, T_TITLE, right - TEXT_X) {
                            self.draw_text(dc, &l, TEXT_X, (cy - th / 2.0).round(), self.ink_of(Level::Primary, s))?;
                        }
                        continue;
                    }
                    out.rows.push((r, i));
                    let s = row_state(i, e);
                    if self.theme.hc.is_some() {
                        self.plate(dc, Role::Row, s, State::Rest, r, crate::theme::concentric(self.theme.radii().panel, VESSEL))?;
                    }
                    let (ink, dim) = (self.ink_of(Level::Primary, s), self.ink_of(Level::Secondary, s));
                    let ir = rect(cx - ICON / 2.0, cy - ICON / 2.0, cx + ICON / 2.0, cy + ICON / 2.0);
                    let loaded = hit.icon.as_ref().and_then(|k| icons.get(k).cloned().flatten());
                    match loaded {
                        Some(icon) => self.icon(dc, &icon, ir)?,
                        None => {
                            let g = match hit.kind {
                                Kind::Calc => G_CALC,
                                Kind::App => G_APP,
                                Kind::Setting => G_SETTINGS,
                                Kind::File => G_FILE,
                                Kind::Note => G_INFO,
                                Kind::Web => G_GLOBE,
                            };
                            self.glyph(dc, g, GLYPH, cx, cy, ink)?;
                        }
                    }
                    let hint = row_hint(hit.kind, selected, self.copy_failed).map(t);
                    let mut right = end;
                    if let Some((l, hw, hh)) = hint.and_then(|t| self.fit_text(t, T_SMALL, 200.0)) {
                        self.draw_text(dc, &l, right - hw, (cy - hh / 2.0).round(), dim)?;
                        right -= hw + space::S4;
                    }
                    let x = TEXT_X;
                    let maxw = right - x;
                    let st = if hit.kind == Kind::Calc { T_STRONG } else { T_TITLE };
                    if hit.detail.is_empty() {
                        if let Some((l, _, th)) = self.fit_text(&hit.title, st, maxw) {
                            self.draw_text(dc, &l, x, (cy - th / 2.0).round(), ink)?;
                        }
                    } else {
                        // The two lines as one block (the line heights are
                        // the gap), centered on the row.
                        let title = self.fit_text(&hit.title, st, maxw);
                        let detail = self.fit_text(&hit.detail, T_SMALL, maxw);
                        let (th, dh) = (title.as_ref().map_or(0.0, |l| l.2), detail.as_ref().map_or(0.0, |l| l.2));
                        let top = (cy - (th + dh) / 2.0).round();
                        if let Some((l, _, _)) = &title {
                            self.draw_text(dc, l, x, top, ink)?;
                        }
                        if let Some((l, _, _)) = &detail {
                            self.draw_text(dc, l, x, top + th, dim)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The field's text, selection, composition underline and caret
    /// (unmirrored DIPs).
    unsafe fn paint_field(&self, dc: &ID2D1DeviceContext) -> Result<()> {
        let mid = FIELD_H / 2.0;
        let Some((shown, comp_at, l, th)) = self.field_layout() else {
            let caret = self.text_x(0.0) - if self.rtl { crate::gfx::CARET_W } else { 0.0 };
            // The hint after the caret, not under it (states-issues: the
            // caret stood on the hint's first glyph): the caret's width and
            // a gap in, on the reading side.
            let room = self.field_x1() - FIELD_X0 - PLACEHOLDER_IN;
            if let Some((l, w, h)) = self.fit_text(t("search.placeholder"), T_FIELD, room) {
                let b = dc.CreateSolidColorBrush(&self.faint(), None)?;
                let x = self.text_x(w) + if self.rtl { -PLACEHOLDER_IN } else { PLACEHOLDER_IN };
                crate::gfx::draw_text(dc, Vector2 { X: x, Y: mid - h / 2.0 }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            if self.caret_on {
                let h = T_FIELD.line;
                crate::gfx::caret(dc, caret, mid - h / 2.0, mid + h / 2.0, &self.theme, 1.0)?;
            }
            return Ok(());
        };
        let x0 = self.text_x(Self::width_of(&l));
        let top = mid - th / 2.0;
        let pos = |i: usize| x0 + Self::x_at(&l, utf16_len(&shown[..i]));
        // Selection (only without a composition).
        let (a, b) = self.field.selection();
        if self.comp.is_empty() && a != b {
            // The selected fill under the selected text (the theme's:
            // Highlight under HighlightText in high contrast).
            self.fill(dc, rgba(self.theme.fill_of(Role::Row, State::Selected)), rect(pos(a).min(pos(b)), top, pos(a).max(pos(b)), top + th), 0.0)?;
            let (lo, hi) = (a.min(b), a.max(b));
            let over = dc.CreateSolidColorBrush(&self.ink_of(Level::Primary, State::Selected), None)?;
            let range = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE { startPosition: utf16_len(&shown[..lo]), length: utf16_len(&shown[lo..hi]) };
            let _ = l.SetDrawingEffect(&over, range);
        }
        let ink = dc.CreateSolidColorBrush(&self.ink(), None)?;
        crate::gfx::draw_text(dc, Vector2 { X: x0, Y: top }, &l, &ink, D2D1_DRAW_TEXT_OPTIONS_NONE);
        if !self.comp.is_empty() {
            let (cx0, cx1) = (pos(comp_at), pos(comp_at + self.comp.len()));
            self.fill(dc, self.ink(), rect(cx0.min(cx1), top + th - 1.0, cx0.max(cx1), top + th), 0.0)?;
        }
        if self.caret_on {
            crate::gfx::caret(dc, x0 + self.caret_x, top, top + th, &self.theme, 1.0)?;
        }
        Ok(())
    }

    /// Tells the IME where the caret is (its candidate list opens there).
    fn place_ime(&self) {
        let ds = self.scale;
        let m = (SHADOW_M * ds).ceil();
        let h = T_FIELD.line;
        let total = self.field_layout().map(|(_, _, l, _)| Self::width_of(&l)).unwrap_or(0.0);
        let x = (m + (self.text_x(total) + self.caret_x) * ds) as i32;
        let top = (m + (FIELD_H / 2.0 - h / 2.0) * ds) as i32;
        let bottom = (m + (FIELD_H / 2.0 + h / 2.0) * ds) as i32;
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.is_invalid() {
                return;
            }
            let cf = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x, y: top }, rcArea: RECT::default() };
            let _ = ImmSetCompositionWindow(himc, &cf);
            let cand = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_EXCLUDE,
                ptCurrentPos: POINT { x, y: bottom },
                rcArea: RECT { left: x, top, right: x + 1, bottom },
            };
            let _ = ImmSetCandidateWindow(himc, &cand);
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
    }

    // ---- input ----

    fn finish(&mut self, after: Option<After>) {
        self.done = true;
        self.after = after;
    }

    fn activate(&mut self, i: usize, ctrl: bool) {
        if self.stale() {
            // Never open a row of an older query.
            return;
        }
        let Some(hit) = self.hits.get(i).cloned() else { return };
        match hit.kind {
            Kind::Calc => {
                let hwnd = self.hwnd;
                if copy_result(&hit, |s| unsafe { copy_text(hwnd, s) }) {
                    self.finish(None);
                } else {
                    // Stay open and say so on the row.
                    crate::log!("search: could not copy the result");
                    self.copy_failed = true;
                    self.redraw();
                }
            }
            Kind::File if ctrl => self.finish(Some(After::Reveal(hit.target))),
            // The file search's note: Enter (and its button) searches
            // again; Ctrl+Enter goes on in Windows (File Explorer's
            // search, or Windows' search settings).
            Kind::Note if ctrl => {
                let other = match self.files_state {
                    FilesState::Done(FileSource::Recent) => "ms-settings:search-moredetails".to_string(),
                    _ => explorer_search(&self.query()),
                };
                self.finish(Some(After::Open(other)));
            }
            Kind::Note => self.retry_files(),
            _ => self.finish(Some(After::Open(hit.target))),
        }
    }

    /// "Try again": the index asked anew, or given longer.
    fn retry_files(&mut self) {
        match self.files_state {
            FilesState::Done(FileSource::Recent) => self.shared.retry_index.store(true, Ordering::SeqCst),
            _ => self.shared.patient.store(true, Ordering::SeqCst),
        }
        self.changed();
    }

    /// The keyboard moves the selection (its cursor): at once, the row
    /// scrolled into view; the pointer's fill goes (design system 6.2: one
    /// highlight, the later input wins).
    fn step(&mut self, dir: i32) {
        let v = self.visible();
        if v.is_empty() || self.stale() {
            return;
        }
        let at = self.sel.and_then(|s| v.iter().position(|&i| i == s));
        let next = match at {
            Some(k) => (k as i32 + dir).clamp(0, v.len() as i32 - 1) as usize,
            None => 0,
        };
        self.sel = Some(v[next]);
        self.by_key = true;
        self.hover = None;
        self.hot_retry = false;
        self.wash.clear();
        if let Some(row) = self.line_of(v[next]) {
            let to = reveal(self.list_off, row, self.height, self.most());
            self.scroll_list(to);
        }
        self.redraw();
    }

    /// The wheel scrolls a list that does not fit; what is under the
    /// pointer then is found again.
    fn on_wheel(&mut self, delta: i32, lp_screen: LPARAM) {
        let to = self.list_off - delta as f32 / WHEEL_DELTA as f32 * WHEEL_ROW;
        if !self.scroll_list(to) {
            return;
        }
        // (The wheel's point is the screen's.)
        let mut p = POINT { x: (lp_screen.0 & 0xFFFF) as i16 as i32, y: ((lp_screen.0 >> 16) & 0xFFFF) as i16 as i32 };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut p);
        }
        let at = self.dip(LPARAM(((p.y as u16 as isize) << 16) | (p.x as u16 as isize)));
        self.pressed = None;
        self.redraw();
        // (The list moved under a still pointer: it keeps the selection
        // only if it had it.)
        self.point_at(at, false);
        self.redraw();
    }

    fn on_key(&mut self, vk: u16) {
        if vk == VK_ESCAPE.0 {
            self.finish(None);
            return;
        }
        if vk == VK_PROCESSKEY.0 || !self.comp.is_empty() {
            // The IME has it.
            return;
        }
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let edited = match vk {
            v if v == VK_UP.0 => return self.step(-1),
            v if v == VK_DOWN.0 => return self.step(1),
            v if v == VK_TAB.0 => return self.step(if shift { -1 } else { 1 }),
            v if v == VK_RETURN.0 => {
                if self.stale() {
                    // Results for this text are on their way.
                    self.pending = Some(ctrl);
                } else if let Some(i) = self.sel {
                    self.activate(i, ctrl);
                }
                return;
            }
            // Arrows move the caret the way they point: over text that reads
            // right to left (right-to-left languages only), Left goes
            // forward.
            v if v == VK_LEFT.0 || v == VK_RIGHT.0 => {
                let forward = crate::i18n::mirror::arrow(self.rtl && crate::i18n::mirror::reads_rtl(&self.query()), v == VK_RIGHT.0) > 0;
                if forward {
                    self.field.right(ctrl, shift);
                } else {
                    self.field.left(ctrl, shift);
                }
                false
            }
            v if v == VK_HOME.0 => {
                self.field.home(shift);
                false
            }
            v if v == VK_END.0 => {
                self.field.end(shift);
                false
            }
            v if v == VK_BACK.0 => {
                self.field.backspace(ctrl);
                true
            }
            v if v == VK_DELETE.0 => {
                self.field.delete(ctrl);
                true
            }
            v if ctrl && v == b'A' as u16 => {
                self.field.select_all();
                false
            }
            v if ctrl && v == b'V' as u16 => match unsafe { clipboard_text(self.hwnd) } {
                Some(s) if !s.is_empty() => {
                    self.field.insert(&s);
                    true
                }
                _ => return,
            },
            _ => return,
        };
        if edited {
            self.changed();
        } else {
            self.caret_on = true;
            self.redraw();
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
        // Control characters come from Ctrl+key and Backspace: handled as keys.
        if (c as u32) < 0x20 || c == '\u{7F}' {
            return;
        }
        self.field.insert(&c.to_string());
        self.changed();
    }

    fn ime_string(&self, himc: windows::Win32::UI::Input::Ime::HIMC, what: IME_COMPOSITION_STRING) -> String {
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
        if flags & GCS_RESULTSTR.0 != 0 {
            let s = self.ime_string(himc, GCS_RESULTSTR);
            self.comp.clear();
            self.field.insert(&s);
        }
        self.comp = if flags & GCS_COMPSTR.0 != 0 { self.ime_string(himc, GCS_COMPSTR).chars().collect() } else { Vec::new() };
        // The IME's own caret inside the composition (UTF-16 offset);
        // at its end when the IME does not say.
        let pos = unsafe { ImmGetCompositionStringW(himc, GCS_CURSORPOS, None, 0) };
        self.comp_cursor = if pos >= 0 { char_index(&self.comp, pos as u32) } else { self.comp.len() };
        unsafe {
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
        self.update_ime_ui();
        self.changed();
    }

    /// While the IME shows a composition or candidates, the mouse is let
    /// go so clicks reach its candidate window (an outside click on another
    /// app still closes the panel: it loses activation); taken back after.
    fn update_ime_ui(&mut self) {
        let want = !self.comp.is_empty() || self.candidates;
        if want == self.ime_ui {
            return;
        }
        self.ime_ui = want;
        unsafe {
            if want {
                let _ = ReleaseCapture();
            } else {
                SetCapture(self.hwnd);
            }
        }
    }

    /// Panel DIP of a client point (lParam).
    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let m = (SHADOW_M * self.scale).ceil();
        let x = (lp.0 & 0xFFFF) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
        // Right to left: back into the (unmirrored) layout's space.
        (crate::i18n::mirror::x(self.rtl, (x - m) / self.scale, 0.0, self.w), (y - m) / self.scale)
    }

    /// The result drawn under (x, y): never one scrolled under the field,
    /// never the note (only its button answers the pointer).
    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        if y < FIELD_H {
            return None;
        }
        self.painted.rows.iter().find(|(r, _)| inside(r, x, y)).map(|(_, i)| *i).filter(|&i| self.hits.get(i).is_some_and(|h| h.kind != Kind::Note))
    }

    /// The note's button is under (x, y).
    fn retry_at(&self, x: f32, y: f32) -> bool {
        y >= FIELD_H && self.painted.retry.is_some_and(|r| inside(&r, x, y))
    }

    /// The pointer is at (x, y): what it is over (rows of an older query
    /// answer nothing). `moved`: the pointer itself moved (or pressed):
    /// what it is on becomes the selection (design system 6.2: the later
    /// input wins; the keyboard's cursor goes). Else the list moved under
    /// it, and only a selection the pointer already had follows. Over
    /// nothing (the field, a heading) the selection stays. True when
    /// something changed.
    fn point_at(&mut self, (x, y): (f32, f32), moved: bool) -> bool {
        let live = !self.stale();
        let hover = self.row_at(x, y).filter(|_| live);
        let hot = live && self.retry_at(x, y);
        let mut changed = hover != self.hover || hot != self.hot_retry;
        self.hover = hover;
        self.hot_retry = hot;
        let on = hover.or(self.note().filter(|_| hot));
        if let Some(i) = on.filter(|_| moved || !self.by_key) {
            changed |= self.by_key || self.sel != Some(i);
            self.by_key = false;
            self.sel = Some(i);
        }
        changed
    }

    fn on_move(&mut self, lp: LPARAM) {
        // Only a real move counts (not the list moving under a still
        // pointer).
        let raw = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
        if raw == self.last_mouse {
            return;
        }
        let first = self.last_mouse == (i32::MIN, i32::MIN);
        self.last_mouse = raw;
        if first {
            return;
        }
        // The row under the pointer fills and is the selection (Enter opens
        // what shows).
        let at = self.dip(lp);
        if self.point_at(at, true) {
            self.redraw();
        }
    }

    fn on_button(&mut self, lp: LPARAM, down: bool, left: bool) {
        let mut p = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut p);
        }
        let (x, y) = self.dip(lp);
        if !down {
            // It runs only when let go on what the press began on.
            let Some(pressed) = self.pressed.take() else { return };
            let on = match pressed {
                Press::Row(i) => self.row_at(x, y) == Some(i),
                Press::Retry => self.retry_at(x, y),
            };
            if left && on && !self.stale() {
                match pressed {
                    Press::Row(i) => {
                        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                        self.activate(i, ctrl);
                    }
                    Press::Retry => self.retry_files(),
                }
            } else {
                self.redraw();
            }
            return;
        }
        if !contains(&self.rect, p) {
            // On an IME's candidate window: not ours to handle or close on.
            if unsafe { ime_window(p) } {
                return;
            }
            // Outside (the top bar's button included): close, click eaten.
            self.finish(None);
            return;
        }
        if !left {
            return;
        }
        if y < FIELD_H && self.comp.is_empty() {
            // Caret where the text was clicked.
            if let Some((shown, _, l, _)) = self.field_layout() {
                let mut trailing = BOOL(0);
                let mut is_inside = BOOL(0);
                let mut m = DWRITE_HIT_TEST_METRICS::default();
                // The field is drawn unmirrored: the pointer's real x.
                let px = crate::i18n::mirror::x(self.rtl, x, 0.0, self.w);
                let total = Self::width_of(&l);
                unsafe {
                    let _ = l.HitTestPoint(px - self.text_x(total), 4.0, &mut trailing, &mut is_inside, &mut m);
                }
                let pos = m.textPosition + if trailing.as_bool() { m.length } else { 0 };
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                self.field.click(char_index(&shown, pos), shift);
                self.caret_on = true;
                self.redraw();
            }
            return;
        }
        // Rows of an older query cannot be picked.
        self.point_at((x, y), true);
        self.pressed = if self.hot_retry { Some(Press::Retry) } else { self.hover.map(Press::Row) };
        if self.pressed.is_some() {
            self.redraw();
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Fading out after it closed: nothing reaches it (it is not the open
    // panel; a new one may be).
    if crate::motion::retired(hwnd) {
        return if msg == WM_NCHITTEST { LRESULT(HTTRANSPARENT as isize) } else { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    match msg {
        WM_MOUSEMOVE => {
            with_panel(|p| p.on_move(lp));
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            with_panel(|p| p.on_button(lp, true, msg == WM_LBUTTONDOWN));
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            with_panel(|p| p.on_button(lp, false, msg == WM_LBUTTONUP));
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            with_panel(|p| p.on_wheel(((wp.0 >> 16) & 0xFFFF) as i16 as i32, lp));
            LRESULT(0)
        }
        WM_KEYDOWN => {
            with_panel(|p| p.on_key(wp.0 as u16));
            LRESULT(0)
        }
        WM_CHAR => {
            with_panel(|p| p.on_char(wp.0 as u16));
            LRESULT(0)
        }
        // Alt+letter: no menu here (and no beep). Letting go of Alt (still
        // held from Alt+Space) must not start menu mode either.
        WM_SYSCHAR => LRESULT(0),
        WM_SYSCOMMAND if (wp.0 & 0xFFF0) as u32 == SC_KEYMENU => LRESULT(0),
        WM_IME_SETCONTEXT => {
            // We draw the composition ourselves; the candidate list stays.
            let lp = LPARAM(lp.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize));
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_IME_STARTCOMPOSITION => {
            with_panel(|p| p.place_ime());
            LRESULT(0)
        }
        WM_IME_COMPOSITION => {
            with_panel(|p| p.on_composition(lp.0 as u32));
            LRESULT(0)
        }
        WM_IME_ENDCOMPOSITION => {
            with_panel(|p| {
                if !p.comp.is_empty() {
                    p.comp.clear();
                    p.update_ime_ui();
                    p.changed();
                }
            });
            LRESULT(0)
        }
        WM_IME_NOTIFY => {
            // IMN_OPENCANDIDATE / IMN_CLOSECANDIDATE.
            if wp.0 == 5 || wp.0 == 4 {
                with_panel(|p| {
                    p.candidates = wp.0 == 5;
                    p.update_ime_ui();
                });
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER if wp.0 == WAIT_TIMER => {
            let _ = KillTimer(Some(hwnd), WAIT_TIMER);
            with_panel(|p| p.wait_over());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CARET_TIMER => {
            with_panel(|p| {
                p.caret_on = !p.caret_on;
                // (NopBar's own "small animations" has no message: asked
                // on the caret's tick.)
                if p.ring_on {
                    p.follow_motion();
                }
                p.redraw();
            });
            LRESULT(0)
        }
        WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETCLIENTAREAANIMATION.0 => {
            // Windows' animations turned off while it is open: the ring
            // stops.
            with_panel(|p| p.follow_motion());
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SEARCH_REMOTE => {
            with_panel(|p| p.sync(wp.0 == 1));
            LRESULT(0)
        }
        WM_CLOSE => {
            with_panel(|p| p.finish(None));
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            with_panel(|p| p.done = true);
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            let now = GetCapture();
            with_panel(|p| {
                // Let go on purpose while the IME has the mouse.
                if now != p.hwnd && !p.ime_ui {
                    p.done = true;
                }
            });
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_GETOBJECT => {
            let answer = a11y::on_getobject(hwnd, wp, lp, || {
                with_panel(|p| p.publish());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == a11y::wm_action() => {
            for (key, act) in a11y::take(hwnd) {
                with_panel(|p| p.asked(&key, act));
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            a11y::forget(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    /// The caret of an empty field stands before its hint, never on it.
    #[test]
    fn the_caret_stands_before_the_hint() {
        assert!(PLACEHOLDER_IN >= crate::gfx::CARET_W + 1.0, "a gap between the caret and the hint");
        let src = include_str!("mod.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(code.contains("let x = self.text_x(w) + if self.rtl { -PLACEHOLDER_IN } else { PLACEHOLDER_IN };"));
    }

    /// Search has no focus ring: its selection (the first result while
    /// typing: Enter opens it, the search panel's) is the list's own. The arrows
    /// move it; a media or volume key does not (crate::input): on_key
    /// passes them by.
    #[test]
    fn media_keys_move_nothing_in_search() {
        let src = include_str!("mod.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(!code.contains("crate::gfx::focus_ring(") && !code.contains("parts::focus_ring("), "no ring");
        assert_eq!(code.matches("self.by_key = true;").count(), 3, "results changed, the arrows, a screen reader's Select");
    }

    use super::*;

    fn hit(kind: Kind, title: &str) -> Hit {
        Hit { kind, title: title.into(), detail: String::new(), target: title.into(), icon: None, score: 0 }
    }

    /// A panel as `show` makes it (its window never shown), off every
    /// display, with workers that do nothing, `quick` and `files` its
    /// answers for its text.
    fn test_panel(quick: &[Hit], files: &[Hit], state: FilesState) -> (std::rc::Rc<crate::gfx::promo_kit::Stage>, Panel) {
        test_panel_in(quick, files, state, 1080.0)
    }

    fn test_panel_in(quick: &[Hit], files: &[Hit], state: FilesState, room: f32) -> (std::rc::Rc<crate::gfx::promo_kit::Stage>, Panel) {
        let stage = crate::gfx::promo_kit::stage();
        let theme = Theme::graphite().resolve(&crate::syslook::SysLook::default());
        let idle = |_: &Arc<Shared>| Workers { quick: channel().0, files: channel().0, icons: channel().0 };
        unsafe {
            register_class();
            let s = 1.5;
            let side = |dip: f32| -32000 + (dip * s) as i32;
            let work = RECT { left: -32000, top: -32000, right: side(1920.0), bottom: side(room) };
            let mut p = Panel::make(&stage.gfx, &theme, s, work, idle).expect("the panel");
            p.rtl = false;
            if !quick.is_empty() || !files.is_empty() {
                p.field.insert("q");
                (p.seq, p.quick_seq) = (1, 1);
            }
            p.quick = quick.to_vec();
            p.files = files.to_vec();
            p.files_state = state;
            p.merge(false);
            p.redraw();
            (stage, p)
        }
    }

    impl Panel {
        /// Its workers told to end, its window gone.
        fn close_for_test(self) {
            self.workers.quit(&self.shared);
            unsafe {
                let _ = KillTimer(Some(self.hwnd), WAIT_TIMER);
                let _ = KillTimer(Some(self.hwnd), CARET_TIMER);
                let _ = DestroyWindow(self.hwnd);
            }
        }

        /// A WM_MOUSEMOVE's lParam for (x, y) DIP in the panel.
        fn lp_at(&self, x: f32, y: f32) -> LPARAM {
            let m = (SHADOW_M * self.scale).ceil();
            let (px, py) = ((x * self.scale + m).round() as i32, (y * self.scale + m).round() as i32);
            LPARAM((((py as u16) as isize) << 16) | ((px as u16) as isize))
        }

        /// ... in the middle of hit `i`'s row as drawn.
        fn lp_on_row(&self, i: usize) -> LPARAM {
            let (r, _) = self.painted.rows.iter().find(|(_, k)| *k == i).copied().expect("drawn");
            self.lp_at((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0)
        }
    }

    #[test]
    fn lines_group_and_every_hit_has_its_row() {
        let kinds = [Kind::App, Kind::App, Kind::Setting, Kind::Web];
        let (lines, h) = layout(&kinds, false);
        assert_eq!(
            lines.iter().map(|l| l.0).collect::<Vec<_>>(),
            vec![Line::Head(Kind::App), Line::Item(0), Line::Item(1), Line::Head(Kind::Setting), Line::Item(2), Line::Item(3)]
        );
        // No line under the field (design system 2.5): the list starts
        // LIST_TOP under it.
        assert_eq!(h, FIELD_H + LIST_TOP + HEAD_H * 2.0 + ROW_H * 4.0 + WEB_GAP + PAD_BOTTOM);
        assert_eq!(lines[1].1, FIELD_H + LIST_TOP + HEAD_H);
        // Nothing typed: just the field.
        assert_eq!(layout(&[], false), (Vec::new(), FIELD_H));
        // Rows by what they hold (3.3): two lines 48, the note's line with
        // its button 32; headings 24.
        assert_eq!((ROW_H, NOTE_H, HEAD_H), (48.0, 32.0, 24.0));
        let (lines, _) = layout(&[Kind::File, Kind::Note, Kind::Web], false);
        assert_eq!(lines.iter().map(|l| l.2).collect::<Vec<_>>(), vec![HEAD_H, ROW_H, NOTE_H, ROW_H]);
        // A long list keeps all its rows (it scrolls: list_view).
        let kinds = [Kind::App, Kind::App, Kind::App, Kind::File, Kind::File, Kind::Web];
        let (lines, content) = layout(&kinds, false);
        assert_eq!(lines.iter().filter(|l| matches!(l.0, Line::Item(_))).count(), 6);
        let (view, most) = list_view(content, 300.0);
        assert_eq!((view, most), (300.0, content - 300.0));
        assert_eq!(list_view(content, 10_000.0), (content, 0.0));
        assert_eq!(list_view(content, 10.0).0, FIELD_H, "never shorter than the field");
    }

    #[test]
    fn nothing_found_is_a_block_over_the_web_row() {
        // Only the web row, and nothing more is coming.
        let web = [Kind::Web];
        assert!(nothing_found(false, false, FilesState::Done(FileSource::Index), &web));
        // Still waiting (the quick results, or the files): not yet.
        assert!(!nothing_found(false, true, FilesState::Done(FileSource::Index), &web));
        assert!(!nothing_found(false, false, FilesState::Waiting, &web));
        // Something was found; nothing typed.
        assert!(!nothing_found(false, false, FilesState::Done(FileSource::Index), &[Kind::App, Kind::Web]));
        assert!(!nothing_found(true, false, FilesState::Idle, &[]));
        // The file search's note is something to show.
        assert!(!nothing_found(false, false, FilesState::TimedOut(FileSource::Index), &[Kind::Note, Kind::Web]));
        // The block (QUIET_MIN_H), the web row right under it.
        let (lines, h) = layout(&web, true);
        assert_eq!(lines, vec![(Line::Quiet, FIELD_H + LIST_TOP, size::QUIET_MIN_H), (Line::Item(0), FIELD_H + LIST_TOP + QUIET_H, ROW_H)]);
        assert_eq!(h, FIELD_H + LIST_TOP + QUIET_H + ROW_H + PAD_BOTTOM);
    }

    #[test]
    fn the_ring_waits_for_what_is_late() {
        // Something for the text is on its way: the ring is armed (it
        // shows after WAIT_MS: nothing for what ends within 300 ms).
        assert!(waiting(false, true, FilesState::Done(FileSource::Index)), "the quick results");
        assert!(waiting(false, false, FilesState::Waiting), "the files");
        assert!(!waiting(false, false, FilesState::Done(FileSource::Recent)));
        assert!(!waiting(false, false, FilesState::TimedOut(FileSource::Index)));
        assert!(!waiting(true, true, FilesState::Waiting), "nothing typed: nothing asked");
        assert_eq!(WAIT_MS as i64, crate::motion::TIP_DELAY_MS);
    }

    #[test]
    fn the_keyboard_keeps_its_row_in_view() {
        let view = 400.0;
        let most = 600.0;
        // In view: the list stays.
        assert_eq!(reveal(0.0, (200.0, ROW_H), view, most), 0.0);
        // Below the panel's foot: up until it is whole, the foot's pad kept.
        assert_eq!(reveal(0.0, (380.0, ROW_H), view, most), 380.0 + ROW_H + PAD_BOTTOM - view);
        // Under the field: down until it is clear of the fading band.
        let off = reveal(300.0, (320.0, ROW_H), view, most);
        assert_eq!(320.0 - off, FIELD_H + size::SCROLL_FADE);
        // The first rows bring their heading back.
        assert_eq!(reveal(300.0, (FIELD_H + LIST_TOP + HEAD_H, ROW_H), view, most), 0.0);
        // Never past the ends.
        assert_eq!(reveal(0.0, (2000.0, ROW_H), view, most), most);
    }

    /// Design system 6.2: one emphasis in a list, the later input wins,
    /// and a list whose selection stays has it follow the cursor. (The
    /// panels' review, 2026-11-11, P2: the keyboard's selection and the
    /// pointer's fill showed on two rows at once, and Enter opened another
    /// row than a click.)
    #[test]
    fn one_emphasis_and_it_is_what_enter_opens() {
        let note = Some(5);
        let none = Emphasis::default();
        // The keyboard moved last: its selection, the selected fill.
        assert_eq!(emphasis(Some(2), true, None, false, note), Emphasis { mark: Some(2), ..none });
        assert_eq!(emphasis(Some(5), true, None, false, note), Emphasis { mark: Some(5), ..none }, "the note answers the keyboard");
        // ... even with the pointer resting on the note's button.
        assert_eq!(emphasis(Some(2), true, None, true, note), Emphasis { mark: Some(2), ..none });
        // The pointer moved last: the row it chose (the selection) under
        // its fill; it stays when the pointer goes on over nothing.
        assert_eq!(emphasis(Some(3), false, None, false, note), Emphasis { lit: Some((3, false)), ..none });
        // A press: there, at once, alone.
        assert_eq!(emphasis(Some(3), false, Some(Press::Row(3)), false, note), Emphasis { lit: Some((3, true)), ..none });
        // The pointer on the note's button: the button, not its row.
        assert_eq!(emphasis(Some(5), false, None, true, note), Emphasis { button: true, ..none });
        assert_eq!(emphasis(Some(5), false, Some(Press::Retry), true, note), Emphasis { button: true, ..none });
        assert_eq!(emphasis(Some(5), false, None, false, note), none, "the note's row has no pointer fill");
        // Rows of an older query: none.
        assert_eq!(emphasis(None, true, None, false, None), none);
        // Never two rows at once, whatever the inputs.
        for sel in [None, Some(0), Some(3), Some(5)] {
            for by_key in [false, true] {
                for pressed in [None, Some(Press::Row(3)), Some(Press::Retry)] {
                    for on_button in [false, true] {
                        let e = emphasis(sel, by_key, pressed, on_button, note);
                        let lit = [e.mark.is_some(), e.lit.is_some(), e.button].iter().filter(|&&b| b).count();
                        assert!(lit <= 1, "{sel:?} {by_key} {pressed:?} {on_button}: {e:?}");
                        // What shows is what Enter opens (a press: where
                        // the pointer chose).
                        if let (Some(i), None) = (e.mark.or(e.lit.map(|l| l.0)), pressed) {
                            assert_eq!(Some(i), sel);
                        }
                    }
                }
            }
        }
        // A row's state (what high contrast draws from).
        let key = Emphasis { mark: Some(2), ..none };
        assert_eq!((row_state(2, key), row_state(3, key)), (State::Cursor, State::Rest));
        let ptr = Emphasis { lit: Some((3, false)), ..none };
        assert_eq!((row_state(3, ptr), row_state(2, ptr)), (State::Hover, State::Rest));
        assert_eq!(row_state(3, Emphasis { lit: Some((3, true)), ..none }), State::Pressed);
    }

    /// The panel itself, in the order events come: arrows, the pointer
    /// moving onto another row and over the field, Enter, the wheel, new
    /// results.
    #[test]
    fn the_later_input_has_the_one_emphasis() {
        let (stage, mut p) = test_panel(&[hit(Kind::App, "a"), hit(Kind::App, "b"), hit(Kind::App, "c"), hit(Kind::Web, "w")], &[], FilesState::Done(FileSource::Index));
        let shows = |p: &Panel| p.emphasis();
        // A new list: the top row, the keyboard's.
        assert_eq!((p.sel, p.by_key), (Some(0), true));
        assert_eq!(shows(&p), Emphasis { mark: Some(0), ..Default::default() });
        p.step(1);
        assert_eq!(shows(&p), Emphasis { mark: Some(1), ..Default::default() });
        // The pointer moves onto row 2: the keyboard's cursor goes, the
        // selection is the pointer's row.
        p.on_move(p.lp_at(-1.0, -1.0));
        p.on_move(p.lp_on_row(2));
        assert_eq!((p.sel, p.by_key), (Some(2), false));
        assert_eq!(shows(&p), Emphasis { lit: Some((2, false)), ..Default::default() });
        // Over the field: the selection stays, still showing.
        p.on_move(p.lp_at(100.0, FIELD_H / 2.0));
        assert_eq!(shows(&p), Emphasis { lit: Some((2, false)), ..Default::default() });
        // A key: the keyboard's again, from where the selection is.
        p.on_move(p.lp_on_row(1));
        p.step(1);
        assert_eq!((p.sel, shows(&p)), (Some(2), Emphasis { mark: Some(2), ..Default::default() }));
        // The pointer still on row 1, the list not moving under it: the
        // keyboard keeps it (only a real move takes over).
        p.on_move(p.lp_on_row(1));
        assert_eq!(shows(&p), Emphasis { mark: Some(2), ..Default::default() });
        // Enter opens what shows.
        p.on_move(p.lp_at(30.0, 30.0));
        p.on_move(p.lp_on_row(3));
        assert_eq!(shows(&p).lit, Some((3, false)));
        p.on_key(VK_RETURN.0);
        assert_eq!(p.after, Some(After::Open("w".into())), "the row that shows");
        // Results came again under the pointer: the selection kept, as the
        // keyboard's (no pointer fill until it moves).
        p.on_move(p.lp_on_row(1));
        assert!(!p.by_key);
        p.merge(true);
        p.redraw();
        assert_eq!((p.sel, shows(&p)), (Some(1), Emphasis { mark: Some(1), ..Default::default() }));
        p.close_for_test();
        drop(stage);
    }

    /// The panels' review (2026-11-11, P3): every answer that came while
    /// the ring was not yet up set its timer again, so quick answers kept
    /// pushing it back. The wait is armed once, when it begins.
    #[test]
    fn the_ring_is_due_from_when_the_wait_began() {
        assert_eq!(wait_step(true, false, false), Wait::Arm);
        assert_eq!(wait_step(true, true, false), Wait::Keep, "armed: kept");
        assert_eq!(wait_step(true, false, true), Wait::Keep, "up: kept");
        for (armed, ring) in [(false, false), (true, false), (false, true)] {
            assert_eq!(wait_step(false, armed, ring), Wait::Over);
        }
        let (_stage, mut p) = test_panel(&[], &[], FilesState::Idle);
        assert_eq!(p.wait_due, None);
        // Typing: the files are asked for: a wait begins.
        p.field.insert("보고");
        p.changed();
        let due = p.wait_due.expect("armed");
        // Answers keep coming (the quick ones, the icons), more keys: the
        // files are still late; the ring stays due when it was.
        std::thread::sleep(std::time::Duration::from_millis(20));
        *p.shared.quick.lock().unwrap() = Some((p.seq, vec![hit(Kind::App, "보고서")]));
        p.sync(false);
        assert_eq!(p.wait_due, Some(due), "an answer");
        p.sync(false);
        p.field.insert("서");
        p.changed();
        assert_eq!(p.wait_due, Some(due), "a key");
        assert!(!p.ring_on);
        // The timer: the ring comes on; nothing is armed any more.
        p.wait_over();
        assert!(p.ring_on && p.wait_due.is_none());
        p.sync(false);
        assert!(p.wait_due.is_none(), "up: not armed again");
        // Everything answers: the ring goes, at once.
        *p.shared.quick.lock().unwrap() = Some((p.seq, vec![hit(Kind::App, "보고서")]));
        *p.shared.files.lock().unwrap() = Some((p.seq, Files { hits: Vec::new(), timed_out: false, from: FileSource::Index }));
        p.sync(false);
        assert!(!p.ring_on && p.wait_due.is_none());
        // Cleared: a wait that ends before its time leaves nothing armed.
        p.field.insert("x");
        p.changed();
        assert!(p.wait_due.is_some());
        p.field.select_all();
        p.field.insert("");
        p.changed();
        assert!(p.wait_due.is_none() && !p.ring_on);
        p.close_for_test();
    }

    /// The panels' review (2026-11-11, P2): the fills under the picture
    /// (selection, pointer, the note's button) did not fade under the field
    /// with the rows' picture. They are seen through the same band now, on
    /// when the list is scrolled.
    #[test]
    fn the_fills_fade_with_the_rows() {
        assert!(!banded(0.0, false));
        assert!(banded(12.0, false));
        assert!(!banded(12.0, true), "high contrast: the picture's line, no band");
        let many: Vec<Hit> = (1..=9).map(|k| hit(Kind::File, &format!("f{k}"))).collect();
        let apps = [hit(Kind::App, "a"), hit(Kind::App, "b"), hit(Kind::Setting, "c"), hit(Kind::Web, "w")];
        let (_stage, mut p) = test_panel_in(&apps, &many, FilesState::Done(FileSource::Index), 400.0);
        assert!(p.most() > 0.0, "it scrolls");
        let edges = p.edges.as_ref().expect("the fills are seen through the band");
        assert!(edges.stage.Parent().is_ok(), "in the plate");
        assert!(!p.banded);
        // The rows once per slice (three: never a second, masked copy
        // under a band), the bottom band always off (the confirm review:
        // a band that is off shows them once, as they are).
        assert_eq!(p.edges.as_ref().unwrap().probe(), ([1.0, 1.0], 3));
        p.scroll_list(40.0);
        p.redraw();
        assert!(p.banded, "scrolled: the band");
        assert_eq!(p.edges.as_ref().unwrap().probe(), ([0.0, 1.0], 3), "the top band on (its cover gone), the bottom one off");
        p.scroll_list(0.0);
        p.redraw();
        assert!(!p.banded, "at the top: none");
        assert_eq!(p.edges.as_ref().unwrap().probe(), ([1.0, 1.0], 3));
        p.close_for_test();
    }

    #[test]
    fn the_panel_stands_on_two_lines() {
        // The rows on the container line, what is read on the content
        // line; text after the 28 slot and 12; the field's on the same.
        assert_eq!((VESSEL, LINE, TEXT_X, FIELD_X0), (8.0, 20.0, 60.0, 60.0));
        assert_eq!((GLYPH, NOTE_GLYPH, ICON), (24.0, 16.0, 28.0));
        // The type is the ramp's (the field: Title, 20 / 600).
        assert_eq!((T_FIELD.size, T_FIELD.weight), (20.0, ramp::SEMIBOLD));
        for st in [T_FIELD, T_TITLE, T_STRONG, T_SMALL] {
            assert!(ramp::RAMP.contains(&st));
        }
        // Nothing here picks a color or branches on high contrast itself.
        let src = include_str!("mod.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for gone in ["fill_row", "text_primary", "text_secondary", "text_tertiary", ".hc()", "fade_visual", "hairline", "DWRITE_FONT_WEIGHT"] {
            assert!(!code.contains(gone), "{gone} is still in search/mod.rs");
        }
        for used in ["fill_on(", "outline_on(", "text_of(", "text_on(", "crate::gfx::scroll_fade(", "crate::gfx::caret(", "crate::motion::open("] {
            assert!(code.contains(used), "{used}");
        }
    }

    #[test]
    fn index_errors_after_the_wait_are_timeouts() {
        assert!(index_timed_out(Duration::from_millis(1950), 2));
        assert!(!index_timed_out(Duration::from_millis(200), 2), "a quick error: the index is unusable");
        assert!(index_timed_out(Duration::from_millis(6000), 6));
        assert!(!index_timed_out(Duration::from_millis(2100), 6), "given 6 s, a 2 s error is not a timeout");
    }

    #[test]
    fn app_list_states() {
        use AppsState::*;
        // First reading.
        assert_eq!(next_apps_state(Some((12, true)), Loading), (Loaded, true));
        assert_eq!(next_apps_state(Some((0, true)), Loading), (Empty, false), "read to the end: really none");
        assert_eq!(next_apps_state(Some((0, false)), Loading), (Failed, false), "failed before the first app: not empty");
        assert_eq!(next_apps_state(Some((5, false)), Loading), (Partial, true), "stopped part way: not a full list");
        assert_eq!(next_apps_state(None, Loading), (Failed, false));
        // A full list from before is kept over a broken reading.
        assert_eq!(next_apps_state(Some((5, false)), Loaded), (Loaded, false));
        assert_eq!(next_apps_state(None, Loaded), (Loaded, false));
        // A partial one is completed by a full reading.
        assert_eq!(next_apps_state(Some((40, true)), Partial), (Loaded, true));
        assert_eq!(next_apps_state(None, Partial), (Partial, false));
    }

    #[test]
    fn the_file_note_joins_the_files_group() {
        // Files, then the note: one heading.
        let (lines, _) = layout(&[Kind::App, Kind::File, Kind::Note, Kind::Web], false);
        let heads: Vec<Line> = lines.iter().filter(|l| matches!(l.0, Line::Head(_))).map(|l| l.0).collect();
        assert_eq!(heads, vec![Line::Head(Kind::App), Line::Head(Kind::File)]);
        // No files: the note brings the Files heading.
        let (lines, _) = layout(&[Kind::Note, Kind::Web], false);
        assert_eq!(lines[0].0, Line::Head(Kind::File));
        assert_eq!(lines[1].0, Line::Item(0));
    }

    #[test]
    fn file_search_states_are_told_apart() {
        assert!(files_note(FilesState::Idle).is_none());
        assert!(files_note(FilesState::Waiting).is_none(), "a search under way says nothing: the ring does");
        assert!(files_note(FilesState::Done(FileSource::Index)).is_none());
        let late = files_note(FilesState::TimedOut(FileSource::Index)).unwrap();
        assert_eq!((late.kind, late.target.as_str()), (Kind::Note, "retry"));
        let recent = files_note(FilesState::Done(FileSource::Recent)).unwrap();
        assert_eq!(recent.target, "retry");
        assert!(late.detail.is_empty() && recent.detail.is_empty(), "one line: no explanations");
        assert_ne!(recent.title, late.title, "recent-only and too slow read differently");
        // Notes sort after the files, before the web row.
        assert!(Kind::File < Kind::Note && Kind::Note < Kind::Web);
    }

    #[test]
    fn explorer_search_links_are_encoded() {
        assert_eq!(explorer_search("report 2026"), "search-ms:query=report%202026");
        assert_eq!(explorer_search("a&b"), "search-ms:query=a%26b");
        assert_eq!(explorer_search("한"), "search-ms:query=%ED%95%9C");
    }

    #[test]
    fn files_go_before_the_web_row() {
        let quick = vec![hit(Kind::App, "a"), hit(Kind::Setting, "s"), hit(Kind::Web, "w")];
        let files: Vec<Hit> = (0..9).map(|i| hit(Kind::File, &format!("f{i}"))).collect();
        let m = merge(&quick, &files);
        let kinds: Vec<Kind> = m.iter().map(|h| h.kind).collect();
        assert_eq!(kinds, vec![Kind::App, Kind::Setting, Kind::File, Kind::File, Kind::File, Kind::File, Kind::File, Kind::Web]);
        assert_eq!(m[2].title, "f0");
    }

    #[test]
    fn panel_sits_a_quarter_down() {
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1040 };
        let r = place(work, 1.0, panel_width(work, 1.0), 100.0);
        assert_eq!((r.left, r.top, r.right - r.left, r.bottom - r.top), (640, 260, 640, 100));
        let wide = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
        let r = place(wide, 1.5, panel_width(wide, 1.5), 100.0);
        assert_eq!((r.left, r.right - r.left), (1920 + 480, 960));
        // EDGE_GAP (8) off the work area's edges.
        assert_eq!(max_height(work, 1.0), 1040.0 - 260.0 - 8.0);
        // Portrait 1080 px at 200 %: narrower than W, and inside.
        let tall = RECT { left: 0, top: 0, right: 1080, bottom: 1920 };
        let w = panel_width(tall, 2.0);
        assert_eq!(w, 540.0 - 16.0);
        let r = place(tall, 2.0, w, 100.0);
        assert!(r.left >= tall.left && r.right <= tall.right, "{r:?}");
    }

    #[test]
    fn ime_windows_by_class() {
        assert!(is_ime_class("IME"));
        assert!(is_ime_class("MSCTFIME UI"));
        assert!(is_ime_class("Microsoft.IME.UIManager.CandidateWindow.Host"));
        assert!(!is_ime_class("NopBarTopBar"));
        assert!(!is_ime_class("Chrome_WidgetWin_1"));
    }

    #[test]
    fn pastes_never_split_a_pair() {
        assert_eq!(paste_end(&[0x61, 0x62, 0, 0x63]), 2);
        assert_eq!(paste_end(&[0x61, 0xD83D]), 1);
        assert_eq!(paste_end(&[0x61, 0xD83D, 0xDE00]), 3);
        assert_eq!(paste_end(&[]), 0);
    }

    #[test]
    fn utf16_positions() {
        let cs: Vec<char> = "a😀b".chars().collect();
        assert_eq!(utf16_len(&cs), 4);
        assert_eq!(char_index(&cs, 1), 1);
        assert_eq!(char_index(&cs, 3), 2);
        assert_eq!(char_index(&cs, 9), 3);
    }

    #[test]
    fn the_calculator_row_copies_its_plain_result() {
        let _l = crate::i18n::test_lock("ko");
        let copied = |text: &str| {
            let hits = sources::quick(text, &[]);
            let row = hits.iter().find(|h| h.kind == Kind::Calc)?;
            let mut got = None;
            assert!(copy_result(row, |s| {
                got = Some(s.to_string());
                true
            }));
            got
        };
        assert_eq!(copied("2*21").as_deref(), Some("42"));
        assert_eq!(copied("5 km to mi").as_deref(), Some("3.106855961"), "the number, not \"mi\"");
        assert_eq!(copied("1/3").as_deref(), Some("0.333333333333"));
        assert_eq!(copied("2026-12-25 - 2026-11-08").as_deref(), Some("47"), "not \"47일\"");
        assert_eq!(copied("2026-12-25 + 1 day").as_deref(), Some("2026-12-26"));
        assert_eq!(copied("notepad"), None, "no row");
        // A clipboard that refuses: the panel stays open (copy_failed).
        let row = sources::quick("1+1", &[]).into_iter().next().unwrap();
        assert!(!copy_result(&row, |_| false));
        // Only the calculator row copies.
        let web = sources::quick("1+1", &[]).into_iter().find(|h| h.kind == Kind::Web).unwrap();
        assert!(!copy_result(&web, |_| panic!("not copied")));
    }

    #[test]
    fn the_calculator_row_shows_no_copy_hint() {
        assert_eq!(row_hint(Kind::Calc, true, false), None);
        assert_eq!(row_hint(Kind::Calc, false, false), None);
        assert_eq!(row_hint(Kind::Calc, true, true), Some("search.copy_failed"), "a failed copy is still said");
        assert_eq!(row_hint(Kind::File, true, false), Some("search.hint.reveal"));
        assert_eq!(row_hint(Kind::File, false, false), None);
        assert_eq!(row_hint(Kind::App, true, false), None);
    }

    #[test]
    fn strings_exist() {
        let _l = crate::i18n::test_lock("ko");
        let mut keys = vec![
            "search.title",
            "search.placeholder",
            "search.group.calculator",
            "search.calc.day",
            "search.calc.days",
            "search.group.apps",
            "search.group.settings",
            "search.group.files",
            "search.copy_failed",
            "search.hint.reveal",
            "search.web",
            "search.none",
            "search.files.timeout",
            "search.files.recent_only",
            "search.files.searching",
            "launch.action.retry",
        ];
        keys.extend(sources::setting_keys());
        for k in keys {
            assert!(crate::i18n::get(k).is_some(), "{k}");
        }
        assert!(crate::i18n::tf("search.web", &[("q", "x")]).contains('x'));
    }
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
mod harmony;
