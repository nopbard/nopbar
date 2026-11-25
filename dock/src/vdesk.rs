//! Windows' virtual desktops, for the top bar's desktop dots and the hot
//! corners' "next / previous desktop".
//!
//! Read from the registry Explorer keeps them in (no undocumented COM):
//!
//! - `HKCU\...\Explorer\VirtualDesktops\VirtualDesktopIDs`: REG_BINARY,
//!   the desktops' GUIDs (16 bytes each) in their order. Absent while there
//!   has only ever been one desktop.
//! - `...\VirtualDesktops\Desktops\{GUID}\Name`: a name the user gave it
//!   (absent = Windows' "Desktop N"). Entries of closed desktops can stay
//!   behind here, so only the ID list counts.
//! - The current desktop: `...\VirtualDesktops\CurrentVirtualDesktop`
//!   (Windows 11), or `...\Explorer\SessionInfo\<session>\VirtualDesktops\
//!   CurrentVirtualDesktop` (Windows 10). Both are read; the first one that
//!   names a listed desktop wins.
//!
//! A background thread waits on RegNotifyChangeKeyValue for those keys
//! (no polling: it sleeps until Explorer writes) and posts WM_DESKTOPS to
//! the primary top bar when what it reads changed, and its own message to
//! each other listener (`listen`: the desktop notes, which show each
//! desktop's own notes). Either one starts the watch.
//!
//! The notes hide notes by it, so they get `sure()`: a reading only when
//! every value it rests on could be read (a missing key is a fact, a read
//! error is not) and a change of each would be heard (its own key armed
//! for values; an ancestor stands in only for a key that does not exist).
//! Anything less is None: not known, and the notes show everything. A
//! watch that could not be armed is tried again a few times, later each
//! time (then it waits for a change or a poke, as before).
//!
//! Switching sends Windows' own shortcuts (Ctrl+Win+Left/Right, Ctrl+Win+D,
//! Ctrl+Win+F4, Win+Tab) through keys::unmixed: nothing is sent while the
//! user holds a modifier.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::time::Duration;
use std::sync::Mutex;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY,
    REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, REG_NOTIFY_FILTER, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::keys::Sent;

/// Posted to the primary top bar: `latest()` changed.
pub const WM_DESKTOPS: u32 = WM_APP + 42;

const EXPLORER: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer";
const VD: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops";

/// Ctrl+Win+Left / Right / D / F4.
const PREV: [u16; 3] = [0x11, 0x5B, 0x25];
const NEXT: [u16; 3] = [0x11, 0x5B, 0x27];
const NEW: [u16; 3] = [0x11, 0x5B, b'D' as u16];
const CLOSE: [u16; 3] = [0x11, 0x5B, 0x73];
/// Between the steps of a switch over several desktops (after each step
/// has shown in the registry).
const STEP_MS: u64 = 40;

/// The desktops, in Windows' order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Desktops {
    /// The desktops' GUIDs (empty with a single desktop: Windows lists
    /// none then).
    pub ids: Vec<[u8; 16]>,
    /// One per desktop: the name the user gave it, if any.
    pub names: Vec<Option<String>>,
    /// Index of the current one (None: not found in the registry).
    pub current: Option<usize>,
}

impl Default for Desktops {
    /// One desktop, the current one (what an empty registry means).
    fn default() -> Self {
        Self { ids: Vec::new(), names: vec![None], current: Some(0) }
    }
}

impl Desktops {
    pub fn count(&self) -> usize {
        self.names.len()
    }

    /// The current desktop's GUID, if known and listed.
    pub fn current_id(&self) -> Option<[u8; 16]> {
        self.current.and_then(|i| self.ids.get(i)).copied()
    }

    /// Desktop i's name: the user's, else "Desktop N" in the UI language.
    pub fn name(&self, i: usize) -> String {
        match self.names.get(i) {
            Some(Some(n)) => n.clone(),
            _ => crate::i18n::tf("topbar.desktops.name", &[("n", &(i + 1).to_string())]),
        }
    }
}

// ---- parsing (pure) -----------------------------------------------------------

/// The GUIDs of a VirtualDesktopIDs blob, in order. A trailing partial GUID
/// (a blob whose length is not a multiple of 16) is ignored.
pub fn parse_ids(blob: &[u8]) -> Vec<[u8; 16]> {
    blob.chunks_exact(16).map(|c| c.try_into().expect("chunks_exact(16)")).collect()
}

/// A GUID's registry key name, "{8B11A721-24E4-43BC-B16C-837FF4A34A88}":
/// the first three fields are little-endian in the blob.
pub fn guid_text(g: &[u8; 16]) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

/// Where `current` (a CurrentVirtualDesktop value) is in `ids`. Anything
/// but exactly one GUID's 16 bytes matches nothing.
pub fn index_of(ids: &[[u8; 16]], current: &[u8]) -> Option<usize> {
    let c: &[u8; 16] = current.try_into().ok()?;
    ids.iter().position(|g| g == c)
}

/// The current desktop's index from the candidates, in order of trust.
pub fn current_index(ids: &[[u8; 16]], candidates: &[Option<Vec<u8>>]) -> Option<usize> {
    candidates.iter().flatten().find_map(|c| index_of(ids, c))
}

