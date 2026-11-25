//! Ctrl+V on the desktop: text on the clipboard becomes a new sticky note
//! there (crate::notes), a picture a new image piece (crate::pieces), at
//! the pointer. Everything else is Windows' own, untouched.
//!
//! Decided in the keyboard hook (launchpad::wintap, on a thread of its own
//! that only pumps its messages) at the moment Ctrl+V goes down, and only
//! then is the key taken: the window in front is the desktop's (Progman /
//! WorkerW), its focus is known and is not an edit box (an icon being
//! renamed), the clipboard holds no files (CF_HDROP, a file group, shell
//! items) but text or a picture, and the widget that would be made is on.
//! Any other press goes on to Windows as it came: nothing is ever typed
//! again in its place.
//!
//! One press at a time is tracked (Taken): only the repeats and the key-up
//! of a V press we took are taken too, and only while the switch is the
//! same (its generation) and the same window is in front; anything else
//! (the switch turned off and on, another window came forward) forgets it,
//! so another app's V is never taken.
//!
//! The hook asks Windows only what returns at once (the window in front,
//! its focus, whether a clipboard format is there: the clipboard is never
//! opened; the formats' ids are registered before the hook needs them),
//! looks further only for Ctrl+V itself, and posts the request with its
//! time and the clipboard's sequence number. The dock's thread makes the
//! note or piece only if the clipboard is still that and the request is
//! fresh; a request that cannot be posted is not taken.
//!
//! Shortcut "desktop_paste" (Settings > Keyboard) turns it off.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::DataExchange::{GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, PostMessageW, GUITHREADINFO, WM_APP};

/// Posted to the host window (desklayer::host): wParam = Make | (tick ms << 8), lParam =
/// the clipboard's sequence number at the press | (the switch's
/// generation << 32).
pub const WM_DESKPASTE: u32 = WM_APP + 79;
/// A request older than this (ms) is not acted on.
pub const FRESH_MS: u32 = 2000;

/// What the clipboard holds, as far as the desktop's Ctrl+V cares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Formats {
    /// Files (Explorer's copy, a mail attachment, a browser's file).
    pub files: bool,
    pub text: bool,
    pub image: bool,
}

/// What Ctrl+V on the desktop makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Make {
    Note = 1,
    Piece = 2,
    /// Windows' own (files, nothing to make, or that widget is off).
    Leave = 0,
}

/// Pure: files go to Windows; text makes a note (a spreadsheet's cells
/// copy as text and as a picture: the text wins) if the notes are on; a
/// picture alone, a piece if the pieces are on.
pub fn make_of(f: Formats, notes_on: bool, pieces_on: bool) -> Make {
    if f.files {
        Make::Leave
    } else if f.text {
        if notes_on {
            Make::Note
        } else {
            Make::Leave
        }
    } else if f.image && pieces_on {
        Make::Piece
    } else {
        Make::Leave
    }
}

/// A key event as the hook sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Key {
    pub vk: u32,
    pub up: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
}

/// What the world looks like at that moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scene {
    /// The window in front is the desktop's.
    pub desktop: bool,
    /// The focus is in an edit box (an icon being renamed), or could not
    /// be told (then the key is left alone).
    pub renaming: bool,
    pub formats: Formats,
    pub notes_on: bool,
    pub pieces_on: bool,
}

/// What the hook does with the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Not ours: on to Windows as it came.
    Pass,
    /// A repeat or the key-up of a press we took.
    Swallow,
    /// Take it, and make this.
    Take(Make),
}

/// A V press we took: under which switch generation, with which window in
/// front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taken {
    pub gen: u32,
    pub fg: isize,
}

/// Pure: the decision for a Ctrl+V down in `s`.
pub fn decide(s: Scene) -> Step {
    if !s.desktop || s.renaming {
        return Step::Pass;
    }
    match make_of(s.formats, s.notes_on, s.pieces_on) {
        Make::Leave => Step::Pass,
        m => Step::Take(m),
    }
}

