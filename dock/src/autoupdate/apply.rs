//! The updater, two ways:
//! - zip (portable copies): the NEW nopbar.exe, run from the staged folder
//!   as `nopbar.exe --apply-update <staged> <install> <dock pid> <guardian pid> <from version>`;
//! - installer (installed copies): a copy of the CURRENT nopbar.exe in
//!   `updates\<v>\updater\` (outside the install folder, which the
//!   installer replaces), run as `nopbar.exe --apply-setup <setup.exe>
//!   <sha256> <install> <dock pid> <guardian pid> <from> <to>`; see
//!   [`run_setup`]. The zip steps follow.
//!
//! Why a copy of the new exe and not the guardian: the guardian runs from
//! the installed nopbar.exe (it is the file being replaced) and is the old
//! code; the staged copy lives outside the install folder, so it can
//! replace every installed file, and it is what will run next anyway. The
//! guardian keeps its job: it restores the taskbar when the old dock quits,
//! and the new dock gets a fresh guardian of its own.
//!
//! Steps (docs/updates.md, "Automatic updates"):
//! 1. checks: it runs from `<staged>`, its own version is newer than
//!    `<from>`, the install folder holds a plain nopbar.exe, the dock pid
//!    really is that exe;
//! 2. asks the dock to quit through the settings pipe (the normal quit: it
//!    gives the taskbar back first). Not gone in time: nothing is changed,
//!    the failure is recorded and shown;
//! 3. waits for the old guardian (it restores the taskbar after the dock
//!    is gone), ends it if it is still there after a minute, and restores
//!    the taskbar itself if it is still marked hidden;
//! 4. copies the installed files to `%LOCALAPPDATA%\NopBar\updates\backup`,
//!    copies the staged files next to the installed ones, moves each
//!    installed file aside (a running exe can be renamed) and the new one
//!    into place; any error undoes what was done and starts the old dock;
//! 5. starts the new dock and watches it for [`WATCH`]: it must show its
//!    window and answer; a crash, an early exit or no answer rolls back:
//!    every process started from the new exe is ended, the taskbar is
//!    restored, the old files go back and the old dock is started;
//! 6. records the outcome in update.json and shows a notice.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long the new dock must run (window up and answering) to be kept.
pub const WATCH: Duration = Duration::from_secs(20);
/// How long the old dock gets to quit.
const QUIT_WAIT: Duration = Duration::from_secs(30);
/// How long the old guardian gets to restore the taskbar and exit.
const GUARDIAN_WAIT: Duration = Duration::from_secs(60);

/// Every file an update may carry (the portable zip = the installer's files).
pub const FILES: &[&str] = crate::release::INSTALLED_FILES;

// ---------------------------------------------------------------- plan

/// One step of putting the new files in place.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Op {
    /// Copy the installed file into the backup folder.
    Backup { from: PathBuf, to: PathBuf },
    /// Copy the staged file next to the installed one (`<name>.<pid>.nopbar-new`).
    CopyNew { from: PathBuf, to: PathBuf },
    /// Move the installed file aside (`<name>.<pid>.nopbar-old`); allowed
    /// while it runs.
    MoveAside { from: PathBuf, to: PathBuf },
    /// Move the new file into place.
    Place { from: PathBuf, to: PathBuf },
}

pub fn aside_name(file: &str, tag: u32) -> String {
    format!("{file}.{tag}.nopbar-old")
}

fn new_name(file: &str, tag: u32) -> String {
    format!("{file}.{tag}.nopbar-new")
}

/// The steps that replace `files` (staged) in `install`, in order: every
/// backup and every copy first (a full disk or a missing file stops it
/// before an installed file is touched), then file by file: move aside,
/// move in. `installed(name)` says whether the install folder has it now.
/// `tag` keeps the temporary names apart from an earlier attempt's.
pub fn plan(staged: &Path, install: &Path, backup: &Path, files: &[&str], installed: &dyn Fn(&str) -> bool, tag: u32) -> Vec<Op> {
    let mut ops = Vec::new();
    for f in files.iter().filter(|f| installed(f)) {
        ops.push(Op::Backup { from: install.join(f), to: backup.join(f) });
    }
    for f in files {
        ops.push(Op::CopyNew { from: staged.join(f), to: install.join(new_name(f, tag)) });
    }
    for f in files {
        if installed(f) {
            ops.push(Op::MoveAside { from: install.join(f), to: install.join(aside_name(f, tag)) });
        }
        ops.push(Op::Place { from: install.join(new_name(f, tag)), to: install.join(f) });
    }
    ops
}

fn run_op(op: &Op) -> std::io::Result<()> {
    match op {
        Op::Backup { from, to } | Op::CopyNew { from, to } => std::fs::copy(from, to).map(|_| ()),
        Op::MoveAside { from, to } | Op::Place { from, to } => std::fs::rename(from, to),
    }
}

/// Undoes `done` (the steps that went through), newest first: a placed
/// file is moved back to its temporary name, an aside file moved back, a
/// copy deleted. Backups stay. Returns what could not be undone.
pub fn undo(done: &[Op]) -> Vec<String> {
    let mut problems = Vec::new();
    for op in done.iter().rev() {
        let r = match op {
            Op::Backup { .. } => Ok(()),
            Op::CopyNew { to, .. } => std::fs::remove_file(to),
            Op::MoveAside { from, to } => std::fs::rename(to, from),
            Op::Place { from, to } => std::fs::rename(to, from),
        };
        if let Err(e) = r {
            problems.push(format!("undo {op:?}: {e}"));
        }
    }
    problems
}

/// Runs `ops`. On the first error everything done is undone; Err carries
/// the error and whatever the undo could not put back.
pub fn apply(ops: &[Op]) -> Result<(), (String, Vec<String>)> {
    let mut done = Vec::new();
    for op in ops {
        if let Err(e) = run_op(op) {
            let problems = undo(&done);
            return Err((format!("{op:?}: {e}"), problems));
        }
        done.push(op.clone());
    }
    Ok(())
}

/// After a good start: the files moved aside go (they may still be in use
/// by an old settings window; then they stay until the next update or
/// start cleans them, see [`stale_temp_files`]).
pub fn commit(ops: &[Op]) {
    for op in ops {
        if let Op::MoveAside { to, .. } = op {
            if std::fs::remove_file(to).is_err() {
                crate::log!("update: {} still in use; removed later", to.display());
            }
        }
    }
}

/// Puts the old files back after the new dock failed (`ops` all went
/// through): the new file is deleted and the one moved aside moved back;
/// if that file is gone, the backup copy is used. Files the update added
/// are removed. Returns the problems.
pub fn roll_back(ops: &[Op], backup: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for op in ops.iter().rev() {
        let Op::Place { to: target, .. } = op else { continue };
        let name = target.file_name().map(|n| n.to_owned()).unwrap_or_default();
        let aside = ops.iter().find_map(|o| match o {
            Op::MoveAside { from, to } if from == target => Some(to.clone()),
            _ => None,
        });
        if let Err(e) = std::fs::remove_file(target) {
            if e.kind() != std::io::ErrorKind::NotFound {
                // In use (a process we could not end): move it out of the way.
                let away = target.with_extension(format!("failed-{}", std::process::id()));
                if let Err(e2) = std::fs::rename(target, &away) {
                    problems.push(format!("{}: {e}; {e2}", target.display()));
                    continue;
                }
            }
        }
        match aside {
            Some(aside) if aside.is_file() => {
                if let Err(e) = std::fs::rename(&aside, target) {
                    problems.push(format!("{} back: {e}", aside.display()));
                }
            }
            Some(_) => {
                // Moved aside but gone: the backup.
                if let Err(e) = std::fs::copy(backup.join(&name), target) {
                    problems.push(format!("{} from the backup: {e}", target.display()));
                }
            }
            // The update added it: deleted above.
            None => {}
        }
    }
    problems
}

/// Temporary files an earlier update left in the install folder (aside
/// copies still in use then, half-made copies): `<allowed name>.<digits>.nopbar-old|new`.
pub fn stale_temp_files(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            let Some(rest) = lower.strip_suffix(".nopbar-old").or_else(|| lower.strip_suffix(".nopbar-new")) else { return false };
            let Some((file, tag)) = rest.rsplit_once('.') else { return false };
            !tag.is_empty() && tag.bytes().all(|b| b.is_ascii_digit()) && FILES.iter().any(|f| f.eq_ignore_ascii_case(file))
        })
        .cloned()
        .collect()
}

