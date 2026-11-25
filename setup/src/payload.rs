//! The inner installer on disk: written to a fresh folder in %TEMP%, held
//! open read-only while it runs (nobody can swap, change or delete it),
//! checked against the embedded bytes, removed afterwards. Folders left by
//! a crash are swept a day later.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

/// Every folder this exe makes in %TEMP% starts with this.
pub const PREFIX: &str = "NopBarSetup-";
/// A folder of ours this old is a leftover (an install takes seconds; the
/// updater gives up on one after 10 minutes).
pub const STALE: Duration = Duration::from_secs(24 * 60 * 60);

/// The inner installer's file name (its log names it "Original Setup EXE").
pub fn inner_name(version: &str) -> String {
    format!("NopBar-{version}-inner.exe")
}

/// A fresh folder name: pid, time and a counter (tests run in parallel).
pub fn dir_name(pid: u32, nanos: u128) -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    format!("{PREFIX}{pid}-{nanos}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// A folder `name` last changed `age` ago is ours and left over.
pub fn is_stale(name: &str, age: Option<Duration>) -> bool {
    name.starts_with(PREFIX) && age.is_some_and(|a| a >= STALE)
}

/// Removes our leftover folders in `root` (as of `now`). Returns how many.
pub fn sweep_at(root: &Path, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else { return 0 };
    let mut n = 0;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(meta) = e.metadata() else { continue };
        let age = meta.modified().ok().and_then(|m| now.duration_since(m).ok());
        if meta.is_dir() && is_stale(&name, age) && std::fs::remove_dir_all(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

pub fn sweep(root: &Path) -> usize {
    sweep_at(root, SystemTime::now())
}

/// The written installer. Dropping it lets go of the file and removes the
/// folder (retrying while the installer's process lets go of it).
pub struct Extracted {
    pub dir: PathBuf,
    pub exe: PathBuf,
    lock: Option<std::fs::File>,
}

impl Drop for Extracted {
    fn drop(&mut self) {
        self.lock.take();
        for _ in 0..20 {
            match std::fs::remove_dir_all(&self.dir) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => std::thread::sleep(Duration::from_millis(150)),
                _ => return,
            }
        }
    }
}

/// Writes `bytes` as `root\NopBarSetup-...\<name>`, then holds it open
/// for reading only, sharing only reading (running it still works), and
/// checks through that handle that it is exactly `bytes`.
pub fn extract(bytes: &[u8], root: &Path, name: &str) -> std::io::Result<Extracted> {
    use std::io::{Error, ErrorKind};
    if bytes.is_empty() {
        return Err(Error::new(ErrorKind::NotFound, "this setup exe carries no installer"));
    }
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = root.join(dir_name(std::process::id(), nanos));
    std::fs::create_dir(&dir)?;
    let mut x = Extracted { exe: dir.join(name), dir, lock: None };
    {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&x.exe)?;
        f.write_all(bytes)?;
    }
    const FILE_SHARE_READ: u32 = 1;
    let mut lock = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&x.exe)?;
    let mut back = Vec::with_capacity(bytes.len());
    lock.read_to_end(&mut back)?;
    if back != bytes {
        return Err(Error::new(ErrorKind::InvalidData, "the written installer differs from the embedded one"));
    }
    x.lock = Some(lock);
    Ok(x)
}

/// The inner installer with `tail` after its name, byte for byte (no
/// quoting of ours), no console handles.
pub fn passthrough_command(exe: &Path, tail: &OsStr) -> Command {
    let mut c = Command::new(exe);
    if !tail.is_empty() {
        c.raw_arg(tail);
    }
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    c
}

/// The window's own install: the updater's switches (dock's
/// autoupdate::apply::setup_args), so a double-click install and an
/// automatic update run the installer the same way. No wizard, no message
/// boxes, no reboot, no Cancel; its log at `log`.
pub fn silent_args(log: &Path) -> Vec<OsString> {
    let mut log_arg = OsString::from("/LOG=");
    log_arg.push(log.as_os_str());
    ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", "/NOCANCEL"].into_iter().map(OsString::from).chain([log_arg]).collect()
}

pub fn silent_command(exe: &Path, log: &Path) -> Command {
    let mut c = Command::new(exe);
    c.args(silent_args(log)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    c
}

/// Passthrough mode (cli::Mode::Passthrough): runs the inner installer
/// with `tail`, waits, returns its exit code. When it cannot run, 1 (Inno
/// Setup's "failed to initialize") and a line in its `/LOG=` file.
pub fn passthrough(bytes: &[u8], version: &str, tail: &OsStr, log: Option<&Path>, root: &Path) -> i32 {
    sweep(root);
    let x = match extract(bytes, root, &inner_name(version)) {
        Ok(x) => x,
        Err(e) => {
            note(log, &format!("the installer could not be written to {}: {e}", root.display()));
            return 1;
        }
    };
    let code = match passthrough_command(&x.exe, tail).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            note(log, &format!("the installer did not start: {e}"));
            1
        }
    };
    drop(x);
    code
}

