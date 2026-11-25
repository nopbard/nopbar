//! What the top bar's menus contain, and what picking an item does.
//!
//! Hybrid model (spec §7.1, spike S6):
//! - classic Win32 apps with a real menu bar (HMENU): their own menus, read
//!   on a worker thread, commands sent back with WM_COMMAND;
//! - popular apps: built-in presets (dock/menus/presets.json) that press the
//!   app's shortcuts;
//! - everything else: common File/Edit/View menus (shortcuts);
//! - always our own app menu (bold name) and a Window menu that works on
//!   windows directly.

use windows::core::PWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::i18n::{t, tf};
use crate::popup::Item;

const PRESETS: &str = include_str!("../../menus/presets.json");

/// The app in front, as the top bar knows it.
#[derive(Clone, Debug, Default)]
pub struct ActiveApp {
    pub name: String,
    /// Its top-level window in front.
    pub hwnd: isize,
    /// Lowercase full exe path.
    pub exe: Option<String>,
    /// Path that launches it (exe, .lnk or shell:AppsFolder\...).
    pub path: String,
    /// All its windows (from the dock's model).
    pub windows: Vec<isize>,
    /// The desktop is in front (no app menus).
    pub desktop: bool,
    /// Runs with higher rights than us: Windows (UIPI) drops our input and
    /// messages to it, so its menus are shown disabled.
    pub locked: bool,
}

impl ActiveApp {
    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Key chords pressed one after another (e.g. Ctrl+K then Ctrl+O).
    Keys(Vec<Vec<u16>>),
    /// WM_COMMAND id for the app's own menu.
    Command(u32),
    Minimize,
    Zoom,
    CloseWindow,
    NewWindow,
    OpenLocation,
    HideApp,
    QuitApp,
    Focus(isize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    None,
    /// The app's own menu bar: each title's position in its HMENU.
    Native(Vec<u32>),
    /// Index into the presets.
    Preset(usize),
    Common,
}

/// A built menu: rows plus the action for each picked id (id = index + 1).
#[derive(Default)]
pub struct Built {
    pub items: Vec<Item>,
    pub actions: Vec<Action>,
}

impl Built {
    fn add(&mut self, label: &str, shortcut: &str, action: Action) {
        self.actions.push(action);
        self.items.push(Item::new(self.actions.len() as u32, label).shortcut(shortcut));
    }
    fn sep(&mut self) {
        self.items.push(Item::sep());
    }
    pub fn action(&self, id: u32) -> Option<&Action> {
        self.actions.get((id as usize).checked_sub(1)?)
    }
}

// ---- shortcuts --------------------------------------------------------------

/// "Ctrl+Shift+T", "Alt+F4", "Ctrl+K Ctrl+O" -> chords of virtual keys.
pub fn parse_keys(s: &str) -> Option<Vec<Vec<u16>>> {
    s.split_whitespace().map(parse_chord).collect()
}

fn parse_chord(s: &str) -> Option<Vec<u16>> {
    // "Ctrl+-" and "Ctrl+=" : the last part may itself be '+'-free symbols.
    let mut parts: Vec<&str> = s.split('+').collect();
    if s.ends_with("++") {
        parts.pop();
        parts.pop();
        parts.push("+");
    }
    parts.iter().map(|p| key_code(p)).collect()
}

fn key_code(k: &str) -> Option<u16> {
    let up = k.to_ascii_uppercase();
    let vk = match up.as_str() {
        "CTRL" => 0x11,
        "SHIFT" => 0x10,
        "ALT" => 0x12,
        "WIN" => 0x5B,
        "TAB" => 0x09,
        "ENTER" => 0x0D,
        "ESC" => 0x1B,
        "SPACE" => 0x20,
        "DELETE" | "DEL" => 0x2E,
        "BACKSPACE" => 0x08,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "=" | "+" => 0xBB,
        "-" => 0xBD,
        "," => 0xBC,
        "." => 0xBE,
        "/" => 0xBF,
        "`" => 0xC0,
        ";" => 0xBA,
        "[" => 0xDB,
        "]" => 0xDD,
        "\\" => 0xDC,
        "'" => 0xDE,
        _ => {
            if let Some(n) = up.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
                if (1..=24).contains(&n) {
                    return Some(0x70 + n - 1);
                }
                return None;
            }
            let mut c = up.chars();
            match (c.next(), c.next()) {
                (Some(ch), None) if ch.is_ascii_alphanumeric() => ch as u16,
                _ => return None,
            }
        }
    };
    Some(vk)
}

