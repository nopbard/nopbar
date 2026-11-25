//! Switching to dock apps by number (shortcut "dock_apps"): the n-th app
//! in the dock (pinned first, then running, like the taskbar; Start, the
//! separator, stacks and the Recycle Bin do not count) for digit n, 0 for
//! the tenth.
//!
//! Three sets: the Windows key (Win+1..0, +Shift a new window, +Ctrl its
//! windows in turn, +Alt its menu with the jump list) through the keyboard
//! hook, since Windows keeps those keys for its taskbar; or Alt / Ctrl+Alt
//! (+Shift a new window) as ordinary hotkeys for people who want no hook.
//! Off by default; off, Windows' own Win+digit is untouched.

use std::sync::Mutex;

use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};

use super::*;
use crate::shortcuts::{self, DigitAct};

/// Posted by the keyboard hook: wParam slot, lParam DigitAct code (| 0x100:
/// the mask key could not be typed, Start may open too).
pub(super) const WM_DOCK_DIGIT: u32 = WM_APP + 96;

/// What is in force: (owner, set, hotkey ids registered).
static CURRENT: Mutex<Option<(isize, String, Vec<i32>)>> = Mutex::new(None);

fn unregister(owner: isize, ids: &[i32]) {
    for &id in ids {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(owner as *mut _)), id);
        }
    }
}

/// Puts set `v` ("none", "win", "alt", "ctrl+alt") in force for `owner`.
pub(super) fn apply(owner: HWND, v: &str) {
    let Ok(mut cur) = CURRENT.lock() else { return };
    let me = owner.0 as isize;
    let set = v.trim().to_ascii_lowercase();
    if cur.as_ref().is_some_and(|(o, s, _)| *o == me && *s == set) {
        return;
    }
    if let Some((old, _, ids)) = cur.take() {
        unregister(old, &ids);
    }
    crate::launchpad::set_win_digits(owner, set == "win", WM_DOCK_DIGIT);
    let mut ids = Vec::new();
    let mut status = if shortcuts::is_off(&set) { crate::hotkeys::Status::Off } else { crate::hotkeys::Status::Ok };
    for (id, mods, vk) in shortcuts::digit_hotkeys(&set) {
        match unsafe { RegisterHotKey(Some(owner), id, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk) } {
            Ok(()) => ids.push(id),
            Err(e) => {
                // Some of the set may be another app's: the rest still work.
                let st = crate::hotkeys::Status::from_error(&e);
                if status == crate::hotkeys::Status::Ok {
                    status = st;
                }
            }
        }
    }
    crate::log!("dock: apps by number: {set} ({} hotkeys, {})", ids.len(), status.word());
    crate::hotkeys::record(crate::hotkeys::DOCK_APPS, &set, status);
    *cur = Some((me, set, ids));
}

/// Gives the hotkeys and the hook back (the dock window is going away).
pub(super) fn release() {
    let Ok(mut cur) = CURRENT.lock() else { return };
    if let Some((owner, _, ids)) = cur.take() {
        unregister(owner, &ids);
    }
    crate::launchpad::set_win_digits(HWND::default(), false, WM_DOCK_DIGIT);
}

impl Dock {
    /// Which items a digit can reach, in dock order.
    fn digit_items(&self) -> Vec<bool> {
        self.items.iter().map(|it| it.kind == Kind::App && !it.stack).collect()
    }

    /// The app digit `slot` reaches, by its path (the primary dock's order).
    pub(super) fn digit_path(&self, slot: usize) -> Option<String> {
        let i = shortcuts::nth_app(&self.digit_items(), slot)?;
        Some(self.items[i].path.clone())
    }

    /// The item a digit's app is on this dock, if it shows it.
    pub(super) fn digit_item(&self, path: &str) -> Option<usize> {
        let ok = self.digit_items();
        self.items.iter().enumerate().position(|(i, it)| ok[i] && it.path == path)
    }

    /// Digit `slot` (on the primary) or the app at `path` (from
    /// digit_path, on another display's dock) with `act`. Some(item): show
    /// that item's menu (outside the dock's borrow).
    pub(super) unsafe fn digit(&mut self, slot: Option<usize>, path: &str, act: DigitAct) -> Option<usize> {
        let i = match slot {
            Some(s) => shortcuts::nth_app(&self.digit_items(), s)?,
            None => self.digit_item(path)?,
        };
        preview::hide();
        match act {
            DigitAct::Open => self.activate(i),
            DigitAct::NewWindow => self.launch(i),
            DigitAct::Cycle => {
                let wins = self.items[i].windows.clone();
                if wins.len() > 1 {
                    let fg = GetForegroundWindow();
                    let next = wins.iter().position(|&h| h == fg).map(|p| (p + 1) % wins.len()).unwrap_or(0);
                    super::input::focus(wins[next]);
                } else {
                    self.activate(i);
                }
            }
            DigitAct::JumpList => return Some(i),
        }
        None
    }
}
