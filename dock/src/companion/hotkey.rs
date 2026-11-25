use super::HOTKEY_ID;
use std::sync::Mutex;
use windows::Win32::Foundation::HWND;
static CHAT: Mutex<Option<(isize, String, bool)>> = Mutex::new(None);
static HISTORY: Mutex<Option<(isize, String, bool)>> = Mutex::new(None);
pub fn apply_history_hotkey(owner: HWND, spec: &str) {
    apply(owner, spec, &HISTORY, super::chat::HISTORY_HOTKEY_ID, "companion_history");
}
pub fn apply_chat_hotkey(owner: HWND, spec: &str) {
    apply(
        owner,
        spec,
        &CHAT,
        super::chat::HOTKEY_ID,
        "companion_chat",
    );
}
static SNOOZE:Mutex<Option<(isize,String,bool)>>=Mutex::new(None);
static HOTKEY: Mutex<Option<(isize, String, bool)>> = Mutex::new(None);

/// Shortcut "companion" (crate::shortcuts): registers `spec` on `owner`.
pub fn apply_hotkey(owner: HWND, spec: &str) {apply(owner,spec,&HOTKEY,HOTKEY_ID,crate::shortcuts::COMPANION);}
pub fn apply_snooze_hotkey(owner: HWND, spec: &str) {apply(owner,spec,&SNOOZE,super::SNOOZE_HOTKEY_ID,"companion_snooze");}
fn apply(owner:HWND,spec:&str,slot:&Mutex<Option<(isize,String,bool)>>,id:i32,shortcut:&str) {
    use crate::hotkeys::{record, Status};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
    };
    let Ok(mut cur) = slot.lock() else { return };
    let me = owner.0 as isize;
    if cur.as_ref().is_some_and(|(o, s, _)| *o == me && s == spec) {
        return;
    }
    if let Some((old, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), id);
        }
    }
    let row_key = crate::shortcuts::row_key(shortcut);
    let ok = match crate::search::hotkey::parse(spec) {
        Ok(None) => {
            record(row_key, spec, Status::Off);
            false
        }
        Ok(Some((mods, key))) => match unsafe {
            RegisterHotKey(
                Some(owner),
                id,
                HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT,
                key,
            )
        } {
            Ok(()) => {
                crate::log!("companion: hotkey {spec}");
                record(row_key, spec, Status::Ok);
                true
            }
            Err(e) => {
                crate::log!("companion: hotkey {spec} not registered ({e})");
                record(row_key, spec, Status::from_error(&e));
                false
            }
        },
        Err(e) => {
            crate::log!("companion: bad hotkey {e}");
            record(row_key, spec, Status::Invalid);
            false
        }
    };
    *cur = Some((me, spec.to_string(), ok));
}

/// Gives the hotkey back (the dock window is going away).
pub fn release_hotkey() {release(&CHAT, super::chat::HOTKEY_ID);
    release(&HISTORY, super::chat::HISTORY_HOTKEY_ID);
    release(&HOTKEY,HOTKEY_ID);release(&SNOOZE,super::SNOOZE_HOTKEY_ID);}
fn release(slot:&Mutex<Option<(isize,String,bool)>>,id:i32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;
    let Ok(mut cur) = slot.lock() else { return };
    if let Some((old, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_companion_hotkeys_report_registration_errors_to_their_settings_rows() {
        let cases = [
            (crate::shortcuts::COMPANION, apply_hotkey as fn(HWND, &str)),
            ("companion_snooze", apply_snooze_hotkey),
            ("companion_chat", apply_chat_hotkey),
            ("companion_history", apply_history_hotkey),
        ];
        for (id, apply) in cases {
            let spec = "ctrl+not-a-key";
            apply(HWND::default(), spec);
            let key = crate::shortcuts::row_key(id);
            let snapshot = crate::hotkeys::snapshot();
            assert_eq!(snapshot[key]["spec"], spec, "{key}");
            assert_eq!(snapshot[key]["status"], "invalid", "{key}");
        }
    }
}
