//! "Report a problem" (Settings > About): shows dock.log in Explorer and
//! opens a mail draft with the facts a bug report needs. A mailto: link
//! cannot attach files, so the draft asks for dock.log.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows::Win32::Foundation::HWND;

use crate::i18n::t;

pub const TO: &str = "nopbard@gmail.com";
/// Kept in English whatever the UI language, so reports are easy to find.
pub const SUBJECT: &str = "NopBar problem report";

/// What the draft says about this PC.
#[derive(Clone)]
pub struct Facts {
    pub version: String,
    pub windows: String,
    /// One per monitor, e.g. "2560x1440 @ 150% (primary)".
    pub displays: Vec<String>,
    /// NopBar's language, its setting, Windows' languages.
    pub language: String,
}

/// Percent-encodes `s` for a mailto: header value (RFC 6068 / 3986): UTF-8,
/// everything but the unreserved characters escaped, so spaces become %20
/// (not "+"), and "&", "=", "?", "#" cannot end the field early.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A mailto: URL. Line breaks in `body` are sent as CRLF (%0D%0A), as
/// RFC 6068 asks.
pub fn mailto(to: &str, subject: &str, body: &str) -> String {
    let body = body.replace("\r\n", "\n").replace('\n', "\r\n");
    format!("mailto:{to}?subject={}&body={}", encode(subject), encode(&body))
}

/// The draft's text: the user's part first, then the facts.
pub fn body(f: &Facts) -> String {
    let mut s = String::new();
    s.push_str(t("about.report.body.describe"));
    s.push_str("\n\n\n\n");
    s.push_str(t("about.report.body.attach"));
    s.push_str("\n\n----\n");
    s.push_str(&format!("NopBar: {}\n", f.version));
    s.push_str(&format!("Windows: {}\n", f.windows));
    for (i, d) in f.displays.iter().enumerate() {
        s.push_str(&format!("Display {}: {d}\n", i + 1));
    }
    s.push_str(&format!("Language: {}\n", f.language));
    s
}

fn reg_text(key: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 128];
    let mut size = std::mem::size_of_val(&buf) as u32;
    unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key, value, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).ok().ok()?;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n])).filter(|s| !s.is_empty())
}

fn reg_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key, value, RRF_RT_REG_DWORD, None, Some(&mut data as *mut u32 as *mut _), Some(&mut size)).ok().ok()?;
    }
    Some(data)
}

