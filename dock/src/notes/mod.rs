//! Desktop notes: sticky notes that live on the desktop
//! (docs/superpowers/specs/2026-11-08-desktop-notes-design.md).
//!
//! Usually on the desktop layer (crate::desklayer: behind every app
//! window, above the icons, through Show desktop). The shortcut brings
//! them all to the front with the last one used focused; Esc, the shortcut
//! again or a click elsewhere sends them back. A click on a note on the
//! desktop edits it there. + makes one beside it, × deletes (no question;
//! Ctrl+Z brings it back while summoned, for this session). Checklists:
//! `- [ ]` lines show as boxes that toggle on a click.
//!
//! Text is a RichEdit (Windows' own editing, IME, Undo, clipboard), saved
//! as Markdown (crate::notes::store). The notes run on a thread of their
//! own (crate::notes::hub), so the dock never waits on them nor on the
//! desktop's window; nothing runs while nothing changes.
//!
//! Each virtual desktop has its own notes (crate::notes::desks): a note
//! belongs to the desktop it was made on, or to every desktop (its menu's
//! one row); the others' notes are hidden until their desktop comes back.
//! Settings > widgets can turn that off (every note on every desktop).
//!
//! The dock calls: `init` (after the theme is known), `retheme`,
//! `apply_hotkey` (the "notes" shortcut, crate::shortcuts) and `shutdown`.

pub mod check;
pub mod desks;
pub(crate) mod face;
mod hub;
#[cfg(test)]
mod model;
pub(crate) mod note;
mod richedit;
mod textbox;
#[cfg(test)]
mod shots;
pub mod store;
pub mod time;
pub mod inbox;
pub mod reminders;
pub(crate) mod reminder_delivery;
pub mod toast;
pub(crate) mod runtime;
pub(crate) mod widgets;
mod capture;
#[cfg(test)] mod capture_snapshots;

static WIDGETS: Mutex<Option<crate::config::WidgetsCfg>> = Mutex::new(None);
static PANEL_THEME: Mutex<Option<Theme>> = Mutex::new(None);
pub fn configure(c:&crate::config::WidgetsCfg) { *WIDGETS.lock().unwrap_or_else(|e|e.into_inner())=Some(c.clone()); post(hub::WM_REMIND); }
pub fn preferences()->crate::config::WidgetsCfg {WIDGETS.lock().unwrap_or_else(|e|e.into_inner()).clone().unwrap_or_default()}
fn panel_theme()->Theme {PANEL_THEME.lock().unwrap_or_else(|e|e.into_inner()).clone().unwrap_or_else(Theme::graphite)}

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK};

use crate::theme::{Rgba, Theme};

/// The shortcut's RegisterHotKey id (on the hub window).
pub const HOTKEY_ID: i32 = 0x4E54;

/// What the notes are drawn with, taken from the theme on the dock's
/// thread (the notes' thread never touches the theme).
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// The card (opaque).
    pub bg: Rgba,
    pub text: Rgba,
    /// + and ×.
    pub dim: Rgba,
    pub light: bool,
    pub high_contrast: bool,
    /// The card radius (DIP): rounded corners by DWM.
    pub radius: f32,
    pub family: String,
    /// Text size (DIP).
    pub size: f32,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            bg: Rgba { r: 38, g: 38, b: 40, a: 255 },
            text: Rgba { r: 235, g: 235, b: 240, a: 255 },
            dim: Rgba { r: 150, g: 150, b: 155, a: 255 },
            light: false,
            high_contrast: false,
            radius: crate::desklayer::widget::RADIUS,
            family: "Segoe UI".into(),
            size: crate::theme::ramp::BODY.size,
        }
    }
}

