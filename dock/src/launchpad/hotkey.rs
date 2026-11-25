//! The global Launchpad hotkey (shortcut "launchpad"): written like the
//! search hotkey ("alt+f1", "ctrl+shift+space"; parsed by
//! search::hotkey::parse) and registered on the dock window with its own id.
//! "none" (the default) registers nothing.

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};

/// WM_HOTKEY id of the Launchpad hotkey (search uses 0x5345).
pub const ID: i32 = 0x4C50;

/// What is registered now: (owner, spec as given, registered?).
static CURRENT: Mutex<Option<(isize, String, bool)>> = Mutex::new(None);

/// Registers `spec` for `owner`, replacing what was there before. A spec
/// that does not parse, or one another app holds, is logged (Launchpad
/// stays on the top bar and the dock).
pub fn apply(owner: HWND, spec: &str) {
    let Ok(mut cur) = CURRENT.lock() else { return };
    let me = owner.0 as isize;
    if cur.as_ref().is_some_and(|(o, s, _)| *o == me && s == spec) {
        return;
    }
    if let Some((old, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), ID);
        }
    }
    let ok = match crate::search::hotkey::parse(spec) {
        Ok(None) => {
            crate::hotkeys::record(crate::hotkeys::LAUNCHPAD, spec, crate::hotkeys::Status::Off);
            false
        }
        Ok(Some((mods, key))) => match unsafe { RegisterHotKey(Some(owner), ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, key) } {
            Ok(()) => {
                crate::log!("launchpad: hotkey {spec}");
                crate::hotkeys::record(crate::hotkeys::LAUNCHPAD, spec, crate::hotkeys::Status::Ok);
                true
            }
            Err(e) => {
                crate::log!("launchpad: hotkey {spec} not registered ({e})");
                crate::hotkeys::record(crate::hotkeys::LAUNCHPAD, spec, crate::hotkeys::Status::from_error(&e));
                false
            }
        },
        Err(e) => {
            crate::log!("launchpad: bad hotkey {e}");
            crate::hotkeys::record(crate::hotkeys::LAUNCHPAD, spec, crate::hotkeys::Status::Invalid);
            false
        }
    };
    *cur = Some((me, spec.to_string(), ok));
}

/// Gives the hotkey back (the dock window is going away).
pub fn release() {
    let Ok(mut cur) = CURRENT.lock() else { return };
    if let Some((owner, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(owner as *mut _)), ID);
        }
    }
}
