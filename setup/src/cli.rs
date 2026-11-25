//! The command line: window or passthrough.
//!
//! The setup exe has no switches of its own. Every command line with an
//! argument belongs to the Inno Setup installer it carries: the updater of
//! installed NopBar 0.1.37+ (`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-
//! /NOCANCEL /LOG=...`, dock/src/autoupdate/apply.rs `setup_args`), winget
//! (`/SP- /VERYSILENT ...`), admins (`/DIR=`, `/TASKS=`, `/LANG=`, `/?`).
//! Those runs show nothing of ours: the installer gets the very same
//! command line tail, byte for byte, and its exit code is ours.

use std::ffi::{OsStr, OsString};

/// What a start does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No arguments (a double click): the drag-to-install window.
    Window,
    /// Any argument: run the inner installer with the same command line,
    /// no window of ours, its exit code.
    Passthrough,
}

/// The mode for the arguments after the program name (blank ones, e.g. an
/// empty `""` a launcher added, do not count).
pub fn mode<I, S>(args_after_exe: I) -> Mode
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    if args_after_exe.into_iter().any(|a| !a.as_ref().to_string_lossy().trim().is_empty()) {
        Mode::Passthrough
    } else {
        Mode::Window
    }
}

/// The command line after the program name, exactly as given: the program
/// name ends at its closing quote when it starts with one, else at the
/// first space or tab (CommandLineToArgvW's rule for argv[0]); the blanks
/// after it are dropped. What is left goes to the inner installer as is.
pub fn tail(cmdline: &[u16]) -> &[u16] {
    const QUOTE: u16 = b'"' as u16;
    let blank = |c: u16| c == b' ' as u16 || c == b'\t' as u16;
    let mut i = 0;
    if cmdline.first() == Some(&QUOTE) {
        i = 1;
        while i < cmdline.len() && cmdline[i] != QUOTE {
            i += 1;
        }
        i = (i + 1).min(cmdline.len());
    } else {
        while i < cmdline.len() && !blank(cmdline[i]) {
            i += 1;
        }
    }
    while i < cmdline.len() && blank(cmdline[i]) {
        i += 1;
    }
    &cmdline[i..]
}

/// [`tail`] of this process's command line.
pub fn own_tail() -> OsString {
    use std::os::windows::ffi::OsStringExt;
    unsafe {
        let p = windows::Win32::System::Environment::GetCommandLineW();
        OsString::from_wide(tail(p.as_wide()))
    }
}

