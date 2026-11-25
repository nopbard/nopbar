//! Every keyboard shortcut NopBar has, in one list (the registry).
//!
//! RULE: every new NopBar shortcut — a global hotkey, a key the dock or a
//! panel reacts to, a low-level hook chord — is added to `ALL` below. The
//! Settings > Shortcuts page is built from this list, so a shortcut added
//! here appears there (with its recorder, Reset, conflict check and
//! translated label) without any other change; one that is not here is
//! invisible to the user and to the duplicate check.
//!
//! What a shortcut is stored as: `Config::shortcuts[id]` (a spec like
//! "ctrl+alt+space", "none" = off). Older config files kept them in other
//! places (`search.hotkey`, `launchpad.hotkey`, `launchpad.win_key`,
//! `hotkeys.focus_dock`, `hotkeys.win_digits`): those
//! are read once on load (`Config::migrate_shortcuts`) and never written
//! again.
//!
//! This module is pure (parsing, formatting, conflicts, the recorder's
//! rules, digit mapping) apart from each entry's `apply`, which only the
//! dock calls, on its UI thread.

use windows::Win32::Foundation::HWND;

use crate::config::Config;

/// RegisterHotKey's modifier bits (MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN).
pub const ALT: u32 = 0x1;
pub const COMPANION: &str = "companion";
pub const CTRL: u32 = 0x2;
pub const SHIFT: u32 = 0x4;
pub const WIN: u32 = 0x8;

/// One key with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: u32,
    pub vk: u32,
}

/// Named keys (config text, shown text, virtual key). Letters, digits and
/// F1..F24 are handled apart.
const NAMED: &[(&str, &str, u32)] = &[
    ("space", "Space", 0x20),
    ("enter", "Enter", 0x0D),
    ("tab", "Tab", 0x09),
    ("esc", "Esc", 0x1B),
    ("backspace", "Backspace", 0x08),
    ("insert", "Insert", 0x2D),
    ("delete", "Delete", 0x2E),
    ("home", "Home", 0x24),
    ("end", "End", 0x23),
    ("pageup", "PgUp", 0x21),
    ("pagedown", "PgDn", 0x22),
    ("up", "Up", 0x26),
    ("down", "Down", 0x28),
    ("left", "Left", 0x25),
    ("right", "Right", 0x27),
    ("`", "`", 0xC0),
    ("-", "-", 0xBD),
    ("=", "=", 0xBB),
    (",", ",", 0xBC),
    (".", ".", 0xBE),
    ("/", "/", 0xBF),
    (";", ";", 0xBA),
    ("[", "[", 0xDB),
    ("]", "]", 0xDD),
    ("'", "'", 0xDE),
    ("\\", "\\", 0xDC),
];

/// Other spellings accepted in config text.
const ALIASES: &[(&str, &str)] = &[
    ("return", "enter"),
    ("escape", "esc"),
    ("ins", "insert"),
    ("del", "delete"),
    ("pgup", "pageup"),
    ("pgdn", "pagedown"),
    ("backquote", "`"),
    ("minus", "-"),
    ("equals", "="),
    ("comma", ","),
    ("period", "."),
    ("slash", "/"),
    ("semicolon", ";"),
];

/// The virtual key named `name` (lowercase).
pub fn vk_of(name: &str) -> Option<u32> {
    let one: Vec<char> = name.chars().collect();
    if one.len() == 1 && (one[0].is_ascii_lowercase() || one[0].is_ascii_digit()) {
        return Some(one[0].to_ascii_uppercase() as u32);
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x6F + n);
    }
    let name = ALIASES.iter().find(|(a, _)| *a == name).map(|(_, n)| *n).unwrap_or(name);
    NAMED.iter().find(|(n, _, _)| *n == name).map(|(_, _, vk)| *vk)
}

/// (config text, shown text) of virtual key `vk`; None for keys we do not
/// offer (modifiers, media keys...).
pub fn name_of(vk: u32) -> Option<(String, String)> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => {
            let c = char::from_u32(vk)?;
            Some((c.to_ascii_lowercase().to_string(), c.to_string()))
        }
        0x70..=0x87 => Some((format!("f{}", vk - 0x6F), format!("F{}", vk - 0x6F))),
        _ => NAMED.iter().find(|(_, _, v)| *v == vk).map(|(n, s, _)| (n.to_string(), s.to_string())),
    }
}