/// Removes [`stale_temp_files`] from `install` (best effort; plain files only).
pub fn clean_install_dir(install: &Path) {
    let names: Vec<String> = std::fs::read_dir(install).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    for n in stale_temp_files(&names) {
        let p = install.join(&n);
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

// ---------------------------------------------------------------- watch

/// What the updater saw of the new dock so far.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Seen {
    pub elapsed: Duration,
    /// Its exit code, once it has exited.
    pub exit: Option<u32>,
    /// Its window answered a ping at least once.
    pub answered: bool,
    /// The last ping was answered.
    pub answering: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Wait,
    Keep,
    RollBack(&'static str),
}

/// Keep the new version, roll back, or watch on.
pub fn verdict(s: &Seen) -> Verdict {
    if let Some(code) = s.exit {
        return if crate::guardian::is_crash(code) {
            Verdict::RollBack("the new version crashed")
        } else if !s.answered {
            Verdict::RollBack("the new version quit before its window showed")
        } else {
            // It ran and answered; then the user quit it.
            Verdict::Keep
        };
    }
    if s.elapsed < WATCH {
        return Verdict::Wait;
    }
    match (s.answered, s.answering) {
        (true, true) => Verdict::Keep,
        (false, _) => Verdict::RollBack("the new version showed no window"),
        (true, false) => Verdict::RollBack("the new version stopped responding"),
    }
}

// ---------------------------------------------------------------- arguments

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    pub staged: PathBuf,
    pub install: PathBuf,
    pub dock_pid: u32,
    pub guardian_pid: u32,
    pub from: String,
}

pub fn parse_args(args: &[String]) -> Option<Args> {
    if args.get(1).map(|a| a.eq_ignore_ascii_case("--apply-update")) != Some(true) || args.len() != 7 {
        return None;
    }
    let from = args[6].clone();
    crate::updates::Version::parse(&from)?;
    Some(Args { staged: PathBuf::from(&args[2]), install: PathBuf::from(&args[3]), dock_pid: args[4].parse().ok()?, guardian_pid: args[5].parse().ok()?, from })
}

/// The command line the dock starts the updater with.
pub fn command_args(a: &Args) -> Vec<String> {
    vec!["--apply-update".into(), a.staged.display().to_string(), a.install.display().to_string(), a.dock_pid.to_string(), a.guardian_pid.to_string(), a.from.clone()]
}

/// Lowercase, backslashes, no trailing separator: for comparing folders.
pub fn norm(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// The checks before anything is done. `me` = this exe, `current` = this
/// build's version.
pub fn check(a: &Args, me: &Path, current: &str) -> Result<(), String> {
    if me.parent().map(norm) != Some(norm(&a.staged)) {
        return Err(format!("the updater must run from {} (runs from {})", a.staged.display(), me.display()));
    }
    if norm(&a.staged) == norm(&a.install) || norm(&a.staged).starts_with(&format!("{}\\", norm(&a.install))) {
        return Err("the staged folder is inside the install folder".into());
    }
    if !crate::updates::is_newer(current, &a.from) {
        return Err(format!("{current} is not newer than {}: no downgrade", a.from));
    }
    if !a.install.is_absolute() || !a.staged.is_absolute() {
        return Err("folders must be absolute".into());
    }
    if !a.staged.join("nopbar.exe").is_file() {
        return Err("no nopbar.exe in the staged folder".into());
    }
    let exe = a.install.join("nopbar.exe");
    match std::fs::symlink_metadata(&exe) {
        Ok(m) if m.is_file() => Ok(()),
        _ => Err(format!("{} is not a plain file", exe.display())),
    }
}

// ---------------------------------------------------------------- processes

/// Processes running `exe` that started at or after `since` (FILETIME;
/// 0 = any), not this one.
fn processes_of(exe: &Path, since: u64) -> Vec<u32> {
    processes_since(exe, since).into_iter().map(|(pid, _)| pid).collect()
}

/// [`processes_of`] with each one's start time.
fn processes_since(exe: &Path, since: u64) -> Vec<(u32, u64)> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let want = norm(exe);
    let me = std::process::id();
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let pid = e.th32ProcessID;
            if pid != me && image_of(pid).is_some_and(|p| norm(&p) == want) {
                if let Some(t) = crate::apps::process_started(pid).filter(|t| *t >= since) {
                    out.push((pid, t));
                }
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    out
}

fn image_of(pid: u32) -> Option<PathBuf> {
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(h);
        ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

/// A process handle closed on drop.
struct Proc(windows::Win32::Foundation::HANDLE);

impl Drop for Proc {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

impl Proc {
    fn open(pid: u32) -> Option<Proc> {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
        if pid == 0 {
            return None;
        }
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, pid).ok().map(Proc) }
    }

    /// True once it has exited (waits up to `d`).
    fn wait(&self, d: Duration) -> bool {
        use windows::Win32::System::Threading::WaitForSingleObject;
        unsafe { WaitForSingleObject(self.0, d.as_millis().min(u32::MAX as u128 - 1) as u32) == windows::Win32::Foundation::WAIT_OBJECT_0 }
    }

    fn exit_code(&self) -> u32 {
        let mut code = 0u32;
        unsafe {
            let _ = windows::Win32::System::Threading::GetExitCodeProcess(self.0, &mut code);
        }
        code
    }

    fn kill(&self) {
        unsafe {
            let _ = windows::Win32::System::Threading::TerminateProcess(self.0, 1);
        }
        self.wait(Duration::from_secs(5));
    }
}

/// Starts `exe` (in its folder); the process and its pid.
fn start(exe: &Path) -> Result<(Proc, u32), String> {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW};
    let cmd = format!("\"{}\"", exe.display());
    let mut wide: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
    let dir = windows::core::HSTRING::from(exe.parent().unwrap_or(exe).as_os_str());
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(None, Some(PWSTR(wide.as_mut_ptr())), None, None, false, PROCESS_CREATION_FLAGS(0), None, &dir, &si, &mut pi).map_err(|e| format!("start {}: {e}", exe.display()))?;
        let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
    }
    Ok((Proc(pi.hProcess), pi.dwProcessId))
}

/// Posts the dock's own quit message to every dock window of `pid` (the
/// per-display views ignore it; the main one quits).
fn post_quit(pid: u32) {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId, PostMessageW};
    let mut after: Option<HWND> = None;
    unsafe {
        while let Ok(h) = FindWindowExW(None, after, w!("NopBarWindow"), PCWSTR::null()) {
            let mut owner = 0u32;
            GetWindowThreadProcessId(h, Some(&mut owner));
            if owner == pid {
                let _ = PostMessageW(Some(h), crate::dock::WM_QUIT_REQUEST, WPARAM(0), LPARAM(0));
            }
            after = Some(h);
        }
    }
}

/// The new dock's window answered a ping (2 s).
fn answers(pid: u32) -> bool {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_NULL};
    let Some(win) = crate::guardian::dock_window(pid) else { return false };
    let mut r = 0usize;
    unsafe { SendMessageTimeoutW(win, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, 2_000, Some(&mut r)).0 != 0 }
}

fn now_filetime() -> u64 {
    unsafe {
        let ft = windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime();
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }
}

// ---------------------------------------------------------------- run

/// The backup folder (%LOCALAPPDATA%\NopBar\updates\backup).
pub fn backup_dir() -> Option<PathBuf> {
    super::updates_dir().map(|d| d.join("backup"))
}

/// Records the outcome in update.json. A version that was installed and
/// failed to run (`bad`) is never installed automatically again; any other
/// failure is tried again later (crate::autoupdate::next).
fn record(version: &str, outcome: Result<(), String>, bad: bool) {
    let _ = crate::updates::transact(|st| match &outcome {
        Ok(()) => {
            st.pending = Default::default();
            st.installed = version.to_string();
        }
        Err(e) => {
            st.pending.phase = crate::updates::Phase::Failed;
            st.pending.error = e.chars().take(300).collect();
            st.pending.since = crate::updates::unix_now();
            if bad {
                st.bad = version.to_string();
            }
        }
    });
}

fn say(key_title: &str, key_body: &str, vars: &[(&str, &str)]) {
    use crate::i18n::tf;
    crate::updates::notice_now(&tf(key_title, vars), &tf(key_body, vars));
}