/// The installer's log file from its `/LOG=` switch (case-insensitive,
/// quotes dropped; the last one wins, as in Inno Setup). `/LOG` without a
/// file (Inno picks one in %TEMP%) is None.
pub fn log_path<I, S>(args_after_exe: I) -> Option<std::path::PathBuf>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args_after_exe
        .into_iter()
        .filter_map(|a| {
            let a = a.as_ref().to_string_lossy().replace('"', "");
            let head = a.get(..5)?;
            (head.eq_ignore_ascii_case("/LOG=") && a.len() > 5).then(|| std::path::PathBuf::from(&a[5..]))
        })
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn tail_of(s: &str) -> String {
        String::from_utf16(tail(&w(s))).unwrap()
    }

    /// The switches Inno Setup documents ("Setup Command Line Parameters").
    const INNO_SWITCHES: &[&str] = &[
        "/VERYSILENT", "/SILENT", "/SUPPRESSMSGBOXES", "/LOG", r"/LOG=C:\x\setup.log", "/NOCANCEL", "/NORESTART", "/RESTARTEXITCODE=3010",
        "/CLOSEAPPLICATIONS", "/NOCLOSEAPPLICATIONS", "/FORCECLOSEAPPLICATIONS", "/NOFORCECLOSEAPPLICATIONS", "/LOGCLOSEAPPLICATIONS",
        "/RESTARTAPPLICATIONS", "/NORESTARTAPPLICATIONS", "/LOADINF=a.inf", "/SAVEINF=a.inf", "/LANG=ko", r"/DIR=C:\x", "/GROUP=x",
        "/NOICONS", "/TYPE=full", "/COMPONENTS=a", "/TASKS=a", "/MERGETASKS=a", "/PASSWORD=x", "/SP-", "/ALLUSERS", "/CURRENTUSER",
        "/HELP", "/?", "/silent", "-VERYSILENT",
    ];

    #[test]
    fn no_arguments_is_the_window() {
        assert_eq!(mode(Vec::<OsString>::new()), Mode::Window);
        assert_eq!(mode([""]), Mode::Window, "a blank argument is not a switch");
        assert_eq!(mode(["  ", "\t"]), Mode::Window);
    }

    #[test]
    fn every_inno_switch_passes_through() {
        for s in INNO_SWITCHES {
            assert_eq!(mode([*s]), Mode::Passthrough, "{s}");
        }
        assert_eq!(mode(["", "/SP-"]), Mode::Passthrough);
        // Anything else too: Inno Setup decides what it means.
        assert_eq!(mode(["whatever"]), Mode::Passthrough);
    }

    #[test]
    fn the_updaters_command_line_passes_through() {
        // What dock's apply::setup_args gives Command (std quotes the arg
        // with spaces), and the program name std always quotes.
        let line = r#""C:\Users\u\AppData\Local\NopBar\updates\0.2.0\NopBar-0.2.0-setup.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL "/LOG=C:\Users\a b\AppData\Local\NopBar\updates\0.2.0\NopBar-0.2.0-setup.log""#;
        assert_eq!(tail_of(line), r#"/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL "/LOG=C:\Users\a b\AppData\Local\NopBar\updates\0.2.0\NopBar-0.2.0-setup.log""#);
    }

    #[test]
    fn the_tail_is_kept_byte_for_byte() {
        assert_eq!(tail_of(r#"setup.exe /SP-  /LOG="C:\a b\x.log"   /DIR=x "#), r#"/SP-  /LOG="C:\a b\x.log"   /DIR=x "#);
        assert_eq!(tail_of("setup.exe\t/SILENT"), "/SILENT");
        assert_eq!(tail_of(r#""C:\a b\setup.exe""#), "");
        assert_eq!(tail_of("setup.exe"), "");
        assert_eq!(tail_of(r#""C:\a b\setup.exe"/VERYSILENT"#), "/VERYSILENT", "no blank after the closing quote");
        assert_eq!(tail_of(r#""C:\unterminated /SP-"#), "");
        assert_eq!(tail_of(""), "");
        assert_eq!(tail_of(r#""C:\짧은 길\NopBar-1.0.0-setup.exe" /LANG=ko"#), "/LANG=ko");
    }

    #[test]
    fn the_tail_parses_to_the_same_arguments() {
        // Whatever the inner installer gets after its own name parses (by
        // the CommandLineToArgvW rules) to the arguments the outer got.
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        let argv = |line: &str| -> Vec<String> {
            let wide: Vec<u16> = line.encode_utf16().chain([0]).collect();
            let mut n = 0;
            unsafe {
                let p = CommandLineToArgvW(windows::core::PCWSTR(wide.as_ptr()), &mut n);
                let v = (0..n as usize).map(|i| (*p.add(i)).to_string().unwrap()).collect();
                let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(p as _)));
                v
            }
        };
        let outer = r#""C:\Users\u\Downloads\NopBar-0.2.0-setup.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL "/LOG=C:\Users\a b\x.log" /DIR="C:\x y""#;
        let inner = format!(r#""C:\Users\u\AppData\Local\Temp\NopBarSetup-1-2\NopBar-0.2.0-inner.exe" {}"#, tail_of(outer));
        assert_eq!(argv(outer)[1..], argv(&inner)[1..]);
        assert_eq!(argv(&inner)[6], r"/LOG=C:\Users\a b\x.log");
    }

    #[test]
    fn the_log_switch() {
        assert_eq!(log_path(["/VERYSILENT", r"/LOG=C:\a b\x.log"]), Some(r"C:\a b\x.log".into()));
        assert_eq!(log_path([r#"/log="C:\a b\x.log""#]), Some(r"C:\a b\x.log".into()));
        assert_eq!(log_path([r"/LOG=C:\1.log", r"/LOG=C:\2.log"]), Some(r"C:\2.log".into()));
        assert_eq!(log_path(["/LOG"]), None);
        assert_eq!(log_path(["/LOG="]), None);
        assert_eq!(log_path(["/LOGCLOSEAPPLICATIONS"]), None);
        assert_eq!(log_path(["/SP-"]), None);
    }
}
