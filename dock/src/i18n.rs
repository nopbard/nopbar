//! UI text in the user's language.
//!
//! Every visible string is looked up by key: `t("dock.menu.quit")`, or with
//! values filled in: `tf("dock.menu.close_n_windows", &[("n", "3")])` for
//! "Close {n} windows". Catalogs are `dock/locales/<tag>.json` (flat
//! key -> text), compiled into the exe. Missing keys fall back to English,
//! then to the key itself.
//!
//! The language follows Windows' display language unless the user picked
//! one in the settings (`config.language`). It can change while running:
//! everything built afterwards (menus, panels, labels) uses the new one.
//!
//! Dates and times come from Windows' own locale formatting (see `date`).

use std::collections::HashMap;
use std::sync::RwLock;

/// (tag, catalog). Tags are lowercase BCP 47; "zh-cn" / "zh-tw" / "pt-br" /
/// "pt-pt" carry a region where the written language differs.
static CATALOGS: &[(&str, &str)] = &[
    ("en", include_str!("../locales/en.json")),
    ("ko", include_str!("../locales/ko.json")),
    ("ar", include_str!("../locales/ar.json")),
    ("bg", include_str!("../locales/bg.json")),
    ("bn", include_str!("../locales/bn.json")),
    ("cs", include_str!("../locales/cs.json")),
    ("da", include_str!("../locales/da.json")),
    ("de", include_str!("../locales/de.json")),
    ("el", include_str!("../locales/el.json")),
    ("es", include_str!("../locales/es.json")),
    ("et", include_str!("../locales/et.json")),
    ("fa", include_str!("../locales/fa.json")),
    ("fi", include_str!("../locales/fi.json")),
    ("fil", include_str!("../locales/fil.json")),
    ("fr", include_str!("../locales/fr.json")),
    ("he", include_str!("../locales/he.json")),
    ("hi", include_str!("../locales/hi.json")),
    ("hr", include_str!("../locales/hr.json")),
    ("hu", include_str!("../locales/hu.json")),
    ("id", include_str!("../locales/id.json")),
    ("it", include_str!("../locales/it.json")),
    ("ja", include_str!("../locales/ja.json")),
    ("lt", include_str!("../locales/lt.json")),
    ("lv", include_str!("../locales/lv.json")),
    ("ms", include_str!("../locales/ms.json")),
    ("nb", include_str!("../locales/nb.json")),
    ("nl", include_str!("../locales/nl.json")),
    ("pl", include_str!("../locales/pl.json")),
    ("pt-br", include_str!("../locales/pt-br.json")),
    ("pt-pt", include_str!("../locales/pt-pt.json")),
    ("ro", include_str!("../locales/ro.json")),
    ("ru", include_str!("../locales/ru.json")),
    ("sk", include_str!("../locales/sk.json")),
    ("sl", include_str!("../locales/sl.json")),
    ("sr", include_str!("../locales/sr.json")),
    ("sv", include_str!("../locales/sv.json")),
    ("th", include_str!("../locales/th.json")),
    ("tr", include_str!("../locales/tr.json")),
    ("uk", include_str!("../locales/uk.json")),
    ("vi", include_str!("../locales/vi.json")),
    ("zh-cn", include_str!("../locales/zh-cn.json")),
    ("zh-tw", include_str!("../locales/zh-tw.json")),
];

#[cfg(test)]
#[test]
fn notes_entry_labels_exist_in_every_catalog_without_fallback() {
    for (lang, json) in CATALOGS {
        let entries: HashMap<String, String> = serde_json::from_str(json).unwrap();
        for key in ["notes.new", "widgets.notes"] {
            assert!(entries.get(key).is_some_and(|s| !s.trim().is_empty()), "{lang}: {key}");
        }
    }
}

/// Languages offered in the settings, by tag, with their own name.
pub fn languages() -> Vec<(&'static str, String)> {
    CATALOGS
        .iter()
        .map(|(tag, text)| {
            let name = parse(text).get("language.name").map(|s| s.to_string()).unwrap_or_else(|| tag.to_string());
            (*tag, name)
        })
        .collect()
}

type Map = HashMap<String, &'static str>;

struct State {
    tag: &'static str,
    current: &'static Map,
    english: &'static Map,
}