/// `--apply-update ...`: returns the process exit code.
pub fn run(raw: &[String]) -> i32 {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    crate::i18n::init(&crate::config::read_current().map(|c| c.language).unwrap_or_default());
    let to = crate::updates::CURRENT;
    let Some(a) = parse_args(raw) else {
        crate::log!("update: bad --apply-update arguments {raw:?}");
        return 2;
    };
    // One updater at a time.
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\NopBar.Updater")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        crate::log!("update: another updater is running");
        return 3;
    }
    crate::log!("update: updater {to} (from {}) for {}", a.from, a.install.display());
    let me = std::env::current_exe().unwrap_or_default();
    if let Err(e) = check(&a, &me, to) {
        crate::log!("update: not applied: {e}");
        record(to, Err(e), false);
        return 4;
    }
    let install_exe = a.install.join("nopbar.exe");

    // 2. and 3.: the dock quits, its guardian restores the taskbar.
    if let Err(code) = stop_old(to, &install_exe, a.dock_pid, a.guardian_pid) {
        return code;
    }

    // 4. Back up and replace.
    let Some(backup) = backup_dir() else {
        record(to, Err("no %LOCALAPPDATA%".into()), false);
        start_old(&install_exe);
        return 6;
    };
    let _ = std::fs::remove_dir_all(&backup);
    if let Err(e) = std::fs::create_dir_all(&backup) {
        crate::log!("update: backup folder: {e}");
        record(to, Err(format!("backup folder: {e}")), false);
        start_old(&install_exe);
        say("update.failed.title", "update.failed.files", &[("v", to)]);
        return 6;
    }
    clean_install_dir(&a.install);
    let files: Vec<&str> = FILES.iter().copied().filter(|f| a.staged.join(f).is_file()).collect();
    // The staged files cannot change from here on (this exe itself is
    // mapped and cannot be written while it runs).
    let _held: Vec<super::trust::Locked> = files.iter().filter(|f| **f != "nopbar.exe").filter_map(|f| super::trust::Locked::open(&a.staged.join(f)).ok()).collect();
    let install = a.install.clone();
    let ops = plan(&a.staged, &a.install, &backup, &files, &|f| install.join(f).is_file(), std::process::id());
    // A crash from here on is undone at the next sign-in (or by the dock).
    let mut journal = Journal { kind: "zip".into(), phase: "swapping".into(), install: a.install.clone(), backup: backup.clone(), ops: ops.clone(), from: a.from.clone(), to: to.to_string(), updater: me.clone() };
    if let Err(e) = journal.begin() {
        crate::log!("update: no journal ({e}); not applied");
        record(to, Err(format!("journal: {e}")), false);
        start_old(&install_exe);
        return 6;
    }
    if let Err((e, problems)) = apply(&ops) {
        crate::log!("update: replacing files failed: {e}; undo problems: {problems:?}");
        record(to, Err(format!("replacing files: {e}")), false);
        if !problems.is_empty() {
            // Whatever is in place now, the backup has the old files.
            let _ = roll_back(&ops, &backup);
        }
        Journal::end();
        start_old(&install_exe);
        say("update.failed.title", "update.failed.files", &[("v", to)]);
        return 6;
    }
    journal.phase = "watching".into();
    let _ = journal.save();
    crate::log!("update: files replaced ({files:?}); starting {to}");

    // 5. Start the new dock and watch it.
    let since = now_filetime();
    let started = Instant::now();
    let outcome = match start(&install_exe) {
        Err(e) => {
            crate::log!("update: {e}");
            Verdict::RollBack("the new version did not start")
        }
        Ok((p, pid)) => watch(&p, pid, started),
    };
    match outcome {
        Verdict::Keep | Verdict::Wait => {
            commit(&ops);
            Journal::end();
            record(to, Ok(()), false);
            crate::release::set_display_version(&a.install, to);
            crate::log!("update: now running {to}");
            0
        }
        Verdict::RollBack(why) => {
            crate::log!("update: rolling back: {why}");
            end_new(&install_exe, since);
            let problems = roll_back(&ops, &backup);
            if !problems.is_empty() {
                crate::log!("update: roll back problems: {problems:?}");
            }
            Journal::end();
            record(to, Err(why.to_string()), true);
            start_old(&install_exe);
            say("update.failed.title", "update.failed.rolled_back", &[("v", to)]);
            7
        }
    }
}

/// Steps 2 and 3 of either updater: the dock quits the normal way (taskbar
/// back first), its guardian restores the taskbar and exits, the taskbar
/// is checked. Err(exit code) when the dock would not go (nothing changed;
/// recorded and shown).
fn stop_old(to: &str, install_exe: &Path, dock_pid: u32, guardian_pid: u32) -> Result<(), i32> {
    let dock = Proc::open(dock_pid);
    if dock_pid != 0 && !image_of(dock_pid).is_some_and(|p| norm(&p) == norm(install_exe)) {
        crate::log!("update: pid {dock_pid} is not {}; not applied", install_exe.display());
        record(to, Err("the dock is not the installed nopbar.exe".into()), false);
        return Err(4);
    }
    let half = QUIT_WAIT.as_secs() / 2;
    let mut quit = crate::settings::stop_running_dock(half);
    if !quit && dock_pid != 0 {
        // The pipe was busy or did not answer: the same quit, posted to
        // the dock's window (WM_QUIT_REQUEST has kept its number).
        crate::log!("update: the dock did not quit over its pipe; asking its windows");
        post_quit(dock_pid);
        quit = crate::settings::stop_running_dock(half);
    }
    let gone = dock.as_ref().is_none_or(|d| d.wait(Duration::from_secs(5)));
    if !quit || !gone {
        crate::log!("update: the dock did not quit within {} s; nothing changed", QUIT_WAIT.as_secs());
        record(to, Err("NopBar did not quit in time".into()), false);
        say("update.failed.title", "update.failed.quit", &[("v", to)]);
        return Err(5);
    }
    crate::log!("update: the dock quit (code {:#x})", dock.as_ref().map(|d| d.exit_code()).unwrap_or(0));
    let _ = crate::updates::transact(|st| {
        st.pending.phase = crate::updates::Phase::Installing;
        st.pending.since = crate::updates::unix_now();
    });
    // The old guardian restores the taskbar, then exits.
    if let Some(g) = Proc::open(guardian_pid).filter(|_| image_of(guardian_pid).is_some_and(|p| norm(&p) == norm(install_exe))) {
        if !g.wait(GUARDIAN_WAIT) {
            crate::log!("update: the guardian is still there after {} s; ending it", GUARDIAN_WAIT.as_secs());
            g.kill();
        }
    }
    make_sure_taskbar_is_back();
    Ok(())
}

/// Watches the new dock for [`WATCH`].
fn watch(p: &Proc, pid: u32, started: Instant) -> Verdict {
    let mut seen = Seen::default();
    loop {
        seen.elapsed = started.elapsed();
        if p.wait(Duration::from_millis(500)) {
            seen.exit = Some(p.exit_code());
        } else {
            seen.answering = answers(pid);
            seen.answered |= seen.answering;
        }
        seen.elapsed = started.elapsed();
        match verdict(&seen) {
            Verdict::Wait => continue,
            v => {
                crate::log!("update: new dock {pid}: {seen:?} -> {v:?}");
                return v;
            }
        }
    }
}

/// Ends everything started from `install_exe` since `since` (the new
/// dock, its guardian, a dock the guardian restarted), cleans what a
/// killed dock leaves, and brings the taskbar back.
fn end_new(install_exe: &Path, since: u64) {
    for _ in 0..3 {
        let left = processes_of(install_exe, since);
        if left.is_empty() {
            break;
        }
        for pid in left {
            if let Some(p) = Proc::open(pid) {
                p.kill();
            }
        }
    }
    crate::dock::recover_genie();
    crate::dock::remove_stale_tray_icon(crate::dock::TrayIconGone::Any);
    make_sure_taskbar_is_back();
}

/// The taskbar must not stay hidden while no dock runs.
fn make_sure_taskbar_is_back() {
    if crate::taskbar::marker_present() && !crate::taskbar::restore_before_exit(Duration::from_secs(30)) {
        crate::log!("update: taskbar restore not confirmed; the next start restores it");
    }
}

fn start_old(exe: &Path) {
    match start(exe) {
        Ok((_, pid)) => crate::log!("update: started {} again (pid {pid})", exe.display()),
        Err(e) => crate::log!("update: could not start the dock again: {e}"),
    }
}

// ---------------------------------------------------------------- installer updates

/// How long the installer may run.
const SETUP_WAIT: Duration = Duration::from_secs(10 * 60);
/// After the installer: how long its [Run] entry gets to start the dock.
const DOCK_APPEAR: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
pub struct SetupArgs {
    pub setup: PathBuf,
    pub sha256: String,
    pub install: PathBuf,
    pub dock_pid: u32,
    pub guardian_pid: u32,
    pub from: String,
    pub to: String,
}

