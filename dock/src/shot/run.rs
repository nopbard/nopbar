//! The screenshot watcher: a message-only window listening to clipboard
//! changes (only while the setting is on), the "screenshot" worker
//! (crate::idle: no thread while idle) that reads, decodes and writes, and
//! the island's actions. UI thread unless noted.
//!
//! Sessions: every switch on or off starts a new session; work queued in
//! an older one (a read, a decode, a copy, a file) is dropped on arrival.
//! Temporary files are made with create_new under fresh names (never an
//! existing file taken for ours) and written down in a record of what we
//! made (super::Made); only recorded files that are unchanged since we
//! wrote them are ever deleted.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Pictures, FOLDERID_Screenshots, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::image::{self, CF_DIB};
use super::{Detector, Fate, Made};
use crate::idle::{IdleQueue, Jobs};
use crate::topbar::island::model::Shot;

const WM_SHOT_READY: u32 = WM_APP + 1;
const WM_SHOT_COPY: u32 = WM_APP + 2;
/// A temporary file was made for an action (results in TEMP_DONE).
const WM_SHOT_TEMP: u32 = WM_APP + 3;
const WM_SHOT_DONE: u32 = WM_APP + 4;
/// The periodic cleanup is due (wparam: the session that scheduled it).
const WM_SHOT_CLEAN: u32 = WM_APP + 5;
const DEBOUNCE_TIMER: usize = 1;
const EXPIRE_TIMER: usize = 2;
/// The island is busy with the screenshot: look again after this.
const BUSY_MS: u32 = 1500;
const IDLE: Duration = Duration::from_secs(5);
/// The cleanup runs this often while the watcher is on.
const CLEAN_EVERY: Duration = Duration::from_secs(60 * 60);

const AFTER_EDIT: usize = 1;

/// The watcher window (0: off).
static WATCH: AtomicIsize = AtomicIsize::new(0);
/// Bumped on every switch on or off: older work is dropped.
static SESSION: AtomicU64 = AtomicU64::new(0);
/// Bumped for every new screenshot.
static GEN: AtomicU64 = AtomicU64::new(0);
/// A drag of the screenshot is running.
static DRAGGING: AtomicBool = AtomicBool::new(false);

/// The screenshot on show.
struct Current {
    session: u64,
    gen: u64,
    /// The island's key for it ("screenshot:<gen>").
    key: String,
    png: Arc<Vec<u8>>,
    thumb: (u32, u32, Arc<Vec<u8>>),
    /// Its temporary file, once made (the exact path we created).
    temp: Option<PathBuf>,
    saved: bool,
    copied: bool,
}

static CURRENT: Mutex<Option<Current>> = Mutex::new(None);
static COPY_OUT: Mutex<Option<(u64, u64, Vec<u8>)>> = Mutex::new(None);
/// Finished temporary files: (session, gen, path, what for).
static TEMP_DONE: Mutex<Vec<(u64, u64, PathBuf, usize)>> = Mutex::new(Vec::new());
/// One temporary file made at a time (two requests for the same
/// screenshot share one file).
static TEMP_LOCK: Mutex<()> = Mutex::new(());
/// The record of files we made (read-modify-write under this).
static MADE_LOCK: Mutex<()> = Mutex::new(());
static QUEUE: OnceLock<IdleQueue<Job>> = OnceLock::new();

thread_local! {
    static DET: RefCell<Detector> = RefCell::new(Detector::default());
}

fn cur() -> std::sync::MutexGuard<'static, Option<Current>> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner())
}

fn session() -> u64 {
    SESSION.load(Ordering::SeqCst)
}

/// The watcher is on and still in session `s`.
fn live(s: u64) -> bool {
    WATCH.load(Ordering::SeqCst) != 0 && session() == s
}

enum Job {
    /// Read the clipboard's picture if it is still at this sequence number
    /// (session, sequence number).
    Read(u64, u32),
    Save(u64, u64),
    Copy(u64, u64),
    Temp(u64, u64, usize),
    /// Delete what we made that is old enough.
    Cleanup,
}