// ---- presets -------------------------------------------------------------------

struct PresetMenu {
    title: String,
    /// (label, shortcut) or None for a separator.
    items: Vec<Option<(String, String)>>,
}

struct Preset {
    exes: Vec<String>,
    menus: Vec<PresetMenu>,
}

fn presets() -> &'static Vec<Preset> {
    static P: std::sync::OnceLock<Vec<Preset>> = std::sync::OnceLock::new();
    P.get_or_init(|| parse_presets(PRESETS))
}

fn parse_presets(text: &str) -> Vec<Preset> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let Some(apps) = v.get("apps").and_then(|a| a.as_array()) else { return Vec::new() };
    apps.iter()
        .filter_map(|a| {
            let exes = a.get("exe")?.as_array()?.iter().filter_map(|e| e.as_str().map(str::to_lowercase)).collect();
            let menus = a
                .get("menus")?
                .as_array()?
                .iter()
                .filter_map(|m| {
                    let title = m.get("title")?.as_str()?.to_string();
                    let items = m
                        .get("items")?
                        .as_array()?
                        .iter()
                        .map(|it| {
                            let pair = it.as_array()?;
                            Some((pair.first()?.as_str()?.to_string(), pair.get(1)?.as_str()?.to_string()))
                        })
                        .collect();
                    Some(PresetMenu { title, items })
                })
                .collect();
            Some(Preset { exes, menus })
        })
        .collect()
}

fn preset_for(exe: Option<&str>) -> Option<usize> {
    let file = std::path::Path::new(exe?).file_name()?.to_string_lossy().to_lowercase();
    presets().iter().position(|p| p.exes.iter().any(|e| *e == file))
}

/// Apps without a real menu or preset get only what means the same in
/// (almost) every app, plus window management (polish plan section 9).
/// Labels here and in presets.json are English; `localized` shows them in
/// the user's language.
const COMMON: [&str; 1] = ["Edit"];
const WINDOW_TITLE: &str = "Window";

/// A preset / common menu label in the user's language ("preset.<English>"
/// in the catalogs), or the English label itself.
fn localized(en: &str) -> String {
    crate::i18n::get(&format!("preset.{en}")).map(String::from).unwrap_or_else(|| en.to_string())
}

// ---- native menus ---------------------------------------------------------------