pub fn setup_command_args(a: &SetupArgs) -> Vec<String> {
    vec![
        "--apply-setup".into(),
        a.setup.display().to_string(),
        a.sha256.clone(),
        a.install.display().to_string(),
        a.dock_pid.to_string(),
        a.guardian_pid.to_string(),
        a.from.clone(),
        a.to.clone(),
    ]
}

pub fn parse_setup_args(args: &[String]) -> Option<SetupArgs> {
    if args.get(1).map(|a| a.eq_ignore_ascii_case("--apply-setup")) != Some(true) || args.len() != 9 {
        return None;
    }
    crate::updates::Version::parse(&args[7])?;
    crate::updates::Version::parse(&args[8])?;
    super::sha256::parse_hex(&args[3])?;
    Some(SetupArgs {
        setup: PathBuf::from(&args[2]),
        sha256: args[3].to_ascii_lowercase(),
        install: PathBuf::from(&args[4]),
        dock_pid: args[5].parse().ok()?,
        guardian_pid: args[6].parse().ok()?,
        from: args[7].clone(),
        to: args[8].clone(),
    })
}

/// The installer's command line (packaging/inno/nopbar.iss): no wizard, no
/// message boxes, no reboot, no "This will install..." prompt, no Cancel;
/// its log next to it. Its own steps stay: PrepareToInstall runs the new
/// exe with --quit (no dock is running by then), Restart Manager closes
/// whatever still holds a file, and the [Run] entry starts the new dock
/// (it runs in silent mode too). No /DIR: it installs where the last
/// install went (InstallLocation).
pub fn setup_args(log: &Path) -> Vec<String> {
    vec!["/VERYSILENT".into(), "/SUPPRESSMSGBOXES".into(), "/NORESTART".into(), "/SP-".into(), "/NOCANCEL".into(), format!("/LOG={}", log.display())]
}

/// The checks before anything is done: the updater does not run from the
/// install folder, the setup exe is what was checked, `to` is newer than
/// `from`, and `from` is this exe's version (the updater is a copy of it).
pub fn check_setup(a: &SetupArgs, me: &Path, current: &str, setup_sha: Option<&str>) -> Result<(), String> {
    let inside = |p: &Path| norm(p) == norm(&a.install) || norm(p).starts_with(&format!("{}\\", norm(&a.install)));
    if me.parent().is_none_or(inside) {
        return Err(format!("the updater must not run from the install folder ({})", me.display()));
    }
    if inside(&a.setup) {
        return Err("the setup exe is inside the install folder".into());
    }
    if !a.install.is_absolute() || !a.setup.is_absolute() {
        return Err("paths must be absolute".into());
    }
    if !crate::updates::is_newer(&a.to, &a.from) {
        return Err(format!("{} is not newer than {}: no downgrade", a.to, a.from));
    }
    if crate::updates::Version::parse(&a.from) != crate::updates::Version::parse(current) {
        return Err(format!("the updater is {current}, the installed version {}", a.from));
    }
    match setup_sha {
        Some(h) if h.eq_ignore_ascii_case(&a.sha256) => Ok(()),
        Some(_) => Err("the setup exe changed since it was checked".into()),
        None => Err(format!("{} cannot be read", a.setup.display())),
    }
}

/// How a failed installer update is undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undo {
    /// Run the kept installer of the previous version silently.
    CachedSetup(PathBuf),
    /// No usable kept installer: copy the files saved before the install
    /// back (and the Apps & features version).
    Backup,
    /// Nothing to restore from: say so; the taskbar is back already.
    NotifyOnly,
}

/// `cache`: the kept installer; `cache_intact`: its file still has the
/// recorded SHA-256; `backup_ok`: the backup folder holds nopbar.exe.
pub fn undo_plan(cache: Option<&crate::updates::CachedSetup>, from: &str, cache_intact: bool, backup_ok: bool) -> Undo {
    match cache {
        Some(c) if cache_intact && crate::updates::Version::parse(&c.version).is_some() && crate::updates::Version::parse(&c.version) == crate::updates::Version::parse(from) => Undo::CachedSetup(PathBuf::from(&c.path)),
        _ if backup_ok => Undo::Backup,
        _ => Undo::NotifyOnly,
    }
}

/// Where a good installer is kept for the next rollback.
fn setup_cache_dir() -> Option<PathBuf> {
    super::updates_dir().map(|d| d.join("setup-cache"))
}

/// Runs an installer silently and waits for it. Ok(exit code), Err when it
/// did not start or did not finish within [`SETUP_WAIT`] (then it is left
/// running: killing a half-done install is worse).
fn run_installer(setup: &Path, log: &Path) -> Result<u32, String> {
    let mut child = std::process::Command::new(setup).args(setup_args(log)).current_dir(setup.parent().unwrap_or(Path::new("."))).spawn().map_err(|e| format!("{}: {e}", setup.display()))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code().unwrap_or(-1) as u32),
            Ok(None) if start.elapsed() < SETUP_WAIT => std::thread::sleep(Duration::from_millis(500)),
            Ok(None) => return Err(format!("the installer did not finish within {} s", SETUP_WAIT.as_secs())),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The dock the installer's [Run] entry started (the first process of the