impl Look {
    /// Pure: the notes' look for a resolved theme: the panels' surface
    /// (popup) as an opaque card, its text and secondary text, the card
    /// radius, the UI font at the body size and the user's text scale.
    /// Glass: the same card, opaque (RichEdit draws text with GDI, which
    /// cannot sit on DWM's blur), so the finish shows in the color only.
    pub fn of(t: &Theme) -> Look {
        let (bg, text, dim) = match t.hc {
            Some(h) if t.high_contrast => (h.window, h.text, h.text),
            _ => (crate::desklayer::card::surface(t.is_light), Rgba { a: 255, ..t.ink() }, Rgba { a: 255, ..t.text_dim(false) }),
        };
        Look {
            bg,
            text,
            dim,
            light: t.is_light,
            high_contrast: t.high_contrast,
            radius: crate::desklayer::widget::RADIUS,
            family: crate::fonts::map(&t.font.family).into_owned(),
            size: crate::theme::ramp::BODY.size * if t.font.scale.is_finite() && t.font.scale > 0.0 { t.font.scale } else { 1.0 },
        }
    }
}

static HUB: AtomicIsize = AtomicIsize::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
static LOOK: Mutex<Option<Look>> = Mutex::new(None);
/// The shortcut in force: (window it is registered on, spec, registered).
static HOTKEY: Mutex<Option<(isize, String, bool)>> = Mutex::new(None);
static HEADLESS: AtomicBool = AtomicBool::new(false);
/// Settings > widgets > "Each desktop its own notes".
static PER_DESKTOP: AtomicBool = AtomicBool::new(true);

/// Each virtual desktop shows its own notes (else every note shows on
/// every desktop).
pub fn per_desktop() -> bool {
    PER_DESKTOP.load(Ordering::SeqCst)
}

/// The setting (Settings > widgets), from the dock's thread: in force at
/// once if the notes are running, else when they start.
pub fn set_per_desktop(on: bool) {
    if PER_DESKTOP.swap(on, Ordering::SeqCst) != on {
        post(hub::WM_DESKS);
    }
}

/// The hub, for other threads (0 until the notes' thread published it).
fn hub_hwnd() -> HWND {
    HWND(HUB.load(Ordering::SeqCst) as *mut _)
}

/// The notes' thread made its hub (review #11: it publishes it itself,
/// however long init waited).
fn publish(hwnd: HWND) {
    HUB.store(hwnd.0 as isize, Ordering::SeqCst);
}

fn headless() -> bool {
    HEADLESS.load(Ordering::SeqCst)
}