/// Pure: one key event through the tracker. `held`: the press we took,
/// if any; `gen`: the switch's generation now; `fg`: the window in front
/// now; `scene`: asked only for a Ctrl+V down. Returns the step and what
/// is held after it.
pub fn track(held: Option<Taken>, k: Key, gen: u32, fg: isize, scene: impl FnOnce() -> Scene) -> (Step, Option<Taken>) {
    // A press taken under another switch generation or another window is
    // forgotten: its rest goes to whoever has the keyboard now.
    let held = held.filter(|t| t.gen == gen && t.fg == fg);
    if k.vk != b'V' as u32 {
        return (Step::Pass, held);
    }
    if k.up {
        return (if held.is_some() { Step::Swallow } else { Step::Pass }, None);
    }
    if held.is_some() {
        // Its repeats.
        return (Step::Swallow, held);
    }
    if !k.ctrl || k.shift || k.alt || k.win {
        return (Step::Pass, None);
    }
    match decide(scene()) {
        Step::Take(m) => (Step::Take(m), Some(Taken { gen, fg })),
        s => (s, None),
    }
}

/// Pure: may a request made at `tick` with the clipboard at `seq` still be
/// acted on at `now` with the clipboard at `seq_now`?
pub fn fresh(tick: u32, now: u32, seq: u32, seq_now: u32) -> bool {
    seq == seq_now && now.wrapping_sub(tick) <= FRESH_MS
}

/// A taken press's request, carried until its note or piece is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ask {
    pub make: usize,
    /// When it was pressed (GetTickCount).
    pub tick: u32,
    /// The clipboard's sequence then.
    pub seq: u32,
    /// The switch's generation then.
    pub gen: u32,
}

impl Ask {
    pub fn pack(&self) -> (usize, isize) {
        (self.make | ((self.tick as usize) << 8), (self.seq as u64 | ((self.gen as u64) << 32)) as isize)
    }

    pub fn unpack(wp: usize, lp: isize) -> Ask {
        Ask { make: wp & 0xFF, tick: (wp >> 8) as u32, seq: lp as u64 as u32, gen: ((lp as u64) >> 32) as u32 }
    }
}

/// Pure: may `ask` still be made at `now`: in time, the switch not turned
/// off (or off and on: another generation) since, and its widget on.
pub fn still(ask: Ask, now: u32, gen_now: u32, widget_on: bool) -> bool {
    widget_on && ask.gen != 0 && ask.gen == gen_now && now.wrapping_sub(ask.tick) <= FRESH_MS
}

/// The switch's generation now (0 while off).
fn gen_now() -> u32 {
    if ENABLED.load(Ordering::SeqCst) && OWNER.load(Ordering::SeqCst) != 0 {
        GEN.load(Ordering::SeqCst)
    } else {
        0
    }
}

/// `ask` may be made right now (checked again right before making).
fn still_now(ask: Ask) -> bool {
    let on = match ask.make {
        1 => NOTES_ON.load(Ordering::SeqCst),
        2 => PIECES_ON.load(Ordering::SeqCst),
        _ => false,
    };
    still(ask, tick(), gen_now(), on)
}

/// The shortcut is on.
static ENABLED: AtomicBool = AtomicBool::new(true);
static NOTES_ON: AtomicBool = AtomicBool::new(false);
static PIECES_ON: AtomicBool = AtomicBool::new(false);
/// The host window (gets WM_DESKPASTE; 0: none, nothing is taken).
static OWNER: AtomicIsize = AtomicIsize::new(0);
/// Bumped whenever the switch or the owner changes: a press taken before
/// is forgotten.
static GEN: AtomicU32 = AtomicU32::new(1);

thread_local! {
    /// Hook thread: the V press now held that we took.
    static HELD: Cell<Option<Taken>> = const { Cell::new(None) };
}

fn bump() {
    GEN.fetch_add(1, Ordering::SeqCst);
}

