//! Launchpad's clutter (pure): which apps look like clutter, and why.
//! Start menus fill up with things that are not apps: uninstallers,
//! website and support links, manuals and release notes, developer
//! command prompts, 32-bit copies of an app that is there in 64 bits.
//! Uninstallers, web links and documents stay off the first Home NopBar
//! makes (to_library, layout_store::migrate; a Home the user arranged is
//! never touched); every kind sorts after the real apps in its Library
//! category.
//!
//! Each matcher looks at the name and the shell:AppsFolder id (a URL for a
//! web link, a file path for a document) only, offline.

use std::collections::{HashMap, HashSet};

use super::library::has_word;

/// Why an app looks like clutter (in the order they are checked).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    Uninstaller,
    DeveloperTool,
    Documentation,
    WebLink,
    /// A 32-bit copy of an app that is also there in 64 bits.
    Duplicate,
}

/// Clutter of this kind starts in the Library, not on Home: an
/// uninstaller, a web link, a document. Developer tools and 32-bit copies
/// are programs someone may use: they stay where they are.
pub fn to_library(r: Reason) -> bool {
    matches!(r, Reason::Uninstaller | Reason::WebLink | Reason::Documentation)
}

/// Words in a name that mean an uninstaller.
/// ("Uninstall" as a word: "Uninstaller Pro", an app that removes others, stays.)
const UNINSTALL: &[&str] = &["uninstall", "제거", "삭제"];
/// Names of developer prompts and tool shortcuts (not the shells themselves:
/// "Command Prompt" and "Windows PowerShell" stay).
const DEV_TOOLS: &[&str] = &[
    "developer command prompt",
    "developer powershell",
    "native tools command prompt",
    "cross tools command prompt",
    "debuggable package manager",
    "install additional tools for node.js",
];
/// Words in a name that mean documentation (whole words; not a lone
/// "docs": Google Docs is an app).
const DOC_WORDS: &[&str] = &[
    "manual",
    "manuals",
    "documentation",
    "module docs",
    "release notes",
    "readme",
    "read me",
    "changelog",
    "change log",
    "license",
    "faq",
    "faqs",
    "user guide",
    "getting started",
    "what's new",
    "tutorial",
    "api reference",
    "help",
    "설명서",
    "도움말",
];
/// Files that are documents, not programs.
const DOC_FILES: &[&str] = &[".chm", ".pdf", ".txt", ".html", ".htm", ".rtf", ".md", ".hlp", ".doc", ".docx"];
/// Words in a name that mean a web or support link (not a lone "support":
/// HP Support Assistant is an app).
const WEB_WORDS: &[&str] = &[
    "website",
    "web site",
    "homepage",
    "home page",
    "on the web",
    "online help",
    "support center",
    "support site",
    "online support",
    "technical support",
    "customer support",
    "register",
    "registration",
    "홈페이지",
    "웹사이트",
];
/// 32-bit and 64-bit markers in names.
const BITS_32: &[&str] = &["(x86)", "(32-bit)", "(32 bit)", "32-bit", "x86"];
const BITS_64: &[&str] = &["(x64)", "(64-bit)", "(64 bit)", "64-bit", "x64"];

/// Is the app's id a web link (an Internet shortcut)?
fn is_link(id: &str) -> bool {
    let i = id.to_lowercase();
    i.starts_with("http://") || i.starts_with("https://") || i.starts_with("www.") || i.ends_with(".url")
}

/// A name without its 32 / 64-bit marker, lowercase, single spaces: the
/// same app in both builds has the same base.
pub fn base_name(name: &str) -> String {
    let mut n = name.to_lowercase();
    for m in BITS_32.iter().chain(BITS_64) {
        if m.starts_with('(') {
            n = n.replace(m, " ");
        }
    }
    let words: Vec<&str> = n.split_whitespace().filter(|w| !BITS_32.contains(w) && !BITS_64.contains(w)).collect();
    words.join(" ")
}