fn queue() -> &'static IdleQueue<Job> {
    QUEUE.get_or_init(|| {
        IdleQueue::new("screenshot", IDLE, 8, |jobs: &mut Jobs<Job>| {
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            while let Some(job) = jobs.next() {
                work(job);
            }
            if com {
                unsafe { CoUninitialize() };
            }
        })
    })
}

fn send(job: Job) {
    if queue().send(job).is_err() {
        crate::log!("screenshot: busy, a request was dropped");
    }
}

fn post(msg: u32, wp: usize) {
    let h = WATCH.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(wp), LPARAM(0));
        }
    }
}

/// %TEMP%\NopBar\Screenshots.
fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("NopBar").join("Screenshots")
}

/// The record of temporary files we made.
fn made_file() -> PathBuf {
    crate::config::app_dir().join("screenshots-made.json")
}

fn read_made() -> Vec<Made> {
    std::fs::read(made_file()).map(|b| super::parse_made(&String::from_utf8_lossy(&b))).unwrap_or_default()
}

fn write_made(list: &[Made]) {
    let p = made_file();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_file_name("screenshots-made.json.tmp");
    if std::fs::write(&tmp, super::made_text(list)).and_then(|_| std::fs::rename(&tmp, &p)).is_err() {
        crate::log!("screenshot: the record of temporary files was not saved");
        let _ = std::fs::remove_file(&tmp);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A file's size and modification time (ms since 1970), as cleanup
/// compares them with what we wrote.
fn stamp(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(p).ok().filter(|m| m.is_file())?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64;
    Some((m.len(), t))
}

/// The local time as a screenshot file name.
fn base_name() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    super::file_name(t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Writes `bytes` under a fresh name in `dir` (create_new: an existing
/// file is never taken or replaced; a name in use gets a suffix and is
/// tried again). The path made.
fn write_fresh(dir: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let base = base_name();
    for k in 0..1000 {
        let path = dir.join(super::nth_name(&base, k));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
                    drop(f);
                    // Ours, half written: gone again.
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name"))
}

/// Pictures\Screenshots (Windows' own folder for them).
fn screenshots_dir() -> Option<PathBuf> {
    unsafe {
        for id in [&FOLDERID_Screenshots, &FOLDERID_Pictures] {
            if let Ok(p) = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
                let s = p.to_string().ok();
                CoTaskMemFree(Some(p.0 as *const _));
                if let Some(s) = s {
                    let mut dir = PathBuf::from(s);
                    if *id == FOLDERID_Pictures {
                        dir.push("Screenshots");
                    }
                    return Some(dir);
                }
            }
        }
    }
    None
}

/// The PNG of screenshot `gen` in session `s`, if it is still the one.
fn png_of(s: u64, gen: u64) -> Option<Arc<Vec<u8>>> {
    cur().as_ref().filter(|c| c.session == s && c.gen == gen && live(s)).map(|c| c.png.clone())
}

/// Worker thread.
fn work(job: Job) {
    match job {
        Job::Read(s, seq) => {
            if !live(s) {
                return;
            }
            let Some(raw) = image::read(seq) else { return };
            if !live(s) {
                return;
            }
            let Some(d) = image::decode_raw(raw) else {
                crate::log!("screenshot: the picture could not be read");
                return;
            };
            {
                // Stored only if the session is still the one (checked
                // under the same lock forget() empties).
                let mut g = cur();
                if !live(s) {
                    return;
                }
                let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
                *g = Some(Current { session: s, gen, key: format!("screenshot:{gen}"), png: d.png, thumb: d.thumb, temp: None, saved: false, copied: false });
            }
            crate::log!("screenshot: a new one ({}x{})", d.w, d.h);
            post(WM_SHOT_READY, 0);
        }
        Job::Save(s, gen) => {
            let Some(png) = png_of(s, gen) else { return };
            let Some(dir) = screenshots_dir() else {
                crate::log!("screenshot: no Pictures folder");
                return;
            };
            match write_fresh(&dir, &png) {
                Ok(_) => {
                    crate::log!("screenshot: saved to the Screenshots folder");
                    if let Some(c) = cur().as_mut().filter(|c| c.session == s && c.gen == gen) {
                        c.saved = true;
                    }
                    post(WM_SHOT_DONE, 0);
                }
                Err(e) => crate::log!("screenshot: not saved ({e})"),
            }
        }
        Job::Copy(s, gen) => {
            let Some(png) = png_of(s, gen) else { return };
            if let Some(dib) = image::dib_from_png(&png) {
                let mut out = COPY_OUT.lock().unwrap_or_else(|e| e.into_inner());
                if live(s) {
                    *out = Some((s, gen, dib));
                    drop(out);
                    post(WM_SHOT_COPY, 0);
                }
            }
        }
        Job::Temp(s, gen, after) => {
            if let Some(path) = ensure_temp(s, gen) {
                let mut done = TEMP_DONE.lock().unwrap_or_else(|e| e.into_inner());
                if live(s) {
                    done.push((s, gen, path, after));
                    drop(done);
                    post(WM_SHOT_TEMP, 0);
                }
            }
        }
        Job::Cleanup => cleanup(),
    }
}

/// The temporary file of screenshot `gen` (session `s`): the one made
/// before if it is still there as we wrote it, else a new one (any
/// thread). Its exact path, or None.
fn ensure_temp(s: u64, gen: u64) -> Option<PathBuf> {
    let _one = TEMP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (png, before) = {
        let g = cur();
        let c = g.as_ref().filter(|c| c.session == s && c.gen == gen && live(s))?;
        (c.png.clone(), c.temp.clone())
    };
    if let Some(p) = before {
        if stamp(&p).is_some_and(|(len, _)| len == png.len() as u64) {
            return Some(p);
        }
    }
    let path = match write_fresh(&temp_dir(), &png) {
        Ok(p) => p,
        Err(e) => {
            crate::log!("screenshot: temporary file not written ({e})");
            return None;
        }
    };
    // Written down before anyone can use it: only recorded files are ever
    // deleted, and only while unchanged.
    if let (Some(name), Some((size, modified_ms))) = (path.file_name().map(|n| n.to_string_lossy().into_owned()), stamp(&path)) {
        let _rec = MADE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut list = read_made();
        list.retain(|m| m.name != name);
        list.push(Made { name, created: now_secs(), size, modified_ms });
        write_made(&list);
    }
    if let Some(c) = cur().as_mut().filter(|c| c.session == s && c.gen == gen) {
        c.temp = Some(path.clone());
    }
    Some(path)
}

/// Deletes the temporary files we made that are a day old, unchanged since
/// we wrote them; forgets records of files that are
/// gone or were changed by someone else (those are never deleted). Worker
/// thread.
fn cleanup() {
    let _rec = MADE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let list = read_made();
    if list.is_empty() {
        return;
    }
    let dir = temp_dir();
    let now = now_secs();
    let (mut keep, mut gone) = (Vec::new(), 0);
    for m in list {
        let path = dir.join(&m.name);
        match super::fate(&m, now, stamp(&path)) {
            Fate::Keep => keep.push(m),
            Fate::Forget => {}
            Fate::Delete => {
                if std::fs::remove_file(&path).is_ok() {
                    gone += 1;
                } else {
                    keep.push(m);
                }
            }
        }
    }
    write_made(&keep);
    if gone > 0 {
        crate::log!("screenshot: {gone} old temporary file(s) deleted");
    }
}

// ---------------------------------------------------------------- UI thread

/// Turns the watcher on or off (Settings > Top bar > Screenshots, with the
/// island on, primary bar).
pub fn set_enabled(on: bool) {
    let running = WATCH.load(Ordering::SeqCst) != 0;
    if on == running {
        return;
    }
    // A new session either way: work of the old one is dropped.
    let s = SESSION.fetch_add(1, Ordering::SeqCst) + 1;
    if on {
        unsafe {
            let Ok(hinst) = GetModuleHandleW(None) else { return };
            let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: w!("NopBarShotWatch"), ..Default::default() };
            RegisterClassExW(&wc);
            let Ok(h) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("NopBarShotWatch"), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None) else {
                crate::log!("screenshot: no watcher window");
                return;
            };
            if let Err(e) = AddClipboardFormatListener(h) {
                crate::log!("screenshot: cannot listen to the clipboard ({e})");
                let _ = DestroyWindow(h);
                return;
            }
            WATCH.store(h.0 as isize, Ordering::SeqCst);
        }
        // What is on the clipboard already is not new.
        DET.with(|d| {
            let mut d = d.borrow_mut();
            *d = Detector::default();
            d.decide(unsafe { GetClipboardSequenceNumber() }, super::Look::default());
        });
        crate::log!("screenshot: watching for new screenshots");
        cleanup_soon();
        schedule_cleanup(s);
    } else {
        let h = WATCH.swap(0, Ordering::SeqCst);
        unsafe {
            let _ = RemoveClipboardFormatListener(HWND(h as *mut _));
            let _ = DestroyWindow(HWND(h as *mut _));
        }
        forget();
        COPY_OUT.lock().unwrap_or_else(|e| e.into_inner()).take();
        TEMP_DONE.lock().unwrap_or_else(|e| e.into_inner()).clear();
        // One more look at what is old (no timer runs while off).
        cleanup_soon();
        crate::log!("screenshot: watching stopped");
    }
}

/// The next periodic cleanup of session `s` (on the service thread's
/// timer; it only posts to the watcher, which stops the chain when the
/// session is over).
fn schedule_cleanup(s: u64) {
    let _ = crate::service::after("screenshot-cleanup", CLEAN_EVERY, move || {
        if live(s) {
            post(WM_SHOT_CLEAN, s as usize);
        }
    });
}

/// NopBar is quitting.
pub fn shutdown() {
    set_enabled(false);
}

/// The screenshot goes from the island and from memory.
fn forget() {
    *cur() = None;
    crate::topbar::island::set_screenshot(None);
}

/// Deletes old temporary screenshots soon (on the worker; only when we
/// made some).
fn cleanup_soon() {
    if made_file().exists() {
        send(Job::Cleanup);
    }
}

/// Is the island busy with the screenshot (its card out, a drag)?
pub fn busy() -> bool {
    DRAGGING.load(Ordering::SeqCst) || crate::topbar::island::card_open()
}

/// What the card shows for the screenshot with island key `key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ShotState {
    pub saved: bool,
    pub copied: bool,
}

pub fn state(key: &str) -> Option<ShotState> {
    cur().as_ref().filter(|c| c.key == key).map(|c| ShotState { saved: c.saved, copied: c.copied })
}

/// The thumbnail (w, h, premultiplied BGRA) of the screenshot `key`.
pub fn thumb_for(key: &str) -> Option<(u32, u32, Arc<Vec<u8>>)> {
    cur().as_ref().filter(|c| c.key == key).map(|c| c.thumb.clone())
}

fn current() -> Option<(u64, u64)> {
    cur().as_ref().map(|c| (c.session, c.gen))
}

/// A card button: 1 save, 2 copy, 4 open in the image editor.
pub fn act(k: usize) {
    let Some((s, gen)) = current() else { return };
    match k {
        1 => send(Job::Save(s, gen)),
        2 => send(Job::Copy(s, gen)),
        4 => send(Job::Temp(s, gen, AFTER_EDIT)),
        _ => {}
    }
}

/// Drags the screenshot out as a PNG file (its temporary file is made
/// now). Runs a modal loop: call with nothing borrowed.
pub fn drag_out() {
    let Some((s, gen)) = current() else { return };
    let Some(path) = ensure_temp(s, gen) else { return };
    DRAGGING.store(true, Ordering::SeqCst);
    let effect = crate::dragout::drag_own_files(&[(path.to_string_lossy().into_owned(), false)]);
    DRAGGING.store(false, Ordering::SeqCst);
    crate::log!("screenshot: dragged out, effect {effect}");
}

/// "Copy": the picture back on the clipboard (a DIB and the PNG), told
/// apart from a new screenshot by its sequence number.
unsafe fn copy_now(s: u64, gen: u64, dib: Vec<u8>, hwnd: HWND) {
    let Some(png) = png_of(s, gen) else { return };
    DET.with(|d| d.borrow_mut().writing(true, None));
    let mut ok = false;
    if OpenClipboard(Some(hwnd)).is_ok() {
        if EmptyClipboard().is_ok() {
            ok = put(CF_DIB, &dib) & put(image::cf_png(), &png);
        }
        let _ = CloseClipboard();
    }
    let seq = GetClipboardSequenceNumber();
    DET.with(|d| d.borrow_mut().writing(false, Some(seq)));
    if ok {
        if let Some(c) = cur().as_mut().filter(|c| c.session == s && c.gen == gen) {
            c.copied = true;
        }
        crate::topbar::island::poke_screenshot();
    } else {
        crate::log!("screenshot: not copied (the clipboard was busy)");
    }
}

/// One clipboard format from `bytes` (the clipboard owns the memory then).
unsafe fn put(fmt: u32, bytes: &[u8]) -> bool {
    let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { return false };
    let p = GlobalLock(g) as *mut u8;
    if p.is_null() {
        let _ = GlobalFree(Some(g));
        return false;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    let _ = GlobalUnlock(g);
    if SetClipboardData(fmt, Some(HANDLE(g.0))).is_ok() {
        true
    } else {
        let _ = GlobalFree(Some(g));
        false
    }
}

fn now_ms() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

/// Runs a finished temporary file's action: on the exact file made for
/// that request (never whatever is current now).
unsafe fn after_temp(path: &Path, after: usize) {
    let p = path.to_string_lossy().into_owned();
    match after {
        AFTER_EDIT => {
            // Windows' editor for PNG files (Paint unless the user picked
            // another); a viewer if none.
            let result = crate::launch::environment::launch("edit", &p, "", "")
                .or_else(|_| crate::launch::environment::launch("open", &p, "", ""));
            if let Err(code) = result { crate::log!("screenshot: no editor opened ({code})"); }
        }
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            let wait = DET.with(|d| d.borrow_mut().changed(now_ms()));
            SetTimer(Some(hwnd), DEBOUNCE_TIMER, wait as u32, None);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == DEBOUNCE_TIMER => {
            let _ = KillTimer(Some(hwnd), DEBOUNCE_TIMER);
            match DET.with(|d| d.borrow_mut().ready(now_ms())) {
                Err(more) => {
                    SetTimer(Some(hwnd), DEBOUNCE_TIMER, more.max(1) as u32, None);
                }
                Ok(()) => {
                    let seq = GetClipboardSequenceNumber();
                    let look = image::look();
                    if DET.with(|d| d.borrow_mut().decide(seq, look)) {
                        send(Job::Read(session(), seq));
                    }
                }
            }
            LRESULT(0)
        }
        WM_SHOT_READY => {
            let key = cur().as_ref().filter(|c| live(c.session)).map(|c| c.key.clone());
            if let Some(k) = key {
                crate::topbar::island::set_screenshot(Some(Shot { path: k }));
                SetTimer(Some(hwnd), EXPIRE_TIMER, super::SHOW_MS, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == EXPIRE_TIMER => {
            if busy() {
                SetTimer(Some(hwnd), EXPIRE_TIMER, BUSY_MS, None);
            } else {
                let _ = KillTimer(Some(hwnd), EXPIRE_TIMER);
                forget();
            }
            LRESULT(0)
        }
        WM_SHOT_COPY => {
            let got = COPY_OUT.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some((s, gen, dib)) = got.filter(|g| live(g.0)) {
                copy_now(s, gen, dib, hwnd);
            }
            LRESULT(0)
        }
        WM_SHOT_TEMP => {
            let done = std::mem::take(&mut *TEMP_DONE.lock().unwrap_or_else(|e| e.into_inner()));
            // Each on its own file, for the request that made it.
            for (_, _, path, after) in done.into_iter().filter(|d| live(d.0)) {
                after_temp(&path, after);
            }
            LRESULT(0)
        }
        WM_SHOT_DONE => {
            crate::topbar::island::poke_screenshot();
            LRESULT(0)
        }
        WM_SHOT_CLEAN => {
            let s = wp.0 as u64;
            if live(s) {
                cleanup_soon();
                schedule_cleanup(s);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