fn post(msg: u32) {
    let h = hub_hwnd();
    if !h.is_invalid() {
        unsafe {
            let _ = PostMessageW(Some(h), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// Sends `msg` to the hub and waits (up to `ms`) for its answer.
fn send(msg: u32, ms: u32) -> Option<usize> {
    send_with(msg, 0, ms)
}

fn send_with(msg: u32, wp: usize, ms: u32) -> Option<usize> {
    let h = hub_hwnd();
    if h.is_invalid() {
        return None;
    }
    let mut r = 0usize;
    let ok = unsafe { SendMessageTimeoutW(h, msg, WPARAM(wp), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, ms, Some(&mut r)) };
    (ok.0 != 0).then_some(r)
}

fn take_look() -> Option<Look> {
    LOOK.lock().ok()?.take()
}


/// The notes' thread is up (or starting).
pub fn running() -> bool {
    STARTED.load(Ordering::SeqCst)
}

/// Notes asked from outside (the desktop's Ctrl+V): (text, screen px).
/// Asked from outside: (text, screen px, still wanted right before making).
type Made = (String, (i32, i32), Box<dyn Fn() -> bool + Send>);
static MAKE: Mutex<Vec<Made>> = Mutex::new(Vec::new());

/// A new note holding `text`, centered at `at` (screen px) on the desktop
/// (crate::desklayer::deskpaste). Made and saved on the notes' thread.
/// False (nothing kept) when the notes are not running.
/// `still` is asked right before the note is made (the request may have
/// expired or been called off while it waited).
pub fn create_at(text: &str, at: (i32, i32), still: Box<dyn Fn() -> bool + Send>) -> bool {
    let h = hub_hwnd();
    if h.is_invalid() {
        return false;
    }
    if let Ok(mut m) = MAKE.lock() {
        m.push((text.to_string(), at, still));
    }
    let posted = unsafe { PostMessageW(Some(h), hub::WM_MAKE, WPARAM(0), LPARAM(0)) }.is_ok();
    if !posted {
        // Nothing will take it: not kept.
        let _ = take_made();
    }
    posted
}

fn take_made() -> Vec<Made> {
    MAKE.lock().map(|mut m| std::mem::take(&mut *m)).unwrap_or_default()
}

/// Starts the notes' thread (once): loads the notes onto the desktop and
/// registers the shortcut asked for so far.
pub fn init(theme: &Theme) {
    start(theme, Duration::from_secs(2));
}

/// init, waiting at most `wait` for the thread to say it is up. The
/// thread works either way: it publishes its hub itself.
fn start(theme: &Theme, wait: Duration) {
    *PANEL_THEME.lock().unwrap_or_else(|e|e.into_inner())=Some(theme.clone());
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Ok(mut l) = LOOK.lock() {
        *l = Some(Look::of(theme));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("notes".into()).spawn(move || hub::run(tx));
    if spawned.is_err() {
        crate::log!("notes: no thread");
        STARTED.store(false, Ordering::SeqCst);
        return;
    }
    if rx.recv_timeout(wait).is_err() {
        crate::log!("notes: the thread is slow to start; it carries on and publishes itself");
    }
}

/// The theme (light / dark, finish, font, high contrast) changed.
pub fn retheme(theme: &Theme) {
    *PANEL_THEME.lock().unwrap_or_else(|e|e.into_inner())=Some(theme.clone());
    let look = Look::of(theme);
    if let Ok(mut l) = LOOK.lock() {
        *l = Some(look);
    }
    post(hub::WM_LOOK);
}

/// Shortcut "notes" (crate::shortcuts): registered on `owner` (the
/// dock's window), on the caller's thread, like every other shortcut:
/// when this returns the old key is given back and the new one is
/// registered (or its failure recorded), so moving a key to another
/// shortcut and starting a recording in Settings work in order. A press
/// comes to the dock's window, which calls `hotkey_pressed`.
pub fn apply_hotkey(owner: HWND, spec: &str) {
    use crate::hotkeys::{record, Status, NOTES};
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
    let Ok(mut cur) = HOTKEY.lock() else { return };
    let me = owner.0 as isize;
    if cur.as_ref().is_some_and(|(o, s, ok)| *o == me && s == spec && *ok) {
        return;
    }
    if let Some((old, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), HOTKEY_ID);
        }
    }
    let ok = match crate::search::hotkey::parse(spec) {
        Ok(None) => {
            record(NOTES, spec, Status::Off);
            false
        }
        Ok(Some((mods, key))) => match unsafe { RegisterHotKey(Some(owner), HOTKEY_ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, key) } {
            Ok(()) => {
                crate::log!("notes: hotkey {spec}");
                record(NOTES, spec, Status::Ok);
                true
            }
            Err(e) => {
                crate::log!("notes: hotkey {spec} not registered ({e})");
                record(NOTES, spec, Status::from_error(&e));
                false
            }
        },
        Err(e) => {
            crate::log!("notes: bad hotkey {e}");
            record(NOTES, spec, Status::Invalid);
            false
        }
    };
    *cur = Some((me, spec.to_string(), ok));
}

/// Gives the shortcut back (the dock's window is going away).
pub fn release_hotkey() {
    let Ok(mut cur) = HOTKEY.lock() else { return };
    if let Some((owner, _, true)) = cur.take() {
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(Some(HWND(owner as *mut _)), HOTKEY_ID);
        }
    }
}

/// The shortcut was pressed (the dock's WM_HOTKEY): the notes' thread
/// may take the foreground for it, then summons or sends back.
pub fn hotkey_pressed() {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(windows::Win32::System::Threading::GetCurrentProcessId());
    }
    post(hub::WM_TOGGLE);
}

/// Shared entry point; all UI surfaces dispatch to the notes thread.
pub fn new_note() -> bool {
    if !running() { return false; }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(windows::Win32::System::Threading::GetCurrentProcessId());
        let h = hub_hwnd();
        !h.is_invalid() && PostMessageW(Some(h), hub::WM_NEW, WPARAM(0), LPARAM(0)).is_ok()
    }
}

