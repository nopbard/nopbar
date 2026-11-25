//! Where results come from: the apps in shell:AppsFolder, a table of
//! Settings pages, the calculator, recent files (the index's fallback) and
//! the web. All of it runs on the search workers, never the UI thread.

use windows::core::{Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, IShellLinkW, SHCreateItemFromParsingName, ShellLink, SIGDN,
    SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};

use super::calc;
use super::dates;
use super::matching::Query;
use crate::i18n::{t, tf};

/// Result groups, in the order they are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Calc,
    App,
    Setting,
    File,
    /// How the file search went when that needs saying (only recent
    /// files, too slow, still going): with the files, before the web row.
    Note,
    Web,
}

impl Kind {
    /// Group heading (None: the row speaks for itself).
    pub fn heading(self) -> Option<&'static str> {
        match self {
            Kind::Calc => Some(t("search.group.calculator")),
            Kind::App => Some(t("search.group.apps")),
            Kind::Setting => Some(t("search.group.settings")),
            Kind::File => Some(t("search.group.files")),
            Kind::Note | Kind::Web => None,
        }
    }

    /// How many rows the group may show.
    pub fn limit(self) -> usize {
        match self {
            Kind::Calc | Kind::Note | Kind::Web => 1,
            Kind::App => 5,
            Kind::Setting => 3,
            Kind::File => 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub kind: Kind,
    pub title: String,
    /// Second line (a file's folder).
    pub detail: String,
    /// What Enter opens (for the calculator: what it copies).
    pub target: String,
    /// Shell item whose icon is shown (apps, files); None = a glyph.
    pub icon: Option<String>,
    pub score: u32,
}

// ---- apps ------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub name: String,
    /// Its parsing name inside shell:AppsFolder (an AUMID, or a known
    /// folder path for Win32 shortcuts).
    pub id: String,
}

impl App {
    pub fn target(&self) -> String {
        format!("shell:AppsFolder\\{}", self.id)
    }
}

unsafe fn name_of(item: &IShellItem, how: SIGDN) -> Option<String> {
    let p: PWSTR = item.GetDisplayName(how).ok()?;
    let s = p.to_string().ok();
    CoTaskMemFree(Some(p.0 as *const _));
    s.filter(|s| !s.is_empty())
}

/// How reading the app list ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The whole list was read.
    Done,
    /// The shell failed part way (its error).
    Error(String),
    /// `stop` said so (out of time): what was read so far.
    Stopped,
}

/// Everything in shell:AppsFolder (Start's "All apps": Win32 shortcuts and
/// packaged apps), uninstallers left out. Stops early (with what it has,
/// and why) when `stop` says so or the shell fails part way. Needs COM on
/// the calling thread.
pub fn list_apps(stop: &dyn Fn() -> bool) -> Result<(Vec<App>, Ended), String> {
    unsafe {
        let folder: IShellItem = SHCreateItemFromParsingName(&HSTRING::from("shell:AppsFolder"), None).map_err(|e| format!("AppsFolder: {e}"))?;
        let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems).map_err(|e| format!("enumerate: {e}"))?;
        let mut out: Vec<App> = Vec::new();
        let ended = loop {
            if stop() {
                break Ended::Stopped;
            }
            let mut got = [None];
            let mut n = 0u32;
            if let Err(e) = items.Next(&mut got, Some(&mut n)) {
                break Ended::Error(e.to_string());
            }
            if n == 0 {
                break Ended::Done;
            }
            let Some(item) = got[0].take() else { break Ended::Done };
            let (Some(name), Some(id)) = (name_of(&item, SIGDN_NORMALDISPLAY), name_of(&item, SIGDN_PARENTRELATIVEPARSING)) else { continue };
            let lower = name.to_lowercase();
            if lower.starts_with("uninstall") || lower.contains(" uninstall") || name.contains("제거") {
                continue;
            }
            if !out.iter().any(|a| a.name == name && a.id == id) {
                out.push(App { name, id });
            }
        };
        Ok((out, ended))
    }
}

// ---- Settings pages ------------------------------------------------------------------