/// Is `vk` a modifier key (Shift, Ctrl, Alt or a Windows key, either side)?
pub fn is_modifier(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

/// Parses config text ("alt+space", "Ctrl + Shift + F", "win+1"). Ok(None)
/// for "none", "off" or "". A key without a modifier is refused (it would
/// swallow typing everywhere) unless it is F1..F24.
pub fn parse(spec: &str) -> Result<Option<Chord>, String> {
    let s = spec.trim().to_ascii_lowercase();
    if s.is_empty() || s == "none" || s == "off" {
        return Ok(None);
    }
    let mut mods = 0u32;
    let mut key = None;
    // "ctrl++" is not a thing: a trailing empty part is "no key".
    for part in s.split('+').map(str::trim) {
        let m = match part {
            "alt" => ALT,
            "ctrl" | "control" => CTRL,
            "shift" => SHIFT,
            "win" | "windows" | "super" => WIN,
            _ => 0,
        };
        if m != 0 {
            mods |= m;
            continue;
        }
        if key.is_some() {
            return Err(format!("\"{spec}\": more than one key"));
        }
        key = Some(vk_of(part).ok_or_else(|| format!("\"{spec}\": unknown key \"{part}\""))?);
    }
    let vk = key.ok_or_else(|| format!("\"{spec}\": no key"))?;
    if mods == 0 && !(0x70..=0x87).contains(&vk) {
        return Err(format!("\"{spec}\": needs a modifier (alt, ctrl, shift, win)"));
    }
    Ok(Some(Chord { mods, vk }))
}

/// The modifiers in the order they are written and shown.
const MOD_ORDER: [(u32, &str, &str); 4] = [(WIN, "win", "\u{229E} Win"), (CTRL, "ctrl", "Ctrl"), (ALT, "alt", "Alt"), (SHIFT, "shift", "Shift")];

impl Chord {
    pub const fn new(mods: u32, vk: u32) -> Chord {
        Chord { mods, vk }
    }

    /// Config text: "win+ctrl+alt+shift+k" (lowercase, that order).
    pub fn spec(&self) -> String {
        let mut parts: Vec<String> = MOD_ORDER.iter().filter(|(m, _, _)| self.mods & m != 0).map(|(_, n, _)| n.to_string()).collect();
        parts.push(name_of(self.vk).map(|(n, _)| n).unwrap_or_else(|| format!("vk{}", self.vk)));
        parts.join("+")
    }

    /// Shown text: "⊞ Win+Shift+S", "Ctrl+Alt+Space".
    pub fn display(&self) -> String {
        let mut parts: Vec<String> = MOD_ORDER.iter().filter(|(m, _, _)| self.mods & m != 0).map(|(_, _, s)| s.to_string()).collect();
        parts.push(name_of(self.vk).map(|(_, s)| s).unwrap_or_else(|| "?".into()));
        parts.join("+")
    }
}

/// Modifiers alone, as the recorder shows them while they are held
/// ("Ctrl+Alt+…").
pub fn display_mods(mods: u32) -> String {
    let parts: Vec<&str> = MOD_ORDER.iter().filter(|(m, _, _)| mods & m != 0).map(|(_, _, s)| *s).collect();
    if parts.is_empty() {
        return String::new();
    }
    format!("{}+\u{2026}", parts.join("+"))
}

/// Config text as shown: the chord, or the text itself when it does not
/// parse ("none" is handled by the caller).
pub fn display_spec(spec: &str) -> String {
    match parse(spec) {
        Ok(Some(c)) => c.display(),
        // A lone fixed key ("esc", "space"): its shown name.
        _ => name_vk(spec).and_then(name_of).map(|(_, shown)| shown).unwrap_or_else(|| spec.to_string()),
    }
}

/// Config text as its keys, plainly ("win+alt+n" -> Win, Alt, N: no
/// Windows logo glyph), for text that reads it out (a widget's line in
/// Settings) and its keycaps. Empty when off.
pub fn plain_keys(spec: &str) -> Vec<String> {
    if is_off(spec) {
        return Vec::new();
    }
    match parse(spec) {
        Ok(Some(c)) => {
            let mut v: Vec<String> = MOD_ORDER.iter().filter(|(m, _, _)| c.mods & m != 0).map(|(_, _, s)| s.trim_start_matches('\u{229E}').trim().to_string()).collect();
            v.push(name_of(c.vk).map(|(_, s)| s).unwrap_or_else(|| "?".into()));
            v
        }
        _ => vec![display_spec(spec)],
    }
}

// ---------------------------------------------------------------- registry

/// Where the page lists it (a heading per area, in this order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    /// Opening search and Launchpad.
    Open,
    /// The dock: focus, app switching by number.
    Dock,
    /// The window in front: keep on top, hide to tray.
    Windows,
    /// Keys that work only in a moment (an icon held, the pointer on the
    /// dock): shown, not changed.
    Context,
}

pub const AREAS: [(Area, &str); 4] = [
    (Area::Open, "shortcuts.area.open"),
    (Area::Dock, "shortcuts.area.dock"),
    (Area::Windows, "shortcuts.area.windows"),
    (Area::Context, "shortcuts.area.context"),
];

/// Where it works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Everywhere (a global hotkey or the keyboard hook).
    Global,
    /// While the pointer is on the dock or an icon is held.
    Dock,
}

/// How it is caught.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// RegisterHotKey on the dock window.
    Hotkey,
    /// The low-level keyboard hook (launchpad/wintap.rs): for keys Windows
    /// keeps for itself (the Windows key alone, Win+digit).
    Hook,
}

/// What the user can do with it on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Any chord, recorded with the keys.
    Chord,
    /// One fixed key (the text given), on (that text) or off ("none").
    Switch(&'static str),
    /// Digits 1..9, 0 with a chosen modifier set (DIGIT_SETS).
    Digits,
    /// Fixed and always on: listed so it can be found, not changed.
    Fixed,
}

pub struct Shortcut {
    /// Key in Config::shortcuts (the settings row is "shortcuts.<id>").
    pub id: &'static str,
    /// Text keys of its name and of the line under it.
    pub label: &'static str,
    pub hint: &'static str,
    /// Its value in a new config ("none" = off).
    pub default: &'static str,
    pub area: Area,
    pub scope: Scope,
    pub how: How,
    pub kind: Kind,
    /// The config key it lived in before the registry ("" = none): read
    /// from older files only.
    pub legacy: &'static str,
    /// Puts the value in force (dock UI thread). None for Fixed ones (their
    /// owners register them for the moment they work).
    pub apply: Option<fn(HWND, &str)>,
}

