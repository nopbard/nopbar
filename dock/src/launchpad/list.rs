//! The apps Launchpad shows: search's shell:AppsFolder list, sorted by
//! name the way the UI language sorts (CompareStringEx), filtered with
//! search's matcher, and what can be done with each one.

use std::cmp::Ordering;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Globalization::{CompareStringEx, ResolveLocaleName, CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, LINGUISTIC_IGNORECASE, SORT_DIGITSASNUMBERS};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{IShellItem2, SHCreateItemFromParsingName};

use crate::search::matching::Query;
use crate::search::sources::App;

/// The locale names sort in: the UI language's ("ko" -> "ko-KR").
pub fn locale() -> HSTRING {
    let ui = crate::i18n::current();
    let mut out = [0u16; 85];
    let n = unsafe { ResolveLocaleName(&HSTRING::from(ui), Some(&mut out)) };
    if n > 1 { HSTRING::from_wide(&out[..n as usize - 1]) } else { HSTRING::from(ui) }
}

/// Compares two names in `locale`: case-insensitive, linguistic, numbers
/// by value ("App 2" before "App 10"); an ordinal tie-break keeps the
/// order total.
pub fn compare(locale: &HSTRING, a: &str, b: &str) -> Ordering {
    let wa: Vec<u16> = a.encode_utf16().collect();
    let wb: Vec<u16> = b.encode_utf16().collect();
    let r = unsafe { CompareStringEx(PCWSTR(locale.as_ptr()), LINGUISTIC_IGNORECASE | SORT_DIGITSASNUMBERS, &wa, &wb, None, None, None) };
    match r {
        CSTR_LESS_THAN => Ordering::Less,
        CSTR_GREATER_THAN => Ordering::Greater,
        CSTR_EQUAL => a.cmp(b),
        // Failed (an unknown locale): plain lowercase order.
        _ => a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)),
    }
}

/// The apps sorted by name with `cmp`.
pub fn sorted(apps: &[App], cmp: impl Fn(&str, &str) -> Ordering) -> Vec<App> {
    let mut v = apps.to_vec();
    v.sort_by(|a, b| cmp(&a.name, &b.name).then_with(|| a.id.cmp(&b.id)));
    v
}

/// Indexes into `apps` (already sorted) of the ones matching `text`, best
/// match first (same score: list order), at most `limit`.
pub fn filter(apps: &[App], text: &str, limit: usize) -> Vec<usize> {
    let q = Query::new(text);
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(u32, usize)> = apps.iter().enumerate().filter_map(|(i, a)| q.score(&a.name).map(|s| (s, i))).collect();
    // Stable: equal scores keep the alphabetical order.
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    hits.into_iter().take(limit).map(|(_, i)| i).collect()
}

/// Packaged (Store) apps are named by an AUMID with a '!': they cannot be
/// run as administrator.
pub fn packaged(app: &App) -> bool {
    app.id.contains('!')
}

/// The program a desktop app's Start entry points at (its shortcut's
/// target), if there is one.
pub fn target_path(app: &App) -> Option<String> {
    if packaged(app) {
        return None;
    }
    unsafe {
        let item: IShellItem2 = SHCreateItemFromParsingName(&HSTRING::from(app.target()), None).ok()?;
        let p: PWSTR = item.GetString(&windows::Win32::Storage::EnhancedStorage::PKEY_Link_TargetParsingPath).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| s.len() > 3 && std::path::Path::new(s).is_file())
    }
}

/// What "Keep in Dock" pins: the program itself when known (so the dock
/// sees its windows as this app's), else the Start entry.
pub fn pin_path(app: &App, target: Option<&str>) -> String {
    match target {
        Some(t) if t.to_lowercase().ends_with(".exe") => t.to_string(),
        _ => app.target(),
    }
}

/// Is any of `paths` pinned (case does not matter)?
pub fn is_pinned(pinned: &[String], paths: &[&str]) -> bool {
    pinned.iter().any(|p| paths.iter().any(|q| p.eq_ignore_ascii_case(q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> App {
        App { name: name.into(), id: format!("id.{name}") }
    }

    #[test]
    fn sorting_follows_the_language() {
        let names = |v: Vec<App>| v.into_iter().map(|a| a.name).collect::<Vec<_>>();
        let apps = vec![app("b"), app("App 10"), app("가나"), app("App 2"), app("a"), app("Ångström"), app("Zoom")];
        let en = HSTRING::from("en-US");
        let got = names(sorted(&apps, |a, b| compare(&en, a, b)));
        assert_eq!(got, ["a", "Ångström", "App 2", "App 10", "b", "Zoom", "가나"]);
        // Korean: Hangul in its own order, numbers by value.
        let ko = HSTRING::from("ko-KR");
        let apps = vec![app("하나"), app("가나"), app("나비"), app("메모장 2"), app("메모장 10")];
        let got = names(sorted(&apps, |a, b| compare(&ko, a, b)));
        assert_eq!(got, ["가나", "나비", "메모장 2", "메모장 10", "하나"]);
        // Same name: still a fixed order.
        let twins = vec![App { name: "X".into(), id: "2".into() }, App { name: "X".into(), id: "1".into() }];
        assert_eq!(sorted(&twins, |a, b| compare(&en, a, b))[0].id, "1");
    }

    #[test]
    fn the_ui_language_names_the_locale() {
        let _l = crate::i18n::test_lock("ko");
        assert_eq!(locale().to_string().to_lowercase(), "ko-kr");
        drop(_l);
        let _l = crate::i18n::test_lock("en");
        assert!(locale().to_string().to_lowercase().starts_with("en"));
    }

    #[test]
    fn filtering_ranks_best_first() {
        let apps = vec![app("Calculator"), app("Camera"), app("카카오톡"), app("Visual Studio Code"), app("Notepad")];
        let hits: Vec<&str> = filter(&apps, "ca", 10).iter().map(|&i| apps[i].name.as_str()).collect();
        assert_eq!(hits, ["Camera", "Calculator"], "same tier: the shorter name first");
        assert_eq!(filter(&apps, "ㅋㅋㅇ", 10), vec![2]);
        assert_eq!(filter(&apps, "vsc", 10), vec![3]);
        assert!(filter(&apps, "  ", 10).is_empty());
        assert!(filter(&apps, "zzz", 10).is_empty());
        assert_eq!(filter(&apps, "a", 2).len(), 2, "limited");
    }

    #[test]
    fn what_each_app_allows() {
        let store = App { name: "Calc".into(), id: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into() };
        let desktop = App { name: "Tool".into(), id: r"{6D809377-6AF0-444B-8957-A3773F02200E}\Tool\tool.exe".into() };
        assert!(packaged(&store) && !packaged(&desktop));
        assert_eq!(target_path(&store), None);
        assert_eq!(pin_path(&desktop, Some(r"C:\Program Files\Tool\tool.exe")), r"C:\Program Files\Tool\tool.exe");
        assert_eq!(pin_path(&store, None), store.target());
        assert_eq!(pin_path(&desktop, Some(r"C:\x\readme.txt")), desktop.target());
        let pinned = vec![r"c:\program files\tool\TOOL.exe".to_string()];
        assert!(is_pinned(&pinned, &[r"C:\Program Files\Tool\tool.exe"]));
        assert!(!is_pinned(&pinned, &["shell:AppsFolder\\x"]));
    }
}