/// Shortcut "desktop_paste" (crate::shortcuts): "ctrl+v" on, "none" off.
/// The hook runs while this (or another of its uses) is on.
pub fn apply(_owner: HWND, spec: &str) {
    let on = !crate::shortcuts::is_off(spec);
    if ENABLED.swap(on, Ordering::SeqCst) != on {
        bump();
    }
    use crate::hotkeys::{record, Status, DESKTOP_PASTE};
    record(DESKTOP_PASTE, spec, if on { Status::Ok } else { Status::Off });
    crate::launchpad::wintap::set_deskpaste(on && OWNER.load(Ordering::SeqCst) != 0);
}

/// The host window that makes what is pasted (`None`: it went away).
pub fn set_owner(owner: Option<HWND>) {
    // The formats' ids, before the hook ever asks for them.
    let _ = formats_now();
    OWNER.store(owner.map(|h| h.0 as isize).unwrap_or(0), Ordering::SeqCst);
    bump();
    crate::launchpad::wintap::set_deskpaste(ENABLED.load(Ordering::SeqCst) && owner.is_some());
}

/// Which widgets are on (Settings > widgets): text is taken only for
/// notes that are on, pictures only for pieces that are on.
pub fn set_widgets(notes: bool, pieces: bool) {
    NOTES_ON.store(notes, Ordering::SeqCst);
    PIECES_ON.store(pieces, Ordering::SeqCst);
}

fn cf(name: windows::core::PCWSTR, cell: &'static OnceLock<u32>) -> u32 {
    *cell.get_or_init(|| unsafe { RegisterClipboardFormatW(name) })
}

/// The clipboard's formats now (the clipboard is not opened).
pub fn formats_now() -> Formats {
    static GROUP: OnceLock<u32> = OnceLock::new();
    static IDLIST: OnceLock<u32> = OnceLock::new();
    static PNG: OnceLock<u32> = OnceLock::new();
    unsafe {
        let has = |f: u32| f != 0 && IsClipboardFormatAvailable(f).is_ok();
        // CF_HDROP 15; CF_UNICODETEXT 13, CF_TEXT 1; CF_DIB 8, CF_DIBV5 17, CF_BITMAP 2.
        let files = has(15) || has(cf(w!("FileGroupDescriptorW"), &GROUP)) || has(cf(w!("Shell IDList Array"), &IDLIST));
        let text = has(13) || has(1);
        let image = has(8) || has(17) || has(2) || has(cf(w!("PNG"), &PNG));
        Formats { files, text, image }
    }
}

/// Is `h`'s class one of `names` (compared without allocating)?
fn class_is(h: HWND, names: &[&[u16]]) -> bool {
    let mut buf = [0u16; 32];
    let n = unsafe { GetClassNameW(h, &mut buf) }.max(0) as usize;
    let c = &buf[..n];
    let low = |u: u16| if (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u };
    names.iter().any(|x| x.len() == c.len() && x.iter().zip(c).all(|(a, b)| low(*a) == low(*b)))
}

const PROGMAN: &[u16] = &[b'P' as u16, b'r' as u16, b'o' as u16, b'g' as u16, b'm' as u16, b'a' as u16, b'n' as u16];
const WORKERW: &[u16] = &[b'W' as u16, b'o' as u16, b'r' as u16, b'k' as u16, b'e' as u16, b'r' as u16, b'W' as u16];
const EDIT: &[u16] = &[b'E' as u16, b'd' as u16, b'i' as u16, b't' as u16];

/// The scene now (cheap calls only: no waiting on another thread).
fn scene_now(fg: HWND) -> Scene {
    let desktop = !fg.is_invalid() && class_is(fg, &[PROGMAN, WORKERW]);
    if !desktop {
        return Scene::default();
    }
    let tid = unsafe { GetWindowThreadProcessId(fg, None) };
    let mut gi = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    // A focus that cannot be told counts as renaming: the key goes on.
    let renaming = tid == 0 || unsafe { GetGUIThreadInfo(tid, &mut gi) }.is_err() || (!gi.hwndFocus.is_invalid() && class_is(gi.hwndFocus, &[EDIT]));
    let formats = if renaming { Formats::default() } else { formats_now() };
    Scene { desktop, renaming, formats, notes_on: NOTES_ON.load(Ordering::SeqCst) && crate::notes::running(), pieces_on: PIECES_ON.load(Ordering::SeqCst) }
}