pub const SEARCH: &str = "search";
pub const LAUNCHPAD: &str = "launchpad";
pub const WIN_TAP: &str = "launchpad_win_tap";
/// Brings the desktop notes to the front, or sends them back (crate::notes).
pub const NOTES: &str = "notes";
/// Pins a piece of the screen where it was, as an image piece
/// (crate::pieces::overlay).
pub const SNIP: &str = "snip";
/// Ctrl+V on the desktop makes a note or an image piece
/// (crate::desklayer::deskpaste).
pub const DESKTOP_PASTE: &str = "desktop_paste";
/// Starts the last focus timer, or pauses / resumes it (crate::focus_timer).
pub const FOCUS_TIMER: &str = "focus_timer";
pub const FOCUS_DOCK: &str = "focus_dock";
pub const DOCK_APPS: &str = "dock_apps";
/// "Always on top" for the window in front (dock/ontop.rs).
pub const WINDOW_ON_TOP: &str = "window_on_top";
/// "Hide to tray" for the window in front (crate::traywin).
pub const WINDOW_TO_TRAY: &str = "window_to_tray";
/// Space on a selected item of an open folder stack: Quick Look
/// (crate::launchers::preview).
pub const QUICK_LOOK: &str = "stack_quick_look";

/// Modifier sets for switching to dock apps by number: (config text, text
/// key of the option). "win" is the taskbar's own keys, caught by the hook.
pub const DIGIT_SETS: [(&str, &str); 4] = [
    ("none", "shortcuts.dock_apps.off"),
    ("win", "shortcuts.dock_apps.win"),
    ("alt", "shortcuts.dock_apps.alt"),
    ("ctrl+alt", "shortcuts.dock_apps.ctrl_alt"),
];

pub static ALL: &[Shortcut] = &[
    Shortcut {
        id: SEARCH,
        label: "shortcuts.search",
        hint: "shortcuts.search.hint",
        default: "none",
        area: Area::Open,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Chord,
        legacy: "search.hotkey",
        apply: Some(crate::search::hotkey::apply),
    },
    Shortcut {
        id: LAUNCHPAD,
        label: "shortcuts.launchpad",
        hint: "shortcuts.launchpad.hint",
        default: "none",
        area: Area::Open,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Chord,
        legacy: "launchpad.hotkey",
        apply: Some(crate::launchpad::apply_hotkey),
    },
    Shortcut {
        id: WIN_TAP,
        label: "shortcuts.win_tap",
        hint: "shortcuts.win_tap.hint",
        default: "none",
        area: Area::Open,
        scope: Scope::Global,
        how: How::Hook,
        kind: Kind::Switch("win"),
        legacy: "launchpad.win_key",
        apply: Some(crate::launchpad::apply_win_tap),
    },
    Shortcut {
        id: NOTES,
        label: "shortcuts.notes",
        hint: "shortcuts.notes.hint",
        default: "win+alt+n",
        area: Area::Open,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Chord,
        legacy: "",
        apply: Some(crate::notes::apply_hotkey),
    },
    Shortcut {
        id: SNIP,
        label: "shortcuts.snip",
        hint: "shortcuts.snip.hint",
        default: "win+alt+s",
        area: Area::Open,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Chord,
        legacy: "",
        apply: Some(crate::pieces::overlay::apply_hotkey),
    },
    Shortcut {
        id: DESKTOP_PASTE,
        label: "shortcuts.desktop_paste",
        hint: "shortcuts.desktop_paste.hint",
        default: "ctrl+v",
        area: Area::Open,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Switch("ctrl+v"),
        legacy: "",
        apply: Some(crate::desklayer::deskpaste::apply),
    },
    Shortcut {id: "companion_chat",
        label: "shortcuts.companion_chat",
        hint: "shortcuts.companion_chat.hint",
        default: "none",
        area: Area::Open,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Chord,
        legacy: "",
        apply: Some(crate::companion::chat::apply_hotkey),
    },
    Shortcut {
        id: "companion_history", label: "shortcuts.companion_history", hint: "shortcuts.companion_history.hint",
        default: "none", area: Area::Open, scope: Scope::Global, how: How::Hotkey, kind: Kind::Chord,
        legacy: "", apply: Some(crate::companion::chat::apply_history_hotkey),
    },
    Shortcut {
        id: "companion_snooze",label:"companion.rest",hint:"shortcuts.companion_snooze.hint",default:"none",area:Area::Open,scope:Scope::Global,how:How::Hotkey,kind:Kind::Chord,legacy:"",apply:Some(crate::companion::apply_snooze_hotkey),
    },
    Shortcut {
        id: COMPANION,
        label: "shortcuts.companion",
        hint: "shortcuts.companion.hint",
        default: "none",
        area: Area::Open,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Chord,
        legacy: "",
        apply: Some(crate::companion::apply_hotkey),
    },
    Shortcut {
        id: FOCUS_TIMER,
        label: "shortcuts.focus_timer",
        hint: "shortcuts.focus_timer.hint",
        default: "none",
        area: Area::Open,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Chord,
        legacy: "",
        apply: Some(crate::focus_timer::apply_hotkey),
    },
    Shortcut {
        id: FOCUS_DOCK, label: "shortcuts.focus_dock", hint: "shortcuts.focus_dock.hint",
        default: "win+t", area: Area::Dock, scope: Scope::Global, how: How::Hook, kind: Kind::Chord,
        legacy: "hotkeys.focus_dock", apply: Some(crate::dock::apply_focus_hotkey),
    },
    Shortcut {
        // Off by default for now: the bar takes the keyboard but shows no
        // focus yet (seen in Windows Sandbox, 0.1.102). "win+b" when it does.
        id: "taskbar_tray", label: "shortcuts.taskbar_tray", hint: "shortcuts.taskbar_tray.hint",
        default: "none", area: Area::Dock, scope: Scope::Global, how: How::Hook, kind: Kind::Chord, legacy: "",
        apply: Some(crate::launchpad::wintap::apply_tray),
    },
    Shortcut {
        id: DOCK_APPS,
        label: "shortcuts.dock_apps",
        hint: "shortcuts.dock_apps.hint",
        default: "win",
        area: Area::Dock,
        scope: Scope::Global,
        how: How::Hook,
        kind: Kind::Digits,
        legacy: "hotkeys.win_digits",
        apply: Some(crate::dock::apply_digits),
    },
    Shortcut {
        id: WINDOW_ON_TOP,
        label: "shortcuts.window_on_top",
        hint: "shortcuts.window_on_top.hint",
        default: "none",
        area: Area::Windows,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Chord,
        legacy: "",
        apply: Some(crate::dock::apply_ontop_hotkey),
    },
    Shortcut {
        id: WINDOW_TO_TRAY,
        label: "shortcuts.window_to_tray",
        hint: "shortcuts.window_to_tray.hint",
        default: "none",
        area: Area::Windows,
        scope: Scope::Global,
        how: How::Hotkey,
        kind: Kind::Chord,
        legacy: "",
        apply: Some(crate::dock::apply_tray_hotkey),
    },
    Shortcut {
        id: "dock_undo",
        label: "shortcuts.dock_undo",
        hint: "shortcuts.dock_undo.hint",
        default: "ctrl+z",
        area: Area::Context,
        scope: Scope::Dock,
        how: How::Hotkey,
        kind: Kind::Fixed,
        legacy: "",
        apply: None,
    },
    Shortcut {
        id: QUICK_LOOK,
        label: "shortcuts.quick_look",
        hint: "shortcuts.quick_look.hint",
        default: "space",
        area: Area::Context,
        scope: Scope::Dock,
        how: How::Hotkey,
        kind: Kind::Fixed,
        legacy: "",
        // The stack panel catches it while it is open (dock/stack.rs).
        apply: None,
    },
    Shortcut {
        id: "dock_cancel_drag",
        label: "shortcuts.dock_cancel_drag",
        hint: "",
        default: "esc",
        area: Area::Context,
        scope: Scope::Dock,
        how: How::Hotkey,
        kind: Kind::Fixed,
        legacy: "",
        apply: None,
    },
];