/// The desktop next to `current` (forward = to the right), or None at
/// either end (Windows does not wrap).
pub fn neighbor(current: usize, n: usize, forward: bool) -> Option<usize> {
    if forward {
        (current + 1 < n).then_some(current + 1)
    } else {
        current.checked_sub(1).filter(|_| current < n)
    }
}

// ---- reading ------------------------------------------------------------------

fn value(key: &str, name: &str, flags: windows::Win32::System::Registry::REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
    value_ex(key, name, flags).ok().flatten()
}

/// A value: Ok(None) when it (or its key) does not exist, Err when it
/// could not be read (access, type, a size that kept changing).
fn value_ex(key: &str, name: &str, flags: windows::Win32::System::Registry::REG_ROUTINE_FLAGS) -> Result<Option<Vec<u8>>, ()> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND};
    let (k, v) = (HSTRING::from(key), HSTRING::from(name));
    unsafe {
        let mut size = 0u32;
        let r = RegGetValueW(HKEY_CURRENT_USER, &k, &v, flags, None, None, Some(&mut size));
        if r == ERROR_FILE_NOT_FOUND || r == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        if r.is_err() {
            return Err(());
        }
        if size == 0 {
            return Ok(Some(Vec::new()));
        }
        // It may grow between the two calls: retry a few times.
        for _ in 0..3 {
            let mut buf = vec![0u8; size as usize];
            let r = RegGetValueW(HKEY_CURRENT_USER, &k, &v, flags, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size));
            if r.is_ok() {
                buf.truncate(size as usize);
                return Ok(Some(buf));
            }
            if r != ERROR_MORE_DATA {
                return Err(());
            }
        }
        Err(())
    }
}

fn text(key: &str, name: &str) -> Option<String> {
    let b = value(key, name, RRF_RT_REG_SZ)?;
    let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    let s = String::from_utf16_lossy(&w).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn session_vd() -> String {
    let mut sid = 0u32;
    unsafe {
        let _ = windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(std::process::id(), &mut sid);
    }
    format!(r"{EXPLORER}\SessionInfo\{sid}\VirtualDesktops")
}

/// The desktops as the registry has them now.
pub fn read() -> Desktops {
    let ids = value(VD, "VirtualDesktopIDs", RRF_RT_REG_BINARY).map(|b| parse_ids(&b)).unwrap_or_default();
    if ids.len() <= 1 {
        return Desktops::default();
    }
    let candidates = [
        value(VD, "CurrentVirtualDesktop", RRF_RT_REG_BINARY),
        value(&session_vd(), "CurrentVirtualDesktop", RRF_RT_REG_BINARY),
    ];
    let current = current_index(&ids, &candidates);
    let names = ids.iter().map(|g| text(&format!(r"{VD}\Desktops\{}", guid_text(g)), "Name")).collect();
    Desktops { ids, names, current }
}

/// A reading the notes may hide notes by (see the module notes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sure {
    /// The desktops' GUIDs in order; empty or one: a single desktop (empty
    /// when Windows has never listed one).
    pub ids: Vec<[u8; 16]>,
    /// The current one (an index into `ids`; 0 with a single desktop).
    pub current: usize,
}

impl Sure {
    /// The current desktop's GUID, when listed.
    pub fn current_id(&self) -> Option<[u8; 16]> {
        self.ids.get(self.current).copied()
    }
}

/// Pure: a sure reading from the VirtualDesktopIDs value and the
/// CurrentVirtualDesktop candidates (in order of trust), and which
/// candidate named the current desktop. None when anything it rests on
/// could not be read, the list is malformed, or (two desktops or more)
/// no candidate names a listed desktop. A candidate that cannot be read
/// before the one that names a desktop makes it unsure too (it might
/// have named another).
pub fn sure_reading(ids: Result<Option<Vec<u8>>, ()>, cands: &[Result<Option<Vec<u8>>, ()>]) -> Option<(Sure, Option<usize>)> {
    let blob = ids.ok()?.unwrap_or_default();
    if blob.len() % 16 != 0 {
        return None;
    }
    let ids = parse_ids(&blob);
    if ids.len() <= 1 {
        return Some((Sure { ids, current: 0 }, None));
    }
    for (i, c) in cands.iter().enumerate() {
        match c {
            Err(()) => return None,
            Ok(Some(v)) => {
                if let Some(cur) = index_of(&ids, v) {
                    return Some((Sure { ids, current: cur }, Some(i)));
                }
            }
            Ok(None) => {}
        }
    }
    None
}

/// How a key is watched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watched {
    /// The key itself, for its values (every change is heard).
    Own,
    /// It does not exist; an ancestor is watched for it to appear.
    Missing,
    /// Not watched for its values (no access to it, or nothing armed).
    No,
}

/// Pure: the reading the notes may trust, given how the keys it came from
/// are watched (VirtualDesktops: the list and the first candidate; the
/// session's: the second).
pub fn trusted(reading: Option<(Sure, Option<usize>)>, vd: Watched, session: Watched) -> Option<Sure> {
    let (s, src) = reading?;
    let vd_ok = vd == Watched::Own || (vd == Watched::Missing && s.ids.is_empty());
    let src_ok = match src {
        Some(1) => session == Watched::Own,
        _ => true,
    };
    (vd_ok && src_ok).then_some(s)
}