/// (name key, ms-settings: page, English words that also find it).
const SETTINGS: &[(&str, &str, &str)] = &[
    ("search.setting.wifi", "ms-settings:network-wifi", "wifi wi-fi wireless wlan"),
    ("search.setting.bluetooth", "ms-settings:bluetooth", "bluetooth devices"),
    ("search.setting.display", "ms-settings:display", "display screen monitor resolution brightness"),
    ("search.setting.sound", "ms-settings:sound", "sound audio volume speaker microphone"),
    ("search.setting.notifications", "ms-settings:notifications", "notifications"),
    ("search.setting.apps", "ms-settings:appsfeatures", "apps installed programs uninstall"),
    ("search.setting.default_apps", "ms-settings:defaultapps", "default apps browser"),
    ("search.setting.windows_update", "ms-settings:windowsupdate", "windows update"),
    ("search.setting.personalization", "ms-settings:personalization", "personalization wallpaper background colors theme"),
    ("search.setting.taskbar", "ms-settings:taskbar", "taskbar"),
    ("search.setting.mouse", "ms-settings:mousetouchpad", "mouse touchpad pointer"),
    ("search.setting.keyboard", "ms-settings:typing", "keyboard typing"),
    ("search.setting.language", "ms-settings:regionlanguage", "language region input"),
    ("search.setting.date_time", "ms-settings:dateandtime", "date time clock timezone"),
    ("search.setting.power", "ms-settings:powersleep", "power battery sleep"),
    ("search.setting.storage", "ms-settings:storagesense", "storage disk space"),
    ("search.setting.privacy", "ms-settings:privacy", "privacy security permissions"),
    ("search.setting.accounts", "ms-settings:yourinfo", "accounts user"),
    ("search.setting.network", "ms-settings:network-status", "network internet ethernet"),
    ("search.setting.vpn", "ms-settings:network-vpn", "vpn"),
];

/// The i18n keys of the Settings table (for tests).
#[cfg(test)]
pub fn setting_keys() -> Vec<&'static str> {
    SETTINGS.iter().map(|s| s.0).collect()
}

fn settings(q: &Query) -> Vec<Hit> {
    let mut out = Vec::new();
    for &(key, uri, words) in SETTINGS {
        let name = t(key);
        // The name in the UI language first; English words a step lower.
        let score = q.score(name).or_else(|| words.split(' ').filter_map(|w| q.score(w)).max().map(|s| s.saturating_sub(1_000_000)));
        if let Some(score) = score {
            out.push(Hit { kind: Kind::Setting, title: name.to_string(), detail: String::new(), target: uri.to_string(), icon: None, score });
        }
    }
    out
}

// ---- the fast sources together --------------------------------------------------------

fn best(mut v: Vec<Hit>, n: usize) -> Vec<Hit> {
    v.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
    v.truncate(n);
    v
}

/// Calculator, apps, Settings and the web row for `text`.
pub fn quick(text: &str, apps: &[App]) -> Vec<Hit> {
    let q = Query::new(text);
    if q.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(v) = calc::answer(text, dates::today()) {
        let (shown, copy) = calc::shown(v);
        out.push(Hit { kind: Kind::Calc, title: format!("= {shown}"), detail: text.trim().to_string(), target: copy, icon: None, score: 0 });
    }
    let found: Vec<Hit> = apps
        .iter()
        .filter_map(|a| {
            q.score(&a.name).map(|score| Hit { kind: Kind::App, title: a.name.clone(), detail: String::new(), target: a.target(), icon: Some(a.target()), score })
        })
        .collect();
    out.extend(best(found, Kind::App.limit()));
    out.extend(best(settings(&q), Kind::Setting.limit()));
    out.push(web(text));
    out
}

// ---- web --------------------------------------------------------------------------------

/// Query-string encoding (UTF-8, spaces as "+").
pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn web(text: &str) -> Hit {
    let text = text.trim();
    Hit {
        kind: Kind::Web,
        title: tf("search.web", &[("q", text)]),
        detail: String::new(),
        target: format!("https://www.google.com/search?q={}", url_encode(text)),
        icon: None,
        score: 0,
    }
}

// ---- files: recent items (when the index cannot be used) ------------------------------