fn tick() -> u32 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
}

/// The keyboard hook's question (hook thread), for every V event while the
/// desktop's Ctrl+V is on: take this key? Returns at once; the making is
/// posted to the host window.
pub fn on_key(k: Key) -> bool {
    let owner = OWNER.load(Ordering::SeqCst);
    let gen = if ENABLED.load(Ordering::SeqCst) && owner != 0 { GEN.load(Ordering::SeqCst) } else { 0 };
    let fg = unsafe { GetForegroundWindow() };
    let held = HELD.with(|h| h.get());
    let (step, next) = track(held, k, gen, fg.0 as isize, || if gen == 0 { Scene::default() } else { scene_now(fg) });
    match step {
        Step::Pass => {
            HELD.with(|h| h.set(next));
            false
        }
        Step::Swallow => {
            HELD.with(|h| h.set(next));
            true
        }
        Step::Take(m) => {
            let ask = Ask { make: m as usize, tick: tick(), seq: unsafe { GetClipboardSequenceNumber() }, gen };
            let (wp, lp) = ask.pack();
            let posted = unsafe { PostMessageW(Some(HWND(owner as *mut _)), WM_DESKPASTE, WPARAM(wp), LPARAM(lp)) }.is_ok();
            // Not posted: not taken (the key goes on to Windows).
            HELD.with(|h| h.set(if posted { next } else { None }));
            posted
        }
    }
}

/// Where a made widget goes: the pointer (else the primary display's middle).
fn at() -> POINT {
    let mut p = POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_ok() {
        return p;
    }
    crate::monitors::primary().map(|m| POINT { x: (m.work.left + m.work.right) / 2, y: (m.work.top + m.work.bottom) / 2 }).unwrap_or_default()
}

/// WM_DESKPASTE on the dock's thread: makes the note or the piece from
/// what the clipboard holds, only if it is still what it was at the press,
/// the request is fresh and the switch and the widget are still on.
pub fn on_message(wp: WPARAM, lp: LPARAM) {
    let ask = Ask::unpack(wp.0, lp.0);
    if !still_now(ask) || !fresh(ask.tick, tick(), ask.seq, unsafe { GetClipboardSequenceNumber() }) {
        crate::log!("deskpaste: a stale request dropped");
        return;
    }
    match ask.make {
        1 => {
            if let Some(text) = clipboard_text(ask.seq) {
                let p = at();
                // Checked again on the notes' thread right before the note
                // is made (it may wait there).
                if !crate::notes::create_at(&text, (p.x, p.y), Box::new(move || still_now(ask))) {
                    crate::log!("deskpaste: the notes are not running; no note made");
                }
            }
        }
        2 => {
            if let Some(d) = crate::shot::image::read(ask.seq).and_then(crate::shot::image::decode_raw) {
                // Reading and decoding take a moment: checked again.
                if still_now(ask) {
                    crate::pieces::create(&d.png, (d.w, d.h), at());
                }
            }
        }
        _ => {}
    }
}