/// Every catalog loaded so far (each one once: switching back and forth
/// costs nothing more), and keys no catalog has (looked up once each).
static LOADED: std::sync::Mutex<Option<HashMap<&'static str, &'static Map>>> = std::sync::Mutex::new(None);
static MISSING: std::sync::Mutex<Option<HashMap<String, &'static str>>> = std::sync::Mutex::new(None);

static STATE: RwLock<Option<State>> = RwLock::new(None);

fn parse(text: &str) -> HashMap<String, String> {
    serde_json::from_str(text).unwrap_or_default()
}

fn load(tag: &'static str) -> &'static Map {
    let mut guard = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(m) = cache.get(tag) {
        return m;
    }
    let text = CATALOGS.iter().find(|(t, _)| *t == tag).map(|(_, s)| *s).unwrap_or("{}");
    // Leaked once per language for the life of the process (a few KB):
    // lookups hand out &'static.
    let map: Map = parse(text).into_iter().map(|(k, v)| (k, &*Box::leak(v.into_boxed_str()))).collect();
    let map: &'static Map = Box::leak(Box::new(map));
    cache.insert(tag, map);
    map
}

/// Picking the language (shared with the setup exe).
mod pick;

fn has_catalog(tag: &str) -> Option<&'static str> {
    CATALOGS.iter().map(|(c, _)| *c).find(|c| *c == tag)
}

/// The catalog tag that best serves a Windows language tag ("de-AT" -> de,
/// "zh-Hant-HK" -> zh-tw, "pt-PT" -> pt-pt, "pt" -> pt-br).
pub fn best_tag(want: &str) -> Option<&'static str> {
    pick::best_tag(has_catalog, want)
}

/// Windows' preferred UI languages, most preferred first.
pub fn windows_languages() -> Vec<String> {
    pick::windows_languages()
}

/// Picks the language: `setting` is "auto" (follow Windows) or a tag.
pub fn init(setting: &str) {
    let tag = if setting != "auto" { best_tag(setting) } else { None }
        .or_else(|| pick::first_served(has_catalog, &windows_languages()))
        .unwrap_or("en");
    if let Ok(mut s) = STATE.write() {
        if s.as_ref().is_some_and(|s| s.tag == tag) {
            return;
        }
        let english = s.take().map(|s| s.english).unwrap_or_else(|| load("en"));
        *s = Some(State { tag, current: load(tag), english });
    }
}

/// First use without an explicit init: Windows' language, but only if
/// nothing set a language meanwhile (another thread may have, e.g. a
/// test holding test_lock: a late "auto" must not override it).
fn init_default() {
    let tag = pick::first_served(has_catalog, &windows_languages()).unwrap_or("en");
    if let Ok(mut s) = STATE.write() {
        if s.is_none() {
            *s = Some(State { tag, current: load(tag), english: load("en") });
        }
    }
}

/// The language in use.
pub fn current() -> &'static str {
    STATE.read().ok().and_then(|s| s.as_ref().map(|s| s.tag)).unwrap_or("en")
}

/// The language in use is written right to left.
pub fn rtl() -> bool {
    is_rtl(current())
}

fn is_rtl(tag: &str) -> bool {
    pick::is_rtl(tag)
}

/// Pure geometry for right-to-left layouts: everything is laid out left to
/// right as usual, then reflected inside its container when `on`. Every
/// function is the identity when `on` is false (left-to-right languages).
pub mod mirror {
    /// `x` reflected across the middle of `left..right`.
    pub fn x(on: bool, x: f32, left: f32, right: f32) -> f32 {
        if on { left + right - x } else { x }
    }

    /// The left edge of the span `x..x + w` reflected inside `left..right`.
    pub fn span(on: bool, x: f32, w: f32, left: f32, right: f32) -> f32 {
        if on { left + right - x - w } else { x }
    }

    /// The edges `(l, r)` of a box reflected inside `left..right` (they stay
    /// ordered: the new left edge is the reflection of the old right one).
    pub fn edges(on: bool, l: f32, r: f32, left: f32, right: f32) -> (f32, f32) {
        if on { (left + right - r, left + right - l) } else { (l, r) }
    }

    /// A slider's value at `x` on a track from `x0` to `x1`: grows to the
    /// right, or (right to left) to the left. Clamped to 0..1.
    pub fn slider_value(on: bool, x: f32, x0: f32, x1: f32) -> f32 {
        if x1 <= x0 {
            return 0.0;
        }
        let v = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
        if on { 1.0 - v } else { v }
    }