pub fn find(id: &str) -> Option<&'static Shortcut> {
    ALL.iter().find(|s| s.id == id)
}

/// The id of a settings row key ("shortcuts.search" -> "search").
pub fn id_of_key(key: &str) -> Option<&'static str> {
    let id = key.strip_prefix("shortcuts.")?;
    find(id).map(|s| s.id)
}

/// The settings row key of a shortcut ("shortcuts.<id>"), made once.
pub fn row_key(id: &str) -> &'static str {
    static KEYS: std::sync::OnceLock<Vec<(&'static str, &'static str)>> = std::sync::OnceLock::new();
    let keys = KEYS.get_or_init(|| ALL.iter().map(|s| (s.id, &*Box::leak(format!("shortcuts.{}", s.id).into_boxed_str()))).collect());
    keys.iter().find(|(i, _)| *i == id).map(|(_, k)| *k).unwrap_or("shortcuts.?")
}

/// Is `v` a value `s` can hold? (Chords must parse; a switch is its chord
/// or "none"; digits one of DIGIT_SETS.)
pub fn valid(s: &Shortcut, v: &str) -> bool {
    let v = v.trim().to_ascii_lowercase();
    match s.kind {
        Kind::Chord => parse(&v).is_ok(),
        Kind::Switch(on) => v == "none" || v == on,
        Kind::Digits => DIGIT_SETS.iter().any(|(d, _)| *d == v),
        Kind::Fixed => v == s.default,
    }
}

/// A switch's "on" value ("none" for other kinds).
pub fn switch_on(s: &Shortcut) -> &'static str {
    match s.kind {
        Kind::Switch(on) => on,
        _ => "none",
    }
}

/// Shown, not changed: fixed, or working only in a moment (an icon held).
pub fn read_only(s: &Shortcut) -> bool {
    s.kind == Kind::Fixed || s.scope != Scope::Global
}

/// Caught by the keyboard hook while on (a lone Windows key, Win+digit).
pub fn uses_hook(s: &Shortcut, v: &str) -> bool {
    s.how == How::Hook && (v.trim().eq_ignore_ascii_case("win") || (s.kind == Kind::Chord && !is_off(v)))
}

/// Is it off?
pub fn is_off(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "" | "none" | "off")
}