/// Does `name` say it is a 32-bit build?
pub fn is_32(name: &str) -> bool {
    let n = name.to_lowercase();
    BITS_32.iter().any(|m| if m.starts_with('(') { n.contains(m) } else { has_word(&n, m) })
}

/// Why app `name` / `id` looks like clutter (None: it looks like an app).
/// `bases_64`: the base names (base_name) of the apps that are not 32-bit
/// builds, to find the 32-bit copies.
pub fn clutter(name: &str, id: &str, bases_64: &HashSet<String>) -> Option<Reason> {
    let n = name.to_lowercase();
    let i = id.to_lowercase();
    if UNINSTALL.iter().any(|w| if w.is_ascii() { has_word(&n, w) } else { n.contains(w) }) || i.contains("unins000") || i.contains("\\uninstall") {
        return Some(Reason::Uninstaller);
    }
    if DEV_TOOLS.iter().any(|w| n.contains(w)) {
        return Some(Reason::DeveloperTool);
    }
    // What the shortcut opens is the strong evidence: a document or a link.
    if DOC_FILES.iter().any(|e| i.ends_with(e)) {
        return Some(Reason::Documentation);
    }
    if is_link(id) {
        return Some(Reason::WebLink);
    }
    // The name alone only for what is not a program (a program is an app,
    // whatever it is called) nor a packaged app named Help ("Get Help").
    let program = i.ends_with(".exe");
    let packaged = id.contains('!');
    let said = |words: &[&str]| words.iter().any(|w| if w.is_ascii() { has_word(&n, w) } else { n.contains(w) });
    if !program && said(DOC_WORDS) && !(packaged && has_word(&n, "help")) {
        return Some(Reason::Documentation);
    }
    if !program && said(WEB_WORDS) {
        return Some(Reason::WebLink);
    }
    if is_32(name) && bases_64.contains(&base_name(name)) {
        return Some(Reason::Duplicate);
    }
    None
}

/// The base names of `apps` (id, name) that are not 32-bit builds.
pub fn bases_64(apps: &[(String, String)]) -> HashSet<String> {
    apps.iter().filter(|(_, n)| !is_32(n)).map(|(_, n)| base_name(n)).collect()
}