/// Appends a line to the installer's log (the updater keeps it).
fn note(log: Option<&Path>, msg: &str) {
    if let Some(p) = log {
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(p) {
            let _ = writeln!(f, "NopBar setup: {msg}");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh scratch folder under %TEMP%, removed on drop.
    pub struct Scratch(pub PathBuf);
    impl Scratch {
        pub fn new(name: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!("nopbar-setup-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
        pub fn entries(&self) -> Vec<String> {
            std::fs::read_dir(&self.0).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// cmd.exe stands in for the inner installer: it can report its own
    /// command line (%CMDCMDLINE%) and exit with any code.
    pub fn cmd_exe() -> Vec<u8> {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::fs::read(Path::new(&sys).join(r"System32\cmd.exe")).unwrap()
    }

    #[test]
    fn names() {
        assert_eq!(inner_name("0.1.41"), "NopBar-0.1.41-inner.exe");
        let a = dir_name(12, 34);
        assert!(a.starts_with("NopBarSetup-12-34-"));
        assert_ne!(a, dir_name(12, 34), "unique in one process");
    }

    #[test]
    fn only_our_old_folders_are_stale() {
        let day = Duration::from_secs(24 * 3600);
        assert!(is_stale("NopBarSetup-1-2-3", Some(day)));
        assert!(!is_stale("NopBarSetup-1-2-3", Some(day - Duration::from_secs(1))), "maybe still running");
        assert!(!is_stale("NopBarSetup-1-2-3", None), "unknown age");
        assert!(!is_stale("is-ABCDE.tmp", Some(day * 30)), "not ours");
        assert!(!is_stale("nopbarsetup-1", Some(day * 30)), "case matters: only names we make");
    }

    #[test]
    fn sweeping_removes_only_our_leftovers() {
        let s = Scratch::new("sweep");
        std::fs::create_dir(s.0.join("NopBarSetup-1-2-3")).unwrap();
        std::fs::write(s.0.join("NopBarSetup-1-2-3").join("x.exe"), b"x").unwrap();
        std::fs::create_dir(s.0.join("someone-else")).unwrap();
        std::fs::write(s.0.join("NopBarSetup-file"), b"a file, not a folder").unwrap();
        assert_eq!(sweep(&s.0), 0, "fresh folders stay");
        assert_eq!(sweep_at(&s.0, SystemTime::now() + STALE + Duration::from_secs(60)), 1);
        let mut left = s.entries();
        left.sort();
        assert_eq!(left, ["NopBarSetup-file", "someone-else"]);
    }

    #[test]
    fn extracted_files_are_locked_checked_and_cleaned_up() {
        let s = Scratch::new("extract");
        assert!(extract(b"", &s.0, "x.exe").is_err(), "no payload");
        assert!(s.entries().is_empty());
        let x = extract(b"MZ fake installer", &s.0, "NopBar-9.9.9-inner.exe").unwrap();
        assert_eq!(std::fs::read(&x.exe).unwrap(), b"MZ fake installer", "others may read it");
        assert!(std::fs::OpenOptions::new().write(true).open(&x.exe).is_err(), "nobody may change it");
        assert!(std::fs::remove_file(&x.exe).is_err(), "or delete it");
        assert!(std::fs::rename(&x.exe, x.dir.join("y.exe")).is_err(), "or swap it");
        assert!(x.dir.starts_with(&s.0) && x.dir.file_name().unwrap().to_string_lossy().starts_with(PREFIX));
        let dir = x.dir.clone();
        drop(x);
        assert!(!dir.exists(), "removed after the run");
        assert!(s.entries().is_empty());
    }

    #[test]
    fn the_silent_switches_are_the_updaters() {
        let a = silent_args(Path::new(r"C:\Users\a b\AppData\Local\Temp\NopBarSetup-1\setup.log"));
        assert_eq!(a, ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", "/NOCANCEL", r"/LOG=C:\Users\a b\AppData\Local\Temp\NopBarSetup-1\setup.log"]);
    }

    #[test]
    fn passthrough_returns_the_inner_exit_code_and_cleans_up() {
        let s = Scratch::new("passthrough");
        let cmd = cmd_exe();
        for code in [0, 1, 2, 5, 7, 8, 3010] {
            let tail = format!("/d /c exit {code}");
            assert_eq!(passthrough(&cmd, "9.9.9", OsStr::new(&tail), None, &s.0), code);
        }
        assert!(s.entries().is_empty(), "no folder left behind: {:?}", s.entries());
    }

    #[test]
    fn passthrough_without_a_payload_fails_like_inno_and_says_so_in_the_log() {
        let s = Scratch::new("nopayload");
        let log = s.0.join("setup.log");
        std::fs::write(&log, "earlier line\r\n").unwrap();
        assert_eq!(passthrough(b"", "9.9.9", OsStr::new("/VERYSILENT"), Some(&log), &s.0), 1);
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("earlier line\r\n") && text.contains("NopBar setup: the installer could not be written"), "{text}");
    }

    #[test]
    fn the_inner_installer_gets_the_command_line_tail_byte_for_byte() {
        let s = Scratch::new("cmdline");
        let x = extract(&cmd_exe(), &s.0, "NopBar-9.9.9-inner.exe").unwrap();
        // cmd /u echoes in UTF-16 (the temp path may hold Hangul).
        let switches = r#"/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL "/LOG=C:\Users\a b\x.log"  /DIR="C:\x y" /TASKS=a,b"#;
        let tail = format!("/d /u /c echo(%CMDCMDLINE% {switches}");
        let mut c = passthrough_command(&x.exe, OsStr::new(&tail));
        c.stdout(Stdio::piped());
        let out = c.output().unwrap();
        assert!(out.status.success());
        let units: Vec<u16> = out.stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        let echoed = String::from_utf16(&units).unwrap();
        let echoed = echoed.trim_end_matches(['\r', '\n']);
        // %CMDCMDLINE% is the child's whole command line.
        let line = format!("\"{}\" {tail}", x.exe.display());
        assert_eq!(echoed, format!("{line} {switches}"));
    }
}