/// installed exe since `since`); started here if none shows up in time.
fn new_dock(install_exe: &Path, since: u64) -> Result<(Proc, u32), String> {
    let until = Instant::now() + DOCK_APPEAR;
    while Instant::now() < until {
        let mut found = processes_since(install_exe, since);
        found.sort_by_key(|(_, t)| *t);
        if let Some((pid, _)) = found.first() {
            if let Some(p) = Proc::open(*pid) {
                return Ok((p, *pid));
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    crate::log!("update: the installer started no dock; starting it");
    start(install_exe)
}

/// Copies the installed files into `backup` (made fresh). True if
/// nopbar.exe is in it.
fn back_up(install: &Path, backup: &Path) -> bool {
    let _ = std::fs::remove_dir_all(backup);
    if std::fs::create_dir_all(backup).is_err() {
        return false;
    }
    for f in FILES {
        let from = install.join(f);
        if from.is_file() {
            if let Err(e) = std::fs::copy(&from, backup.join(f)) {
                crate::log!("update: backup of {f}: {e}");
            }
        }
    }
    backup.join("nopbar.exe").is_file()
}

/// Copies the backup over the installed files (nothing may be running
/// them). Returns the problems.
fn restore_backup(install: &Path, backup: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for f in FILES {
        let from = backup.join(f);
        if !from.is_file() {
            continue;
        }
        let tmp = install.join(format!("{f}.{}.nopbar-new", std::process::id()));
        let r = std::fs::copy(&from, &tmp).and_then(|_| {
            let target = install.join(f);
            if target.exists() {
                let aside = install.join(aside_name(f, std::process::id()));
                std::fs::rename(&target, &aside)?;
                let r = std::fs::rename(&tmp, &target);
                if r.is_ok() {
                    let _ = std::fs::remove_file(&aside);
                } else {
                    let _ = std::fs::rename(&aside, &target);
                }
                r
            } else {
                std::fs::rename(&tmp, &target)
            }
        });
        if let Err(e) = r {
            let _ = std::fs::remove_file(&tmp);
            problems.push(format!("{f}: {e}"));
        }
    }
    problems
}

/// `--apply-setup ...` (installed copies): returns the process exit code.
///
/// 1. checks ([`check_setup`]), one updater at a time;
/// 2. the dock quits, the guardian restores the taskbar ([`stop_old`]):
///    the taskbar is back before any file changes;
/// 3. the installed files are copied to `updates\backup\`;
/// 4. the installer runs silently and is waited for (10 min at most);
///    a failed installer: the backup goes back, the old dock starts;
/// 5. the new dock (started by the installer) is watched like a zip
///    update's ([`verdict`]);
/// 6. kept: the installer is kept in `updates\setup-cache\` (the next
///    update's rollback) and recorded; failed: everything the new exe
///    started is ended, the taskbar restored, then [`undo_plan`]: the
///    previous version's kept installer is run silently, else the backup
///    is copied back, else a notice only; the old dock is started if
///    nothing started it, and the version is never installed again.
pub fn run_setup(raw: &[String]) -> i32 {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    crate::i18n::init(&crate::config::read_current().map(|c| c.language).unwrap_or_default());
    let Some(a) = parse_setup_args(raw) else {
        crate::log!("update: bad --apply-setup arguments {raw:?}");
        return 2;
    };
    let to = a.to.as_str();
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\NopBar.Updater")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        crate::log!("update: another updater is running");
        return 3;
    }
    crate::log!("update: installer update {} -> {to} for {}", a.from, a.install.display());
    let me = std::env::current_exe().unwrap_or_default();
    // Opened so that it cannot be changed, checked through this handle and
    // held until the installer has finished.
    let mut locked = super::trust::Locked::open(&a.setup).ok();
    let sha = locked.as_mut().and_then(|l| l.sha256().ok());
    if let Err(e) = check_setup(&a, &me, crate::updates::CURRENT, sha.as_deref()) {
        crate::log!("update: not applied: {e}");
        record(to, Err(e), false);
        return 4;
    }
    let install_exe = a.install.join("nopbar.exe");
    if let Err(code) = stop_old(to, &install_exe, a.dock_pid, a.guardian_pid) {
        return code;
    }
    let Some(backup) = backup_dir() else {
        record(to, Err("no %LOCALAPPDATA%".into()), false);
        start_old(&install_exe);
        return 6;
    };
    let backup_ok = back_up(&a.install, &backup);
    if !backup_ok {
        crate::log!("update: no backup of nopbar.exe; a failed install can only be undone by the kept installer");
    }
    let log = a.setup.with_extension("log");
    let since = now_filetime();
    let journal = Journal { kind: "setup".into(), phase: "installer".into(), install: a.install.clone(), backup: backup.clone(), ops: Vec::new(), from: a.from.clone(), to: to.to_string(), updater: me.clone() };
    if let Err(e) = journal.begin() {
        crate::log!("update: no journal ({e}); not applied");
        record(to, Err(format!("journal: {e}")), false);
        start_old(&install_exe);
        return 6;
    }
    crate::log!("update: running {} silently (log {})", a.setup.display(), log.display());
    let ran = run_installer(&a.setup, &log);
    drop(locked);
    let outcome = match ran {
        Ok(0) => match new_dock(&install_exe, since) {
            Ok((p, pid)) => watch(&p, pid, Instant::now()),
            Err(e) => {
                crate::log!("update: {e}");
                Verdict::RollBack("the new version did not start")
            }
        },
        Ok(code) => {
            crate::log!("update: the installer exited with {code}");
            Verdict::RollBack("the installer failed")
        }
        Err(e) => {
            // Still running: leave it alone; the taskbar is back already.
            // The journal stays: the next sign-in finishes or undoes it.
            crate::log!("update: {e}");
            record(to, Err(e), false);
            say("update.failed.title", "update.failed.files", &[("v", to)]);
            return 8;
        }
    };
    match outcome {
        Verdict::Keep | Verdict::Wait => {
            Journal::end();
            keep_setup(&a);
            record(to, Ok(()), false);
            crate::log!("update: now running {to}");
            0
        }
        Verdict::RollBack(why) => {
            crate::log!("update: rolling back: {why}");
            end_new(&install_exe, since);
            let st = crate::updates::load_state();
            let cache = st.cached_setup.as_ref();
            let intact = cache.is_some_and(|c| super::sha256::of_file(Path::new(&c.path)).ok().map(|d| super::sha256::hex(&d)).as_deref() == Some(c.sha256.as_str()));
            let mut plan = undo_plan(cache, &a.from, intact, backup_ok);
            crate::log!("update: undo with {plan:?}");
            if let Undo::CachedSetup(old) = &plan {
                let since_old = now_filetime();
                match run_installer(old, &old.with_extension("log")) {
                    Ok(0) => {
                        // Its [Run] entry starts the old dock; start it if not.
                        if processes_of(&install_exe, since_old).is_empty() && new_dock(&install_exe, since_old).is_err() {
                            crate::log!("update: the old dock did not start");
                        }
                    }
                    r => {
                        crate::log!("update: the kept installer failed ({r:?}); using the backup");
                        end_new(&install_exe, since_old);
                        plan = if backup_ok { Undo::Backup } else { Undo::NotifyOnly };
                    }
                }
            }
            match plan {
                Undo::CachedSetup(_) => {}
                Undo::Backup => {
                    let problems = restore_backup(&a.install, &backup);
                    if !problems.is_empty() {
                        crate::log!("update: restoring the backup: {problems:?}");
                    }
                    crate::release::set_display_version(&a.install, &a.from);
                    start_old(&install_exe);
                }
                Undo::NotifyOnly => {
                    // The installed files are the new version's; it is not
                    // started again. The taskbar is back (end_new).
                    crate::log!("update: nothing to roll back with; NopBar is not running, the taskbar is shown");
                }
            }
            Journal::end();
            record(to, Err(why.to_string()), true);
            say("update.failed.title", "update.failed.rolled_back", &[("v", to)]);
            7
        }
    }
}

/// After a good install: this installer becomes the kept one (the
/// previous kept installer is deleted).
fn keep_setup(a: &SetupArgs) {
    let Some(dir) = setup_cache_dir() else { return };
    let _ = std::fs::remove_dir_all(&dir);
    let name = a.setup.file_name().map(|n| n.to_owned()).unwrap_or_else(|| "setup.exe".into());
    let dest = dir.join(name);
    let r = std::fs::create_dir_all(&dir).and_then(|_| std::fs::copy(&a.setup, &dest));
    match r {
        Ok(_) => {
            let cached = crate::updates::CachedSetup { version: a.to.clone(), path: dest.display().to_string(), sha256: a.sha256.clone() };
            let _ = crate::updates::transact(|st| st.cached_setup = Some(cached.clone()));
        }
        Err(e) => crate::log!("update: could not keep the installer for a later rollback: {e}"),
    }
}

// ---------------------------------------------------------------- journal and recovery

/// What an updater is in the middle of, on disk (`updates\journal.json`),
/// so that a crash or power cut can be undone: written before the first
/// installed file changes, removed when the update is kept or rolled back.
/// While it exists a RunOnce value runs `"<updater>" --recover-update` at
/// the next sign-in (the updater's own copy, outside the install folder,
/// so it works even when nopbar.exe is missing).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Journal {
    /// "zip" or "setup".
    pub kind: String,
    /// zip: "swapping" (files being moved), "watching" (all in place);
    /// setup: "installer" (the installer may have run).
    pub phase: String,
    pub install: PathBuf,
    pub backup: PathBuf,
    /// zip: the replacement plan.
    pub ops: Vec<Op>,
    pub from: String,
    pub to: String,
    /// The exe the RunOnce value runs.
    pub updater: PathBuf,
}

const RUNONCE_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce";
const RUNONCE_VALUE: &str = "NopBarUpdateRecovery";

pub fn journal_path() -> Option<PathBuf> {
    super::updates_dir().map(|d| d.join("journal.json"))
}

/// Writes `text` to `p` through a temporary file in the same folder, flushed
/// to disk before the rename (the file is never seen half written). The
/// flush needs a handle opened for writing: FlushFileBuffers on a read-only
/// handle is refused with ERROR_ACCESS_DENIED, which made every update fail
/// with "journal: access denied" before 0.1.37.
fn write_synced(p: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = p.with_extension(format!("json.{}.tmp", std::process::id()));
    let r = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(text.as_bytes())?;
        f.sync_all()
    });
    let r = r.and_then(|_| std::fs::rename(&tmp, p));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

impl Journal {
    fn save(&self) -> Result<(), String> {
        let p = journal_path().ok_or("no %LOCALAPPDATA%")?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_synced(&p, &text).map_err(|e| e.to_string())
    }

    /// Saves it and arms the sign-in recovery.
    fn begin(&self) -> Result<(), String> {
        self.save()?;
        set_runonce(&self.updater)
    }

    /// The update is over (kept or rolled back).
    fn end() {
        if let Some(p) = journal_path() {
            let _ = std::fs::remove_file(p);
        }
        clear_runonce();
    }

    fn load() -> Option<Journal> {
        let p = journal_path()?;
        let b = crate::config::read_capped(&p, 256 * 1024).ok()?;
        serde_json::from_slice(&b).ok()
    }
}

/// The RunOnce command for `updater` (no URL or other text from outside).
pub fn runonce_command(updater: &Path) -> String {
    format!("\"{}\" --recover-update", updater.display())
}

fn set_runonce(updater: &Path) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    let v: Vec<u16> = runonce_command(updater).encode_utf16().chain(Some(0)).collect();
    let r = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUNONCE_KEY), &HSTRING::from(RUNONCE_VALUE), REG_SZ.0, Some(v.as_ptr().cast()), (v.len() * 2) as u32) };
    if r.is_err() {
        return Err(format!("RunOnce: {r:?}"));
    }
    Ok(())
}

