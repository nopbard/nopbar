//! Shortcut "launchpad_win_tap": a lone press-and-release of the Windows key opens
//! Launchpad instead of Start.
//!
//! A WH_KEYBOARD_LL hook, installed only while the option is on (removed as
//! soon as it is off), that never blocks or changes a key: every event goes
//! on to Windows untouched. When a Windows key comes back up with nothing
//! else pressed since it went down (and within 600 ms), the hook types one
//! unassigned key (VK 0xE8, the "menu mask" AutoHotkey uses) just before the
//! release goes through: to the shell the Win press is then a Win+key chord,
//! so Start stays closed, and Launchpad is asked to open. Win+anything is
//! never touched. Keys we type ourselves (injected) are ignored.
//!
//! The same hook also serves Win+1..9, 0 (shortcut "dock_apps" set to the
//! Windows key): a digit pressed while a Windows key is held (alone, or
//! with Shift, Ctrl or Alt) is swallowed (its key-up and repeats too), the
//! mask key is typed so Start stays closed when Win comes up, and the dock
//! is told which slot and what to do. Everything else, Win+Ctrl+Shift+digit
//! included, goes on to Windows. The Windows key itself is never swallowed,
//! so it can never stay stuck. With both options off there is no hook.
//!
//! It also asks crate::desklayer::deskpaste about Ctrl+V while the
//! desktop's Ctrl+V is on (a press taken there never reaches Windows; all
//! others go on untouched).
//!
//! The hook lives on a thread of its own that only pumps its messages, so
//! a busy UI thread (shell calls, drawing, config saves) can never make it
//! miss LowLevelHooksTimeout and get it silently removed.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN,
    VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostMessageW, PostThreadMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_KEYUP, WM_QUIT, WM_SYSKEYUP,
};

/// Longer than this from press to release is a hold, not a tap.
pub const MAX_TAP_MS: u32 = 600;
/// Unassigned virtual key typed to keep Start closed.
const MASK_VK: u16 = 0xE8;

/// One keyboard event as the tap detector sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A Windows key went down (`vk`: which one; `mods`: Shift, Ctrl or
    /// Alt was already held). Auto-repeat sends more of these.
    WinDown { vk: u32, time: u32, mods: bool },
    WinUp { vk: u32, time: u32 },
    /// Any other key, down or up.
    Other,
}

/// The lone-tap state machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tap {
    /// The Windows key held: (which, when it went down).
    down: Option<(u32, u32)>,
    /// Something else happened while it was held.
    spoiled: bool,
}

impl Tap {
    /// Takes one event; true when it completes a lone tap.
    pub fn feed(&mut self, k: Key) -> bool {
        match k {
            Key::WinDown { vk, time, mods } => {
                match self.down {
                    None => {
                        self.down = Some((vk, time));
                        self.spoiled = mods;
                    }
                    // The other Windows key as well: a chord of sorts.
                    Some((held, _)) if held != vk => self.spoiled = true,
                    // Auto-repeat.
                    Some(_) => {}
                }
                false
            }
            Key::Other => {
                if self.down.is_some() {
                    self.spoiled = true;
                }
                false
            }
            Key::WinUp { vk, time } => {
                let Some((held, t0)) = self.down else { return false };
                if held != vk {
                    // The other Windows key let go first.
                    self.spoiled = true;
                    return false;
                }
                self.down = None;
                let spoiled = std::mem::take(&mut self.spoiled);
                !spoiled && time.wrapping_sub(t0) <= MAX_TAP_MS
            }
        }
    }
}

/// Who owns the hook (audit A14: the UI used to wait 2 s for the hook
/// thread and forget a thread that started later, leaving a hook nobody
/// could remove). Now the UI never waits: each start gets a generation,
/// and a hook thread that comes up after its generation was turned off (or
/// replaced) removes its own hook and ends. All under one lock, so a thread
/// is either published (and turned off by `set`) or ends by itself.
#[derive(Debug, Default, PartialEq, Eq)]
struct Owner {
    /// The running hook thread (0 = none).
    thread: u32,
    /// Bumped by every on/off request; a start belongs to one value.
    gen: u32,
    /// A start for the current generation is on its way.
    starting: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Do {
    Nothing,
    /// Start a hook thread for this generation.
    Start(u32),
    /// Tell this thread to remove its hook and end.
    Quit(u32),
}

impl Owner {
    fn request(&mut self, on: bool) -> Do {
        if on {
            if self.thread != 0 || self.starting {
                return Do::Nothing;
            }
            self.gen = self.gen.wrapping_add(1);
            self.starting = true;
            return Do::Start(self.gen);
        }
        self.gen = self.gen.wrapping_add(1);
        self.starting = false;
        match std::mem::take(&mut self.thread) {
            0 => Do::Nothing,
            t => Do::Quit(t),
        }
    }