/// Who is quitting: decides whether the notes may hold it up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Quit {
    /// The user, in NopBar (tray or dock menu): called off while a text
    /// can be kept nowhere.
    User,
    /// Another process (Settings, an installer's --quit, the updater, a
    /// GPU restart): never held up.
    External,
    /// An automatic update about to start: put off once.
    Update,
    /// The same update the next time: goes on.
    UpdateAgain,
}

impl From<usize> for Quit {
    fn from(v: usize) -> Quit {
        match v {
            0 => Quit::User,
            2 => Quit::Update,
            3 => Quit::UpdateAgain,
            _ => Quit::External,
        }
    }
}

/// Pure: may `kind` go on with `lost` notes whose text could be kept
/// nowhere (neither its file nor a recovery copy)?
pub fn may_go(kind: Quit, lost: usize) -> bool {
    lost == 0 || matches!(kind, Quit::External | Quit::UpdateAgain)
}

/// Pure: may `kind` go on when the notes' thread does not answer? The
/// user's own quit is called off (nothing is known to be safe); others
/// go on (an installer or update must not wait forever: the known limit).
pub fn on_no_answer(kind: Quit) -> bool {
    kind != Quit::User
}

/// An update was put off for the notes (the next one goes on).
static UPDATE_PUT_OFF: AtomicBool = AtomicBool::new(false);

/// Asks the notes before anything goes (the dock is still there): every
/// note is saved now. False: stay (the notes are shown); only for the
/// user's own quit or an update the first time, and only while a text
/// (or the layout) can be kept nowhere. A notes' thread that does not
/// answer in 3 s: the user's own quit is called off (nothing is known to
/// be safe); any other goes on (see the known limits in the spec).
pub fn prepare_quit(kind: Quit) -> bool {
    if hub_hwnd().is_invalid() {
        return true;
    }
    let kind = match kind {
        Quit::Update if UPDATE_PUT_OFF.load(Ordering::SeqCst) => Quit::UpdateAgain,
        k => k,
    };
    let go = match send_with(hub::WM_PREPARE_QUIT, kind as usize, 3_000) {
        Some(r) => r != 0,
        None => {
            let go = on_no_answer(kind);
            crate::log!("notes: no answer before quitting; {}", if go { "going on" } else { "the quit is called off" });
            go
        }
    };
    if matches!(kind, Quit::Update | Quit::UpdateAgain) {
        UPDATE_PUT_OFF.store(!go, Ordering::SeqCst);
    }
    go
}

