//! Which catalog serves the user: Windows' preferred UI languages, each
//! matched to the catalogs there are. Shared word for word by the app
//! (dock/src/i18n.rs) and the setup exe (setup/src/strings.rs, by #[path]),
//! so both speak the same language without setup depending on dock.

/// The catalog tag that best serves a Windows language tag ("de-AT" -> de,
/// "zh-Hant-HK" -> zh-tw, "pt-PT" -> pt-pt, "pt" -> pt-br). `has(tag)`
/// returns the catalog's own tag when there is one (lowercase BCP 47;
/// "zh-cn" / "zh-tw" / "pt-br" / "pt-pt" carry a region).
pub fn best_tag<'a>(has: impl Fn(&str) -> Option<&'a str>, want: &str) -> Option<&'a str> {
    let w = want.to_lowercase().replace('_', "-");
    if let Some(t) = has(&w) {
        return Some(t);
    }
    let base = w.split('-').next().unwrap_or("");
    match base {
        "zh" => {
            let trad = w.contains("hant") || w.ends_with("-tw") || w.ends_with("-hk") || w.ends_with("-mo");
            has(if trad { "zh-tw" } else { "zh-cn" })
        }
        "pt" => has(if w.ends_with("-pt") { "pt-pt" } else { "pt-br" }).or_else(|| has("pt-br")),
        "nb" | "nn" | "no" => has("nb"),
        _ => has(base),
    }
}

/// The first of Windows' languages (most preferred first) a catalog
/// serves; None when none does (the caller falls back to English).
pub fn first_served<'a, S: AsRef<str>>(has: impl Fn(&str) -> Option<&'a str>, preferred: &[S]) -> Option<&'a str> {
    preferred.iter().find_map(|l| best_tag(&has, l.as_ref()))
}

/// Windows' preferred UI languages, most preferred first.
pub fn windows_languages() -> Vec<String> {
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let mut count = 0u32;
    let mut len = 0u32;
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len).is_err() || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len).is_err() {
            return Vec::new();
        }
        String::from_utf16_lossy(&buf).split('\0').filter(|s| !s.is_empty()).map(String::from).collect()
    }
}

/// The language `tag` is written right to left (Arabic, Persian, Hebrew).
pub fn is_rtl(tag: &str) -> bool {
    matches!(tag.split('-').next(), Some("ar" | "fa" | "he"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOME: [&str; 8] = ["en", "ko", "de", "zh-cn", "zh-tw", "pt-br", "pt-pt", "nb"];

    fn has(t: &str) -> Option<&'static str> {
        SOME.iter().copied().find(|c| *c == t)
    }

    #[test]
    fn windows_tags_find_their_catalog() {
        for (want, got) in [
            ("ko-KR", Some("ko")),
            ("en-GB", Some("en")),
            ("de-AT", Some("de")),
            ("DE_ch", Some("de")),
            ("zh-CN", Some("zh-cn")),
            ("zh-Hans-SG", Some("zh-cn")),
            ("zh-SG", Some("zh-cn")),
            ("zh-Hant-HK", Some("zh-tw")),
            ("zh-TW", Some("zh-tw")),
            ("zh-HK", Some("zh-tw")),
            ("zh-MO", Some("zh-tw")),
            ("pt-PT", Some("pt-pt")),
            ("pt-BR", Some("pt-br")),
            ("pt-AO", Some("pt-br")),
            ("pt", Some("pt-br")),
            ("nn-NO", Some("nb")),
            ("no", Some("nb")),
            ("xx-YY", None),
            ("", None),
        ] {
            assert_eq!(best_tag(has, want), got, "{want}");
        }
    }

    #[test]
    fn the_first_served_language_wins() {
        assert_eq!(first_served(has, &["xx-YY", "de-DE", "ko-KR"]), Some("de"), "skips what no catalog serves");
        assert_eq!(first_served(has, &["ko-KR", "en-US"]), Some("ko"));
        assert_eq!(first_served(has, &["xx", "yy"]), None, "the caller's English");
        assert_eq!(first_served(has, &[] as &[&str]), None);
        // Whatever Windows says here, it is a list of tags.
        assert!(windows_languages().iter().all(|l| !l.is_empty() && !l.contains('\0')));
    }

    #[test]
    fn right_to_left_languages() {
        assert!(is_rtl("ar") && is_rtl("fa") && is_rtl("he") && is_rtl("ar-SA"));
        assert!(!is_rtl("en") && !is_rtl("ko") && !is_rtl("fil") && !is_rtl("fi") && !is_rtl(""));
    }
}