/// The clipboard's text, read once for the note the user asked for, if the
/// clipboard is still at `seq`.
fn clipboard_text(seq: u32) -> Option<String> {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    unsafe {
        OpenClipboard(None).ok()?;
        let r = (|| {
            if GetClipboardSequenceNumber() != seq {
                return None;
            }
            let h: HANDLE = GetClipboardData(13).ok()?;
            let g = HGLOBAL(h.0);
            let n = GlobalSize(g) / 2;
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let s = std::slice::from_raw_parts(p, n);
            let end = s.iter().position(|c| *c == 0).unwrap_or(n);
            let text = String::from_utf16_lossy(&s[..end]);
            let _ = GlobalUnlock(g);
            Some(text)
        })();
        let _ = CloseClipboard();
        r.filter(|t| !t.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILES: Formats = Formats { files: true, text: true, image: true };
    const TEXT: Formats = Formats { files: false, text: true, image: false };
    const IMAGE: Formats = Formats { files: false, text: false, image: true };
    const CELLS: Formats = Formats { files: false, text: true, image: true };
    const DESK: isize = 0x10;
    const APP: isize = 0x20;

    fn v(up: bool, ctrl: bool) -> Key {
        Key { vk: b'V' as u32, up, ctrl, ..Default::default() }
    }
    fn desk(f: Formats) -> Scene {
        Scene { desktop: true, renaming: false, formats: f, notes_on: true, pieces_on: true }
    }

    #[test]
    fn files_go_to_windows_text_makes_a_note_a_picture_a_piece() {
        assert_eq!(make_of(FILES, true, true), Make::Leave, "Explorer's copy is never taken");
        assert_eq!(make_of(TEXT, true, true), Make::Note);
        assert_eq!(make_of(IMAGE, true, true), Make::Piece);
        assert_eq!(make_of(CELLS, true, true), Make::Note, "cells copy as text and picture: the text wins");
        assert_eq!(make_of(Formats::default(), true, true), Make::Leave);
    }

    #[test]
    fn a_widget_that_is_off_takes_nothing() {
        assert_eq!(make_of(TEXT, false, true), Make::Leave, "notes off: text goes on to Windows");
        assert_eq!(make_of(CELLS, false, true), Make::Leave);
        assert_eq!(make_of(IMAGE, true, false), Make::Leave, "pieces off");
        assert_eq!(decide(Scene { notes_on: false, ..desk(TEXT) }), Step::Pass);
    }

    #[test]
    fn ctrl_v_is_taken_only_on_the_desktop_with_something_to_make() {
        assert_eq!(decide(desk(TEXT)), Step::Take(Make::Note));
        assert_eq!(decide(desk(IMAGE)), Step::Take(Make::Piece));
        assert_eq!(decide(Scene { desktop: false, ..desk(TEXT) }), Step::Pass);
        assert_eq!(decide(desk(FILES)), Step::Pass, "files: Windows pastes them");
        assert_eq!(decide(Scene { renaming: true, ..desk(TEXT) }), Step::Pass, "renaming (or the focus unknown)");
        // The formats at the press decide: text then files copied: passes.
        let (s, _) = track(None, v(false, true), 1, DESK, || desk(FILES));
        assert_eq!(s, Step::Pass);
    }

    #[test]
    fn only_ctrl_v_asks_about_the_scene() {
        for k in [Key { shift: true, ..v(false, true) }, Key { alt: true, ..v(false, true) }, Key { win: true, ..v(false, true) }, v(false, false), Key { vk: b'C' as u32, ..v(false, true) }] {
            let (s, h) = track(None, k, 1, DESK, || panic!("not asked for {k:?}"));
            assert_eq!((s, h), (Step::Pass, None), "{k:?}");
        }
    }

    #[test]
    fn a_taken_press_takes_only_its_own_repeats_and_key_up() {
        let (s, h) = track(None, v(false, true), 1, DESK, || desk(TEXT));
        assert_eq!(s, Step::Take(Make::Note));
        let (s, h2) = track(h, v(false, true), 1, DESK, || panic!("a repeat asks nothing"));
        assert_eq!(s, Step::Swallow);
        let (s, h3) = track(h2, v(true, true), 1, DESK, || unreachable!());
        assert_eq!((s, h3), (Step::Swallow, None));
        // The next key-up (nothing taken) goes on.
        assert_eq!(track(h3, v(true, false), 1, DESK, || unreachable!()).0, Step::Pass);
    }

    #[test]
    fn turning_it_off_and_on_or_another_window_forgets_the_press() {
        let (_, h) = track(None, v(false, true), 1, DESK, || desk(TEXT));
        // Turned off (gen 0) then on (gen 2): the old press is not ours.
        assert_eq!(track(h, v(true, false), 0, DESK, || unreachable!()).0, Step::Pass);
        assert_eq!(track(h, v(false, false), 2, APP, Scene::default).0, Step::Pass, "another app's V");
        assert_eq!(track(h, v(true, false), 2, APP, || unreachable!()).0, Step::Pass, "its key-up too");
        // Another window came forward with the key held: its V goes on.
        assert_eq!(track(h, v(false, false), 1, APP, Scene::default).0, Step::Pass);
    }

    /// Every order of presses and releases on the desktop and in an app,
    /// with the switch turned off and on between them (up to 7 events):
    /// an app's V is never taken, and a key-up is taken only right after a
    /// taken press under the same switch and window.
    #[test]
    fn another_apps_v_is_never_taken_whatever_the_order() {
        #[derive(Clone, Copy, Debug)]
        enum Ev {
            DeskDown,
            DeskUp,
            AppDown,
            AppUp,
            Off,
            On,
        }
        const ALL: [Ev; 6] = [Ev::DeskDown, Ev::DeskUp, Ev::AppDown, Ev::AppUp, Ev::Off, Ev::On];
        fn run(seq: &[Ev]) {
            let (mut held, mut on, mut gen, mut last_taken): (Option<Taken>, bool, u32, Option<(u32, isize)>) = (None, true, 1, None);
            for ev in seq {
                let (k, fg) = match ev {
                    Ev::DeskDown => (v(false, true), DESK),
                    Ev::DeskUp => (v(true, true), DESK),
                    Ev::AppDown => (v(false, true), APP),
                    Ev::AppUp => (v(true, true), APP),
                    Ev::Off => {
                        if on {
                            on = false;
                            gen += 1;
                        }
                        continue;
                    }
                    Ev::On => {
                        if !on {
                            on = true;
                            gen += 1;
                        }
                        continue;
                    }
                };
                let g = if on { gen } else { 0 };
                let (s, next) = track(held, k, g, fg, || if fg == DESK { desk(TEXT) } else { Scene::default() });
                if fg == APP {
                    assert_eq!(s, Step::Pass, "{seq:?}: an app's V taken at {ev:?}");
                }
                if s == Step::Swallow && k.up {
                    assert_eq!(last_taken, Some((g, fg)), "{seq:?}: a key-up taken without its press");
                }
                if let Step::Take(_) = s {
                    last_taken = Some((g, fg));
                }
                if k.up {
                    last_taken = None;
                }
                held = next;
            }
        }
        fn all(prefix: &mut Vec<Ev>, n: usize) {
            run(prefix);
            if n == 0 {
                return;
            }
            for e in ALL {
                prefix.push(e);
                all(prefix, n - 1);
                prefix.pop();
            }
        }
        all(&mut Vec::new(), 7);
    }

    #[test]
    fn a_request_carries_its_time_and_generation_to_the_making() {
        let ask = Ask { make: 1, tick: 0xFFFF_FFF0, seq: 0xDEAD_BEEF, gen: 77 };
        let (wp, lp) = ask.pack();
        assert_eq!(Ask::unpack(wp, lp), ask, "through the message and back");
        // Made only in time, under the same switch generation, the widget on.
        assert!(still(ask, 0xFFFF_FFF0u32.wrapping_add(100), 77, true));
        assert!(!still(ask, 0xFFFF_FFF0u32.wrapping_add(FRESH_MS + 1), 77, true), "waited too long on the notes' thread");
        assert!(!still(ask, 0xFFFF_FFF1, 78, true), "the switch turned off and on meanwhile");
        assert!(!still(ask, 0xFFFF_FFF1, 0, true), "the switch is off");
        assert!(!still(ask, 0xFFFF_FFF1, 77, false), "the notes were turned off");
        assert!(!still(Ask { gen: 0, ..ask }, 0xFFFF_FFF1, 0, true), "never made under no generation");
    }

    #[test]
    fn a_request_is_made_only_while_fresh_and_the_clipboard_unchanged() {
        assert!(fresh(1000, 1500, 7, 7));
        assert!(!fresh(1000, 1500, 7, 8), "B was copied before it was made");
        assert!(!fresh(1000, 1000 + FRESH_MS + 1, 7, 7), "the dock was busy too long");
        assert!(fresh(u32::MAX - 10, 100, 7, 7), "the tick count wrapped");
    }
}