/// The dock is gone: saves what can be and closes the notes.
pub fn shutdown() {
    if hub_hwnd().is_invalid() {
        return;
    }
    if send(hub::WM_SHUTDOWN, 5_000).is_none() {
        crate::log!("notes: no answer at quit");
    }
    WIDGET_REQUESTS.lock().unwrap_or_else(|e|e.into_inner()).hub=0;
    HUB.store(0, Ordering::SeqCst);
    // Asked but not made: dropped with the notes.
    let _ = take_made();
    // Closed: they may be started again (Settings > widgets).
    STARTED.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_look_follows_the_theme() {
        let sys = |light| crate::syslook::SysLook { light, accent: Rgba { r: 0, g: 120, b: 215, a: 255 }, transparency: true, high_contrast: None, animations: true, ui_font: 0 };
        let t = Theme::graphite().resolve_fx(&sys(true), false);
        let l = Look::of(&t);
        assert!(l.light);
        assert_eq!(l.bg.a, 255);
        assert_eq!(l.radius, crate::desklayer::widget::RADIUS);
        assert!(!l.family.is_empty());
        let dark = Look::of(&Theme::graphite().resolve_fx(&sys(false), false));
        assert!(!dark.light);
        assert_ne!(l.bg, dark.bg);
        // Text reads on the card in both.
        for x in [&l, &dark] {
            let lum = |c: Rgba| crate::theme::luminance(c);
            assert!((lum(x.bg) - lum(x.text)).abs() > 0.4, "{x:?}");
        }
    }

    /// The notes' thread from start to quit, on a temporary folder, its
    /// windows never shown: it publishes itself even when init stops
    /// waiting at once (review #11); a shortcut change is in force when
    /// apply returns (review #7); quitting saves what the editor holds even
    /// though its "changed" message is still queued behind the quit
    /// (review #4) and confirms only after it is on disk (review #5).
    #[test]
    fn the_notes_thread_lives_its_whole_life() {
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId, SendMessageW};
        let dir = std::env::temp_dir().join(format!("nopbar-notes-life-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let e = store::Entry { id: "n1".into(), place: crate::desklayer::place::Place { mon: [-32000, -32000, -31000, -31000], ..Default::default() }, desktop: None, size:None, hidden:false };
        let mut disk = store::RealDisk { base: dir.clone(), rescue: dir.join("rescue-local") };
        let mut doc = store::Doc { id: "n1".into(), text: "before".into(), ..Default::default() };
        assert_eq!(doc.settle(&mut disk, 0), store::Kept::Saved);
        assert!(store::write_index(&mut disk, &store::Index { version: 1, seeded: false, notes: vec![e], last: "n1".into() }, &store::Index::default()).is_some());
        store::set_dir(dir.clone());
        HEADLESS.store(true, Ordering::SeqCst);
        start(&Theme::graphite(), Duration::ZERO);
        let t0 = std::time::Instant::now();
        while hub_hwnd().is_invalid() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!hub_hwnd().is_invalid(), "the hub published itself");
        // The shortcut (review #7): registered on the caller's window on
        // the caller's thread; given back before apply returns, so another
        // window can take the same key at once.
        use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE};
        let mk = || unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), windows::core::w!("STATIC"), None, WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None) }.unwrap();
        let (dock, other) = (mk(), mk());
        let spec = "ctrl+alt+shift+f24";
        apply_hotkey(dock, spec);
        let st = crate::hotkeys::parse(&crate::hotkeys::snapshot());
        assert!(st.iter().any(|(k, s, _)| k == crate::hotkeys::NOTES && s == spec), "{st:?}");
        let registered = HOTKEY.lock().unwrap().as_ref().is_some_and(|(_, _, ok)| *ok);
        apply_hotkey(dock, "none");
        if registered {
            let c = crate::shortcuts::parse(spec).unwrap().unwrap();
            assert!(unsafe { RegisterHotKey(Some(other), 0x77, HOT_KEY_MODIFIERS(c.mods) | MOD_NOREPEAT, c.vk) }.is_ok(), "the key is free when apply returns");
            unsafe {
                let _ = UnregisterHotKey(Some(other), 0x77);
            }
        }
        let st = crate::hotkeys::parse(&crate::hotkeys::snapshot());
        assert!(st.iter().any(|(k, s, x)| k == crate::hotkeys::NOTES && s == "none" && *x == crate::hotkeys::Status::Off));
        unsafe {
            let _ = DestroyWindow(dock);
            let _ = DestroyWindow(other);
        }
        // Change the loaded note's text from here, then quit at once.
        // The hub's own note (other tests make notes on their threads).
        let hub_thread = unsafe { GetWindowThreadProcessId(hub_hwnd(), None) };
        let mut note_hwnd = HWND::default();
        for _ in 0..300 {
            let mut after: Option<HWND> = None;
            while let Ok(n) = unsafe { FindWindowExW(None, after, note::CLASS, windows::core::PCWSTR::null()) } {
                if unsafe { GetWindowThreadProcessId(n, None) } == hub_thread {
                    note_hwnd = n;
                    break;
                }
                after = Some(n);
            }
            if !note_hwnd.is_invalid() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!note_hwnd.is_invalid(), "the note was loaded");
        // Typed on the card (its RichEdit has no window): as if typed.
        let text: Vec<u16> = "after \u{2610} 한글".encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            SendMessageW(note_hwnd, note::WM_TYPE_TEST, Some(WPARAM(0)), Some(LPARAM(text.as_ptr() as isize)));
        }
        assert!(prepare_quit(Quit::User), "every note saved: the quit goes on");
        shutdown();
        let loaded = store::load(&mut store::RealDisk { base: dir.clone(), rescue: dir.join("rescue-local") }, store::now_ms());
        // The text file keeps its name.
        assert_eq!(std::fs::read_to_string(dir.join("n1.md")).unwrap(), "after \u{2610} 한글");
        assert_eq!(loaded.notes.len(), 1);
        assert_eq!(loaded.notes[0].1, "after \u{2610} 한글");
        assert!(!dir.join("recovery").exists() || std::fs::read_dir(dir.join("recovery")).unwrap().next().is_none());
        assert!(hub_hwnd().is_invalid());
        // The real hub protocol, always hidden and on this temporary folder.
        // With history but no cards, shortcut summon creates a disposable card.
        let mut disk = store::RealDisk { base: dir.clone(), rescue: dir.join("rescue-local") };
        let mut doc = store::Doc { id: "n1".into(), text: loaded.notes[0].1.clone(), saved: Some(loaded.notes[0].1.clone()), kept: None };
        assert!(store::delete(&mut disk, &mut doc, store::now_ms()));
        start(&Theme::graphite(), Duration::from_secs(2));
        assert_eq!(send(hub::WM_TOGGLE, 5000), Some(0));
        assert!(prepare_quit(Quit::User));
        assert_eq!(store::load(&mut disk, store::now_ms()).notes.len(), 1);
        assert_eq!(send(hub::WM_TOGGLE, 5000), Some(0));
        assert!(prepare_quit(Quit::User));
        assert!(store::load(&mut disk, store::now_ms()).notes.is_empty());
        // Entry commands always add one, including when already summoned.
        assert_eq!(send(hub::WM_NEW, 5000), Some(0));
        assert_eq!(send(hub::WM_NEW, 5000), Some(0));
        assert_eq!(send(hub::WM_TOGGLE, 5000), Some(0));
        assert!(prepare_quit(Quit::User));
        shutdown();
        let entry = store::load(&mut disk, store::now_ms());
        assert_eq!(entry.notes.len(), 2);
        assert!(entry.notes.iter().all(|(_, text)| text.is_empty()));
        assert!(entry.index.seeded);
        assert!(!new_note(), "a disabled/stopped widget rejects stale UI requests");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S3 / S4: only the user's own quit and an update the first time are
    /// held up, and only while a text can be kept nowhere; installers and
    /// other processes never are; an update put off once goes on next time.
    #[test]
    fn who_may_be_held_up() {
        for k in [Quit::User, Quit::External, Quit::Update, Quit::UpdateAgain] {
            assert!(may_go(k, 0), "{k:?} with every text kept");
            assert_eq!(Quit::from(k as usize), k);
        }
        assert!(!may_go(Quit::User, 1));
        assert!(!may_go(Quit::Update, 1));
        assert!(may_go(Quit::UpdateAgain, 1));
        assert!(may_go(Quit::External, 3));
        // No answer from the notes' thread: only the user's quit waits.
        assert!(!on_no_answer(Quit::User));
        assert!(on_no_answer(Quit::External) && on_no_answer(Quit::Update) && on_no_answer(Quit::UpdateAgain));
        assert_eq!(Quit::from(99), Quit::External);
    }

    #[test]
    fn the_shortcut_is_in_the_registry() {
        let s = crate::shortcuts::find(crate::shortcuts::NOTES).expect("notes shortcut");
        assert_eq!(s.kind, crate::shortcuts::Kind::Chord);
        assert_eq!(s.scope, crate::shortcuts::Scope::Global);
        assert_eq!(s.area, crate::shortcuts::Area::Open);
        assert!(s.apply.is_some());
        let c = crate::shortcuts::parse(s.default).unwrap().expect("on by default");
        assert!(c.mods != 0);
        // Not another shortcut's default.
        for o in crate::shortcuts::ALL.iter().filter(|o| o.id != s.id) {
            assert_ne!(crate::shortcuts::parse(o.default).ok().flatten(), Some(c), "{} has it", o.id);
        }
        assert_eq!(crate::shortcuts::row_key(s.id), crate::hotkeys::NOTES);
        for key in [s.label, s.hint, "notes.window", "notes.new", "notes.delete"] {
            assert!(crate::i18n::get(key).is_some(), "{key}");
        }
    }
}