/// "&File\tCtrl+F" -> ("File", "Ctrl+F"); "&&" stays a literal '&'.
pub fn split_label(raw: &str) -> (String, String) {
    let (label, shortcut) = match raw.split_once('\t') {
        Some((l, s)) => (l, s.trim()),
        None => (raw, ""),
    };
    // "파일(&F)": the bracketed mnemonic goes away completely.
    let mut label = label.to_string();
    if let Some(start) = label.find("(&") {
        if let Some(len) = label[start..].find(')') {
            if len <= 4 {
                label.replace_range(start..start + len + 1, "");
            }
        }
    }
    let mut out = String::new();
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if chars.peek() == Some(&'&') {
                out.push('&');
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    (out.trim().to_string(), shortcut.to_string())
}

/// Reads one item of `menu` (works across processes; S6).
unsafe fn read_item(menu: HMENU, i: u32) -> Option<(MENUITEMINFOW, String)> {
    let mut mii = MENUITEMINFOW {
        cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_STRING,
        ..Default::default()
    };
    GetMenuItemInfoW(menu, i, true, &mut mii).ok()?;
    let mut text = String::new();
    if mii.fType & MFT_SEPARATOR == MENU_ITEM_TYPE(0) && mii.cch > 0 {
        let mut buf = vec![0u16; mii.cch as usize + 1];
        mii.dwTypeData = PWSTR(buf.as_mut_ptr());
        mii.cch += 1;
        GetMenuItemInfoW(menu, i, true, &mut mii).ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        text = String::from_utf16_lossy(&buf[..n]);
    }
    Some((mii, text))
}

/// Titles of a window's own menu bar with their HMENU positions (empty if
/// it has none). Items without text keep their positions out of the list.
pub fn native_titles(hwnd: HWND) -> Vec<(u32, String)> {
    unsafe {
        let menu = GetMenu(hwnd);
        if menu.is_invalid() {
            return Vec::new();
        }
        let n = GetMenuItemCount(Some(menu)).max(0) as u32;
        (0..n)
            .filter_map(|i| read_item(menu, i).map(|(_, t)| (i, split_label(&t).0)))
            .filter(|(_, t)| !t.is_empty())
            .collect()
    }
}

/// Builds one level of a native menu; `actions` collects WM_COMMAND ids.
unsafe fn read_native(target: HWND, menu: HMENU, depth: u32, actions: &mut Vec<Action>) -> Vec<Item> {
    let n = GetMenuItemCount(Some(menu)).max(0) as u32;
    let mut out = Vec::new();
    for i in 0..n.min(100) {
        let Some((mii, text)) = read_item(menu, i) else { continue };
        if mii.fType & MFT_SEPARATOR != MENU_ITEM_TYPE(0) {
            out.push(Item::sep());
            continue;
        }
        let (label, shortcut) = split_label(&text);
        let label = if label.is_empty() { "…".to_string() } else { label };
        let state = mii.fState;
        let enabled = state & (MFS_DISABLED | MFS_GRAYED) == MENU_ITEM_STATE(0);
        let checked = state & MFS_CHECKED != MENU_ITEM_STATE(0);
        let sub = GetSubMenu(menu, i as i32);
        if !sub.is_invalid() && depth < 3 {
            // Let the app fill / update it, as if it were opening.
            let _ = SendMessageTimeoutW(target, WM_INITMENUPOPUP, WPARAM(sub.0 as usize), LPARAM(i as isize), SMTO_ABORTIFHUNG, 200, None);
            let children = read_native(target, sub, depth + 1, actions);
            out.push(Item::new(0, label).enabled(enabled).submenu(children));
        } else {
            actions.push(Action::Command(mii.wID));
            out.push(Item::new(actions.len() as u32, label).shortcut(shortcut).enabled(enabled).checked(checked));
        }
    }
    // Drop leading/trailing/double separators.
    let mut clean: Vec<Item> = Vec::new();
    for it in out {
        if it.separator && clean.last().is_none_or(|l| l.separator) {
            continue;
        }
        clean.push(it);
    }
    while clean.last().is_some_and(|l| l.separator) {
        clean.pop();
    }
    clean
}

/// Finished native reads wait here; the window message only says "look".
/// (A message carrying a pointer would leak if the bar were destroyed
/// before handling it.)
static READY: std::sync::Mutex<Option<(u64, Built)>> = std::sync::Mutex::new(None);

/// Takes the finished read for `token`, if that is what is waiting.
pub fn take_ready(token: u64) -> Option<Built> {
    let mut slot = READY.lock().ok()?;
    match slot.take() {
        Some((t, b)) if t == token => Some(b),
        _ => None,
    }
}

pub fn clear_ready() {
    if let Ok(mut slot) = READY.lock() {
        *slot = None;
    }
}

/// Reads native menu at `pos` on a worker thread. The menu handle is looked
/// up fresh there (apps replace menus), the app is asked to update it first
/// (with timeouts), and the result is dropped if anything fails. Then
/// `msg` is posted to `reply` with the token in wParam.
/// Returns false if the thread could not start.
pub fn read_native_async(target: HWND, pos: u32, reply: HWND, msg: u32, token: u64) -> bool {
    let (t, r) = (target.0 as isize, reply.0 as isize);
    std::thread::Builder::new()
        .name("menu-read".into())
        .spawn(move || unsafe {
            let target = HWND(t as *mut _);
            let built = read_native_now(target, pos).unwrap_or_default();
            if let Ok(mut slot) = READY.lock() {
                *slot = Some((token, built));
            }
            let _ = PostMessageW(Some(HWND(r as *mut _)), msg, WPARAM(token as usize), LPARAM(0));
        })
        .is_ok()
}

unsafe fn read_native_now(target: HWND, pos: u32) -> Option<Built> {
    if !IsWindow(Some(target)).as_bool() {
        return None;
    }
    let menu = GetMenu(target);
    if menu.is_invalid() {
        return None;
    }
    let ok = |r: LRESULT| r.0 != 0;
    let mut res = 0usize;
    if !ok(SendMessageTimeoutW(target, WM_INITMENU, WPARAM(menu.0 as usize), LPARAM(0), SMTO_ABORTIFHUNG, 300, Some(&mut res))) {
        return None;
    }
    // WM_INITMENU may have swapped the menu.
    let menu = GetMenu(target);
    let sub = GetSubMenu(menu, pos as i32);
    if sub.is_invalid() {
        return None;
    }
    if !ok(SendMessageTimeoutW(target, WM_INITMENUPOPUP, WPARAM(sub.0 as usize), LPARAM(pos as isize), SMTO_ABORTIFHUNG, 500, Some(&mut res))) {
        return None;
    }
    // Still the same submenu at the same place?
    if GetMenu(target) != menu || GetSubMenu(menu, pos as i32) != sub {
        return None;
    }
    let mut built = Built::default();
    built.items = read_native(target, sub, 0, &mut built.actions);
    Some(built)
}

// ---- the menu set ---------------------------------------------------------------

/// Which menus the bar shows for `app`, and their titles.
pub fn menus_for(app: &ActiveApp) -> (Source, Vec<String>) {
    if app.desktop || app.hwnd == 0 {
        return (Source::None, Vec::new());
    }
    let native = native_titles(app.hwnd());
    if !native.is_empty() {
        let (pos, titles) = native.into_iter().unzip();
        return (Source::Native(pos), titles);
    }
    if let Some(p) = preset_for(app.exe.as_deref()) {
        let mut t: Vec<String> = presets()[p].menus.iter().map(|m| localized(&m.title)).collect();
        t.push(localized(WINDOW_TITLE));
        return (Source::Preset(p), t);
    }
    let mut t: Vec<String> = COMMON.iter().map(|s| localized(s)).collect();
    t.push(localized(WINDOW_TITLE));
    (Source::Common, t)
}

fn add_keys(b: &mut Built, label: &str, keys: &str) {
    match parse_keys(keys) {
        Some(k) => b.add(label, keys, Action::Keys(k)),
        None => {
            b.items.push(Item::new(0, label).shortcut(keys).enabled(false));
        }
    }
}

/// A common menu by its English title.
fn common_menu(title: &str) -> Built {
    let mut b = Built::default();
    let rows: &[(&str, &str)] = match title {
        "Edit" => &[("Undo", "Ctrl+Z"), ("Redo", "Ctrl+Y"), ("-", ""), ("Cut", "Ctrl+X"), ("Copy", "Ctrl+C"), ("Paste", "Ctrl+V"), ("Select all", "Ctrl+A"), ("-", ""), ("Find", "Ctrl+F")],
        _ => &[],
    };
    for (l, k) in rows {
        if *l == "-" {
            b.sep();
        } else {
            add_keys(&mut b, &localized(l), k);
        }
    }
    b
}

/// Window menu: acts on windows directly, plus this app's window list.
pub fn window_menu(app: &ActiveApp, titles: &[(isize, String)]) -> Built {
    let mut b = Built::default();
    b.add(&localized("Minimize"), "", Action::Minimize);
    b.add(&localized("Zoom"), "", Action::Zoom);
    b.sep();
    add_keys(&mut b, &localized("Move to left half"), "Win+Left");
    add_keys(&mut b, &localized("Move to right half"), "Win+Right");
    if titles.len() > 1 {
        b.sep();
        for (h, t) in titles {
            let label: String = t.chars().take(60).collect();
            b.actions.push(Action::Focus(*h));
            let id = b.actions.len() as u32;
            b.items.push(Item::new(id, label).checked(*h == app.hwnd).literal());
        }
    }
    b.sep();
    b.add(&localized("Close window"), "", Action::CloseWindow);
    b
}

/// The bold app-name menu.
pub fn app_menu(app: &ActiveApp) -> Built {
    let mut b = Built::default();
    if app.desktop {
        add_keys(&mut b, t("topbar.menu.app.new_explorer"), "Win+E");
        return b;
    }
    if !app.path.is_empty() {
        b.add(t("topbar.menu.app.new_window"), "", Action::NewWindow);
    }
    if app.exe.is_some() {
        b.add(t("topbar.menu.app.open_location"), "", Action::OpenLocation);
    }
    b.sep();
    b.add(t("topbar.menu.app.minimize_all"), "", Action::HideApp);
    b.sep();
    let n = app.windows.len().max(1);
    let close = if n > 1 { tf("topbar.menu.app.close_n_windows", &[("n", &n.to_string())]) } else { t("topbar.menu.app.close_window").to_string() };
    b.add(&close, "", Action::QuitApp);
    if app.locked {
        // Launching still works; talking to its windows does not.
        for it in b.items.iter_mut() {
            if matches!(b.actions.get((it.id as usize).wrapping_sub(1)), Some(Action::HideApp | Action::QuitApp)) {
                it.enabled = false;
            }
        }
    }
    b
}

/// Menus of an app we cannot send input to: shown, but disabled, under a
/// short line that says why ("Running as administrator").
pub fn lock(mut b: Built) -> Built {
    fn off(items: &mut [Item]) {
        for it in items {
            it.enabled = false;
            off(&mut it.sub);
        }
    }
    off(&mut b.items);
    let mut items = vec![Item::new(0, t("topbar.menu.locked")).enabled(false), Item::sep()];
    items.append(&mut b.items);
    b.items = items;
    b
}

/// Builds title `i` for menus that need no worker (presets, common, window).
/// None for native menus (use read_native_async).
pub fn build(source: &Source, titles: &[String], i: usize, app: &ActiveApp, window_titles: &[(isize, String)]) -> Option<Built> {
    titles.get(i)?;
    // Presets and common menus end with the Window menu.
    if i + 1 == titles.len() && matches!(source, Source::Preset(_) | Source::Common) {
        return Some(window_menu(app, window_titles));
    }
    match source {
        Source::Native(_) | Source::None => None,
        Source::Common => Some(common_menu(COMMON.get(i)?)),
        Source::Preset(p) => {
            let m = presets()[*p].menus.get(i)?;
            let mut b = Built::default();
            for it in &m.items {
                match it {
                    Some((l, k)) => add_keys(&mut b, &localized(l), k),
                    None => b.sep(),
                }
            }
            Some(b)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse() {
        assert_eq!(parse_keys("Ctrl+Shift+T"), Some(vec![vec![0x11, 0x10, b'T' as u16]]));
        assert_eq!(parse_keys("Alt+F4"), Some(vec![vec![0x12, 0x73]]));
        assert_eq!(parse_keys("Ctrl+K Ctrl+O"), Some(vec![vec![0x11, b'K' as u16], vec![0x11, b'O' as u16]]));
        assert_eq!(parse_keys("Ctrl+="), Some(vec![vec![0x11, 0xBB]]));
        assert_eq!(parse_keys("Ctrl+-"), Some(vec![vec![0x11, 0xBD]]));
        assert_eq!(parse_keys("Ctrl+`"), Some(vec![vec![0x11, 0xC0]]));
        assert_eq!(parse_keys("Win+Left"), Some(vec![vec![0x5B, 0x25]]));
        assert_eq!(parse_keys("F11"), Some(vec![vec![0x7A]]));
        assert_eq!(parse_keys("Ctrl+Nope"), None);
    }

    #[test]
    fn every_preset_shortcut_parses() {
        let p = parse_presets(PRESETS);
        assert!(p.len() >= 8, "presets loaded: {}", p.len());
        for app in &p {
            for m in &app.menus {
                assert!(!m.items.is_empty(), "{} is empty", m.title);
                for (l, k) in m.items.iter().flatten() {
                    assert!(parse_keys(k).is_some(), "{l}: {k}");
                }
            }
        }
        assert!(preset_for(Some("c:\\program files\\google\\chrome\\application\\chrome.exe")).is_some());
        assert!(preset_for(Some("c:\\x\\unknown.exe")).is_none());
    }

    #[test]
    fn labels_lose_mnemonics() {
        assert_eq!(split_label("&File"), ("File".into(), "".into()));
        assert_eq!(split_label("Save &As...\tCtrl+Shift+S"), ("Save As...".into(), "Ctrl+Shift+S".into()));
        assert_eq!(split_label("R&&D"), ("R&D".into(), "".into()));
        assert_eq!(split_label("파일(&F)"), ("파일".into(), "".into()));
    }

    #[test]
    fn ids_map_back_to_actions() {
        let _l = crate::i18n::test_lock("ko");
        let b = common_menu("Edit");
        let copy = b.items.iter().find(|i| i.label == "복사").unwrap();
        assert_eq!(b.action(copy.id), Some(&Action::Keys(vec![vec![0x11, b'C' as u16]])));
        assert!(b.action(0).is_none());
        let app = ActiveApp { name: "X".into(), hwnd: 5, path: "x.exe".into(), exe: Some("c:\\x.exe".into()), ..Default::default() };
        let locked = lock(common_menu("Edit"));
        assert!(locked.items.iter().all(|i| !i.enabled));
        assert_eq!(locked.items.len(), common_menu("Edit").items.len() + 2);
        let m = window_menu(&app, &[(5, "one".into()), (6, "two".into())]);
        assert!(m.items.iter().any(|i| i.label == "one" && i.checked));
        let am = app_menu(&app);
        assert_eq!(am.items.last().unwrap().label, "창 닫기");
        // Titles show in the user's language; unknown labels stay English.
        assert_eq!(menus_for_common_titles(), ["편집", "창"]);
        assert_eq!(localized("Not in any catalog"), "Not in any catalog");
        let built = build(&Source::Common, &menus_for_common_titles(), 1, &app, &[]).unwrap();
        assert_eq!(built.items.last().unwrap().label, "창 닫기", "last title is the Window menu");
        // A made-up menu is its rows only: no "Quick commands" note and
        // separator on top (owner, 2026-11-08).
        let edit = build(&Source::Common, &menus_for_common_titles(), 0, &app, &[]).unwrap();
        assert_eq!(edit.items, common_menu("Edit").items);
        assert!(!edit.items[0].wrap && edit.items[0].enabled && !edit.items[0].separator);
        assert_eq!(edit.action(edit.items.iter().find(|i| i.label == "복사").unwrap().id), Some(&Action::Keys(vec![vec![0x11, b'C' as u16]])), "ids still map");
        // A preset's menu too.
        let p = preset_for(Some("c:\\program files\\google\\chrome\\application\\chrome.exe")).unwrap();
        let titles: Vec<String> = presets()[p].menus.iter().map(|m| localized(&m.title)).chain([localized(WINDOW_TITLE)]).collect();
        let first = build(&Source::Preset(p), &titles, 0, &app, &[]).unwrap();
        assert!(first.items.iter().all(|i| !i.wrap), "no note row");
        assert!(!first.items[0].separator && first.items[0].id != 0, "a command first");
    }

    #[test]
    fn the_desktop_menu_is_just_a_new_explorer_window() {
        let _l = crate::i18n::test_lock("ko");
        let desk = ActiveApp { desktop: true, ..Default::default() };
        let m = app_menu(&desk);
        // No "Settings (Win+I)" row: the system menu has it.
        assert_eq!(m.items.iter().map(|i| (i.label.as_str(), i.shortcut.as_str())).collect::<Vec<_>>(), [("새 탐색기 창", "Win+E")]);
    }

    #[test]
    fn an_admin_app_s_menu_says_so_in_a_few_words() {
        let _l = crate::i18n::test_lock("ko");
        let locked = lock(common_menu("Edit"));
        assert_eq!(locked.items[0].label, "관리자 권한 앱");
        assert!(!locked.items[0].enabled && locked.items[1].separator);
    }

    fn menus_for_common_titles() -> Vec<String> {
        COMMON.iter().map(|s| localized(s)).chain([localized(WINDOW_TITLE)]).collect()
    }
}