/// Where a shortcut points (empty if it does not point at a file).
unsafe fn link_target(path: &str) -> Option<String> {
    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
    link.cast::<IPersistFile>().ok()?.Load(PCWSTR(HSTRING::from(path).as_ptr()), STGM_READ).ok()?;
    let mut buf = [0u16; 1024];
    link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..end]);
    (!s.is_empty()).then_some(s)
}

/// Recent files (%APPDATA%\Microsoft\Windows\Recent) whose names match,
/// newest first among equals. Needs COM on the calling thread.
pub fn recent(text: &str, limit: usize, stop: &dyn Fn() -> bool) -> Vec<Hit> {
    let q = Query::new(text);
    recent_scored(&|stem| q.score(stem), limit, stop)
}

/// The newest recent files and folders, all of them (the top bar's system
/// menu). Needs COM on the calling thread.
pub fn recent_newest(limit: usize, stop: &dyn Fn() -> bool) -> Vec<Hit> {
    recent_scored(&|_| Some(0), limit, stop)
}

/// Recent items `score` keeps (by shortcut name), best first, then newest;
/// those whose target is gone are left out.
fn recent_scored(score: &dyn Fn(&str) -> Option<u32>, limit: usize, stop: &dyn Fn() -> bool) -> Vec<Hit> {
    let Some(dir) = std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("Microsoft\\Windows\\Recent")) else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut found: Vec<(u32, std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
            continue;
        }
        let Some(stem) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        if let Some(score) = score(&stem) {
            let when = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            found.push((score, when, p));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut out = Vec::new();
    for (score, _, p) in found {
        if out.len() >= limit || stop() {
            break;
        }
        let Some(target) = (unsafe { link_target(&p.to_string_lossy()) }) else { continue };
        let tp = std::path::Path::new(&target);
        if !tp.exists() {
            continue;
        }
        out.push(file_hit(&target, tp.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), score));
    }
    out
}

/// A file row: its name, and its folder below it.
pub fn file_hit(path: &str, name: String, score: u32) -> Hit {
    let folder = std::path::Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let title = if name.is_empty() { path.to_string() } else { name };
    Hit { kind: Kind::File, title, detail: folder, target: path.to_string(), icon: Some(path.to_string()), score }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_queries_are_encoded() {
        assert_eq!(url_encode("a b&c"), "a+b%26c");
        assert_eq!(url_encode("카톡"), "%EC%B9%B4%ED%86%A1");
        assert_eq!(web("  rust lang ").target, "https://www.google.com/search?q=rust+lang");
    }

    #[test]
    fn quick_results_are_grouped_and_ranked() {
        let _l = crate::i18n::test_lock("ko");
        let apps = vec![
            App { name: "카카오톡".into(), id: "kakao".into() },
            App { name: "Notepad".into(), id: "notepad".into() },
            App { name: "Kakao Map".into(), id: "map".into() },
        ];
        let r = quick("ㅋㅋㅇ", &apps);
        assert_eq!(r[0].kind, Kind::App);
        assert_eq!(r[0].title, "카카오톡");
        assert_eq!(r[0].target, "shell:AppsFolder\\kakao");
        assert_eq!(r.last().map(|h| h.kind), Some(Kind::Web));
        let r = quick("2*21", &apps);
        assert_eq!((r[0].kind, r[0].target.as_str()), (Kind::Calc, "42"));
        // English words find Settings pages in any language.
        let r = quick("wifi", &apps);
        assert!(r.iter().any(|h| h.kind == Kind::Setting && h.target == "ms-settings:network-wifi"), "{r:?}");
        assert!(quick("   ", &apps).is_empty());
        // Groups come in display order.
        let kinds: Vec<Kind> = quick("no", &apps).iter().map(|h| h.kind).collect();
        let mut sorted = kinds.clone();
        sorted.sort();
        assert_eq!(kinds, sorted);
    }

    #[test]
    fn file_rows_show_the_folder() {
        let h = file_hit(r"C:\Docs\plan.txt", "plan.txt".into(), 1);
        assert_eq!((h.title.as_str(), h.detail.as_str(), h.kind), ("plan.txt", r"C:\Docs", Kind::File));
    }
}