/// The value an older config file (`old`, as read) holds in `s`'s old key,
/// as registry text (switches become the switch's chord or "none"; the
/// Windows-key digits "win" or "none"). None when the file does not have it.
pub fn legacy_value(old: &serde_json::Value, s: &Shortcut) -> Option<String> {
    let (section, field) = s.legacy.split_once('.')?;
    let v = old.get(section)?.get(field)?;
    match (v.as_str(), v.as_bool()) {
        (Some(text), _) => Some(text.to_string()),
        (None, Some(on)) => Some(match s.kind {
            Kind::Digits => (if on { "win" } else { "none" }).to_string(),
            _ => (if on { switch_on(s) } else { "none" }).to_string(),
        }),
        _ => None,
    }
}

/// Puts every shortcut of `cfg` in force (dock UI thread). Unchanged ones
/// cost nothing (each apply keeps what it registered).
pub fn apply_all(owner: HWND, cfg: &Config) {
    for s in ALL {
        if let Some(apply) = s.apply {
            apply(owner, &cfg.shortcut(s.id));
        }
    }
}

/// Turns every shortcut off for a while (Settings is recording one): the
/// keys reach the recorder instead of opening search, Launchpad... The
/// config is untouched; apply_all puts them back.
pub fn suspend_all(owner: HWND) {
    for s in ALL {
        if let Some(apply) = s.apply {
            apply(owner, "none");
        }
    }
}

// ---------------------------------------------------------------- conflicts

/// Only a Windows-key cycle acquires an implicit reverse shortcut.
pub fn has_reverse_cycle(mods: u32) -> bool {
    mods & (WIN | SHIFT) == WIN
}

/// The chords a shortcut holding `v` uses (a digit set: all twenty or
/// forty of them). Empty when off.
pub fn chords(s: &Shortcut, v: &str) -> Vec<Chord> {
    match s.kind {
        Kind::Chord | Kind::Fixed => match parse(v) {
            Ok(Some(c)) if s.id == FOCUS_DOCK && has_reverse_cycle(c.mods) => vec![c, Chord::new(c.mods | SHIFT, c.vk)],
            Ok(Some(c)) => vec![c],
            // Esc alone (Fixed) does not parse as a global chord.
            _ if s.kind == Kind::Fixed => name_vk(v).map(|vk| vec![Chord::new(0, vk)]).unwrap_or_default(),
            _ => Vec::new(),
        },
        // The Windows key alone: not a chord anything else could take.
        Kind::Switch(_) => Vec::new(),
        Kind::Digits => digit_chords(v),
    }
}

fn name_vk(v: &str) -> Option<u32> {
    vk_of(&v.trim().to_ascii_lowercase())
}

/// Every chord a digit set catches: (modifiers) x digits 0..9.
pub fn digit_chords(set: &str) -> Vec<Chord> {
    let mods: &[u32] = match set.trim().to_ascii_lowercase().as_str() {
        "win" => &[WIN, WIN | SHIFT, WIN | CTRL, WIN | ALT],
        "alt" => &[ALT, ALT | SHIFT],
        "ctrl+alt" => &[CTRL | ALT, CTRL | ALT | SHIFT],
        _ => &[],
    };
    mods.iter().flat_map(|&m| (0x30..=0x39).map(move |vk| Chord::new(m, vk))).collect()
}

/// The other shortcut of `cfg` (not `id`) that already uses `chord`.
fn overlapping(a:Scope,b:Scope)->bool {a==Scope::Global||b==Scope::Global||a==b}
pub fn used_by(cfg: &Config, id: &str, chord: Chord) -> Option<&'static Shortcut> {
    let reverse = (id == FOCUS_DOCK && has_reverse_cycle(chord.mods)).then(|| Chord::new(chord.mods | SHIFT, chord.vk));
    ALL.iter().filter(|s| s.id != id && overlapping(find(id).map_or(Scope::Global,|a|a.scope),s.scope))
        .find(|s| chords(s, &cfg.shortcut(s.id)).iter().any(|c| *c == chord || Some(*c) == reverse))
}

/// The translated name of the other shortcut (not `except`) that already
/// uses `chord`. Only the shortcuts in ALL count: an entry an older build
/// left in the config's map (a removed feature's) holds no key.
pub fn clash(cfg: &Config, except: &str, chord: Chord) -> Option<String> {
    used_by(cfg, except, chord).map(|s| crate::i18n::t(s.label).to_string())
}

/// Shortcuts of `cfg` that clash with each other: (a, b) pairs, each once.
#[cfg(test)]
pub fn duplicates(cfg: &Config) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    for (i, a) in ALL.iter().enumerate() {
        let ca = chords(a, &cfg.shortcut(a.id));
        for b in &ALL[i + 1..] {
            if overlapping(a.scope,b.scope) && chords(b, &cfg.shortcut(b.id)).iter().any(|c| ca.contains(c)) {
                out.push((a.id, b.id));
            }
        }
    }
    out
}

/// Other chords to offer for `chord` (same key, other modifiers): not the
/// one asked about, not one NopBar uses (`used`), not one `taken` says
/// another app holds. At most three.
pub fn suggest(chord: Chord, used: &[Chord], taken: &dyn Fn(Chord) -> bool) -> Vec<Chord> {
    const SETS: [u32; 6] = [CTRL | ALT, CTRL | SHIFT, ALT | SHIFT, CTRL | ALT | SHIFT, WIN | ALT, WIN | SHIFT];
    SETS.iter()
        .map(|&m| Chord::new(m, chord.vk))
        .filter(|c| *c != chord && !used.contains(c))
        .filter(|c| !taken(*c))
        .take(3)
        .collect()
}