/// Pure: the wait before retry `n` of a watch that could not be armed
/// (2, 4, 8 ... s, at most a minute); None: no more retries.
pub fn rearm_wait(n: u32) -> Option<Duration> {
    (n < 8).then(|| Duration::from_secs((2u64 << n.min(5)).min(60)))
}

fn read_sure(session: &str) -> Option<(Sure, Option<usize>)> {
    let ids = value_ex(VD, "VirtualDesktopIDs", RRF_RT_REG_BINARY);
    let cands = [value_ex(VD, "CurrentVirtualDesktop", RRF_RT_REG_BINARY), value_ex(session, "CurrentVirtualDesktop", RRF_RT_REG_BINARY)];
    sure_reading(ids, &cands)
}

// ---- watching -----------------------------------------------------------------

static LATEST: Mutex<Option<Desktops>> = Mutex::new(None);
/// The window WM_DESKTOPS goes to (0 = none).
static TARGET: AtomicIsize = AtomicIsize::new(0);
/// Auto-reset event that makes the watcher read again (Explorer restarted).
static POKE: AtomicIsize = AtomicIsize::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
/// Other windows told of changes: (window, message).
static LISTENERS: Mutex<Vec<(isize, u32)>> = Mutex::new(Vec::new());
/// The last sure reading (None: not known), and whether the watcher has
/// published once.
static SURE: Mutex<Option<Sure>> = Mutex::new(None);
static READ_ONCE: AtomicBool = AtomicBool::new(false);

/// The desktops as far as they are sure (see the module notes); None
/// while not known, the watch not running or not hearing every change.
pub fn sure() -> Option<Sure> {
    if !STARTED.load(Ordering::SeqCst) {
        return None;
    }
    SURE.lock().ok().and_then(|g| g.clone())
}

/// Waits (at most `max`) for the watcher's first reading, so a listener
/// that starts it does not show everything for a moment.
pub fn wait_first(max: Duration) {
    let t0 = std::time::Instant::now();
    while STARTED.load(Ordering::SeqCst) && !READ_ONCE.load(Ordering::SeqCst) && t0.elapsed() < max {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The last desktops read (reads them now if the watcher has not yet).
pub fn latest() -> Desktops {
    if let Some(d) = LATEST.lock().ok().and_then(|g| g.clone()) {
        return d;
    }
    let d = read();
    if let Ok(mut g) = LATEST.lock() {
        g.get_or_insert(d.clone());
    }
    d
}

/// Starts watching (once per process); changes are posted to `bar`.
/// Called again (the bar was re-created), it redirects them and re-reads.
pub fn start(bar: HWND) {
    TARGET.store(bar.0 as isize, Ordering::SeqCst);
    if STARTED.swap(true, Ordering::SeqCst) {
        poke();
        return;
    }
    // A watch on the shared service thread (no thread of its own).
    let ok = start_watch();
    if !ok {
        STARTED.store(false, Ordering::SeqCst);
        crate::log!("desktops: cannot start the watcher");
    }
}

/// Starts the watch if nobody has yet (once per process). A watch that
/// cannot start leaves `sure()` None (the notes show everything).
fn ensure_watch() {
    if !STARTED.swap(true, Ordering::SeqCst) && !start_watch() {
        STARTED.store(false, Ordering::SeqCst);
        crate::log!("desktops: cannot start the watcher");
    }
}

/// `msg` is posted to `hwnd` whenever the desktops change (another
/// thread's window: the notes'), with or without the top bar. Starts the
/// watch if need be. The caller reads `latest()` after this, so nothing
/// that changes in between is missed.
pub fn listen(hwnd: HWND, msg: u32) {
    if let Ok(mut l) = LISTENERS.lock() {
        let me = hwnd.0 as isize;
        l.retain(|(h, _)| *h != me);
        l.push((me, msg));
    }
    ensure_watch();
}

/// `hwnd` is going away: no more messages to it.
pub fn unlisten(hwnd: HWND) {
    if let Ok(mut l) = LISTENERS.lock() {
        l.retain(|(h, _)| *h != hwnd.0 as isize);
    }
}

/// The bar is gone: stop posting to its window, and stop any switch.
pub fn detach() {
    TARGET.store(0, Ordering::SeqCst);
    cancel();
}

/// Read again now (Explorer restarted and may have rewritten the keys).
pub fn poke() {
    let h = POKE.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = SetEvent(HANDLE(h as *mut _));
        }
    }
}

fn publish(d: Desktops, sure: Option<Sure>) {
    let changed = match LATEST.lock() {
        Ok(mut g) => {
            let changed = g.as_ref() != Some(&d);
            *g = Some(d);
            changed
        }
        Err(_) => false,
    };
    let sure_changed = set_sure(sure);
    READ_ONCE.store(true, Ordering::SeqCst);
    let h = TARGET.load(Ordering::SeqCst);
    if changed && h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_DESKTOPS, WPARAM(0), LPARAM(0));
        }
    }
    if changed || sure_changed {
        tell_listeners();
    }
}

/// True: it changed.
fn set_sure(sure: Option<Sure>) -> bool {
    match SURE.lock() {
        Ok(mut g) => {
            let changed = *g != sure;
            *g = sure;
            changed
        }
        Err(_) => false,
    }
}