/// Every app in `apps` (id, name) that looks like clutter, and why.
pub fn reasons(apps: &[(String, String)]) -> HashMap<String, Reason> {
    let b = bases_64(apps);
    apps.iter().filter_map(|(id, n)| clutter(n, id, &b).map(|r| (id.clone(), r))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(i, n)| (i.to_string(), n.to_string())).collect()
    }

    /// The user's clutter (145 apps over 6 pages, at least 27 of these),
    /// and the apps next to them that must stay.
    #[test]
    fn the_users_clutter_is_found_and_their_apps_are_not() {
        let progs = "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}";
        let pf = "{6D809377-6AF0-444B-8957-A3773F02200E}";
        let lnk = |n: &str| format!("{progs}\\Folder\\{n}.lnk");
        let clutter_list: Vec<(String, &str, Reason)> = vec![
            (lnk("Uninstall Node.js"), "Uninstall Node.js", Reason::Uninstaller),
            (lnk("x"), "Uninstall Python 3.15", Reason::Uninstaller),
            (format!("{pf}\\Foo\\unins000.exe"), "Foo", Reason::Uninstaller),
            (lnk("x"), "카카오톡 제거", Reason::Uninstaller),
            ("https://nodejs.org/".into(), "Node.js website", Reason::WebLink),
            (lnk("x"), "Python 3.15 Manuals (64-bit)", Reason::Documentation),
            (lnk("x"), "Python 3.15 Module Docs (64-bit)", Reason::Documentation),
            (format!("{pf}\\Git\\ReleaseNotes.html"), "Git Release Notes", Reason::Documentation),
            (format!("{pf}\\Git\\git-faq.html"), "Git FAQs (Frequently Asked Questions)", Reason::Documentation),
            (lnk("x"), "Node.js documentation", Reason::Documentation),
            (format!("{pf}\\7-Zip\\7-zip.chm"), "7-Zip Help", Reason::Documentation),
            (lnk("x"), "Developer Command Prompt for VS 2022", Reason::DeveloperTool),
            (lnk("x"), "Developer PowerShell for VS 2022", Reason::DeveloperTool),
            (lnk("x"), "x64 Native Tools Command Prompt for VS 2022", Reason::DeveloperTool),
            (lnk("x"), "x86 Native Tools Command Prompt for VS 2022", Reason::DeveloperTool),
            (lnk("x"), "x64_x86 Cross Tools Command Prompt for VS 2022", Reason::DeveloperTool),
            (lnk("x"), "x86_x64 Cross Tools Command Prompt for VS 2022", Reason::DeveloperTool),
            (lnk("x"), "ARM64 Native Tools Command Prompt for VS 2022", Reason::DeveloperTool),
            (lnk("x"), "Developer Command Prompt for VS 2019", Reason::DeveloperTool),
            (lnk("x"), "Install Additional Tools for Node.js", Reason::DeveloperTool),
            ("{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}\\WindowsPowerShell\\v1.0\\powershell.exe".into(), "Windows PowerShell (x86)", Reason::Duplicate),
            ("{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}\\WindowsPowerShell\\v1.0\\powershell_ise.exe".into(), "Windows PowerShell ISE (x86)", Reason::Duplicate),
            (lnk("x"), "ODBC Data Sources (32-bit)", Reason::Duplicate),
            ("https://www.python.org/".into(), "Python Website", Reason::WebLink),
            (format!("{progs}\\Dell\\Dell Support.url"), "Dell Support", Reason::WebLink),
            (lnk("x"), "Blender Website", Reason::WebLink),
            (lnk("x"), "Register Product", Reason::WebLink),
        ];
        assert!(clutter_list.len() >= 27);
        let keep: Vec<(String, &str)> = vec![
            (format!("{pf}\\nodejs\\node.exe"), "Node.js"),
            ("C:\\Python315\\python.exe".into(), "Python 3.15 (64-bit)"),
            ("C:\\Python315\\Lib\\idlelib\\idle.pyw".into(), "IDLE (Python 3.15 64-bit)"),
            (format!("{pf}\\Git\\git-bash.exe"), "Git Bash"),
            ("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\cmd.exe".into(), "Command Prompt"),
            ("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe".into(), "Windows PowerShell"),
            ("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell_ise.exe".into(), "Windows PowerShell ISE"),
            ("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\odbcad32.exe".into(), "ODBC Data Sources (64-bit)"),
            ("Microsoft.VisualStudioCode".into(), "Visual Studio Code"),
            (format!("{pf}\\Microsoft Visual Studio\\Installer\\setup.exe"), "Visual Studio Installer"),
            ("Microsoft.GetHelp_8wekyb3d8bbwe!App".into(), "Get Help"),
            ("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App".into(), "Terminal"),
            ("C:\\Apps\\Steam (x86)\\steam.exe".into(), "Steam (x86)"),
            (format!("{pf}\\Dell\\SupportAssist.exe"), "Dell SupportAssist"),
            ("C:\\Tools\\WebStorm.exe".into(), "WebStorm"),
        ];
        let mut all: Vec<(String, String)> = clutter_list.iter().map(|(i, n, _)| (i.clone(), n.to_string())).collect();
        all.extend(keep.iter().map(|(i, n)| (i.clone(), n.to_string())));
        let b = bases_64(&all);
        for (id, name, why) in &clutter_list {
            assert_eq!(clutter(name, id, &b), Some(*why), "{name}");
        }
        for (id, name) in &keep {
            assert_eq!(clutter(name, id, &b), None, "{name} must stay");
        }
    }

    #[test]
    fn real_apps_with_doc_or_support_in_their_names_stay() {
        let none = HashSet::new();
        let pf = "{6D809377-6AF0-444B-8957-A3773F02200E}";
        for (name, id) in [
            ("Google Docs", "Chrome._crx_aohghmighlieiainnegkcijnfilokake".to_string()),
            ("Google Docs", format!("{pf}\\Google\\Chrome\\Application\\chrome_proxy.exe")),
            ("HP Support Assistant", "AD2F1837.HPSupportAssistant_v10z8vjag6ke6!App".to_string()),
            ("HP Support Assistant", "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\\HP\\HP Support Framework\\HPSF.exe".to_string()),
            ("Dell SupportAssist", format!("{pf}\\Dell\\SupportAssistAgent\\bin\\SupportAssistLauncher.exe")),
            ("Docker Desktop", format!("{pf}\\Docker\\Docker\\Docker Desktop.exe")),
            ("Microsoft Support and Recovery Assistant", format!("{pf}\\SaRA\\SaraSetup.exe")),
            ("Product Manual Viewer", format!("{pf}\\Viewer\\viewer.exe")),
        ] {
            assert_eq!(clutter(name, &id, &none), None, "{name} ({id})");
        }
        // The same words with a document or link behind them still count.
        assert_eq!(clutter("HP Support Center", "https://support.hp.com/", &none), Some(Reason::WebLink));
        assert_eq!(clutter("Foo Support Center", "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}\\Foo\\Foo Support Center.lnk", &none), Some(Reason::WebLink));
        assert_eq!(clutter("Foo Online Documentation", "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}\\Foo\\Docs.lnk", &none), Some(Reason::Documentation));
        assert_eq!(clutter("Foo Docs", "C:\\Foo\\docs\\index.html", &none), Some(Reason::Documentation), "by its file");
    }

    #[test]
    fn edge_cases() {
        let none = HashSet::new();
        // An app named like an uninstaller is fine either way, but not an error.
        let _ = clutter("Uninstaller Pro", "C:\\UP\\up.exe", &none);
        assert_eq!(clutter("Uninstaller Pro", "C:\\UP\\up.exe", &none), None, "the uninstaller app itself");
        // 32-bit with no 64-bit twin: it is the only one, it stays.
        assert_eq!(clutter("Old Tool (x86)", "C:\\x\\t.exe", &none), None);
        let b: HashSet<String> = ["old tool".to_string()].into_iter().collect();
        assert_eq!(clutter("Old Tool (x86)", "C:\\x\\t.exe", &b), Some(Reason::Duplicate));
        assert_eq!(base_name("Windows PowerShell (x86)"), "windows powershell");
        assert_eq!(base_name("ODBC Data Sources (64-bit)"), "odbc data sources");
        assert!(is_32("ODBC Data Sources (32-bit)") && !is_32("ODBC Data Sources (64-bit)") && !is_32("x64 Native Tools"));
        // A Korean help file.
        assert_eq!(clutter("한글 도움말", "C:\\x\\hwp.chm", &none), Some(Reason::Documentation));
        // Web links by their id alone.
        assert_eq!(clutter("Anything", "https://example.com/", &none), Some(Reason::WebLink));
        assert_eq!(clutter("Anything", "C:\\x\\Anything.url", &none), Some(Reason::WebLink));
        assert_eq!(clutter("Calculator", "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", &none), None);
    }

    #[test]
    fn uninstallers_links_and_documents_go_to_the_library() {
        let all = apps(&[
            ("u", "Uninstall Foo"),
            ("w", "Foo website"),
            ("d", "Foo Manual"),
            ("t", "Developer Command Prompt for VS 2022"),
            ("x86", "Foo (x86)"),
            ("x64", "Foo"),
            ("a", "Calculator"),
        ]);
        let r = reasons(&all);
        let moves: Vec<&str> = ["u", "w", "d", "t", "x86", "x64", "a"].into_iter().filter(|id| r.get(*id).is_some_and(|r| to_library(*r))).collect();
        assert_eq!(moves, ["u", "w", "d"], "dev tools, 32-bit copies and real apps stay");
        assert_eq!(r.get("t"), Some(&Reason::DeveloperTool));
        assert_eq!(r.get("x86"), Some(&Reason::Duplicate));
    }
}