    /// The thread of generation `gen` has its hook: keep it (true) or, if
    /// that generation is over, the thread removes it again (false).
    fn started(&mut self, gen: u32, tid: u32) -> bool {
        if gen != self.gen || !self.starting {
            return false;
        }
        self.starting = false;
        self.thread = tid;
        true
    }

    /// The thread of generation `gen` could not set its hook.
    fn failed(&mut self, gen: u32) {
        if gen == self.gen {
            self.starting = false;
        }
    }
}

static OWNER_STATE: std::sync::Mutex<Owner> = std::sync::Mutex::new(Owner { thread: 0, gen: 0, starting: false });
/// Gets WM_LAUNCHPAD when a tap is seen (the dock window).
static OWNER: AtomicIsize = AtomicIsize::new(0);
/// The lone tap opens Launchpad.
static TAP_ON: AtomicBool = AtomicBool::new(false);
/// Win+digit goes to the dock (posted as DIGIT_MSG).
static DIGITS_ON: AtomicBool = AtomicBool::new(false);
static DIGIT_MSG: AtomicU32 = AtomicU32::new(0);
/// Ctrl+V on the desktop is asked about (crate::desklayer::deskpaste).
pub(crate) const WM_TASKBAR_FOCUS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 201;
static CYCLE: AtomicU32 = AtomicU32::new(0);
static TRAY: AtomicU32 = AtomicU32::new(0);

fn chord_setting(spec: &str, registration: Option<crate::hotkeys::Status>) -> (u32, crate::hotkeys::Status) {
    use crate::hotkeys::Status;
    match crate::shortcuts::parse(spec) {
        Ok(Some(chord)) => {
            let status = registration.unwrap_or(Status::Ok);
            let key = if status == Status::Ok { chord.mods << 16 | chord.vk } else { 0 };
            (key, status)
        }
        Ok(None) => (0, Status::Off),
        Err(_) => (0, Status::Invalid),
    }
}

fn set_chord(owner: HWND, spec: &str, slot: &AtomicU32, key: &'static str, registration: Option<crate::hotkeys::Status>) {
    let (value, status) = chord_setting(spec, registration);
    slot.store(value, Ordering::Release);
    crate::hotkeys::record(key, spec, status);
    sync(owner);
}

pub fn apply_cycle(owner: HWND, spec: &str, registration: Option<crate::hotkeys::Status>) {
    set_chord(owner, spec, &CYCLE, crate::hotkeys::FOCUS_DOCK, registration);
}
pub fn apply_tray(owner: HWND, spec: &str) { set_chord(owner, spec, &TRAY, "shortcuts.taskbar_tray", None); }
pub fn taskbar_changed() { sync(HWND::default()); }

static DESKPASTE_ON: AtomicBool = AtomicBool::new(false);
static DESKTOP_FRONT: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Only ever touched on the hook thread.
    static TAP: Cell<Tap> = Cell::new(Tap::default());
    /// Modifiers held when the focus hook took each physical key-down.
    static FOCUS_DOWN: [Cell<Option<u32>>; 256] = const { [const { Cell::new(None) }; 256] };
    /// Digits whose repeats and key-up belong to the hook.
    static DIGITS_DOWN: Cell<u16> = const { Cell::new(0) };
}

/// The lone tap on or off (the hook runs while it or Win+digit is on).
pub fn set_tap(owner: HWND, on: bool) {
    TAP_ON.store(on, Ordering::SeqCst);
    sync(owner);
}

/// Win+digit on or off; digits are posted to `owner` as `msg` (wParam
/// the slot, lParam the DigitAct code, | 0x100 if the mask key could not
/// be typed).
pub fn set_digits(owner: HWND, on: bool, msg: u32) {
    DIGIT_MSG.store(msg, Ordering::SeqCst);
    DIGITS_ON.store(on, Ordering::SeqCst);
    sync(owner);
}

