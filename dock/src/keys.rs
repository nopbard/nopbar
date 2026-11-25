//! Synthetic key presses for shell shortcuts (Start, quick settings,
//! notification center). Used in direct response to a click on our UI, or
//! to the pointer resting in a hot corner (`unmixed`).

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};

/// Arrows and the navigation block are "extended" keys; without the flag
/// some apps read them as the numeric keypad.
fn extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C)
}

/// dwExtraInfo of every key NopBar types itself, so our keyboard hook can
/// tell them from keys other programs inject (the on-screen keyboard, a
/// key remapper), which it treats like typed ones where Windows does.
pub const OWN_INPUT: usize = 0x4E4F_5042; // "NOPB"

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { Default::default() };
    if extended(vk.0) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, dwExtraInfo: OWN_INPUT, ..Default::default() } },
    }
}

/// Of a chord's inputs (all downs, then the ups in reverse), how many keys
/// are still down after Windows took only the first `taken`: keys[..n].
fn still_down(taken: usize, keys: usize) -> usize {
    if taken <= keys {
        taken
    } else {
        keys.saturating_sub(taken - keys)
    }
}

/// Press and release `keys` as a chord (first key down first, released
/// last). False if Windows did not take all of it (e.g. blocked by UIPI);
/// then the keys it did press are released again, so none stays stuck.
fn chord(keys: &[VIRTUAL_KEY]) -> bool {
    let mut inputs: Vec<INPUT> = keys.iter().map(|&k| key(k, false)).collect();
    inputs.extend(keys.iter().rev().map(|&k| key(k, true)));
    let size = std::mem::size_of::<INPUT>() as i32;
    let taken = unsafe { SendInput(&inputs, size) } as usize;
    if taken == inputs.len() {
        return true;
    }
    let down = still_down(taken, keys.len());
    if down > 0 {
        let ups: Vec<INPUT> = keys[..down].iter().rev().map(|&k| key(k, true)).collect();
        unsafe {
            SendInput(&ups, size);
        }
    }
    false
}

/// How an automatic shortcut went.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sent {
    Ok,
    /// The user holds Shift, Ctrl, Alt or Win: nothing was sent (it would
    /// mix with them into another shortcut, and their keys are theirs).
    ModifiersHeld,
    /// Windows did not take it (e.g. UIPI with an elevated app in front).
    Failed,
}

/// Shift, Ctrl, Alt or a Win key is held down right now.
pub fn modifiers_held() -> bool {
    [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(|k| unsafe { GetAsyncKeyState(k.0 as i32) } < 0)
}

/// A shortcut not asked for by a key press (hot corners): only sent when no
/// modifier is held, never touching the user's own keys.
pub fn unmixed(c: &[u16]) -> Sent {
    if modifiers_held() {
        return Sent::ModifiersHeld;
    }
    if press_chord(c) { Sent::Ok } else { Sent::Failed }
}

/// One chord of a menu item's shortcut. False if it did not go out.
pub fn press_chord(c: &[u16]) -> bool {
    let keys: Vec<VIRTUAL_KEY> = c.iter().map(|&k| VIRTUAL_KEY(k)).collect();
    chord(&keys)
}

/// Start menu.
pub fn win() {
    let _ = chord(&[VK_LWIN]);
}

pub(crate) fn is_start_app(exe: Option<&str>) -> bool {
    exe.and_then(|path| path.rsplit(['\\', '/']).next())
        .is_some_and(|name| name.eq_ignore_ascii_case("StartMenuExperienceHost.exe"))
}

/// The visible Windows Start surface's monitor, if it has the foreground.
pub(crate) fn start_monitor(hwnd: windows::Win32::Foundation::HWND) -> Option<isize> {
    use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONULL};
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, IsWindowVisible};
    unsafe {
        let mut name = [0; 64];
        let len = GetClassNameW(hwnd, &mut name).max(0) as usize;
        let class = String::from_utf16_lossy(&name[..len]);
        if !matches!(class.as_str(), "Windows.UI.Core.CoreWindow" | "XamlExplorerHostIslandWindow")
            || !IsWindowVisible(hwnd).as_bool() || crate::apps::is_cloaked(hwnd)
            || !is_start_app(crate::apps::exe_of_window(hwnd).as_deref())
        {
            return None;
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
        (!monitor.is_invalid()).then_some(monitor.0 as isize)
    }
}