fn clear_runonce() {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegDeleteKeyValueW, HKEY_CURRENT_USER};
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUNONCE_KEY), &HSTRING::from(RUNONCE_VALUE));
    }
}

/// What an unfinished journal needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Files were being swapped: put the old ones back.
    RollBack,
    /// Everything was in place: keep it, remove the aside copies.
    Commit,
    /// The installer may have stopped half way and nopbar.exe is gone:
    /// copy the backup back.
    RestoreBackup,
    /// Nothing to undo.
    Nothing,
}

pub fn recovery_action(kind: &str, phase: &str, installed_exe_present: bool) -> Recovery {
    match (kind, phase) {
        ("zip", "swapping") => Recovery::RollBack,
        ("zip", "watching") => Recovery::Commit,
        ("setup", _) if !installed_exe_present => Recovery::RestoreBackup,
        _ => Recovery::Nothing,
    }
}

/// An updater holds its mutex while it runs.
fn updater_running() -> bool {
    use windows::core::w;
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\NopBar.Updater")) } {
        Ok(h) => {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
            true
        }
        Err(_) => false,
    }
}

/// `--recover-update` (RunOnce at sign-in, or started by the dock): an
/// update that did not finish is undone or finished, and NopBar runs
/// again. Returns the exit code.
pub fn recover() -> i32 {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    let Some(j) = Journal::load() else {
        clear_runonce();
        return 0;
    };
    if updater_running() {
        crate::log!("update: recovery: an updater is still running; left to it");
        return 0;
    }
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\NopBar.Updater")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return 0;
    }
    crate::i18n::init(&crate::config::read_current().map(|c| c.language).unwrap_or_default());
    let install_exe = j.install.join("nopbar.exe");
    let action = recovery_action(&j.kind, &j.phase, install_exe.is_file());
    crate::log!("update: recovering an unfinished update {} -> {} ({} / {}): {action:?}", j.from, j.to, j.kind, j.phase);
    match action {
        Recovery::RollBack | Recovery::RestoreBackup => {
            // Whatever runs from the half-swapped folder goes first (the
            // dock quits the normal way), then the taskbar is checked.
            if !crate::settings::stop_running_dock(15) {
                for pid in processes_of(&install_exe, 0) {
                    if let Some(p) = Proc::open(pid) {
                        p.kill();
                    }
                }
            }
            for pid in processes_of(&install_exe, 0) {
                if let Some(p) = Proc::open(pid) {
                    p.kill();
                }
            }
            make_sure_taskbar_is_back();
            let problems = if action == Recovery::RollBack { roll_back(&j.ops, &j.backup) } else { restore_backup(&j.install, &j.backup) };
            if !problems.is_empty() {
                crate::log!("update: recovery problems: {problems:?}");
            }
            record(&j.to, Err("the update was interrupted and undone".into()), false);
        }
        Recovery::Commit => commit(&j.ops),
        Recovery::Nothing => {}
    }
    Journal::end();
    if processes_of(&install_exe, 0).is_empty() && install_exe.is_file() {
        start_old(&install_exe);
    }
    0
}