/// The desktop's Ctrl+V on or off (crate::desklayer::deskpaste).
pub fn set_deskpaste(on: bool) {
    DESKPASTE_ON.store(on, Ordering::SeqCst);
    foreground(unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() });
}

pub fn foreground(hwnd: HWND) {
    DESKTOP_FRONT.store(crate::desklayer::win::is_shell(hwnd), Ordering::SeqCst);
    sync(HWND::default());
}

fn needs_hook(tap: bool, digits: bool, paste: bool, desktop: bool) -> bool {
    tap || digits || (paste && desktop)
}

/// All off, hook removed (the dock window is going away).
pub fn off() {
    CYCLE.store(0, Ordering::Release);
    TRAY.store(0, Ordering::Release);
    TAP_ON.store(false, Ordering::SeqCst);
    DIGITS_ON.store(false, Ordering::SeqCst);
    DESKPASTE_ON.store(false, Ordering::SeqCst);
    request(HWND::default(), false);
}

fn sync(owner: HWND) {
    let focus = (CYCLE.load(Ordering::Acquire) | TRAY.load(Ordering::Acquire)) != 0 && crate::taskbar::hidden_by_us();
    request(owner, focus || needs_hook(TAP_ON.load(Ordering::SeqCst), DIGITS_ON.load(Ordering::SeqCst), DESKPASTE_ON.load(Ordering::SeqCst), DESKTOP_FRONT.load(Ordering::SeqCst)));
}

/// Hook on (`on`) or off. Taps are posted to `owner` as WM_LAUNCHPAD
/// (lParam 1: the mask key could not be typed, Start may open too).
/// Never waits: the hook comes up on its own thread.
fn request(owner: HWND, on: bool) {
    if !owner.is_invalid() {
        OWNER.store(owner.0 as isize, Ordering::SeqCst);
    }
    let action = match OWNER_STATE.lock() {
        Ok(mut o) => o.request(on),
        Err(_) => return,
    };
    match action {
        Do::Nothing => {}
        Do::Quit(tid) => {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            crate::log!("keyboard hook off");
        }
        Do::Start(gen) => {
            let spawned = std::thread::Builder::new().name("launchpad-winkey".into()).spawn(move || unsafe { hook_thread(gen) });
            if spawned.is_err() {
                if let Ok(mut o) = OWNER_STATE.lock() {
                    o.failed(gen);
                }
                crate::log!("keyboard hook thread not started; Windows keeps its keys");
            }
        }
    }
}

/// Sets the hook, and keeps it only while its generation is current.
unsafe fn hook_thread(gen: u32) {
    let tid = GetCurrentThreadId();
    // A message queue first, so WM_QUIT can reach it.
    let mut msg = MSG::default();
    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
    let hinst = GetModuleHandleW(None).ok().map(|h| h.into());
    let h = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), hinst, 0) {
        Ok(h) => h,
        Err(e) => {
            if let Ok(mut o) = OWNER_STATE.lock() {
                o.failed(gen);
            }
            crate::log!("keyboard hook not installed ({e}); Windows keeps its keys");
            return;
        }
    };
    let keep = OWNER_STATE.lock().map(|mut o| o.started(gen, tid)).unwrap_or(false);
    if !keep {
        // Turned off (or restarted) while this one was coming up.
        let _ = UnhookWindowsHookEx(h);
        return;
    }
    crate::log!("keyboard hook on (lone Windows key: {}, Win+digit: {})", TAP_ON.load(Ordering::SeqCst), DIGITS_ON.load(Ordering::SeqCst));
    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    let _ = UnhookWindowsHookEx(h);
}

fn held(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

/// Types the mask key (down and up); false if Windows did not take it.
/// Its events come back through the hook marked injected and are ignored.
fn mask() -> bool {
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(MASK_VK), dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, dwExtraInfo: crate::keys::OWN_INPUT, ..Default::default() } },
    };
    unsafe { SendInput(&[key(false), key(true)], std::mem::size_of::<INPUT>() as i32) == 2 }
}

/// What the hook does with a digit key (pure: `down_mask` is the digits
/// whose key-down it took, `win` whether a Windows key is held).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigitStep {
    /// Not ours: on to Windows.
    Pass,
    /// A repeat or the key-up of a digit we took: swallow it.
    Swallow,
    /// Take it: swallow, type the mask, post (slot, act).
    Take(usize, crate::shortcuts::DigitAct),
}