/// How long a Start request waits for the user's Shift / Ctrl / Alt /
/// Win to come up, and how often it looks (ms).
const START_WAIT_MS: u128 = 1000;
const START_POLL_MS: u32 = 25;

/// What a waiting Start request does now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StartStep {
    Send,
    Wait,
    /// Too old (START_WAIT_MS, whatever the keys do now: a late timer
    /// never opens Start into what the user went on to), or no longer
    /// wanted (`live` false): given up, never mixed in.
    Drop,
}

/// Pure: a Start request `waited` ms old, still wanted (`live`: its bar
/// is there, the same window is in front, none of our menus is open),
/// with modifiers `held` now. The deadline is checked first.
fn start_step(live: bool, held: bool, waited: u128) -> StartStep {
    if !live || waited >= START_WAIT_MS {
        StartStep::Drop
    } else if held {
        StartStep::Wait
    } else {
        StartStep::Send
    }
}

/// A Start request waiting for the modifiers to come up.
#[derive(Clone, Copy)]
struct StartWait {
    timer: usize,
    since: std::time::Instant,
    /// The bar that asked (its HWND).
    owner: isize,
    /// The window in front when it asked: another one in front means the
    /// user moved on.
    front: isize,
}

thread_local! {
    /// The waiting Start request.
    static START_WAIT: std::cell::Cell<Option<StartWait>> = const { std::cell::Cell::new(None) };
}

fn front_window() -> isize {
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0 as isize
}

/// Windows' own Start menu, asked for by our UI (the top bar `owner`'s
/// Start button: its right click, a screen reader's context menu on it).
/// With Shift / Ctrl / Alt / Win held (Shift+F10, a modified click) the
/// Win key would make a chord with them: it goes out once they are
/// released, on a short timer of this thread (dispatched by any of its
/// message loops), or not at all if they are still held after
/// START_WAIT_MS. The user's own keys are never touched. A second request
/// from the same bar while one waits is the same one; another bar's
/// replaces it. It is called off (`cancel_start`) when the bar goes, when
/// another of its menus opens, by any other command on a bar (a click, a
/// screen reader's request: topbar::access::calls_off_start), and when
/// another window comes to the front.
pub fn start_menu(owner: isize) {
    if let Some(w) = START_WAIT.with(|w| w.get()) {
        if w.owner == owner {
            return;
        }
        cancel_start(None);
    }
    if !modifiers_held() {
        win();
        return;
    }
    use windows::Win32::UI::WindowsAndMessaging::SetTimer;
    let timer = unsafe { SetTimer(None, 0, START_POLL_MS, Some(start_tick)) };
    if timer == 0 {
        crate::log!("keys: no timer to wait for the modifiers; Start not sent");
        return;
    }
    START_WAIT.with(|w| w.set(Some(StartWait { timer, since: std::time::Instant::now(), owner, front: front_window() })));
}

/// Pure: whether `cancel_start(of)` calls off a request made by `owner`
/// (None: any request).
fn cancels(of: Option<isize>, owner: isize) -> bool {
    of.is_none_or(|o| o == owner)
}

/// Call off a waiting Start request: bar `of`'s (it is going), or any
/// (None: another menu or action took over).
pub fn cancel_start(of: Option<isize>) {
    let Some(w) = START_WAIT.with(|w| w.get()) else { return };
    if !cancels(of, w.owner) {
        return;
    }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(None, w.timer);
    }
    START_WAIT.with(|s| s.set(None));
}