// ---------------------------------------------------------------- recorder

/// What one key press means to the recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rec {
    /// Only modifiers so far (shown as "Ctrl+Alt+…").
    Pending(u32),
    /// A whole chord.
    Done(Chord),
    /// Esc: stop recording, keep the old value.
    Cancel,
    /// Backspace or Delete alone: the shortcut is turned off.
    Clear,
    /// A plain key (no modifier): refused, keep listening.
    NeedsModifier,
}

/// A key went down while recording (`mods`: the modifiers held).
pub fn record(vk: u32, mods: u32) -> Rec {
    if is_modifier(vk) {
        return Rec::Pending(mods);
    }
    if vk == 0x1B {
        return Rec::Cancel;
    }
    if mods == 0 && (vk == 0x08 || vk == 0x2E) {
        return Rec::Clear;
    }
    if name_of(vk).is_none() {
        return Rec::Pending(mods);
    }
    if mods == 0 && !(0x70..=0x87).contains(&vk) {
        return Rec::NeedsModifier;
    }
    Rec::Done(Chord::new(mods, vk))
}

// ---------------------------------------------------------------- dock apps by number

/// What a digit chord asks of the app in that dock slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigitAct {
    /// Open it, bring it forward, or (in front) minimize / cycle, like a click.
    Open,
    /// Start another instance (+Shift).
    NewWindow,
    /// Cycle its windows (+Ctrl, Windows-key set only).
    Cycle,
    /// Its menu with the jump list (+Alt, Windows-key set only).
    JumpList,
}

impl DigitAct {
    pub fn code(self) -> usize {
        self as usize
    }
    pub fn from_code(c: usize) -> Option<DigitAct> {
        [DigitAct::Open, DigitAct::NewWindow, DigitAct::Cycle, DigitAct::JumpList].get(c).copied()
    }
}

/// The dock slot of a digit key: 1..9 are the first nine, 0 the tenth.
pub fn digit_slot(vk: u32) -> Option<usize> {
    match vk {
        0x31..=0x39 => Some((vk - 0x31) as usize),
        0x30 => Some(9),
        _ => None,
    }
}

/// The Windows-key set (the hook): a digit pressed with Win held and these
/// modifiers. Win+Ctrl+Shift (run as administrator) and other mixes are
/// left to Windows.
pub fn hook_digit(vk: u32, shift: bool, ctrl: bool, alt: bool) -> Option<(usize, DigitAct)> {
    let slot = digit_slot(vk)?;
    let act = match (shift, ctrl, alt) {
        (false, false, false) => DigitAct::Open,
        (true, false, false) => DigitAct::NewWindow,
        (false, true, false) => DigitAct::Cycle,
        (false, false, true) => DigitAct::JumpList,
        _ => return None,
    };
    Some((slot, act))
}

/// RegisterHotKey ids of the Alt / Ctrl+Alt sets: open is DIGIT_ID + slot,
/// new window DIGIT_ID + 16 + slot.
pub const DIGIT_ID: i32 = 0x4E30;
pub const DIGIT_IDS: std::ops::Range<i32> = DIGIT_ID..DIGIT_ID + 32;

/// The hotkeys an Alt / Ctrl+Alt set registers: (id, modifiers, vk).
pub fn digit_hotkeys(set: &str) -> Vec<(i32, u32, u32)> {
    let base = match set.trim().to_ascii_lowercase().as_str() {
        "alt" => ALT,
        "ctrl+alt" => CTRL | ALT,
        _ => return Vec::new(),
    };
    let mut v = Vec::new();
    for slot in 0..10usize {
        let vk = if slot == 9 { 0x30 } else { 0x31 + slot as u32 };
        v.push((DIGIT_ID + slot as i32, base, vk));
        v.push((DIGIT_ID + 16 + slot as i32, base | SHIFT, vk));
    }
    v
}

/// What a digit hotkey id means: (slot, act).
pub fn digit_hotkey(id: i32) -> Option<(usize, DigitAct)> {
    let n = id.checked_sub(DIGIT_ID)?;
    match n {
        0..=9 => Some((n as usize, DigitAct::Open)),
        16..=25 => Some(((n - 16) as usize, DigitAct::NewWindow)),
        _ => None,
    }
}