pub fn digit_step(vk: u32, up: bool, down_mask: u16, win: bool, shift: bool, ctrl: bool, alt: bool) -> DigitStep {
    let Some(slot) = crate::shortcuts::digit_slot(vk) else { return DigitStep::Pass };
    if down_mask & (1u16 << slot) != 0 {
        return DigitStep::Swallow;
    }
    if up || !win {
        return DigitStep::Pass;
    }
    match crate::shortcuts::hook_digit(vk, shift, ctrl, alt) {
        Some((slot, act)) => DigitStep::Take(slot, act),
        None => DigitStep::Pass,
    }
}

/// Win+digit: swallowed (true) or not.
unsafe fn digit(k: &KBDLLHOOKSTRUCT, up: bool) -> bool {
    if !DIGITS_ON.load(Ordering::SeqCst) || !(0x30..=0x39).contains(&k.vkCode) {
        return false;
    }
    let down = DIGITS_DOWN.get();
    let bit = 1u16 << crate::shortcuts::digit_slot(k.vkCode).unwrap_or(0);
    let win = held(VK_LWIN) || held(VK_RWIN);
    match digit_step(k.vkCode, up, down, win, held(VK_SHIFT), held(VK_CONTROL), held(VK_MENU)) {
        DigitStep::Pass => false,
        DigitStep::Swallow => {
            if up {
                DIGITS_DOWN.set(down & !bit);
            }
            true
        }
        DigitStep::Take(slot, act) => {
            DIGITS_DOWN.set(down | bit);
            // Not a lone tap any more.
            TAP.with(|t| {
                let mut s = t.get();
                s.feed(Key::Other);
                t.set(s);
            });
            // Windows saw Win go down: with the digit gone it would open
            // Start when Win comes up, unless it sees another key now.
            let masked = mask();
            let owner = OWNER.load(Ordering::SeqCst);
            let msg = DIGIT_MSG.load(Ordering::SeqCst);
            if owner != 0 && msg != 0 {
                let lp = act.code() as isize | if masked { 0 } else { 0x100 };
                let _ = PostMessageW(Some(HWND(owner as *mut _)), msg, WPARAM(slot), LPARAM(lp));
            }
            true
        }
    }
}

/// Whether a key event may be a taskbar focus key: typed, or injected by
/// another program, but not one NopBar typed itself (crate::keys::OWN_INPUT:
/// the Win+B of the notification area button must reach Explorer).
fn takes_focus_key(injected: bool, extra: usize) -> bool {
    !(injected && extra == crate::keys::OWN_INPUT)
}

fn focus_action(vk: u32, mods: u32, cycle: u32, tray: u32, hidden: bool) -> Option<usize> {
    if !hidden || vk == 0 { return None; }
    let chord = mods << 16 | vk;
    if chord == tray { return Some(2); }
    if chord == cycle { return Some(0); }
    (crate::shortcuts::has_reverse_cycle(cycle >> 16) && chord == (cycle | (crate::shortcuts::SHIFT << 16))).then_some(1)
}

/// A missing release (for example on the secure desktop) must not swallow
/// a later ordinary keystroke. Always consume the matching release.
fn focus_tail(mark: &Cell<Option<u32>>, up: bool, mods: u32) -> Option<bool> {
    let required = mark.get()?;
    let still_held = mods & required == required;
    if up || !still_held { mark.set(None); }
    Some(up || still_held)
}

unsafe fn focus_key(vk: u32, up: bool) -> bool {
    use crate::shortcuts::{ALT, CTRL, SHIFT, WIN};
    if vk >= 256 { return false; }
    let marked = FOCUS_DOWN.with(|keys| keys[vk as usize].get().is_some());
    let cycle = CYCLE.load(Ordering::Acquire);
    let tray = TRAY.load(Ordering::Acquire);
    if !marked && (up || (vk != (cycle & 0xffff) && vk != (tray & 0xffff))) { return false; }
    let mods = u32::from(held(VK_LWIN) || held(VK_RWIN)) * WIN | u32::from(held(VK_CONTROL)) * CTRL
        | u32::from(held(VK_MENU)) * ALT | u32::from(held(VK_SHIFT)) * SHIFT;
    if let Some(take) = FOCUS_DOWN.with(|keys| focus_tail(&keys[vk as usize], up, mods)) {
        return take;
    }
    let Some(action) = focus_action(vk, mods, cycle, tray, crate::taskbar::hidden_by_us()) else { return false };
    if action == 2 && !crate::topbar::can_focus_notification_area() { return false; }
    let owner = HWND(OWNER.load(Ordering::Acquire) as *mut _);
    if owner.is_invalid() || PostMessageW(Some(owner), WM_TASKBAR_FOCUS, WPARAM(action), LPARAM(0)).is_err() { return false; }
    FOCUS_DOWN.with(|keys| keys[vk as usize].set(Some(mods)));
    TAP.with(|t| { let mut s = t.get(); s.feed(Key::Other); t.set(s); });
    if mods & WIN != 0 { mask(); }
    true
}

