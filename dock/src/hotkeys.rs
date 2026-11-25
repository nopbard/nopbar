//! Whether each global hotkey (every shortcut in crate::shortcuts that
//! registers one) really got registered. The dock records every attempt; the settings window
//! reads them over the pipe and says when a key is held by another app,
//! with keys to try instead. A saved choice and a working key are two
//! different things: the setting stays, the row says it does not work.

use std::sync::Mutex;

/// The settings rows' keys of the shortcuts the dock reports on.
pub const SEARCH: &str = "shortcuts.search";
pub const LAUNCHPAD: &str = "shortcuts.launchpad";
pub const FOCUS_DOCK: &str = "shortcuts.focus_dock";
pub const DOCK_APPS: &str = "shortcuts.dock_apps";
pub const NOTES: &str = "shortcuts.notes";
pub const SNIP: &str = "shortcuts.snip";
pub const DESKTOP_PASTE: &str = "shortcuts.desktop_paste";
pub const FOCUS_TIMER: &str = "shortcuts.focus_timer";
pub const WINDOW_ON_TOP: &str = "shortcuts.window_on_top";
pub const WINDOW_TO_TRAY: &str = "shortcuts.window_to_tray";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// "none": nothing to register.
    Off,
    Ok,
    /// Another app (or Windows) already has this key.
    Taken,
    /// The text names no key we know.
    Invalid,
    /// Windows refused it for another reason.
    Failed,
}

impl Status {
    pub fn word(self) -> &'static str {
        match self {
            Status::Off => "off",
            Status::Ok => "ok",
            Status::Taken => "taken",
            Status::Invalid => "invalid",
            Status::Failed => "failed",
        }
    }

    pub fn from_word(w: &str) -> Option<Status> {
        Some(match w {
            "off" => Status::Off,
            "ok" => Status::Ok,
            "taken" => Status::Taken,
            "invalid" => Status::Invalid,
            "failed" => Status::Failed,
            _ => return None,
        })
    }

    /// RegisterHotKey's error (an HRESULT from windows-rs).
    pub fn from_error(e: &windows::core::Error) -> Status {
        // ERROR_HOTKEY_ALREADY_REGISTERED.
        if (e.code().0 as u32) & 0xFFFF == 1409 {
            Status::Taken
        } else {
            Status::Failed
        }
    }
}

/// (config key, spec as registered, status), newest per key.
static STATE: Mutex<Vec<(&'static str, String, Status)>> = Mutex::new(Vec::new());

/// The dock tried to register `spec` for the setting `key`.
pub fn record(key: &'static str, spec: &str, status: Status) {
    if let Ok(mut s) = STATE.lock() {
        s.retain(|(k, _, _)| *k != key);
        s.push((key, spec.to_string(), status));
    }
}

/// What the dock found, for the settings window (a JSON object).
pub fn snapshot() -> serde_json::Value {
    let mut o = serde_json::Map::new();
    if let Ok(s) = STATE.lock() {
        for (k, spec, st) in s.iter() {
            o.insert((*k).to_string(), serde_json::json!({ "spec": spec, "status": st.word() }));
        }
    }
    serde_json::Value::Object(o)
}

/// Pure: the dock's answer read back: (config key, spec, status).
pub fn parse(v: &serde_json::Value) -> Vec<(String, String, Status)> {
    let Some(o) = v.as_object() else { return Vec::new() };
    o.iter()
        .filter_map(|(k, e)| {
            let spec = e.get("spec")?.as_str()?.to_string();
            let st = Status::from_word(e.get("status")?.as_str()?)?;
            Some((k.clone(), spec, st))
        })
        .collect()
}

/// How a key spec is shown: "ctrl+alt+space" -> "Ctrl+Alt+Space", Win as
/// "⊞ Win".
pub fn display(spec: &str) -> String {
    crate::shortcuts::display_spec(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_round_trip() {
        for s in [Status::Off, Status::Ok, Status::Taken, Status::Invalid, Status::Failed] {
            assert_eq!(Status::from_word(s.word()), Some(s));
        }
    }

    #[test]
    fn taken_is_told_apart() {
        let taken = windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_0581u32 as i32));
        assert_eq!(Status::from_error(&taken), Status::Taken);
        let other = windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_0005u32 as i32));
        assert_eq!(Status::from_error(&other), Status::Failed);
    }

    #[test]
    fn snapshot_reads_back() {
        record(SEARCH, "ctrl+space", Status::Taken);
        record(SEARCH, "alt+space", Status::Ok);
        let got = parse(&snapshot());
        let s: Vec<_> = got.iter().filter(|(k, _, _)| k == SEARCH).collect();
        assert_eq!(s.len(), 1, "newest only");
        assert_eq!((s[0].1.as_str(), s[0].2), ("alt+space", Status::Ok));
        assert!(parse(&serde_json::json!({"x": {"spec": "a", "status": "?"}})).is_empty());
        assert!(parse(&serde_json::json!(null)).is_empty());
    }

    #[test]
    fn keys_are_shown_nicely() {
        assert_eq!(display("ctrl+alt+space"), "Ctrl+Alt+Space");
        assert_eq!(display("alt+f1"), "Alt+F1");
        assert_eq!(display("win+s"), "\u{229E} Win+S");
    }
}
