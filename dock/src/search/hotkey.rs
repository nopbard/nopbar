//! The global search hotkey (shortcut "search", crate::shortcuts): parsed from
//! text like "alt+space" or "ctrl+shift+f", registered on the dock's UI
//! thread with RegisterHotKey. "none" (or empty) registers nothing.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};

/// WM_HOTKEY id of the search hotkey (any value in 0..0xBFFF).
pub const ID: i32 = 0x5345;

/// The window the hotkey is registered for (0 = none yet).
static OWNER: AtomicIsize = AtomicIsize::new(0);

/// The dock window the search hotkey belongs to (0 = none yet). A search
/// press can be posted to it (WM_HOTKEY with ID), e.g. to open NopBar's
/// own search when another launcher failed to start.
pub fn owner() -> isize {
    OWNER.load(Ordering::SeqCst)
}
/// What is registered now: (spec as given, registered?).
static CURRENT: Mutex<Option<(String, bool)>> = Mutex::new(None);

/// (modifiers, virtual key) of a spec; Ok(None) for "none" / "".
/// (crate::shortcuts::parse, as numbers.)
pub fn parse(spec: &str) -> Result<Option<(u32, u32)>, String> {
    crate::shortcuts::parse(spec).map(|c| c.map(|c| (c.mods, c.vk)))
}

/// Registers `spec` for `owner` (the dock window), replacing what was
/// registered before. A spec that does not parse, or a hotkey another app
/// already holds, is logged: search stays on the top bar button only.
pub fn apply(owner: HWND, spec: &str) {
    let Ok(mut cur) = CURRENT.lock() else { return };
    let old_owner = OWNER.swap(owner.0 as isize, Ordering::SeqCst);
    if old_owner == owner.0 as isize && cur.as_ref().is_some_and(|(s, _)| s == spec) {
        return;
    }
    if let Some((_, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old_owner as *mut _)), ID);
        }
    }
    let ok = match parse(spec) {
        Ok(None) => {
            crate::log!("search: no hotkey (\"{spec}\")");
            crate::hotkeys::record(crate::hotkeys::SEARCH, spec, crate::hotkeys::Status::Off);
            false
        }
        Ok(Some((mods, key))) => match unsafe { RegisterHotKey(Some(owner), ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, key) } {
            Ok(()) => {
                crate::log!("search: hotkey {spec}");
                crate::hotkeys::record(crate::hotkeys::SEARCH, spec, crate::hotkeys::Status::Ok);
                true
            }
            Err(e) => {
                crate::log!("search: hotkey {spec} not registered ({e}); the top bar button still opens search");
                crate::hotkeys::record(crate::hotkeys::SEARCH, spec, crate::hotkeys::Status::from_error(&e));
                false
            }
        },
        Err(e) => {
            crate::log!("search: bad hotkey {e}; the top bar button still opens search");
            crate::hotkeys::record(crate::hotkeys::SEARCH, spec, crate::hotkeys::Status::Invalid);
            false
        }
    };
    *cur = Some((spec.to_string(), ok));
}

/// Gives the hotkey back (the dock window is going away).
pub fn release() {
    let Ok(mut cur) = CURRENT.lock() else { return };
    if let Some((_, true)) = cur.take() {
        let owner = OWNER.load(Ordering::SeqCst);
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(owner as *mut _)), ID);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

    #[test]
    fn specs() {
        assert_eq!(parse("alt+space"), Ok(Some((MOD_ALT.0, 0x20))));
        assert_eq!(parse(" Ctrl + Shift + F "), Ok(Some((MOD_CONTROL.0 | MOD_SHIFT.0, b'F' as u32))));
        assert_eq!(parse("win+s"), Ok(Some((MOD_WIN.0, b'S' as u32))));
        assert_eq!(parse("ctrl+alt+1"), Ok(Some((MOD_CONTROL.0 | MOD_ALT.0, b'1' as u32))));
        assert_eq!(parse("f13"), Ok(Some((0, 0x7C))));
        assert_eq!(parse("none"), Ok(None));
        assert_eq!(parse(""), Ok(None));
        assert!(parse("space").is_err());
        assert!(parse("alt+").is_err());
        assert!(parse("alt+a+b").is_err());
        assert!(parse("alt+wat").is_err());
        assert!(parse("ctrl+f25").is_err());
    }
}