/// Classify, feed the state machine, and on a tap type the mask and post
/// to the UI thread. No logging, no file or shell calls here.
unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && lp.0 != 0 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let injected = k.flags.contains(LLKHF_INJECTED);
        // Win+T / Win+B as Windows takes them: also from the on-screen
        // keyboard or a remapper (injected), never from keys we type.
        if takes_focus_key(injected, k.dwExtraInfo) && focus_key(k.vkCode, matches!(wp.0 as u32, WM_KEYUP | WM_SYSKEYUP)) {
            return LRESULT(1);
        }
        if !injected {
            let up = matches!(wp.0 as u32, WM_KEYUP | WM_SYSKEYUP);
            if digit(k, up) {
                return LRESULT(1);
            }
            if DESKPASTE_ON.load(Ordering::SeqCst) && k.vkCode == b'V' as u32 {
                let key = crate::desklayer::deskpaste::Key {
                    vk: k.vkCode,
                    up,
                    ctrl: held(VK_CONTROL),
                    shift: held(VK_SHIFT),
                    alt: held(VK_MENU),
                    win: held(VK_LWIN) || held(VK_RWIN),
                };
                if crate::desklayer::deskpaste::on_key(key) {
                    return LRESULT(1);
                }
            }
            let win = k.vkCode == VK_LWIN.0 as u32 || k.vkCode == VK_RWIN.0 as u32;
            let ev = match (win, up) {
                (true, false) => Key::WinDown { vk: k.vkCode, time: k.time, mods: [VK_SHIFT, VK_CONTROL, VK_MENU].into_iter().any(held) },
                (true, true) => Key::WinUp { vk: k.vkCode, time: k.time },
                _ => Key::Other,
            };
            // (State is stored before mask(): its events re-enter this hook.)
            let tap = TAP.with(|t| {
                let mut s = t.get();
                let tap = s.feed(ev);
                t.set(s);
                tap
            });
            if tap && TAP_ON.load(Ordering::SeqCst) {
                let masked = mask();
                let owner = OWNER.load(Ordering::SeqCst);
                if owner != 0 {
                    let _ = PostMessageW(Some(HWND(owner as *mut _)), super::WM_LAUNCHPAD, WPARAM(super::OPEN_TOGGLE), LPARAM(if masked { 0 } else { 1 }));
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

#[cfg(test)]
mod tests {
    #[test]
    fn custom_focus_keys_do_not_reserve_a_shift_partner() {
        use crate::shortcuts::{parse, chords, find, FOCUS_DOCK, SHIFT};
        for spec in ["f9", "ctrl+alt+d"] {
            let chord = parse(spec).unwrap().unwrap();
            let cycle = chord.mods << 16 | chord.vk;
            assert_eq!(super::focus_action(chord.vk, chord.mods, cycle, 0, true), Some(0));
            assert_eq!(super::focus_action(chord.vk, chord.mods | SHIFT, cycle, 0, true), None);
            assert_eq!(chords(find(FOCUS_DOCK).unwrap(), spec), vec![chord]);
        }
    }

    #[test]
    fn missed_key_up_releases_plain_and_partially_modified_keys() {
        use crate::shortcuts::{WIN, CTRL, ALT};
        let mark = Cell::new(Some(WIN));
        assert_eq!(focus_tail(&mark, false, WIN), Some(true)); // auto-repeat
        assert_eq!(focus_tail(&mark, false, 0), Some(false));  // new plain T
        assert_eq!(focus_tail(&mark, true, 0), None);          // its key-up passes
        mark.set(Some(CTRL | ALT));
        assert_eq!(focus_tail(&mark, false, CTRL), Some(false));
        mark.set(Some(WIN));
        assert_eq!(focus_tail(&mark, true, 0), Some(true));    // Win was released first
        assert_eq!(mark.get(), None);
    }

    #[test]
    fn failed_registration_is_reported_and_disables_the_cycle_hook() {
        use crate::hotkeys::Status;
        for status in [Status::Taken, Status::Failed] {
            assert_eq!(chord_setting("ctrl+alt+d", Some(status)), (0, status));
        }
        assert_eq!(chord_setting("win+t", None), (crate::shortcuts::WIN << 16 | 84, Status::Ok));
        assert_eq!(chord_setting("none", Some(Status::Off)), (0, Status::Off));
    }

    #[test]
    fn focus_keys_come_from_people_and_other_programs_not_from_us() {
        assert!(super::takes_focus_key(false, 0), "typed");
        assert!(super::takes_focus_key(true, 0), "the on-screen keyboard, a remapper");
        assert!(super::takes_focus_key(false, crate::keys::OWN_INPUT), "typed keys carry no mark of ours");
        assert!(!super::takes_focus_key(true, crate::keys::OWN_INPUT), "our own Win+B goes on to Explorer");
    }

    #[test]
    fn taskbar_focus_only_takes_configured_chords_while_hidden() {
        use crate::shortcuts::{CTRL, SHIFT, WIN};
        let cycle = WIN << 16 | 84;
        let tray = WIN << 16 | 66;
        for (vk, mods, hidden, want) in [(84, WIN, true, Some(0)), (84, WIN|SHIFT, true, Some(1)),
            (66, WIN, true, Some(2)), (66, WIN|CTRL, true, None), (84, WIN, false, None)] {
            assert_eq!(super::focus_action(vk, mods, cycle, tray, hidden), want);
        }
        assert_eq!(super::focus_action(84, WIN, 0, 0, true), None);
        assert_eq!(super::focus_action(74, CTRL, CTRL << 16 | 74, tray, true), Some(0));
    }
    use super::*;

    #[test]
    fn paste_hook_only_while_desktop_is_foreground() {
        assert!(!needs_hook(false, false, true, false));
        assert!(needs_hook(false, false, true, true));
        assert!(needs_hook(true, false, true, false));
        assert!(needs_hook(false, true, true, false));
        assert!(!needs_hook(false, false, false, true));
    }

    #[test]
    fn a_hook_that_comes_up_late_removes_itself() {
        let mut o = Owner::default();
        let Do::Start(g) = o.request(true) else { panic!() };
        assert_eq!(o.request(true), Do::Nothing, "already starting: no second thread");
        assert_eq!(o.request(false), Do::Nothing, "nothing running yet");
        assert!(!o.started(g, 7), "its generation is over: unhook and end");
        assert_eq!(o.thread, 0);
        // On, off, on again before the first came up: only the newest stays.
        let Do::Start(g1) = o.request(true) else { panic!() };
        o.request(false);
        let Do::Start(g2) = o.request(true) else { panic!() };
        assert!(!o.started(g1, 8));
        assert!(o.started(g2, 9));
        assert_eq!(o.request(false), Do::Quit(9));
        assert_eq!(o.request(false), Do::Nothing);
    }

    #[test]
    fn a_failed_start_can_be_tried_again() {
        let mut o = Owner::default();
        let Do::Start(g) = o.request(true) else { panic!() };
        o.failed(g);
        assert!(matches!(o.request(true), Do::Start(_)));
    }

    #[test]
    fn win_digits_are_taken_whole_and_only_with_win() {
        use crate::shortcuts::DigitAct;
        let one = b'1' as u32;
        // Win+1: taken; its repeats and its key-up too (even after Win is up).
        assert_eq!(digit_step(one, false, 0, true, false, false, false), DigitStep::Take(0, DigitAct::Open));
        assert_eq!(digit_step(one, false, 1, true, false, false, false), DigitStep::Swallow);
        assert_eq!(digit_step(one, true, 1, false, false, false, false), DigitStep::Swallow);
        // Without Win: Windows' (typing a 1 is never touched).
        assert_eq!(digit_step(one, false, 0, false, false, false, false), DigitStep::Pass);
        assert_eq!(digit_step(one, true, 0, true, false, false, false), DigitStep::Pass, "an up we did not take");
        // The variants, and the one left to Windows.
        assert_eq!(digit_step(b'0' as u32, false, 0, true, true, false, false), DigitStep::Take(9, DigitAct::NewWindow));
        assert_eq!(digit_step(b'2' as u32, false, 0, true, false, true, false), DigitStep::Take(1, DigitAct::Cycle));
        assert_eq!(digit_step(b'2' as u32, false, 0, true, false, false, true), DigitStep::Take(1, DigitAct::JumpList));
        assert_eq!(digit_step(b'2' as u32, false, 0, true, true, true, false), DigitStep::Pass);
        // Not a digit.
        assert_eq!(digit_step(b'E' as u32, false, 0, true, false, false, false), DigitStep::Pass);
        // Another digit's bit does not swallow this one.
        assert_eq!(digit_step(b'2' as u32, true, 1, false, false, false, false), DigitStep::Pass);
    }

    const L: u32 = 0x5B;
    const R: u32 = 0x5C;

    fn run(events: &[Key]) -> Vec<bool> {
        let mut t = Tap::default();
        events.iter().map(|&e| t.feed(e)).collect()
    }

    #[test]
    fn a_lone_tap_counts() {
        let r = run(&[Key::WinDown { vk: L, time: 1000, mods: false }, Key::WinUp { vk: L, time: 1100 }]);
        assert_eq!(r, [false, true]);
        // The right Windows key too.
        let r = run(&[Key::WinDown { vk: R, time: 0, mods: false }, Key::WinUp { vk: R, time: 50 }]);
        assert_eq!(r, [false, true]);
    }

    #[test]
    fn win_plus_another_key_is_not_a_tap() {
        // Win+E: down, E down, E up, up.
        let r = run(&[Key::WinDown { vk: L, time: 0, mods: false }, Key::Other, Key::Other, Key::WinUp { vk: L, time: 100 }]);
        assert_eq!(r, [false, false, false, false]);
        // Shift (or Ctrl, Alt) held first: Shift+Win.
        let r = run(&[Key::WinDown { vk: L, time: 0, mods: true }, Key::WinUp { vk: L, time: 100 }]);
        assert_eq!(r, [false, false]);
        // Both Windows keys.
        let r = run(&[
            Key::WinDown { vk: L, time: 0, mods: false },
            Key::WinDown { vk: R, time: 10, mods: false },
            Key::WinUp { vk: R, time: 20 },
            Key::WinUp { vk: L, time: 30 },
        ]);
        assert!(r.iter().all(|&x| !x), "{r:?}");
    }

    #[test]
    fn a_long_hold_is_not_a_tap() {
        let r = run(&[Key::WinDown { vk: L, time: 0, mods: false }, Key::WinUp { vk: L, time: MAX_TAP_MS + 1 }]);
        assert_eq!(r, [false, false]);
        // Exactly the limit still counts.
        let r = run(&[Key::WinDown { vk: L, time: 0, mods: false }, Key::WinUp { vk: L, time: MAX_TAP_MS }]);
        assert_eq!(r, [false, true]);
        // Auto-repeat keeps the first press time.
        let r = run(&[
            Key::WinDown { vk: L, time: 0, mods: false },
            Key::WinDown { vk: L, time: 500, mods: false },
            Key::WinDown { vk: L, time: 550, mods: false },
            Key::WinUp { vk: L, time: 700 },
        ]);
        assert_eq!(r.last(), Some(&false));
        // The tick count wrapping around does not matter.
        let r = run(&[Key::WinDown { vk: L, time: u32::MAX - 10, mods: false }, Key::WinUp { vk: L, time: 40 }]);
        assert_eq!(r, [false, true]);
    }

    #[test]
    fn each_tap_starts_fresh() {
        let mut t = Tap::default();
        // Win+E, then a clean tap.
        for e in [Key::WinDown { vk: L, time: 0, mods: false }, Key::Other, Key::WinUp { vk: L, time: 50 }] {
            assert!(!t.feed(e));
        }
        assert!(!t.feed(Key::WinDown { vk: L, time: 100, mods: false }));
        assert!(t.feed(Key::WinUp { vk: L, time: 150 }));
        // Keys typed with no Windows key held change nothing.
        assert!(!t.feed(Key::Other));
        assert!(!t.feed(Key::WinUp { vk: L, time: 200 }), "an up without a down");
        assert_eq!(t, Tap::default());
    }
}
