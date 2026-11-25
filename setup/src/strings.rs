//! The window's few words in the user's Windows language, as the app
//! picks it (dock/src/i18n.rs): the first of Windows' preferred UI
//! languages a catalog serves, else English. The words are the `setup.*`
//! keys of dock/locales/*.json, baked in by build.rs.

use std::sync::OnceLock;

/// Picking the language: the app's own code (dock/src/i18n/pick.rs).
#[path = "../../dock/src/i18n/pick.rs"]
mod pick;

/// A language there is a table for (an index into TAGS / TABLES).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lang(usize);

impl Lang {
    /// English (build.rs puts it first).
    pub const EN: Lang = Lang(0);

    /// The table for the catalog tag `tag` ("ko", "zh-tw"), exactly.
    pub fn from_tag(tag: &str) -> Option<Lang> {
        TAGS.iter().position(|t| *t == tag).map(Lang)
    }

    /// The first of `preferred` (Windows language tags, most preferred
    /// first) a table serves; English when none does.
    pub fn pick<S: AsRef<str>>(preferred: &[S]) -> Lang {
        pick::first_served(|t| TAGS.iter().copied().find(|c| *c == t), preferred).and_then(Lang::from_tag).unwrap_or(Lang::EN)
    }

    /// This user's language (Windows' preferred UI languages; read once).
    pub fn current() -> Lang {
        static CURRENT: OnceLock<Lang> = OnceLock::new();
        *CURRENT.get_or_init(|| Lang::pick(&pick::windows_languages()))
    }

    /// Its catalog tag ("en", "pt-br").
    pub fn tag(self) -> &'static str {
        TAGS[self.0]
    }

    /// Written right to left (Arabic, Persian, Hebrew): the window is
    /// mirrored (ui::draw).
    pub fn rtl(self) -> bool {
        pick::is_rtl(self.tag())
    }

    /// The locale text formats get (Han glyphs and fallback follow it).
    pub fn locale(self) -> &'static str {
        match self.tag() {
            "en" => "en-US",
            "ko" => "ko-KR",
            "ja" => "ja-JP",
            "zh-cn" => "zh-CN",
            "zh-tw" => "zh-TW",
            "pt-br" => "pt-BR",
            "pt-pt" => "pt-PT",
            other => other,
        }
    }
}

/// The window shows one line (`hint`), a failure and its retry; the rest
/// is for the taskbar and screen readers only. Fields are the `setup.*`
/// keys ("setup.failed.moved" -> failed_moved; build.rs FIELDS).
#[derive(Debug)]
pub struct Strings {
    /// Taskbar and screen readers.
    pub window_title: &'static str,
    /// The one line shown (also the install action's description).
    pub hint: &'static str,
    /// The install action's accessible name and default action.
    pub action: &'static str,
    pub action_verb: &'static str,
    pub installing: &'static str,
    pub retry: &'static str,
    /// Inno Setup's 7: PrepareToInstall refused; the only refusal
    /// nopbar.iss has is an install in another folder (MovedInstall).
    pub failed_moved: &'static str,
    /// 8: the same, and Windows needs a restart.
    pub failed_restart: &'static str,
    pub failed_unpack: &'static str,
    pub failed_empty: &'static str,
    pub failed_start: &'static str,
    pub failed_other: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/setup_strings.rs"));

pub fn table(lang: Lang) -> &'static Strings {
    &TABLES[lang.0]
}

/// Why an install did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// This build carries no installer (a dev build).
    NoPayload,
    /// The installer could not be written to %TEMP%.
    Extract,
    /// It could not be started.
    Start,
    /// It ran and exited with this code (Inno Setup's exit codes).
    Code(i32),
}