    /// Where value `v` sits on a track from `x0` to `x1` (see slider_value).
    pub fn slider_x(on: bool, v: f32, x0: f32, x1: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        if on { x1 - (x1 - x0) * v } else { x0 + (x1 - x0) * v }
    }

    /// `s` reads right to left: its first strong letter is Hebrew, Arabic
    /// (Persian included) or another right-to-left script.
    pub fn reads_rtl(s: &str) -> bool {
        for c in s.chars() {
            let u = c as u32;
            if matches!(u, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF) {
                return true;
            }
            if c.is_alphabetic() {
                return false;
            }
        }
        false
    }

    /// Left/right arrow keys in reading order: +1 for the key that moves
    /// forward (Right, or Left when right to left), -1 for the other.
    pub fn arrow(on: bool, right_key: bool) -> i32 {
        if right_key != on { 1 } else { -1 }
    }
}

/// Text for `key` in the current language.
pub fn t(key: &str) -> &'static str {
    let Ok(guard) = STATE.read() else { return "" };
    if guard.is_none() {
        drop(guard);
        init_default();
        return t(key);
    }
    let s = guard.as_ref().unwrap();
    s.current.get(key).or_else(|| s.english.get(key)).copied().unwrap_or_else(|| {
        let mut guard = MISSING.lock().unwrap_or_else(|e| e.into_inner());
        let m = guard.get_or_insert_with(HashMap::new);
        *m.entry(key.to_string()).or_insert_with(|| Box::leak(key.to_string().into_boxed_str()))
    })
}

/// Explicit-language labels (reminder parsers carry their own language).
pub fn in_language(tag: &str, key: &str) -> &'static str {
    load(best_tag(tag).unwrap_or("en")).get(key)
        .or_else(|| load("en").get(key)).copied().unwrap_or_else(|| t(key))
}

#[cfg(test)]
#[path = "i18n/source_tests.rs"]
mod source_tests;

/// Text for `key` in the current language or English; None if neither has it.
pub fn get(key: &str) -> Option<&'static str> {
    let Ok(guard) = STATE.read() else { return None };
    if guard.is_none() {
        drop(guard);
        init_default();
        return get(key);
    }
    let s = guard.as_ref().unwrap();
    s.current.get(key).or_else(|| s.english.get(key)).copied()
}