/// The dock item of slot `slot`: the `slot`-th of the items that count
/// (`counts[i]`: item i is an app or a pin, in dock order — pinned first,
/// then running, like the taskbar).
pub fn nth_app(counts: &[bool], slot: usize) -> Option<usize> {
    counts.iter().enumerate().filter(|(_, c)| **c).map(|(i, _)| i).nth(slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_keys_have_no_logo_glyph() {
        assert_eq!(plain_keys("win+alt+n"), ["Win", "Alt", "N"]);
        assert_eq!(plain_keys("ctrl+shift+f2"), ["Ctrl", "Shift", "F2"]);
        assert!(plain_keys("none").is_empty() && plain_keys("").is_empty());
        assert!(display_spec("win+alt+n").contains('\u{229E}'), "the field keeps its glyph");
    }

    #[test]
    fn chords_parse_and_format() {
        assert_eq!(parse("alt+space"), Ok(Some(Chord::new(ALT, 0x20))));
        assert_eq!(parse(" Ctrl + Shift + F "), Ok(Some(Chord::new(CTRL | SHIFT, b'F' as u32))));
        assert_eq!(parse("win+1"), Ok(Some(Chord::new(WIN, b'1' as u32))));
        assert_eq!(parse("Windows+Return"), Ok(Some(Chord::new(WIN, 0x0D))));
        assert_eq!(parse("f13"), Ok(Some(Chord::new(0, 0x7C))));
        assert_eq!(parse("none"), Ok(None));
        assert_eq!(parse(""), Ok(None));
        assert!(parse("space").is_err());
        assert!(parse("alt+").is_err());
        assert!(parse("alt+a+b").is_err());
        assert!(parse("alt+wat").is_err());
        assert!(parse("ctrl+f25").is_err());
        // Shown: Win as ⊞ Win, first; then Ctrl, Alt, Shift.
        assert_eq!(parse("shift+win+s").unwrap().unwrap().display(), "\u{229E} Win+Shift+S");
        assert_eq!(parse("alt+ctrl+space").unwrap().unwrap().display(), "Ctrl+Alt+Space");
        assert_eq!(parse("alt+f1").unwrap().unwrap().display(), "Alt+F1");
        assert_eq!(parse("ctrl+pgdn").unwrap().unwrap().display(), "Ctrl+PgDn");
        // Written back the same way, and read again to the same chord.
        for s in ["alt+space", "win+ctrl+alt+shift+k", "ctrl+alt+0", "win+.", "shift+f12", "ctrl+pagedown", "alt+\\"] {
            let c = parse(s).unwrap().unwrap();
            assert_eq!(parse(&c.spec()).unwrap(), Some(c), "{s}");
        }
        assert_eq!(parse("Shift+Alt+Ctrl+Win+K").unwrap().unwrap().spec(), "win+ctrl+alt+shift+k");
        assert_eq!(display_mods(CTRL | ALT), "Ctrl+Alt+\u{2026}");
        assert_eq!(display_mods(0), "");
        assert_eq!(display_spec("win+1"), "\u{229E} Win+1");
        // Fixed lone keys show their names.
        assert_eq!(display_spec("space"), "Space");
        assert_eq!(display_spec("esc"), "Esc");
    }

    #[test]
    fn registry_is_complete() {
        let mut ids = std::collections::HashSet::new();
        for s in ALL {
            assert!(ids.insert(s.id), "{} twice", s.id);
            assert!(crate::i18n::get(s.label).is_some(), "{}: no label text {}", s.id, s.label);
            assert!(s.hint.is_empty() || crate::i18n::get(s.hint).is_some(), "{}: no hint text {}", s.id, s.hint);
            assert!(valid(s, s.default), "{}: default {} is not valid", s.id, s.default);
            assert_eq!(s.apply.is_none(), s.kind == Kind::Fixed, "{}: only fixed ones have no apply", s.id);
            assert_eq!(id_of_key(row_key(s.id)), Some(s.id));
        }
        for (_, k) in AREAS {
            assert!(crate::i18n::get(k).is_some(), "{k}");
        }
        for (_, k) in DIGIT_SETS {
            assert!(crate::i18n::get(k).is_some(), "{k}");
        }
        // Every area that has shortcuts is listed.
        for s in ALL {
            assert!(AREAS.iter().any(|(a, _)| *a == s.area));
        }
    }

    #[test]
    fn defaults_never_clash() {
        let c = Config::default();
        assert!(duplicates(&c).is_empty(), "{:?}", duplicates(&c));
        // Turned on with their offered sets, still none.
        let mut c = Config::default();
        c.set_shortcut(DOCK_APPS, "alt");
        c.set_shortcut(LAUNCHPAD, "ctrl+shift+space");
        c.set_shortcut(FOCUS_DOCK, "ctrl+alt+d");
        assert!(duplicates(&c).is_empty(), "{:?}", duplicates(&c));
    }

    #[test]
    fn duplicates_are_found() {
        let mut c = Config::default();
        c.set_shortcut(SEARCH, "alt+space");
        c.set_shortcut(LAUNCHPAD, "Alt + Space");
        assert_eq!(duplicates(&c), vec![(SEARCH, LAUNCHPAD)]);
        let ch = parse("alt+space").unwrap().unwrap();
        assert_eq!(used_by(&c, FOCUS_DOCK, ch).map(|s| s.id), Some(SEARCH));
        assert_eq!(used_by(&c, SEARCH, ch).map(|s| s.id), Some(LAUNCHPAD));
        // A digit set takes its whole family.
        c.set_shortcut(LAUNCHPAD, "none");
        c.set_shortcut(DOCK_APPS, "ctrl+alt");
        let d = parse("ctrl+alt+shift+3").unwrap().unwrap();
        assert_eq!(used_by(&c, FOCUS_DOCK, d).map(|s| s.id), Some(DOCK_APPS));
        c.set_shortcut(FOCUS_DOCK, "ctrl+alt+3");
        assert_eq!(duplicates(&c), vec![(FOCUS_DOCK, DOCK_APPS)]);
        // The fixed ones count too.
        let z = parse("ctrl+z").unwrap().unwrap();
        assert_eq!(used_by(&c, SEARCH, z).map(|s| s.id), Some("dock_undo"));
        assert_eq!(digit_chords("win").len(), 40);
        assert_eq!(digit_chords("alt").len(), 20);
        assert!(digit_chords("none").is_empty());
    }

    #[test]
    fn suggestions_skip_used_and_taken() {
        let ch = parse("ctrl+space").unwrap().unwrap();
        let used = [parse("ctrl+alt+space").unwrap().unwrap()];
        let taken = |c: Chord| c == Chord::new(CTRL | SHIFT, 0x20);
        let s = suggest(ch, &used, &taken);
        assert_eq!(s, vec![Chord::new(ALT | SHIFT, 0x20), Chord::new(CTRL | ALT | SHIFT, 0x20), Chord::new(WIN | ALT, 0x20)]);
        assert!(suggest(ch, &[], &|_| true).is_empty());
    }

    #[test]
    fn clashes_name_the_shortcut_and_ignore_removed_ones() {
        let _l = crate::i18n::test_lock("en");
        let mut c = Config::default();
        c.set_shortcut(SEARCH, "alt+space");
        let space = parse("alt+space").unwrap().unwrap();
        assert_eq!(clash(&c, "notes", space).as_deref(), Some(crate::i18n::t("shortcuts.search")));
        assert_eq!(clash(&c, "search", space), None, "its own keys");
        // Keys a removed feature's entry kept in the file (the shelf's)
        // are free for any shortcut.
        c.shortcuts.insert("shelf".into(), "ctrl+alt+s".into());
        let s = parse("ctrl+alt+s").unwrap().unwrap();
        assert_eq!(clash(&c, "notes", s), None);
        assert!(duplicates(&c).is_empty());
    }

    #[test]
    fn recorder_rules() {
        assert_eq!(record(0x11, CTRL), Rec::Pending(CTRL));
        assert_eq!(record(0x5B, WIN), Rec::Pending(WIN));
        assert_eq!(record(0x1B, 0), Rec::Cancel);
        assert_eq!(record(0x1B, CTRL), Rec::Cancel);
        assert_eq!(record(0x08, 0), Rec::Clear);
        assert_eq!(record(0x2E, 0), Rec::Clear);
        assert_eq!(record(0x2E, CTRL | ALT), Rec::Done(Chord::new(CTRL | ALT, 0x2E)));
        assert_eq!(record(b'K' as u32, 0), Rec::NeedsModifier);
        assert_eq!(record(0x70, 0), Rec::Done(Chord::new(0, 0x70)));
        assert_eq!(record(b'K' as u32, CTRL | ALT), Rec::Done(Chord::new(CTRL | ALT, b'K' as u32)));
        assert_eq!(record(b'1' as u32, WIN), Rec::Done(Chord::new(WIN, b'1' as u32)));
        // Keys we cannot name (media keys): still listening.
        assert_eq!(record(0xB3, CTRL), Rec::Pending(CTRL));
    }

    #[test]
    fn digits_map_to_dock_order() {
        assert_eq!(digit_slot(b'1' as u32), Some(0));
        assert_eq!(digit_slot(b'9' as u32), Some(8));
        assert_eq!(digit_slot(b'0' as u32), Some(9));
        assert_eq!(digit_slot(b'A' as u32), None);
        // Dock: [Start] [Launchpad] [Edge] [sep] [Notepad (running)] [stack] [Trash]
        let counts = [false, true, true, false, true, false, false];
        assert_eq!(nth_app(&counts, 0), Some(1));
        assert_eq!(nth_app(&counts, 1), Some(2));
        assert_eq!(nth_app(&counts, 2), Some(4), "running apps after the pins");
        assert_eq!(nth_app(&counts, 3), None);
        // The hook's variants.
        assert_eq!(hook_digit(b'3' as u32, false, false, false), Some((2, DigitAct::Open)));
        assert_eq!(hook_digit(b'3' as u32, true, false, false), Some((2, DigitAct::NewWindow)));
        assert_eq!(hook_digit(b'0' as u32, false, true, false), Some((9, DigitAct::Cycle)));
        assert_eq!(hook_digit(b'1' as u32, false, false, true), Some((0, DigitAct::JumpList)));
        assert_eq!(hook_digit(b'1' as u32, true, true, false), None, "Win+Ctrl+Shift stays Windows'");
        assert_eq!(hook_digit(b'Q' as u32, false, false, false), None);
        // The hotkey sets: ids round-trip.
        let hk = digit_hotkeys("ctrl+alt");
        assert_eq!(hk.len(), 20);
        for (id, mods, vk) in &hk {
            let (slot, act) = digit_hotkey(*id).unwrap();
            assert_eq!(Some(slot), digit_slot(*vk));
            assert_eq!(act == DigitAct::NewWindow, mods & SHIFT != 0);
            assert!(DIGIT_IDS.contains(id));
        }
        assert!(digit_hotkeys("win").is_empty(), "the hook's, not hotkeys");
        assert_eq!(digit_hotkey(DIGIT_ID + 12), None);
        for a in [DigitAct::Open, DigitAct::NewWindow, DigitAct::Cycle, DigitAct::JumpList] {
            assert_eq!(DigitAct::from_code(a.code()), Some(a));
        }
    }
}