/// The one-line message for a failure.
pub fn failure(lang: Lang, f: Failure) -> &'static str {
    let s = table(lang);
    match f {
        Failure::Code(7) => s.failed_moved,
        Failure::Code(8) => s.failed_restart,
        Failure::Extract => s.failed_unpack,
        Failure::NoPayload => s.failed_empty,
        Failure::Start => s.failed_start,
        Failure::Code(_) => s.failed_other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const FAILURES: [Failure; 8] = [Failure::NoPayload, Failure::Extract, Failure::Start, Failure::Code(1), Failure::Code(4), Failure::Code(7), Failure::Code(8), Failure::Code(-1)];

    fn all(s: &Strings) -> [&'static str; 12] {
        [s.window_title, s.hint, s.action, s.action_verb, s.installing, s.retry, s.failed_moved, s.failed_restart, s.failed_unpack, s.failed_empty, s.failed_start, s.failed_other]
    }

    fn catalog(tag: &str) -> HashMap<String, String> {
        let p = format!("{}/../dock/locales/{tag}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap_or_else(|e| panic!("{p}: {e}"))
    }

    #[test]
    fn every_locale_has_every_setup_key() {
        // Every catalog the app has, and nothing filled in from English.
        let dir = format!("{}/../dock/locales", env!("CARGO_MANIFEST_DIR"));
        let mut files: Vec<String> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".json").map(String::from)).collect();
        files.sort();
        let mut tags: Vec<String> = TAGS.iter().map(|t| t.to_string()).collect();
        tags.sort();
        assert_eq!(tags, files);
        assert_eq!(TAGS.len(), 42);
        assert_eq!((TAGS[0], TAGS[1]), ("en", "ko"));
        assert!(FELL_BACK.is_empty(), "missing setup keys: {FELL_BACK:?}");
        assert!(UNKNOWN.is_empty(), "setup keys no field reads: {UNKNOWN:?}");
        // The table says what the files say.
        for (i, tag) in TAGS.iter().enumerate() {
            let m = catalog(tag);
            let setup: Vec<&String> = m.keys().filter(|k| k.starts_with("setup.")).collect();
            assert_eq!(setup.len(), 12, "{tag}: {setup:?}");
            let s = &TABLES[i];
            assert_eq!(m["setup.hint"], s.hint, "{tag}");
            assert_eq!(m["setup.action_verb"], s.action_verb, "{tag}");
            assert_eq!(m["setup.failed.moved"], s.failed_moved, "{tag}");
            assert_eq!(m["setup.failed.other"], s.failed_other, "{tag}");
        }
    }

    #[test]
    fn every_string_is_there_short_and_translated() {
        let en = table(Lang::EN);
        for tag in TAGS {
            let lang = Lang::from_tag(tag).unwrap();
            assert_eq!(lang.tag(), tag);
            let s = table(lang);
            for t in all(s) {
                assert!(!t.trim().is_empty() && t.trim() == t, "{tag}: {t:?}");
                assert!(t.chars().count() <= 80, "{tag}: {t}");
            }
            // The key the hint names is on the keyboard as "Enter".
            assert!(s.hint.contains("Enter"), "{tag}: {}", s.hint);
            // For screen readers and the taskbar: short, and the name stays.
            for t in [s.window_title, s.action] {
                assert!(t.contains("NopBar") && t.chars().count() <= 24, "{tag}: {t}");
            }
            for f in FAILURES {
                let m = failure(lang, f);
                assert!(!m.is_empty() && m.chars().count() <= 80, "{tag}: {m}");
            }
            assert_ne!(failure(lang, Failure::Code(7)), failure(lang, Failure::Code(4)), "{tag}");
            if lang != Lang::EN {
                for (a, b) in all(s).into_iter().zip(all(en)).skip(1) {
                    assert_ne!(a, b, "{tag}: still English");
                }
            }
        }
        assert_eq!(table(Lang::from_tag("ko").unwrap()).hint, "끌어다 놓거나 Enter");
        assert_eq!(en.hint, "Drag, or press Enter");
    }

    #[test]
    fn the_language_is_picked_as_the_app_picks_it() {
        let tag = |l: &[&str]| Lang::pick(l).tag();
        assert_eq!(tag(&["ko-KR", "en-US"]), "ko");
        assert_eq!(tag(&["en-GB"]), "en");
        assert_eq!(tag(&["de-AT", "en-US"]), "de");
        assert_eq!(tag(&["ja-JP"]), "ja");
        assert_eq!(tag(&["zh-Hans-CN"]), "zh-cn");
        assert_eq!(tag(&["zh-CN"]), "zh-cn");
        assert_eq!(tag(&["zh-Hant-TW"]), "zh-tw");
        assert_eq!(tag(&["zh-HK"]), "zh-tw");
        assert_eq!(tag(&["pt-BR"]), "pt-br");
        assert_eq!(tag(&["pt-PT"]), "pt-pt");
        assert_eq!(tag(&["nn-NO"]), "nb");
        assert_eq!(tag(&["fil-PH"]), "fil");
        assert_eq!(tag(&["ar-EG"]), "ar");
        assert_eq!(tag(&["he-IL"]), "he");
        assert_eq!(tag(&["fa-IR"]), "fa");
        // Not served: the next preferred one, else English.
        assert_eq!(tag(&["ca-ES", "es-ES"]), "es");
        assert_eq!(tag(&["eu-ES", "gl-ES"]), "en");
        assert_eq!(tag(&[]), "en");
        assert_eq!(Lang::from_tag("xx"), None);
        let _ = Lang::current();
        assert_eq!(Lang::current(), Lang::current(), "read once");
    }

    /// The inner installer (packaging/inno/nopbar.iss) speaks the user's
    /// language when Inno Setup has it, else English, never asking; its
    /// one message of our own (another install folder) is the setup
    /// window's sentence in every language it lists.
    #[test]
    fn the_inner_installer_speaks_the_same_language() {
        let iss = include_str!("../../packaging/inno/nopbar.iss");
        assert!(iss.lines().any(|l| l.trim() == "ShowLanguageDialog=no"), "no Select Language box");
        let names: Vec<&str> = iss.lines().filter_map(|l| l.strip_prefix("{#Lang(\"")?.split('"').next()).collect();
        assert_eq!(names.len(), 41);
        for name in names {
            let tag = match name {
                "zh_cn" => "zh-cn",
                "zh_tw" => "zh-tw",
                "pt_br" => "pt-br",
                "pt" => "pt-pt",
                n => n,
            };
            let lang = Lang::from_tag(tag).unwrap_or_else(|| panic!("{name}: no table for {tag}"));
            let line = iss.lines().find_map(|l| l.strip_prefix(&format!("{name}.MovedInstall="))).unwrap_or_else(|| panic!("{name}.MovedInstall"));
            assert!(line.contains("%1"), "{name}: the folder");
            if name != "ko" {
                assert_eq!(line, format!("{}%n%1", table(lang).failed_moved), "{name}");
            }
        }
    }

    #[test]
    fn right_to_left_and_locales() {
        for (t, rtl) in [("ar", true), ("fa", true), ("he", true), ("en", false), ("ko", false), ("fil", false), ("fi", false)] {
            assert_eq!(Lang::from_tag(t).unwrap().rtl(), rtl, "{t}");
        }
        for (t, loc) in [("en", "en-US"), ("ko", "ko-KR"), ("ja", "ja-JP"), ("zh-cn", "zh-CN"), ("zh-tw", "zh-TW"), ("pt-br", "pt-BR"), ("de", "de")] {
            assert_eq!(Lang::from_tag(t).unwrap().locale(), loc);
        }
    }
}