/// "Windows 11 24H2 (build 26100.2894)". The registry's ProductName still
/// says "Windows 10" on 11, so the name comes from the build number.
fn windows_version() -> String {
    let key = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let build = reg_text(key, w!("CurrentBuild")).unwrap_or_default();
    let ubr = reg_dword(key, w!("UBR"));
    let display = reg_text(key, w!("DisplayVersion")).unwrap_or_default();
    let name = match build.parse::<u32>() {
        Ok(b) if b >= 22000 => "Windows 11",
        Ok(_) => "Windows 10",
        Err(_) => "Windows",
    };
    let build = match ubr {
        Some(u) if !build.is_empty() => format!("{build}.{u}"),
        _ if !build.is_empty() => build,
        _ => "?".into(),
    };
    [name.to_string(), display, format!("(build {build})")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

pub fn gather() -> Facts {
    let displays = crate::monitors::all()
        .iter()
        .map(|m| {
            let (w, h) = (m.rect.right - m.rect.left, m.rect.bottom - m.rect.top);
            let pct = (m.scale() * 100.0).round() as i32;
            format!("{w}x{h} @ {pct}%{}", if m.primary { " (primary)" } else { "" })
        })
        .collect();
    let setting = crate::config::read_current().map(|c| c.language).unwrap_or_else(|| "auto".into());
    let language = format!("{} (setting: {setting}; Windows: {})", crate::i18n::current(), crate::i18n::windows_languages().join(", "));
    Facts { version: env!("CARGO_PKG_VERSION").to_string(), windows: windows_version(), displays, language }
}

/// mailto: URLs longer than this may be refused by mail apps or by the
/// shell (2048 is a common limit); the draft is shortened to fit.
pub const MAX_URL: usize = 2000;

/// The draft as a mailto: URL of at most `max` chars: in full if it fits;
/// else with the first two displays and only NopBar's language; else with
/// lines dropped from the end (the facts go first).
pub fn fitted_mailto(f: &Facts, max: usize) -> String {
    let url = |b: &str| mailto(TO, SUBJECT, b);
    let full = url(&body(f));
    if full.len() <= max {
        return full;
    }
    let mut short = f.clone();
    if short.displays.len() > 2 {
        let more = short.displays.len() - 2;
        short.displays.truncate(2);
        short.displays.push(format!("(+{more} more)"));
    }
    short.language = short.language.split(' ').next().unwrap_or("").to_string();
    let text = body(&short);
    if url(&text).len() <= max {
        return url(&text);
    }
    let mut lines: Vec<&str> = text.lines().collect();
    while !lines.is_empty() && url(&lines.join("\n")).len() > max {
        lines.pop();
    }
    url(&lines.join("\n"))
}

/// ShellExecuteW succeeded (it returns a value > 32 then).
unsafe fn shell_open(file: &HSTRING, args: Option<&HSTRING>) -> bool {
    crate::launch::environment::launch("open", &file.to_string(), &args.map(|a| a.to_string()).unwrap_or_default(), "").is_ok()
}

/// Shows dock.log in Explorer (the folder if there is no log yet) and opens
/// the mail draft. Returns a note for the window when something did not
/// open: without a mail app the report goes to the clipboard instead.
pub fn open(owner: HWND) -> Option<String> {
    let dir = crate::config::app_dir();
    let log = dir.join("dock.log");
    let facts = gather();
    unsafe {
        let shown = if log.is_file() {
            let args = HSTRING::from(format!("/select,\"{}\"", log.display()));
            shell_open(&HSTRING::from("explorer.exe"), Some(&args))
        } else {
            let _ = std::fs::create_dir_all(&dir);
            shell_open(&HSTRING::from(dir.as_os_str()), None)
        };
        if !shown {
            crate::log!("report: could not show {}", dir.display());
        }
        let url = fitted_mailto(&facts, MAX_URL);
        if shell_open(&HSTRING::from(url), None) {
            return (!shown).then(|| t("about.report.no_folder").to_string());
        }
        crate::log!("report: no mail app opened the draft");
        let text = format!("To: {TO}\nSubject: {SUBJECT}\n\n{}", body(&facts)).replace('\n', "\r\n");
        let key = if crate::dock::copy_text(owner, &text) { "about.report.no_mail" } else { "about.report.no_mail_copy" };
        Some(t(key).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_escapes_all_but_unreserved() {
        assert_eq!(encode("NopBar problem report"), "NopBar%20problem%20report");
        assert_eq!(encode("a&b=c?d#e+f/g%h"), "a%26b%3Dc%3Fd%23e%2Bf%2Fg%25h");
        assert_eq!(encode("Az09-._~"), "Az09-._~");
        // UTF-8, byte by byte.
        assert_eq!(encode("메모"), "%EB%A9%94%EB%AA%A8");
        assert_eq!(encode("é\n"), "%C3%A9%0A");
    }

    #[test]
    fn mailto_has_crlf_lines_and_one_query() {
        let url = mailto(TO, SUBJECT, "line 1\nline 2\r\nx & y");
        assert_eq!(url, "mailto:nopbard@gmail.com?subject=NopBar%20problem%20report&body=line%201%0D%0Aline%202%0D%0Ax%20%26%20y");
        assert_eq!(url.matches('?').count(), 1);
        assert_eq!(url.matches('&').count(), 1);
    }

    #[test]
    fn body_lists_the_facts() {
        let f = Facts {
            version: "1.2.3".into(),
            windows: "Windows 11 24H2 (build 26100.1)".into(),
            displays: vec!["2560x1440 @ 150% (primary)".into(), "1920x1080 @ 100%".into()],
            language: "en (setting: auto; Windows: en-US)".into(),
        };
        let b = body(&f);
        assert!(b.contains("NopBar: 1.2.3\n"));
        assert!(b.contains("Windows: Windows 11 24H2 (build 26100.1)\n"));
        assert!(b.contains("Display 2: 1920x1080 @ 100%\n"));
        assert!(b.contains("Language: en (setting: auto; Windows: en-US)"));
        assert!(b.contains("dock.log"), "asks for the log");
        // The user writes first; the facts follow the divider.
        assert!(b.find("
----
").is_some_and(|i| i < b.find("NopBar: ").unwrap()));
    }

    #[test]
    fn long_drafts_are_shortened_to_fit() {
        // The draft is in the UI language: the same one throughout.
        let _l = crate::i18n::test_lock("en");
        let f = Facts {
            version: "1.0.0".into(),
            windows: "Windows 11".into(),
            displays: (0..30).map(|i| format!("3840x2160 @ 150% display number {i}")).collect(),
            language: "ko (setting: auto; Windows: ko-KR, en-US, ja-JP, de-DE, fr-FR)".into(),
        };
        let full = mailto(TO, SUBJECT, &body(&f));
        assert!(full.len() > MAX_URL);
        let fit = fitted_mailto(&f, MAX_URL);
        assert!(fit.len() <= MAX_URL);
        assert!(fit.contains("%28%2B28%20more%29"), "says how many displays were left out");
        assert!(fit.contains("Display%202"));
        assert!(!fit.contains("Display%204"), "the rest are counted on the third line");
        // Tighter still: lines go from the end, the URL stays well-formed.
        let tiny = fitted_mailto(&f, 300);
        assert!(tiny.len() <= 300, "{}", tiny.len());
        assert!(tiny.starts_with("mailto:nopbard@gmail.com?subject=NopBar%20problem%20report&body="));
        // Short drafts are left alone.
        let small = Facts { displays: vec!["1920x1080 @ 100% (primary)".into()], ..f };
        assert_eq!(fitted_mailto(&small, MAX_URL), mailto(TO, SUBJECT, &body(&small)));
    }

    #[test]
    fn notes_exist() {
        for k in ["about.report.no_mail", "about.report.no_mail_copy", "about.report.no_folder", "about.report.body.describe", "about.report.body.attach"] {
            assert!(crate::i18n::get(k).is_some(), "missing text key {k}");
        }
    }

    #[test]
    fn windows_version_names_the_build() {
        let v = windows_version();
        assert!(v.starts_with("Windows"), "{v}");
        assert!(v.contains("(build "), "{v}");
    }
}