/// `t(key)` with "{name}" placeholders filled in.
pub fn tf(key: &str, values: &[(&str, &str)]) -> String {
    let mut s = t(key).to_string();
    for (name, value) in values {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

/// Tests that switch the language hold this (the language is process-wide
/// and tests run in parallel).
#[cfg(test)]
pub fn test_lock(tag: &str) -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A first lookup elsewhere picks "auto" (Windows' language) once: let
    // that happen before the switch, not while it is held.
    let _ = get("language.name");
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init(tag);
    g
}

/// Dates and times as the user's Windows region writes them.
pub mod date {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::Globalization::{GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, GetUserDefaultLocaleName, ResolveLocaleName, ENUM_DATE_FORMATS_FLAGS, LOCALE_SMONTHDAY, LOCALE_STIMEFORMAT, TIME_FORMAT_FLAGS, TIME_NOSECONDS};

    /// Which locale formats dates: the user's own region settings when the
    /// region speaks the UI language (keeps their custom formats), else the
    /// UI language's usual region ("en" -> en-US), so English UI on a Korean
    /// region does not show Korean dates.
    fn locale() -> Option<HSTRING> {
        let ui = super::current();
        let base = ui.split('-').next().unwrap_or(ui);
        let mut buf = [0u16; 85];
        let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
        let user = if n > 1 { String::from_utf16_lossy(&buf[..n as usize - 1]).to_lowercase() } else { String::new() };
        if user.split('-').next() == Some(base) && (ui == base || user.starts_with(ui)) {
            return None;
        }
        // A few languages' default region uses another calendar (ar -> ar-SA,
        // Hijri); someone whose region is elsewhere expects Gregorian dates.
        let ui = match ui {
            "ar" => "ar-EG",
            "fa" => "fa-AF",
            other => other,
        };
        let mut out = [0u16; 85];
        let n = unsafe { ResolveLocaleName(&HSTRING::from(ui), Some(&mut out)) };
        (n > 1).then(|| HSTRING::from_wide(&out[..n as usize - 1]))
    }

    fn ptr(l: &Option<HSTRING>) -> PCWSTR {
        l.as_ref().map(|h| PCWSTR(h.as_ptr())).unwrap_or(PCWSTR::null())
    }

    fn date(st: &SYSTEMTIME, flags: ENUM_DATE_FORMATS_FLAGS, pattern: Option<&str>) -> String {
        let loc = locale();
        let mut buf = [0u16; 128];
        let pat = pattern.map(HSTRING::from);
        let n = unsafe { GetDateFormatEx(ptr(&loc), flags, Some(st), pat.as_ref().map(|p| PCWSTR(p.as_ptr())).unwrap_or(PCWSTR::null()), Some(&mut buf), None) };
        if n > 0 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { String::new() }
    }

    /// "November 1" / "11월 1일" / "1. November".
    pub fn month_day(st: &SYSTEMTIME) -> String {
        // GetDateFormatEx refuses DATE_MONTHDAY (ERROR_INVALID_FLAGS), so
        // ask for the locale's month-day pattern and format with that.
        let loc = locale();
        let mut buf = [0u16; 80];
        let n = unsafe { GetLocaleInfoEx(ptr(&loc), LOCALE_SMONTHDAY, Some(&mut buf)) };
        let pattern = if n > 1 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { "MMMM d".to_string() };
        date(st, ENUM_DATE_FORMATS_FLAGS(0), Some(&pattern))
    }

    /// Short weekday name ("Sun", "일", "So.").
    pub fn weekday(st: &SYSTEMTIME) -> String {
        date(st, ENUM_DATE_FORMATS_FLAGS(0), Some("ddd"))
    }

    /// Month and year ("November 2026", "2026년 11월").
    pub fn year_month(st: &SYSTEMTIME) -> String {
        date(st, windows::Win32::Globalization::DATE_YEARMONTH, None)
    }

    /// The long date and the time, for screen readers ("Monday, November
    /// 2, 2026 8:23 PM").
    pub fn full(st: &SYSTEMTIME) -> String {
        format!("{} {}", long(st), time(st))
    }

    /// The same with the seconds (the bar's clock while it shows them).
    pub fn full_seconds(st: &SYSTEMTIME) -> String {
        format!("{} {}", long(st), time_seconds(st))
    }

    /// "8:23 PM" / "오후 8:23" / "20:23".
    pub fn time(st: &SYSTEMTIME) -> String {
        time_as(st, TIME_NOSECONDS, None)
    }

    /// With the seconds, in the region's own order and 12 / 24 hours:
    /// "8:23:05 PM" / "오후 8:23:05" / "20:23:05". A time format set
    /// without seconds ("h:mm tt") gets them (time_pattern); one that
    /// cannot be read leaves the locale's own long time.
    pub fn time_seconds(st: &SYSTEMTIME) -> String {
        let p = time_pattern(true);
        if p.is_empty() { time_as(st, TIME_FORMAT_FLAGS(0), None) } else { time_with(st, &p) }
    }

    /// `st` in the time pattern `pattern` ("h:mm:ss tt"), in the same
    /// locale as the rest.
    pub fn time_with(st: &SYSTEMTIME, pattern: &str) -> String {
        time_as(st, TIME_FORMAT_FLAGS(0), Some(pattern))
    }

    /// The time format in force (LOCALE_STIMEFORMAT of the locale above:
    /// the user's own when it is their region), with a seconds field when
    /// `seconds` (with_seconds); "" when it cannot be read or given one.
    pub fn time_pattern(seconds: bool) -> String {
        let loc = locale();
        let mut buf = [0u16; 80];
        let n = unsafe { GetLocaleInfoEx(ptr(&loc), LOCALE_STIMEFORMAT, Some(&mut buf)) };
        let p = if n > 1 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { String::new() };
        if seconds { with_seconds(&p).unwrap_or_default() } else { p }
    }

    /// Pure: Windows time pattern `p` with a seconds field. As it is when it
    /// has one ("s" outside quotes); else "ss" right after its minutes,
    /// joined by the separator between its hours and minutes (":" when that
    /// is not plain punctuation), so the order, AM / PM and separators stay:
    /// "h:mm tt" -> "h:mm:ss tt", "tt h:mm" -> "tt h:mm:ss", "HH.mm" ->
    /// "HH.mm.ss". None without a minutes field.
    pub fn with_seconds(p: &str) -> Option<String> {
        let chars: Vec<char> = p.chars().collect();
        let (mut quoted, mut hour_end, mut minutes) = (false, None, None);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\'' {
                quoted = !quoted;
            } else if !quoted && c == 's' {
                return Some(p.to_string());
            } else if !quoted && matches!(c, 'h' | 'H' | 'm') {
                let start = i;
                while i < chars.len() && chars[i] == c {
                    i += 1;
                }
                if c == 'm' {
                    minutes = Some((start, i));
                } else {
                    hour_end = Some(i);
                }
                continue;
            }
            i += 1;
        }
        let (m0, m1) = minutes?;
        let sep: String = match hour_end {
            Some(h) if h <= m0 => chars[h..m0].iter().collect(),
            _ => String::new(),
        };
        let sep = if !sep.is_empty() && sep.chars().all(|c| !c.is_alphanumeric() && c != '\'') { sep } else { ":".to_string() };
        let mut out: String = chars[..m1].iter().collect();
        out.push_str(&sep);
        out.push_str("ss");
        out.extend(&chars[m1..]);
        Some(out)
    }

    fn time_as(st: &SYSTEMTIME, flags: TIME_FORMAT_FLAGS, pattern: Option<&str>) -> String {
        let loc = locale();
        let mut buf = [0u16; 64];
        let pat = pattern.map(HSTRING::from);
        let n = unsafe { GetTimeFormatEx(ptr(&loc), flags, Some(st), pat.as_ref().map(|p| PCWSTR(p.as_ptr())).unwrap_or(PCWSTR::null()), Some(&mut buf)) };
        if n > 0 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { String::new() }
    }

    /// The long date alone ("Thursday, November 5, 2026").
    pub fn long(st: &SYSTEMTIME) -> String {
        date(st, windows::Win32::Globalization::DATE_LONGDATE, None)
    }

    /// The long date pattern in force ("dddd, MMMM d, yyyy"); "" when it
    /// cannot be read.
    pub fn long_pattern() -> String {
        let loc = locale();
        let mut buf = [0u16; 80];
        let n = unsafe { GetLocaleInfoEx(ptr(&loc), windows::Win32::Globalization::LOCALE_SLONGDATE, Some(&mut buf)) };
        if n > 1 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { String::new() }
    }

    /// `st` in the date pattern `pattern`, in the same locale as the rest.
    pub fn with_pattern(st: &SYSTEMTIME, pattern: &str) -> String {
        date(st, ENUM_DATE_FORMATS_FLAGS(0), Some(pattern))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_formats_without_seconds_get_them_where_they_belong() {
        use date::with_seconds as w;
        let some = |s: &str| Some(s.to_string());
        assert_eq!(w("h:mm tt"), some("h:mm:ss tt"));
        assert_eq!(w("HH:mm"), some("HH:mm:ss"));
        assert_eq!(w("tt h:mm"), some("tt h:mm:ss"), "AM / PM first stays first");
        assert_eq!(w("H:mm"), some("H:mm:ss"));
        assert_eq!(w("HH.mm"), some("HH.mm.ss"), "the region's separator");
        // Already with seconds: as it is.
        assert_eq!(w("h:mm:ss tt"), some("h:mm:ss tt"));
        assert_eq!(w("tt h:mm:ss"), some("tt h:mm:ss"));
        assert_eq!(w("H:mm:s"), some("H:mm:s"));
        // A quoted literal is not a field ("s" in it, letters between).
        assert_eq!(w("h:mm 'pass'"), some("h:mm:ss 'pass'"));
        assert_eq!(w("H'h'mm"), some("H'h'mm:ss"), "a word between: ':'");
        assert_eq!(w("HHmm"), some("HHmm:ss"));
        // No minutes to follow: the caller keeps the locale's own.
        assert_eq!(w("tt"), None);
        assert_eq!(w(""), None);
        // Formatted with the rewritten pattern, the seconds show.
        let _l = test_lock("en");
        let st = windows::Win32::Foundation::SYSTEMTIME { wYear: 2026, wMonth: 11, wDay: 5, wHour: 21, wMinute: 4, wSecond: 9, ..Default::default() };
        for (p, want) in [("h:mm tt", "9:04:09 PM"), ("HH:mm", "21:04:09"), ("tt h:mm", "PM 9:04:09")] {
            assert_eq!(date::time_with(&st, &w(p).unwrap()), want, "{p}");
        }
        assert!(date::time_seconds(&st).contains("09"));
        assert!(date::time_pattern(true).contains('s'));
    }

    /// Final audit 11: the Korean settings sidebar said "Launchpad·검색",
    /// the one English word among its pages. A catalog in a script of its
    /// own names the sidebar's pages in it (no Latin letters), and Korean
    /// uses the Windows name 모든 앱.
    #[test]
    fn the_sidebar_speaks_its_language() {
        let own_script = ["ko", "ja", "zh-cn", "zh-tw", "ar", "fa", "he", "hi", "bn", "th", "ru", "uk", "bg", "el"];
        for (tag, src) in CATALOGS.iter().filter(|(t, _)| own_script.contains(t)) {
            for (k, v) in parse(src).iter().filter(|(k, _)| k.starts_with("settings.tab.")) {
                assert!(!v.chars().any(|c| c.is_ascii_alphabetic()), "{tag} {k}: {v}");
            }
        }
        let ko = parse(CATALOGS.iter().find(|(t, _)| *t == "ko").unwrap().1);
        assert_eq!(ko.get("settings.tab.search").map(String::as_str), Some("앱·검색"));
        assert!(ko.values().all(|v| !v.contains("Launchpad")), "Korean says 모든 앱");
    }

    #[test]
    fn catalogs_have_no_long_dashes() {
        let forbidden = ['\u{2013}', '\u{2014}', '\u{2015}'];
        for (tag, text) in CATALOGS {
            assert!(!text.contains(forbidden), "{tag} contains a forbidden dash");
            let entries: HashMap<String, String> = serde_json::from_str(text).unwrap();
            for (key, value) in entries {
                assert!(
                    !key.contains(forbidden) && !value.contains(forbidden),
                    "{tag}: {key} contains a forbidden dash"
                );
            }
        }
    }

    #[test]
    fn every_catalog_parses_and_has_every_english_key() {
        let en = parse(CATALOGS[0].1);
        // D2 explicitly ships Korean/English and English fallback elsewhere.
        let d2: Vec<String> = serde_json::from_str(include_str!("i18n_fallback_d2.json")).unwrap();
        assert!(!en.is_empty());
        for (tag, text) in CATALOGS {
            let m: HashMap<String, String> = serde_json::from_str(text).unwrap_or_else(|e| panic!("{tag}: {e}"));
            for k in en.keys() {
                if *tag != "ko" && d2.contains(k) {continue;}
                // The full-quit brief explicitly uses English fallback in
                // other locales. Keep the exception limited to these keys.
                if *tag != "ko" && matches!(k.as_str(), "dock.menu.quit_completely" | "dock.menu.quit_completely_denied") {
                    continue;
                }
                assert!(m.contains_key(k), "{tag} lacks {k}");
            }
            for (k, v) in &m {
                // Placeholders must survive translation.
                let want: Vec<&str> = en.get(k).map(|e| placeholders(e)).unwrap_or_default();
                let got = placeholders(v);
                assert_eq!(sorted(want), sorted(got), "{tag}: {k} placeholders");
            }
        }
    }

    /// Every key the code asks for by name (`t("…")`, `tf("…", …)`) is in
    /// the English catalog, so no screen ever shows a bare key. A feature
    /// removal once took `topbar.panel.power.task_manager` with it while
    /// the status panel still used it.
    #[test]
    fn every_key_the_code_names_is_in_the_catalog() {
        let en = parse(CATALOGS[0].1);
        let mut missing = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().is_none_or(|x| x != "rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&p).unwrap();
                for call in ["t(\"", "tf(\""] {
                    let mut rest = src.as_str();
                    while let Some(at) = rest.find(call) {
                        let before = rest[..at].chars().next_back();
                        rest = &rest[at + call.len()..];
                        // Only the function itself, not a name ending in t/tf.
                        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                            continue;
                        }
                        let Some(end) = rest.find('"') else { break };
                        let key = &rest[..end];
                        let keyish = key.contains('.') && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_');
                        if keyish && !en.contains_key(key) {
                            missing.push(format!("{}: {key}", p.display()));
                        }
                    }
                }
            }
        }
        assert!(missing.is_empty(), "keys missing from en.json:\n{}", missing.join("\n"));
    }

    /// The strings added with the Settings redesign are each language's
    /// own words, not English left in place.
    #[test]
    fn settings_redesign_strings_are_translated() {
        const KEYS: [&str; 12] = [
            "settings.section.clock",
            "settings.section.updates",
            "settings.tab.corners",
            "settings.intro.appearance",
            "settings.intro.clock",
            "settings.intro.updates",
            "settings.group.taskbar",
            "settings.group.text",
            "settings.group.clock",
            "settings.topbar.height.hint",
            "settings.search.filtered",
            "settings.search.show_all",
        ];
        // The same word is right there ("Updates" is German and Dutch too,
        // as their about.updates says; "Text" is Czech, Romanian, Slovak
        // and Swedish).
        const SAME: [(&str, &str); 6] = [
            ("de", "settings.section.updates"),
            ("nl", "settings.section.updates"),
            ("cs", "settings.group.text"),
            ("ro", "settings.group.text"),
            ("sk", "settings.group.text"),
            ("sv", "settings.group.text"),
        ];
        let en = parse(CATALOGS[0].1);
        for (tag, text) in CATALOGS.iter().filter(|(tag, _)| *tag != "en") {
            let m = parse(text);
            for k in KEYS {
                assert!(en.contains_key(k), "en lacks {k}");
                if !SAME.contains(&(*tag, k)) {
                    assert_ne!(m.get(k), en.get(k), "{tag}: {k} is still English");
                }
            }
        }
    }

    /// The tray icon's stats panel: NopBar's own rows and the tray hint
    /// that says what a click does are in every language, in its words.
    #[test]
    fn tray_stats_strings_are_translated() {
        const KEYS: [&str; 3] = ["topbar.stats.nopbar.gpu_memory", "topbar.stats.nopbar.procs", "settings.general.tray_icon.hint"];
        let en = parse(CATALOGS[0].1);
        for (tag, text) in CATALOGS.iter().filter(|(tag, _)| *tag != "en") {
            let m = parse(text);
            for k in KEYS {
                assert!(en.contains_key(k), "en lacks {k}");
                assert!(m.get(k).is_some_and(|v| !v.trim().is_empty()), "{tag} lacks {k}");
                assert_ne!(m.get(k), en.get(k), "{tag}: {k} is still English");
            }
            assert!(m["topbar.stats.nopbar.procs"].contains("{n}"), "{tag}: the count");
        }
        // The hint no longer promises quick access to Settings: it says
        // what a click and a right-click do.
        assert!(en["settings.general.tray_icon.hint"].contains("right-click"));
    }

    /// Both Start buttons name the same Windows action in every language.
    #[test]
    fn the_start_button_says_what_it_opens() {
        for (tag, text) in CATALOGS {
            let m = parse(text);
            assert_eq!(m.get("a11y.topbar.start"), m.get("dock.item.start"), "{tag}");
            assert_eq!(m["a11y.topbar.start.help"], "Win+X");
        }
    }

    /// The same action has the same name everywhere it is offered (the
    /// Dock's menu, Launchpad, the tray menu, Settings), in every language.
    #[test]
    fn same_action_same_words() {
        const SAME: [(&str, &str); 3] = [
            ("launchpad.menu.keep_in_dock", "dock.menu.pin"),
            ("launchpad.menu.remove_from_dock", "dock.menu.unpin"),
            ("dock.menu.quit", "settings.general.quit"),
        ];
        for (tag, text) in CATALOGS {
            let m = parse(text);
            for (a, b) in SAME {
                assert_eq!(m.get(a), m.get(b), "{tag}: {a} and {b} name the same action");
            }
        }
        // Korean spelling: 작업 표시줄 (spaced), 상단 바 (spaced).
        let ko = parse(CATALOGS.iter().find(|c| c.0 == "ko").unwrap().1);
        for (k, v) in &ko {
            assert!(!v.contains("작업표시줄"), "ko {k}: 작업 표시줄");
            assert!(!v.contains("상단바"), "ko {k}: 상단 바");
        }
    }

    fn placeholders(s: &str) -> Vec<&str> {
        s.match_indices('{').filter_map(|(i, _)| s[i + 1..].find('}').map(|j| &s[i + 1..i + 1 + j])).collect()
    }

    fn sorted(mut v: Vec<&str>) -> Vec<&str> {
        v.sort();
        v
    }

    #[test]
    fn languages_are_matched() {
        assert_eq!(best_tag("ko-KR"), Some("ko"));
        assert_eq!(best_tag("en-GB"), Some("en"));
        assert_eq!(best_tag("xx-YY"), None);
        assert!(is_rtl("ar") && is_rtl("fa") && is_rtl("he"));
        assert!(!is_rtl("en") && !is_rtl("ko") && !is_rtl("fil"));
    }

    #[test]
    fn mirroring_reflects_inside_the_container() {
        use super::mirror;
        // Left to right: everything stays where it is.
        assert_eq!(mirror::x(false, 30.0, 10.0, 110.0), 30.0);
        assert_eq!(mirror::span(false, 30.0, 20.0, 10.0, 110.0), 30.0);
        assert_eq!(mirror::edges(false, 30.0, 50.0, 10.0, 110.0), (30.0, 50.0));
        // Right to left: 20 from the left becomes 20 from the right.
        assert_eq!(mirror::x(true, 30.0, 10.0, 110.0), 90.0);
        assert_eq!(mirror::span(true, 30.0, 20.0, 10.0, 110.0), 70.0);
        assert_eq!(mirror::edges(true, 30.0, 50.0, 10.0, 110.0), (70.0, 90.0));
        // Twice is the identity.
        for x in [0.0, 17.5, 64.0, 100.0] {
            assert_eq!(mirror::x(true, mirror::x(true, x, 0.0, 100.0), 0.0, 100.0), x);
            let s = mirror::span(true, x, 8.0, 0.0, 100.0);
            assert_eq!(mirror::span(true, s, 8.0, 0.0, 100.0), x);
        }
    }

    #[test]
    fn sliders_and_arrows_follow_the_reading_order() {
        use super::mirror;
        assert_eq!(mirror::slider_value(false, 25.0, 0.0, 100.0), 0.25);
        assert_eq!(mirror::slider_value(true, 25.0, 0.0, 100.0), 0.75);
        assert_eq!(mirror::slider_value(true, -5.0, 0.0, 100.0), 1.0);
        assert_eq!(mirror::slider_value(true, 5.0, 10.0, 10.0), 0.0);
        for on in [false, true] {
            for v in [0.0, 0.3, 1.0] {
                let x = mirror::slider_x(on, v, 20.0, 220.0);
                assert!((mirror::slider_value(on, x, 20.0, 220.0) - v).abs() < 1e-6);
            }
        }
        assert_eq!(mirror::slider_x(true, 0.0, 20.0, 220.0), 220.0);
        assert_eq!(mirror::arrow(false, true), 1);
        assert_eq!(mirror::arrow(false, false), -1);
        assert_eq!(mirror::arrow(true, true), -1);
        assert_eq!(mirror::arrow(true, false), 1);
        assert!(mirror::reads_rtl("مرحبا"));
        assert!(mirror::reads_rtl("12 שלום abc"));
        assert!(mirror::reads_rtl("  فارسی"));
        assert!(!mirror::reads_rtl("abc مرحبا"));
        assert!(!mirror::reads_rtl("123 ..."));
        assert!(!mirror::reads_rtl(""));
    }

    #[test]
    fn placeholders_fill_in() {
        let _l = test_lock("en");
        let k = "test.never_defined {n}";
        assert_eq!(tf(k, &[("n", "3")]), "test.never_defined 3");
    }
}

#[cfg(test)] mod widget_shell_tests {
 use super::*;
 #[test] fn review_13_shell_label_is_present_in_every_language(){
  for (tag,_) in CATALOGS {assert!(!load(tag).get("widgets.add").unwrap_or_else(||panic!("missing widget verb: {tag}")).is_empty());}
  assert_eq!(load("ko")["widgets.add"],"위젯 추가");assert_eq!(load("en")["widgets.add"],"Add widgets");
 }
}