fn tell_listeners() {
    let others = LISTENERS.lock().map(|l| l.clone()).unwrap_or_default();
    for (w, msg) in others {
        unsafe {
            let _ = PostMessageW(Some(HWND(w as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// One armed registry key: its handle and its (manual-reset) event.
struct Armed(HKEY, HANDLE);

impl Drop for Armed {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
            let _ = CloseHandle(self.1);
        }
    }
}

/// Opens `path` and asks for one notification of `filter` on it. Err(true):
/// the key does not exist; Err(false): it could not be armed.
fn arm(path: &str, subtree: bool, filter: REG_NOTIFY_FILTER) -> Result<Armed, bool> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};
    unsafe {
        let mut k = HKEY::default();
        let r = RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, KEY_NOTIFY, &mut k);
        if r == ERROR_FILE_NOT_FOUND || r == ERROR_PATH_NOT_FOUND {
            return Err(true);
        }
        if r.is_err() {
            return Err(false);
        }
        let Ok(ev) = CreateEventW(None, true, false, PCWSTR::null()) else {
            let _ = RegCloseKey(k);
            return Err(false);
        };
        let armed = Armed(k, ev);
        RegNotifyChangeKeyValue(k, subtree, filter, Some(ev), true).ok().map_err(|_| false)?;
        Ok(armed)
    }
}

/// The parent of a registry path ("" for a top-level one).
fn parent(path: &str) -> &str {
    path.rsplit_once('\\').map(|(p, _)| p).unwrap_or("")
}

/// The keys to try for `path`, nearest first: the key itself (watched with
/// its subtree), then each ancestor (watched for a subkey appearing).
pub fn watch_chain(path: &str) -> Vec<(&str, bool)> {
    let mut out = vec![(path, true)];
    let mut p = parent(path);
    while !p.is_empty() {
        out.push((p, false));
        p = parent(p);
    }
    out
}

/// Arms `path`, or if it does not exist the nearest ancestor that does
/// (for its next subkey: after that wake the walk starts again one level
/// deeper, until `path` itself exists). None if nothing could be armed;
/// `what` names it in the log, once per change of outcome. Also how
/// `path`'s values are watched: an ancestor stands in only for a key
/// that does not exist (one that cannot be armed is not watched).
fn arm_nearest(path: &str, values: REG_NOTIFY_FILTER, what: &str, last: &mut Option<String>) -> (Option<Armed>, Watched) {
    let mut how = Watched::No;
    for (p, own) in watch_chain(path) {
        let filter = if own { values } else { REG_NOTIFY_CHANGE_NAME };
        match arm(p, own, filter) {
            Ok(a) => {
                if last.as_deref() != Some(p) {
                    if !own {
                        crate::log!("desktops: {what} key {}; waiting on {p}", if how == Watched::Missing { "missing" } else { "cannot be watched" });
                    }
                    *last = Some(p.to_string());
                }
                return (Some(a), if own { Watched::Own } else { how });
            }
            Err(missing) if own => how = if missing { Watched::Missing } else { Watched::No },
            Err(_) => {}
        }
    }
    if last.as_deref() != Some("") {
        crate::log!("desktops: cannot watch the {what} key or any of its parents");
        *last = Some(String::new());
    }
    (None, Watched::No)
}

/// The watcher (on the service thread): arms the keys, reads, waits until
/// one changes, repeats. A key that does not exist yet is waited for on its
/// nearest existing ancestor (a subkey appearing there); keys are re-opened
/// after every wake, so ones created later or deleted and created again
/// (Explorer restarting) are followed.
struct Watcher {
    /// Auto-reset event that makes it read again (POKE).
    poke: isize,
    session: String,
    armed: Vec<Armed>,
    last_vd: Option<String>,
    last_session: Option<String>,
    /// Retries of a watch that could not hear every change.
    rearms: u32,
}

// Registry keys and events are kernel handles: usable from any thread.
unsafe impl Send for Watcher {}

impl Watcher {
    /// True: every value the reading rests on is heard.
    fn arm_and_read(&mut self) -> bool {
        let values = REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_CHANGE_NAME;
        self.armed.clear();
        let (a, vd) = arm_nearest(VD, values, "virtual desktops", &mut self.last_vd);
        self.armed.extend(a);
        let (a, ses) = arm_nearest(&self.session, values, "session", &mut self.last_session);
        self.armed.extend(a);
        // Read after arming: nothing written in between is missed.
        let reading = read_sure(&self.session);
        let read_ok = reading.is_some();
        let sure = trusted(reading, vd, ses);
        let heard = sure.is_some() || !read_ok;
        publish(read(), sure);
        heard
    }
}

impl crate::service::Watch for Watcher {
    fn name(&self) -> &'static str {
        "desktops"
    }
    fn handles(&self) -> Vec<HANDLE> {
        let mut h = vec![HANDLE(self.poke as *mut _)];
        h.extend(self.armed.iter().map(|a| a.1));
        h
    }
    fn wake(&mut self, why: crate::service::Wake) -> crate::service::Next {
        use crate::service::{Next, Wake};
        match why {
            Wake::Start | Wake::Timer => {
                if self.arm_and_read() {
                    self.rearms = 0;
                    return Next::Wait;
                }
                // Read but not heard: not known to the notes (they show
                // everything); armed again a little later, a few times.
                match rearm_wait(self.rearms) {
                    Some(d) => {
                        self.rearms += 1;
                        crate::log!("desktops: the watch cannot hear every change; trying again in {} s", d.as_secs());
                        Next::Sleep(d)
                    }
                    None => Next::Wait,
                }
            }
            // Explorer writes a switch as a few values in a row: read once
            // they are all in.
            Wake::Signaled(_) | Wake::Poked => {
                self.armed.clear();
                Next::Sleep(std::time::Duration::from_millis(30))
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        POKE.store(0, Ordering::SeqCst);
        STARTED.store(false, Ordering::SeqCst);
        // No watch: nothing is sure any more.
        if set_sure(None) {
            tell_listeners();
        }
        unsafe {
            let _ = CloseHandle(HANDLE(self.poke as *mut _));
        }
    }
}

fn start_watch() -> bool {
    let Ok(poke) = (unsafe { CreateEventW(None, false, false, PCWSTR::null()) }) else {
        crate::log!("desktops: cannot create the watcher's event");
        return false;
    };
    POKE.store(poke.0 as isize, Ordering::SeqCst);
    let w = Watcher { poke: poke.0 as isize, session: session_vd(), armed: Vec::new(), last_vd: None, last_session: None, rearms: 0 };
    // (On failure the watch is dropped, which closes the event.)
    crate::service::add(Box::new(w)).is_some()
}

// ---- switching ----------------------------------------------------------------

/// Bumped by every desktop action: a multi-step switch still under way
/// stops at its next step when it is no longer the latest action.
static GEN: AtomicU64 = AtomicU64::new(0);
/// Held around every shortcut sent, so a switch thread's step and another
/// action never interleave (and a cancelled switch cannot press once more).
static PRESS: Mutex<()> = Mutex::new(());
/// How long a step may take to show in the registry before a switch gives up.
const ARRIVE_MS: u64 = 600;
const ARRIVE_POLL_MS: u64 = 20;

fn cancel() -> u64 {
    GEN.fetch_add(1, Ordering::SeqCst) + 1
}

/// Sends `chord` now, ending any switch under way (the newest action wins).
fn act(chord: &[u16]) -> Sent {
    let _g = PRESS.lock().unwrap_or_else(|e| e.into_inner());
    cancel();
    crate::keys::unmixed(chord)
}

/// Sends `chord` only if action `gen` is still the latest one.
fn press_for(gen: u64, chord: &[u16]) -> Option<Sent> {
    let _g = PRESS.lock().unwrap_or_else(|e| e.into_inner());
    (GEN.load(Ordering::SeqCst) == gen).then(|| crate::keys::unmixed(chord))
}

/// Which way to press from desktop `cur` to the one with id `target`:
/// Some(true) = right, Some(false) = left, None = there (or unknown).
pub fn next_press(d: &Desktops, target: &[u8; 16]) -> Option<bool> {
    let cur = d.current?;
    let to = d.ids.iter().position(|g| g == target)?;
    (to != cur).then_some(to > cur)
}

/// What the registry says after one press from `from` towards `expected`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// The step landed.
    Arrived,
    /// Not recorded yet: keep waiting.
    Waiting,
    /// Somewhere else (another switch, or the desktops changed): stop.
    Diverted,
}

pub fn progress(from: usize, expected: usize, now: Option<usize>) -> Progress {
    match now {
        Some(n) if n == expected => Progress::Arrived,
        Some(n) if n == from => Progress::Waiting,
        _ => Progress::Diverted,
    }
}

/// Switches to desktop `target`: one Ctrl+Win+Left/Right at a time, each
/// confirmed in the registry before the next (~40 ms apart), stopping at
/// the target, on any other change of desktop, or when a newer desktop
/// action comes in. Refused while the user holds a modifier.
pub fn go_to(target: usize) -> Sent {
    // An explicit pick: the wheel's notches still to go are dropped.
    wheel_queue().clear();
    let gen = cancel();
    let now = read();
    let Some(&id) = now.ids.get(target) else {
        // One desktop (or the list changed under the click): nothing to do.
        return Sent::Ok;
    };
    if now.current.is_none() {
        crate::log!("desktops: current desktop unknown; not switching");
        return Sent::Failed;
    }
    if next_press(&now, &id).is_none() {
        return Sent::Ok;
    }
    if crate::keys::modifiers_held() {
        return Sent::ModifiersHeld;
    }
    let spawned = std::thread::Builder::new().name("vdesk-switch".into()).spawn(move || switch_to(gen, id, now));
    if spawned.is_err() {
        crate::log!("desktops: cannot start a switch");
        return Sent::Failed;
    }
    Sent::Ok
}

fn sleep(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// The steps of go_to, on their own thread. `now` was read just before.
fn switch_to(gen: u64, target: [u8; 16], mut now: Desktops) {
    // Never more presses than desktops: a safety stop if the registry lies.
    for _ in 0..now.count() {
        let Some(forward) = next_press(&now, &target) else { return };
        let Some(from) = now.current else { return };
        let expected = if forward { from + 1 } else { from - 1 };
        match press_for(gen, if forward { &NEXT } else { &PREV }) {
            None => return, // a newer action took over
            Some(Sent::Ok) => {}
            Some(s) => {
                crate::log!("desktops: switch stopped ({s:?})");
                return;
            }
        }
        // Wait until Explorer has recorded this step before the next one.
        let mut waited = 0;
        loop {
            sleep(ARRIVE_POLL_MS);
            waited += ARRIVE_POLL_MS;
            if GEN.load(Ordering::SeqCst) != gen {
                return;
            }
            now = read();
            match progress(from, expected, now.current) {
                Progress::Arrived => break,
                Progress::Waiting if waited < ARRIVE_MS => {}
                Progress::Waiting => {
                    crate::log!("desktops: switch stopped (step not recorded)");
                    return;
                }
                Progress::Diverted => {
                    crate::log!("desktops: switch stopped (the desktop changed elsewhere)");
                    return;
                }
            }
        }
        sleep(STEP_MS);
    }
}

/// One desktop to the right (forward) or left, ending any switch under
/// way. The edge check reads the registry now (never the watcher's cache);
/// at either end nothing is sent (Windows does not wrap); with the current
/// desktop unknown the shortcut is sent anyway (at an end Windows ignores it).
pub fn step(forward: bool) -> Sent {
    let d = read();
    if let Some(cur) = d.current {
        if neighbor(cur, d.count(), forward).is_none() {
            cancel();
            return Sent::Ok;
        }
    }
    act(if forward { &NEXT } else { &PREV })
}

/// Pure: whole wheel notches (120) in the delta so far (`acc` + `delta`),
/// and what is left over for the next message.
pub fn notches(acc: i32, delta: i32) -> (i32, i32) {
    let total = acc.saturating_add(delta);
    let steps = total / 120;
    (steps, total - steps * 120)
}

/// The wheel's desktop moves: one counter of notches still to go (each
/// notch +1 forward, -1 back; opposite notches cancel out), drained one
/// step at a time by a single worker thread that exists only while the
/// counter is not 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WheelQueue {
    pub pending: i32,
    /// A worker is draining it.
    pub running: bool,
}

impl WheelQueue {
    /// Adds `n` notches. True: no worker runs, start one.
    pub fn add(&mut self, n: i32) -> bool {
        self.pending = self.pending.saturating_add(n);
        if self.pending != 0 && !self.running {
            self.running = true;
            return true;
        }
        false
    }

    /// The worker's next step (Some(true): forward), one notch taken off;
    /// None: nothing left, the worker ends.
    pub fn next(&mut self) -> Option<bool> {
        if self.pending == 0 {
            self.running = false;
            return None;
        }
        let forward = self.pending > 0;
        self.pending -= self.pending.signum();
        Some(forward)
    }

    /// An explicit desktop pick, or a stop: the notches still to go are
    /// dropped (the worker ends after its current step).
    pub fn clear(&mut self) {
        self.pending = 0;
    }
}

static WHEEL: Mutex<WheelQueue> = Mutex::new(WheelQueue { pending: 0, running: false });

fn wheel_queue() -> std::sync::MutexGuard<'static, WheelQueue> {
    WHEEL.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long a wheel step waits to see its desktop change before the next.
const WHEEL_ARRIVE_MS: u64 = 600;
const WHEEL_LOOK_MS: u64 = 30;

/// `steps` wheel notches (negative: back): added to the counter, which one
/// worker drains a desktop at a time with the plain single step (step():
/// it reads where we are first and sends nothing at either end). Nothing
/// is cancelled by later notches; an explicit pick (go_to) clears what is
/// still to go.
pub fn wheel(steps: i32) -> Sent {
    if steps == 0 || !wheel_queue().add(steps) {
        return Sent::Ok;
    }
    let spawned = std::thread::Builder::new().name("vdesk-wheel".into()).spawn(|| {
        loop {
            // The guard is dropped right here: the body may lock the queue
            // again (clear() on a refused step).
            let next = wheel_queue().next();
            let Some(forward) = next else { break };
            let before = read().current;
            let s = step(forward);
            if s != Sent::Ok {
                crate::log!("desktops: wheel step not sent ({s:?}); the rest dropped");
                wheel_queue().clear();
                continue;
            }
            // Let it land before the next step (bounded).
            let mut waited = 0;
            while waited < WHEEL_ARRIVE_MS && read().current == before {
                sleep(WHEEL_LOOK_MS);
                waited += WHEEL_LOOK_MS;
            }
        }
    });
    if spawned.is_err() {
        crate::log!("desktops: cannot start the wheel's worker");
        *wheel_queue() = WheelQueue::default();
        return Sent::Failed;
    }
    Sent::Ok
}

/// Ctrl+Win+D: a new desktop (Windows switches to it). Wheel notches
/// still to go are dropped: this is an explicit move.
pub fn new_desktop() -> Sent {
    wheel_queue().clear();
    act(&NEW)
}

/// Ctrl+Win+F4 on desktop `expected` (the one the confirmation named):
/// the current desktop is read again right before, and if it is another
/// one (or unknown) nothing is closed.
pub fn close(expected: &[u8; 16]) -> Sent {
    wheel_queue().clear();
    let _g = PRESS.lock().unwrap_or_else(|e| e.into_inner());
    cancel();
    let now = read();
    if now.current_id() != Some(*expected) {
        crate::log!("desktops: close cancelled: the current desktop is no longer the one confirmed");
        return Sent::Failed;
    }
    crate::keys::unmixed(&CLOSE)
}

/// Win+Tab.
pub fn task_view() -> Sent {
    act(&crate::keys::TASK_VIEW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_notches_add_up_and_several_move_as_many() {
        assert_eq!(notches(0, 120), (1, 0));
        assert_eq!(notches(0, -240), (-2, 0), "two notches in one message");
        assert_eq!(notches(0, 360), (3, 0));
        assert_eq!(notches(30, 250), (2, 40), "the rest waits for the next message");
        assert_eq!(notches(0, 40), (0, 40));
        assert_eq!(notches(-100, 40), (0, -60));
    }

    #[test]
    fn the_wheel_counter_drains_one_step_at_a_time() {
        let mut q = WheelQueue::default();
        assert!(q.add(2), "the first notches start the worker");
        assert!(!q.add(1), "later ones add to the count, no second worker");
        assert_eq!(q.pending, 3);
        assert_eq!(q.next(), Some(true));
        // A notch back while it runs cancels one of those still to go.
        assert!(!q.add(-1));
        assert_eq!(q.next(), Some(true));
        assert_eq!(q.next(), None, "drained: the worker ends");
        assert!(!q.running);
        // Back the other way, then a pick clears what is still to go.
        assert!(q.add(-3));
        assert_eq!(q.next(), Some(false));
        q.clear();
        assert_eq!(q.next(), None);
        assert!(!q.running && q.pending == 0);
        // Notches that cancel out start nothing.
        assert!(!q.add(0));
        let mut q = WheelQueue::default();
        q.add(1);
        q.add(-1);
        assert_eq!(q.next(), None);
    }

    fn g(b: u8) -> [u8; 16] {
        let mut x = [0u8; 16];
        x[0] = b;
        x[15] = b;
        x
    }

    #[test]
    fn id_blobs_parse_into_guids() {
        assert!(parse_ids(&[]).is_empty());
        let mut blob = Vec::new();
        for b in 1..=3 {
            blob.extend_from_slice(&g(b));
        }
        assert_eq!(parse_ids(&blob), vec![g(1), g(2), g(3)]);
        // Odd lengths: the partial tail is not a desktop.
        blob.extend_from_slice(&[9, 9, 9]);
        assert_eq!(parse_ids(&blob).len(), 3);
        assert!(parse_ids(&[1; 15]).is_empty());
        assert_eq!(parse_ids(&[7; 17]), vec![[7; 16]]);
    }

    #[test]
    fn guid_text_matches_the_registry_key_names() {
        // {8B11A721-24E4-43BC-B16C-837FF4A34A88} as Windows stores it.
        let raw = [0x21, 0xA7, 0x11, 0x8B, 0xE4, 0x24, 0xBC, 0x43, 0xB1, 0x6C, 0x83, 0x7F, 0xF4, 0xA3, 0x4A, 0x88];
        assert_eq!(guid_text(&raw), "{8B11A721-24E4-43BC-B16C-837FF4A34A88}");
    }

    #[test]
    fn current_desktop_is_found() {
        let ids = vec![g(1), g(2), g(3)];
        assert_eq!(index_of(&ids, &g(2)), Some(1));
        assert_eq!(index_of(&ids, &g(9)), None);
        // Not exactly 16 bytes: nothing.
        assert_eq!(index_of(&ids, &g(2)[..15]), None);
        let mut long = g(2).to_vec();
        long.push(0);
        assert_eq!(index_of(&ids, &long), None);
        // The first candidate that names a listed desktop wins.
        assert_eq!(current_index(&ids, &[None, Some(g(3).to_vec())]), Some(2));
        assert_eq!(current_index(&ids, &[Some(g(9).to_vec()), Some(g(1).to_vec())]), Some(0));
        assert_eq!(current_index(&ids, &[Some(g(1).to_vec()), Some(g(3).to_vec())]), Some(0));
        assert_eq!(current_index(&ids, &[None, None]), None);
    }

    #[test]
    fn neighbors_never_wrap() {
        assert_eq!(neighbor(0, 3, true), Some(1));
        assert_eq!(neighbor(2, 3, true), None, "no wrap at the right end");
        assert_eq!(neighbor(0, 3, false), None, "no wrap at the left end");
        assert_eq!(neighbor(2, 3, false), Some(1));
        assert_eq!(neighbor(0, 1, true), None);
        assert_eq!(neighbor(5, 3, false), None);
    }

    #[test]
    fn switches_head_for_the_target_and_stop_there() {
        let d = |cur: Option<usize>| Desktops { ids: vec![g(1), g(2), g(3), g(4)], names: vec![None; 4], current: cur };
        assert_eq!(next_press(&d(Some(0)), &g(4)), Some(true));
        assert_eq!(next_press(&d(Some(3)), &g(1)), Some(false));
        assert_eq!(next_press(&d(Some(2)), &g(3)), None, "there: stop");
        // Moved past it meanwhile (another switch): turn back, never on.
        assert_eq!(next_press(&d(Some(3)), &g(3)), Some(false));
        assert_eq!(next_press(&d(None), &g(3)), None, "unknown: stop");
        assert_eq!(next_press(&d(Some(0)), &g(9)), None, "target closed: stop");
        // After a press from 1 towards 2.
        assert_eq!(progress(1, 2, Some(2)), Progress::Arrived);
        assert_eq!(progress(1, 2, Some(1)), Progress::Waiting);
        assert_eq!(progress(1, 2, Some(0)), Progress::Diverted);
        assert_eq!(progress(1, 2, None), Progress::Diverted);
        assert_eq!(d(Some(1)).current_id(), Some(g(2)));
        assert_eq!(d(None).current_id(), None);
        assert_eq!(Desktops::default().current_id(), None);
    }

    #[test]
    fn missing_keys_are_waited_for_on_the_nearest_ancestor() {
        assert_eq!(watch_chain(r"A\B\C"), vec![(r"A\B\C", true), (r"A\B", false), ("A", false)]);
        assert_eq!(watch_chain("A"), vec![("A", true)]);
        let s = format!(r"{EXPLORER}\SessionInfo\3\VirtualDesktops");
        let chain = watch_chain(&s);
        assert_eq!(chain[1].0, format!(r"{EXPLORER}\SessionInfo\3"));
        assert_eq!(chain[2].0, format!(r"{EXPLORER}\SessionInfo"));
        assert_eq!(chain[3].0, EXPLORER);
        assert!(chain[1..].iter().all(|(_, own)| !own));
    }

    #[test]
    fn only_a_full_reading_is_sure() {
        let blob = |gs: &[u8]| -> Result<Option<Vec<u8>>, ()> { Ok(Some(gs.iter().flat_map(|b| [*b; 16]).collect())) };
        let cur = |b: u8| -> Result<Option<Vec<u8>>, ()> { Ok(Some(vec![b; 16])) };
        // Two desktops, the current one named first or second.
        assert_eq!(sure_reading(blob(&[1, 2]), &[cur(2), Ok(None)]), Some((Sure { ids: vec![[1; 16], [2; 16]], current: 1 }, Some(0))));
        assert_eq!(sure_reading(blob(&[1, 2]), &[Ok(None), cur(1)]).map(|r| r.1), Some(Some(1)));
        // A stale first value that names no listed desktop: the next one.
        assert_eq!(sure_reading(blob(&[1, 2]), &[cur(9), cur(1)]).map(|r| (r.0.current, r.1)), Some((0, Some(1))));
        // Not sure: the list or an earlier candidate unreadable, nothing
        // named, a malformed list.
        assert_eq!(sure_reading(Err(()), &[cur(1), Ok(None)]), None);
        assert_eq!(sure_reading(blob(&[1, 2]), &[Err(()), cur(1)]), None);
        assert_eq!(sure_reading(blob(&[1, 2]), &[Ok(None), Ok(None)]), None);
        assert_eq!(sure_reading(Ok(Some(vec![1; 20])), &[cur(1), Ok(None)]), None);
        // One desktop (listed, or never listed): sure, whatever the current.
        assert_eq!(sure_reading(blob(&[3]), &[Err(()), Err(())]), Some((Sure { ids: vec![[3; 16]], current: 0 }, None)));
        assert_eq!(sure_reading(Ok(None), &[Ok(None), Ok(None)]), Some((Sure { ids: vec![], current: 0 }, None)));
    }

    #[test]
    fn only_a_heard_reading_is_trusted() {
        let two = || Some((Sure { ids: vec![[1; 16], [2; 16]], current: 0 }, Some(0)));
        let none = || Some((Sure { ids: vec![], current: 0 }, None));
        assert!(trusted(two(), Watched::Own, Watched::No).is_some(), "Windows 11: the session's key is not needed");
        // Review #1: readable but not watchable, or only an ancestor watched.
        assert!(trusted(two(), Watched::No, Watched::Own).is_none());
        assert!(trusted(two(), Watched::Missing, Watched::Own).is_none());
        // The current desktop from the session's key: that key must be heard.
        let from_session = Some((Sure { ids: vec![[1; 16], [2; 16]], current: 1 }, Some(1)));
        assert!(trusted(from_session.clone(), Watched::Own, Watched::No).is_none());
        assert!(trusted(from_session, Watched::Own, Watched::Own).is_some());
        // Never more than one desktop (no key yet): its appearing is heard.
        assert!(trusted(none(), Watched::Missing, Watched::No).is_some());
        assert!(trusted(none(), Watched::No, Watched::No).is_none());
        assert!(trusted(None, Watched::Own, Watched::Own).is_none());
    }

    #[test]
    fn a_deaf_watch_is_tried_again_a_few_times() {
        assert_eq!(rearm_wait(0), Some(Duration::from_secs(2)));
        assert_eq!(rearm_wait(1), Some(Duration::from_secs(4)));
        assert_eq!(rearm_wait(7), Some(Duration::from_secs(60)));
        assert_eq!(rearm_wait(8), None);
    }

    #[test]
    fn default_names_are_numbered() {
        let _l = crate::i18n::test_lock("en");
        let d = Desktops { ids: vec![g(1), g(2)], names: vec![None, Some("Work".into())], current: Some(1) };
        assert_eq!(d.name(0), "Desktop 1");
        assert_eq!(d.name(1), "Work");
        assert_eq!(Desktops::default().count(), 1);
    }
}