static HIDDEN_WIDGETS:Mutex<Vec<(String,String)>>=Mutex::new(Vec::new());
pub fn hidden_widgets()->Vec<(String,String)>{HIDDEN_WIDGETS.lock().unwrap_or_else(|e|e.into_inner()).clone()}
pub fn widget_action(action:crate::desklayer::edit::Action){
 let mut q=WIDGET_REQUESTS.lock().unwrap_or_else(|e|e.into_inner());
 if let Some((h,action))=q.push(action){crate::desklayer::edit::send(HWND(h as *mut _),action);}
}

static HIDDEN_PREVIEWS:Mutex<Vec<(String,String)>>=Mutex::new(Vec::new());
pub fn hidden_widget_preview(id:&str)->Option<crate::desklayer::edit::Preview>{HIDDEN_PREVIEWS.lock().unwrap_or_else(|e|e.into_inner()).iter().find(|(key,_)|key==id).map(|(_,text)|crate::desklayer::edit::Preview::Note(text.clone(),String::new()))}


#[cfg(test)] mod widget_review_tests {
 use super::*;
 use crate::desklayer::{edit::{Action,Kind},widget::Size};
 #[test] fn review_09_add_waits_until_hub_is_ready_and_drains_once(){
  let mut q=WidgetRequests::default();
  let add=||Action::Add{kind:Kind::Note,size:Size::LARGE,monitor:1,note:None};
  assert!(q.push(add()).is_none());assert!(q.push(add()).is_none());
  let pending=q.ready(123);assert_eq!(pending.len(),2);assert!(q.ready(123).is_empty());
  assert!(matches!(q.push(add()),Some((123,Action::Add{size:Size::LARGE,..}))));
  q.hub=0;assert!(q.push(add()).is_none());assert_eq!(q.ready(456).len(),1);
 }
}

pub(crate) fn editing_body(h:HWND,p:windows::Win32::Foundation::POINT)->bool {note::editing_body(h,p)}
#[derive(Default)]
struct WidgetRequests {hub:isize,pending:Vec<crate::desklayer::edit::Action>}
impl WidgetRequests {
 fn push(&mut self,action:crate::desklayer::edit::Action)->Option<(isize,crate::desklayer::edit::Action)> {
  if self.hub==0 {self.pending.push(action);None}else{Some((self.hub,action))}
 }
 fn ready(&mut self,hub:isize)->Vec<crate::desklayer::edit::Action>{self.hub=hub;std::mem::take(&mut self.pending)}
}
static WIDGET_REQUESTS:Mutex<WidgetRequests>=Mutex::new(WidgetRequests{hub:0,pending:Vec::new()});
fn widgets_ready(h:HWND){
 let mut q=WIDGET_REQUESTS.lock().unwrap_or_else(|e|e.into_inner());
 for action in q.ready(h.0 as isize){crate::desklayer::edit::send(h,action);}
}