/// The dock's check: a journal no updater is working on (it crashed, and
/// the sign-in recovery has not run yet) is handed to its recovery command.
pub fn recover_stale_journal() {
    let Some(p) = journal_path() else { return };
    let Ok(meta) = std::fs::metadata(&p) else { return };
    let old = meta.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d >= Duration::from_secs(120));
    if !old || updater_running() {
        return;
    }
    let Some(j) = Journal::load() else { return };
    if !j.updater.is_file() {
        crate::log!("update: an unfinished update's recovery exe is gone; journal kept");
        return;
    }
    crate::log!("update: an unfinished update ({} -> {}); starting its recovery", j.from, j.to);
    let _ = std::process::Command::new(&j.updater).arg("--recover-update").spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journal_is_written_and_flushed_and_replaces_an_old_one() {
        // Regression: the flush once used a read-only handle and every
        // update failed with "journal: access denied" (os error 5).
        let dir = std::env::temp_dir().join(format!("nopbar-journal-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("journal.json");
        write_synced(&p, "{\"a\":1}").expect("first write");
        write_synced(&p, "{\"a\":2}").expect("replace");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":2}");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "no temporary file left: {left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    use crate::autoupdate::testutil::Scratch;

    fn put(p: &Path, s: &str) {
        std::fs::write(p, s).unwrap();
    }

    fn get(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap_or_else(|_| "<missing>".into())
    }

    /// staged/, install/, backup/ with old and new files.
    fn setup(name: &str, with_license: bool) -> (Scratch, PathBuf, PathBuf, PathBuf) {
        let s = Scratch::new(name);
        let (staged, install, backup) = (s.0.join("staged"), s.0.join("install"), s.0.join("backup"));
        for d in [&staged, &install, &backup] {
            std::fs::create_dir_all(d).unwrap();
        }
        put(&staged.join("nopbar.exe"), "new exe");
        put(&staged.join("nopbar-mcp.exe"), "new shim");
        put(&staged.join("LICENSE.txt"), "new license");
        put(&install.join("nopbar.exe"), "old exe");
        if with_license {
            put(&install.join("LICENSE.txt"), "old license");
        }
        (s, staged, install, backup)
    }

    fn plan_for(staged: &Path, install: &Path, backup: &Path) -> Vec<Op> {
        let i = install.to_path_buf();
        plan(staged, install, backup, FILES, &|f| i.join(f).is_file(), 7)
    }

    #[test]
    fn the_plan_backs_up_and_copies_before_touching_anything() {
        let (st, i, b) = (Path::new(r"C:\s"), Path::new(r"C:\i"), Path::new(r"C:\b"));
        let ops = plan(st, i, b, FILES, &|f| f == "nopbar.exe", 7);
        assert_eq!(
            ops,
            vec![
                Op::Backup { from: i.join("nopbar.exe"), to: b.join("nopbar.exe") },
                Op::CopyNew { from: st.join("nopbar.exe"), to: i.join("nopbar.exe.7.nopbar-new") },
                Op::CopyNew { from: st.join("nopbar-mcp.exe"), to: i.join("nopbar-mcp.exe.7.nopbar-new") },
                Op::CopyNew { from: st.join("LICENSE.txt"), to: i.join("LICENSE.txt.7.nopbar-new") },
                Op::MoveAside { from: i.join("nopbar.exe"), to: i.join("nopbar.exe.7.nopbar-old") },
                Op::Place { from: i.join("nopbar.exe.7.nopbar-new"), to: i.join("nopbar.exe") },
                Op::Place { from: i.join("nopbar-mcp.exe.7.nopbar-new"), to: i.join("nopbar-mcp.exe") },
                // Not installed before: only placed.
                Op::Place { from: i.join("LICENSE.txt.7.nopbar-new"), to: i.join("LICENSE.txt") },
            ]
        );
        // Every path stays inside the three folders.
        for op in &ops {
            let (Op::Backup { from, to } | Op::CopyNew { from, to } | Op::MoveAside { from, to } | Op::Place { from, to }) = op;
            for p in [from, to] {
                assert!(p.starts_with(st) || p.starts_with(i) || p.starts_with(b), "{p:?}");
            }
        }
    }

    #[test]
    fn apply_commit_replaces_and_cleans_up() {
        let (_s, staged, install, backup) = setup("apply-ok", true);
        let ops = plan_for(&staged, &install, &backup);
        apply(&ops).unwrap();
        assert_eq!(get(&install.join("nopbar.exe")), "new exe");
        assert_eq!(get(&install.join("nopbar-mcp.exe")), "new shim");
        assert_eq!(get(&install.join("LICENSE.txt")), "new license");
        assert_eq!(get(&backup.join("nopbar.exe")), "old exe");
        assert_eq!(get(&backup.join("LICENSE.txt")), "old license");
        assert_eq!(get(&install.join("nopbar.exe.7.nopbar-old")), "old exe");
        commit(&ops);
        let mut left: Vec<String> = std::fs::read_dir(&install).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["LICENSE.txt", "nopbar-mcp.exe", "nopbar.exe"]);
        // The staged files are untouched (copied, not moved).
        assert_eq!(get(&staged.join("nopbar.exe")), "new exe");
    }

    #[test]
    fn roll_back_puts_the_old_files_back() {
        let (_s, staged, install, backup) = setup("apply-rollback", false);
        let ops = plan_for(&staged, &install, &backup);
        apply(&ops).unwrap();
        assert!(roll_back(&ops, &backup).is_empty());
        assert_eq!(get(&install.join("nopbar.exe")), "old exe");
        assert!(!install.join("LICENSE.txt").exists(), "added by the update: removed again");
        assert!(!install.join("nopbar-mcp.exe").exists(), "new shim rolls back with the update");
        let left: Vec<_> = std::fs::read_dir(&install).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary files left");
    }

    #[test]
    fn roll_back_uses_the_backup_when_the_aside_file_is_gone() {
        let (_s, staged, install, backup) = setup("apply-rollback-backup", true);
        let ops = plan_for(&staged, &install, &backup);
        apply(&ops).unwrap();
        std::fs::remove_file(install.join("nopbar.exe.7.nopbar-old")).unwrap();
        assert!(roll_back(&ops, &backup).is_empty());
        assert_eq!(get(&install.join("nopbar.exe")), "old exe");
        assert_eq!(get(&install.join("LICENSE.txt")), "old license");
    }

    #[test]
    fn a_failure_midway_leaves_the_install_as_it_was() {
        let (_s, staged, install, backup) = setup("apply-fail", true);
        let ops = plan_for(&staged, &install, &backup);
        // The second file cannot be placed: a folder sits at its new name's
        // target... simulated by removing a staged file after planning.
        std::fs::remove_file(staged.join("LICENSE.txt")).unwrap();
        let err = apply(&ops).unwrap_err();
        assert!(err.0.contains("CopyNew"), "{err:?}");
        assert!(err.1.is_empty(), "undo went through: {err:?}");
        assert_eq!(get(&install.join("nopbar.exe")), "old exe");
        assert_eq!(get(&install.join("LICENSE.txt")), "old license");
        let mut left: Vec<String> = std::fs::read_dir(&install).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["LICENSE.txt", "nopbar.exe"]);

        // A failure after a file was already swapped is undone too.
        put(&staged.join("LICENSE.txt"), "new license");
        let ops = plan_for(&staged, &install, &backup);
        let place_license = ops.iter().position(|o| matches!(o, Op::Place { to, .. } if to.ends_with("LICENSE.txt"))).unwrap();
        // Run everything before it, then undo as apply would.
        let done: Vec<Op> = ops[..place_license].to_vec();
        for op in &done {
            run_op(op).unwrap();
        }
        assert_eq!(get(&install.join("nopbar.exe")), "new exe");
        assert!(undo(&done).is_empty());
        assert_eq!(get(&install.join("nopbar.exe")), "old exe");
        assert_eq!(get(&install.join("LICENSE.txt")), "old license");
    }

    #[test]
    fn stale_temp_files_are_only_ours() {
        let names: Vec<String> = ["nopbar.exe", "nopbar.exe.123.nopbar-old", "LICENSE.txt.9.nopbar-new", "NOPBAR.EXE.5.NOPBAR-OLD", "mine.txt.1.nopbar-old", "nopbar.exe.x1.nopbar-old", "nopbar.exe..nopbar-old", "nopbar.exe.nopbar-old", "notes.txt"].iter().map(|s| s.to_string()).collect();
        assert_eq!(stale_temp_files(&names), ["nopbar.exe.123.nopbar-old", "LICENSE.txt.9.nopbar-new", "NOPBAR.EXE.5.NOPBAR-OLD"]);
        let s = Scratch::new("apply-clean");
        put(&s.0.join("nopbar.exe"), "exe");
        put(&s.0.join("nopbar.exe.4.nopbar-old"), "old");
        put(&s.0.join("keep.txt"), "user's");
        std::fs::create_dir(s.0.join("LICENSE.txt.5.nopbar-new")).unwrap();
        clean_install_dir(&s.0);
        assert!(s.0.join("nopbar.exe").is_file() && s.0.join("keep.txt").is_file());
        assert!(!s.0.join("nopbar.exe.4.nopbar-old").exists());
        assert!(s.0.join("LICENSE.txt.5.nopbar-new").is_dir(), "folders are left alone");
    }

    #[test]
    fn rollback_decisions() {
        let s = |secs: u64, exit: Option<u32>, answered: bool, answering: bool| Seen { elapsed: Duration::from_secs(secs), exit, answered, answering };
        // Still starting.
        assert_eq!(verdict(&s(3, None, false, false)), Verdict::Wait);
        assert_eq!(verdict(&s(10, None, true, true)), Verdict::Wait, "keeps watching for the whole window");
        // Up and answering at the end: kept.
        assert_eq!(verdict(&s(20, None, true, true)), Verdict::Keep);
        // Never showed a window / hung at the end: rolled back.
        assert!(matches!(verdict(&s(20, None, false, false)), Verdict::RollBack(_)));
        assert!(matches!(verdict(&s(21, None, true, false)), Verdict::RollBack(_)));
        // Crashed (any time): rolled back.
        assert!(matches!(verdict(&s(2, Some(0xC000_0409), false, false)), Verdict::RollBack(_)));
        assert!(matches!(verdict(&s(15, Some(0xC000_0005), true, true)), Verdict::RollBack(_)));
        // Quit before its window: rolled back; quit by the user after it ran: kept.
        assert!(matches!(verdict(&s(1, Some(0), false, false)), Verdict::RollBack(_)));
        assert_eq!(verdict(&s(12, Some(0), true, true)), Verdict::Keep);
        assert_eq!(verdict(&s(12, Some(1), true, false)), Verdict::Keep, "Task Manager");
    }

    fn args(a: &[&str]) -> Vec<String> {
        std::iter::once("nopbar.exe").chain(a.iter().copied()).map(String::from).collect()
    }

    #[test]
    fn arguments_round_trip() {
        let a = Args { staged: PathBuf::from(r"C:\Users\u\AppData\Local\NopBar\updates\0.2.0\staged"), install: PathBuf::from(r"D:\Apps\Nop Bar"), dock_pid: 42, guardian_pid: 43, from: "0.1.0".into() };
        let mut argv = vec!["nopbar.exe".to_string()];
        argv.extend(command_args(&a));
        assert_eq!(parse_args(&argv), Some(a));
        assert_eq!(parse_args(&args(&["--apply-update", "a", "b", "1", "2"])), None, "too few");
        assert_eq!(parse_args(&args(&["--apply-update", "a", "b", "x", "2", "0.1.0"])), None);
        assert_eq!(parse_args(&args(&["--apply-update", "a", "b", "1", "2", "junk"])), None);
        assert_eq!(parse_args(&args(&["--settings", "a", "b", "1", "2", "0.1.0"])), None);
    }

    #[test]
    fn checks_before_anything_is_done() {
        let (s, staged, install, _backup) = setup("apply-check", true);
        let a = Args { staged: staged.clone(), install: install.clone(), dock_pid: 0, guardian_pid: 0, from: "0.1.0".into() };
        let me = staged.join("nopbar.exe");
        assert_eq!(check(&a, &me, "0.2.0"), Ok(()));
        assert!(check(&a, &me, "0.1.0").is_err(), "same version");
        assert!(check(&a, &me, "0.0.9").is_err(), "downgrade");
        assert!(check(&a, &install.join("nopbar.exe"), "0.2.0").is_err(), "must run from the staged folder");
        let inside = Args { staged: install.join("x"), ..a.clone() };
        assert!(check(&inside, &install.join("x").join("nopbar.exe"), "0.2.0").is_err());
        std::fs::remove_file(install.join("nopbar.exe")).unwrap();
        std::fs::create_dir(install.join("nopbar.exe")).unwrap();
        assert!(check(&a, &me, "0.2.0").is_err(), "installed exe must be a plain file");
        drop(s);
    }

    #[test]
    fn installer_arguments_are_silent() {
        let a = setup_args(Path::new(r"C:\Users\u\AppData\Local\NopBar\updates\0.2.0\NopBar-0.2.0-setup.log"));
        assert_eq!(a, ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", "/NOCANCEL", r"/LOG=C:\Users\u\AppData\Local\NopBar\updates\0.2.0\NopBar-0.2.0-setup.log"]);
        assert!(!a.iter().any(|x| x.to_ascii_uppercase().starts_with("/DIR")), "installs where the last install went");
        assert!(!a.iter().any(|x| x.eq_ignore_ascii_case("/NOCLOSEAPPLICATIONS")), "Restart Manager closes what holds a file");
    }

    #[test]
    fn setup_arguments_round_trip() {
        let a = SetupArgs {
            setup: PathBuf::from(r"C:\Users\u\AppData\Local\NopBar\updates\0.2.0\NopBar-0.2.0-setup.exe"),
            sha256: "ab".repeat(32),
            install: PathBuf::from(r"C:\Users\u\AppData\Local\Programs\NopBar"),
            dock_pid: 10,
            guardian_pid: 11,
            from: "0.1.0".into(),
            to: "0.2.0".into(),
        };
        let mut argv = vec!["nopbar.exe".to_string()];
        argv.extend(setup_command_args(&a));
        assert_eq!(parse_setup_args(&argv), Some(a.clone()));
        let mut bad = argv.clone();
        bad[3] = "abc".into();
        assert_eq!(parse_setup_args(&bad), None, "not a SHA-256");
        let mut bad = argv.clone();
        bad[8] = "junk".into();
        assert_eq!(parse_setup_args(&bad), None);
        assert_eq!(parse_setup_args(&argv[..8]), None);
    }

    #[test]
    fn installer_update_checks() {
        let sha = "cd".repeat(32);
        let a = SetupArgs {
            setup: PathBuf::from(r"C:\L\NopBar\updates\0.2.0\NopBar-0.2.0-setup.exe"),
            sha256: sha.clone(),
            install: PathBuf::from(r"C:\L\Programs\NopBar"),
            dock_pid: 0,
            guardian_pid: 0,
            from: "0.1.0".into(),
            to: "0.2.0".into(),
        };
        let me = Path::new(r"C:\L\NopBar\updates\0.2.0\updater\nopbar.exe");
        assert_eq!(check_setup(&a, me, "0.1.0", Some(&sha)), Ok(()));
        assert_eq!(check_setup(&a, me, "0.1.0", Some(&sha.to_uppercase())), Ok(()));
        assert!(check_setup(&a, Path::new(r"C:\L\Programs\NopBar\nopbar.exe"), "0.1.0", Some(&sha)).is_err(), "not from the install folder");
        assert!(check_setup(&a, Path::new(r"c:\l\programs\nopbar\sub\nopbar.exe"), "0.1.0", Some(&sha)).is_err());
        assert!(check_setup(&a, me, "0.1.0", Some(&"00".repeat(32))).is_err(), "changed since checked");
        assert!(check_setup(&a, me, "0.1.0", None).is_err());
        assert!(check_setup(&a, me, "0.0.9", Some(&sha)).is_err(), "the updater must be the installed version");
        assert!(check_setup(&SetupArgs { to: "0.1.0".into(), ..a.clone() }, me, "0.1.0", Some(&sha)).is_err(), "same version");
        assert!(check_setup(&SetupArgs { to: "0.0.5".into(), ..a.clone() }, me, "0.1.0", Some(&sha)).is_err(), "downgrade");
        let inside = SetupArgs { setup: PathBuf::from(r"C:\L\Programs\NopBar\setup.exe"), ..a.clone() };
        assert!(check_setup(&inside, me, "0.1.0", Some(&sha)).is_err());
    }

    /// What a silent installer update relies on in packaging/inno/nopbar.iss.
    #[test]
    fn installer_script_is_silent_safe() {
        let iss = include_str!("../../../packaging/inno/nopbar.iss");
        let run: Vec<&str> = iss.lines().skip_while(|l| l.trim() != "[Run]").skip(1).take_while(|l| !l.trim_start().starts_with('[')).filter(|l| l.contains("nopbar.exe")).collect();
        assert_eq!(run.len(), 1, "one [Run] entry starts the dock");
        assert!(run[0].contains("postinstall") && !run[0].contains("skipifsilent"), "it must run in silent mode too: {}", run[0]);
        for want in ["PrivilegesRequired=lowest", "CloseApplications=force", "RestartApplications=no", "DisableDirPage=yes", "'--quit'", "VersionInfoProductTextVersion={#AppVersion}"] {
            assert!(iss.contains(want), "nopbar.iss lost {want}");
        }
        assert!(!iss.contains("MsgBox("), "a message box silent mode would have to answer");
    }

    #[test]
    fn cached_setup_rollback_decisions() {
        use crate::updates::CachedSetup;
        let c = |v: &str| CachedSetup { version: v.into(), path: format!(r"C:\L\NopBar\updates\setup-cache\NopBar-{v}-setup.exe"), sha256: "ef".repeat(32) };
        // The previous version's installer, intact: run it.
        assert_eq!(undo_plan(Some(&c("0.1.0")), "0.1.0", true, true), Undo::CachedSetup(PathBuf::from(r"C:\L\NopBar\updates\setup-cache\NopBar-0.1.0-setup.exe")));
        assert_eq!(undo_plan(Some(&c("v0.1.0")), "0.1.0", true, false), Undo::CachedSetup(PathBuf::from(r"C:\L\NopBar\updates\setup-cache\NopBar-v0.1.0-setup.exe")));
        // Changed on disk, or another version's (installed by hand since):
        // the backup, else a notice only.
        assert_eq!(undo_plan(Some(&c("0.1.0")), "0.1.0", false, true), Undo::Backup);
        assert_eq!(undo_plan(Some(&c("0.0.9")), "0.1.0", true, true), Undo::Backup);
        assert_eq!(undo_plan(Some(&c("junk")), "junk", true, true), Undo::Backup);
        // The very first installer update: no kept installer.
        assert_eq!(undo_plan(None, "0.1.0", false, true), Undo::Backup);
        assert_eq!(undo_plan(None, "0.1.0", false, false), Undo::NotifyOnly);
        assert_eq!(undo_plan(Some(&c("0.0.9")), "0.1.0", true, false), Undo::NotifyOnly);
    }

    #[test]
    fn backup_and_restore_round_trip() {
        let (_s, _staged, install, backup) = setup("setup-backup", true);
        assert!(back_up(&install, &backup));
        // The installer replaced both files.
        put(&install.join("nopbar.exe"), "installed new");
        put(&install.join("LICENSE.txt"), "installed new license");
        assert!(restore_backup(&install, &backup).is_empty());
        assert_eq!(get(&install.join("nopbar.exe")), "old exe");
        assert_eq!(get(&install.join("LICENSE.txt")), "old license");
        let left: Vec<_> = std::fs::read_dir(&install).unwrap().collect();
        assert_eq!(left.len(), 2, "no temporary files left");
        // No exe to back up: reported.
        std::fs::remove_file(install.join("nopbar.exe")).unwrap();
        assert!(!back_up(&install, &backup));
    }

    #[test]
    fn recovery_decisions() {
        assert_eq!(recovery_action("zip", "swapping", true), Recovery::RollBack);
        assert_eq!(recovery_action("zip", "swapping", false), Recovery::RollBack, "nopbar.exe moved aside, not yet back");
        assert_eq!(recovery_action("zip", "watching", true), Recovery::Commit);
        assert_eq!(recovery_action("setup", "installer", false), Recovery::RestoreBackup);
        assert_eq!(recovery_action("setup", "installer", true), Recovery::Nothing, "the installer finished or never started");
        assert_eq!(recovery_action("junk", "x", false), Recovery::Nothing);
    }

    #[test]
    fn a_journal_survives_and_rolls_a_half_swap_back() {
        let (_s, staged, install, backup) = setup("journal", true);
        let ops = plan_for(&staged, &install, &backup);
        // The plan round-trips through JSON (what is on disk).
        let j = Journal { kind: "zip".into(), phase: "swapping".into(), install: install.clone(), backup: backup.clone(), ops: ops.clone(), from: "0.1.0".into(), to: "0.2.0".into(), updater: staged.join("nopbar.exe") };
        let text = serde_json::to_string(&j).unwrap();
        let back: Journal = serde_json::from_str(&text).unwrap();
        assert_eq!(back, j);
        // Crash right after nopbar.exe was moved aside, before the new one
        // was placed.
        let aside = ops.iter().position(|o| matches!(o, Op::MoveAside { from, .. } if from.ends_with("nopbar.exe"))).unwrap();
        for op in &ops[..=aside] {
            run_op(op).unwrap();
        }
        assert!(!install.join("nopbar.exe").exists());
        assert_eq!(recovery_action(&back.kind, &back.phase, install.join("nopbar.exe").is_file()), Recovery::RollBack);
        assert!(roll_back(&back.ops, &back.backup).is_empty());
        assert_eq!(get(&install.join("nopbar.exe")), "old exe");
        assert_eq!(get(&install.join("LICENSE.txt")), "old license");
    }

    #[test]
    fn the_runonce_command_names_only_the_updater() {
        assert_eq!(runonce_command(Path::new(r"C:\L\NopBar\updates\0.2.0\run-1-2\nopbar.exe")), r#""C:\L\NopBar\updates\0.2.0\run-1-2\nopbar.exe" --recover-update"#);
    }
}