unsafe extern "system" fn start_tick(_: windows::Win32::Foundation::HWND, _: u32, id: usize, _: u32) {
    use windows::Win32::UI::WindowsAndMessaging::{IsWindow, KillTimer};
    let Some(w) = START_WAIT.with(|w| w.get()).filter(|w| w.timer == id) else {
        let _ = KillTimer(None, id);
        return;
    };
    let live = IsWindow(Some(windows::Win32::Foundation::HWND(w.owner as *mut _))).as_bool() && front_window() == w.front && !crate::popup::is_tracking();
    let step = start_step(live, modifiers_held(), w.since.elapsed().as_millis());
    if step == StartStep::Wait {
        return;
    }
    let _ = KillTimer(None, w.timer);
    START_WAIT.with(|s| s.set(None));
    match step {
        StartStep::Send => win(),
        _ => crate::log!("keys: Start request expired or called off; not sent"),
    }
}

/// Windows' power-user menu (Win+X), for the Start button's right click.
pub fn win_x() {
    let _ = chord(&[VK_LWIN, VIRTUAL_KEY(b'X' as u16)]);
}

/// Cast to a wireless display (Win+K).
pub fn cast() {
    let _ = chord(&[VK_LWIN, VIRTUAL_KEY(b'K' as u16)]);
}

/// Notification center and calendar (Win+N).
pub fn notifications() {
    let _ = chord(&[VK_LWIN, VIRTUAL_KEY(b'N' as u16)]);
}

/// Win+Tab (Task View), Win+D (desktop), Win+N (notifications), Win+A
/// (quick settings), for `unmixed`.
pub const TASK_VIEW: [u16; 2] = [0x5B, 0x09];
pub const SHOW_DESKTOP: [u16; 2] = [0x5B, b'D' as u16];
pub const NOTIFICATIONS: [u16; 2] = [0x5B, b'N' as u16];
pub const QUICK_SETTINGS: [u16; 2] = [0x5B, b'A' as u16];
/// Win+B: Windows' "set focus in the notification area" (the taskbar's
/// "Show hidden icons" button), for the top bar's notification area button.
pub const NOTIFICATION_AREA: [u16; 2] = [0x5B, b'B' as u16];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partly_sent_chords_release_what_went_down() {
        // Win+Tab: [Win down, Tab down, Tab up, Win up].
        assert_eq!(still_down(0, 2), 0);
        assert_eq!(still_down(1, 2), 1, "Win down");
        assert_eq!(still_down(2, 2), 2, "both down");
        assert_eq!(still_down(3, 2), 1, "Tab released, Win still down");
        assert_eq!(still_down(4, 2), 0);
        assert_eq!(TASK_VIEW[0], VK_LWIN.0);
    }

    #[test]
    fn start_waits_for_the_modifiers_but_not_for_ever() {
        assert_eq!(start_step(true, false, 0), StartStep::Send, "nothing held: at once");
        assert_eq!(start_step(true, true, 0), StartStep::Wait, "Shift still down (Shift+F10)");
        assert_eq!(start_step(true, true, START_WAIT_MS - 1), StartStep::Wait);
        assert_eq!(start_step(true, false, 400), StartStep::Send, "released: now");
        assert_eq!(start_step(true, true, START_WAIT_MS), StartStep::Drop, "held too long: dropped, not mixed in");
    }

    #[test]
    fn a_late_timer_never_sends_after_the_deadline() {
        // The UI thread was busy: the first tick after the deadline finds
        // the keys already up. Still too late.
        assert_eq!(start_step(true, false, 1000), StartStep::Drop);
        assert_eq!(start_step(true, false, 2000), StartStep::Drop);
        assert_eq!(start_step(true, false, START_WAIT_MS - 1), StartStep::Send);
    }

    #[test]
    fn a_request_no_longer_wanted_is_dropped() {
        // Bar gone, another window in front, or our own menu open.
        assert_eq!(start_step(false, false, 0), StartStep::Drop);
        assert_eq!(start_step(false, true, 10), StartStep::Drop);
    }

    #[test]
    fn cancel_is_tied_to_the_requesting_bar() {
        assert!(cancels(Some(7), 7), "its bar goes");
        assert!(!cancels(Some(8), 7), "another bar going leaves it");
        assert!(cancels(None, 7), "another menu or action: any request");
    }
}
