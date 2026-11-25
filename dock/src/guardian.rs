//! The guardian: a second copy of the exe (`--guardian <pid>`) that makes
//! sure the taskbar always comes back and the dock comes back after a crash.
//!
//! - Every 5 s it pings the dock's UI thread (`WM_NULL`, 2 s timeout).
//!   No answer for ~15 s: give the taskbar back. Answers again: ask the dock
//!   to hide it again. No answer for ~30 s: end the dock and start a new one.
//! - The dock exits with an NTSTATUS error (panic = 0xC0000409, access
//!   violation, ...): restore and start a new one. Exit code 0 (quit) or 1
//!   (Task Manager / taskkill) means the user wanted it gone: restore only.
//!   Either way the taskbar is restored after the dock is gone, checked,
//!   and tried again while the dock's marker says it is still hidden.
//! - Whenever the dock is gone, a genie it left half done is undone (the
//!   minimized window's DWM transitions, see dock/genie_shared.rs), and a tray
//!   icon it left (crash, hang kill) is removed (dock/trayicon.rs).
//! - At most 3 restarts in 10 minutes, so a dock that dies on startup does
//!   not loop forever.

use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Diagnostics::Debug::{MiniDumpNormal, MiniDumpWithThreadInfo, MiniDumpWriteDump};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, PROCESS_VM_READ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, PostMessageW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_NULL,
};

use crate::config::app_dir;
use crate::taskbar;

const CHECK_EVERY_MS: u32 = 5_000;
const PING_TIMEOUT_MS: u32 = 2_000;
/// Measured from the first unanswered ping (which itself took 2 s).
const HANG_RESTORE: Duration = Duration::from_secs(13);
const HANG_RESTART: Duration = Duration::from_secs(28);
const DUMP_TIMEOUT: Duration = Duration::from_secs(3);
const DUMP_LIMIT: usize = 3;
const DUMP_PREFIX: &str = "nopbar-hang-";
const DUMP_PART_AGE: Duration = Duration::from_secs(60 * 60);
static DUMP_WRITER: AtomicBool = AtomicBool::new(false);

struct DumpWriter;

impl DumpWriter {
    fn claim() -> Option<Self> {
        DUMP_WRITER.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).ok().map(|_| Self)
    }
}

impl Drop for DumpWriter {
    fn drop(&mut self) {
        DUMP_WRITER.store(false, Ordering::Release);
    }
}

const BUDGET_WINDOW_SECS: u64 = 600;
const BUDGET_MAX: usize = 3;
/// NTSTATUS error codes (0xC0000000..=0xCFFFFFFF: panic 0xC0000409, access
/// violation 0xC0000005, ...) mean a crash. Anything else is a choice: 0 =
/// quit, 1 = taskkill / Task Manager, 0xFFFFFFFF = .NET Process.Kill.
pub fn is_crash(code: u32) -> bool {
    (0xC000_0000..0xD000_0000).contains(&code)
}

/// Restart budget: `history` holds epoch seconds of earlier restarts.
/// Returns whether another restart is allowed and the history to store.
pub fn allow_restart(history: &[u64], now: u64) -> (bool, Vec<u64>) {
    let mut recent: Vec<u64> = history.iter().copied().filter(|&t| now.saturating_sub(t) < BUDGET_WINDOW_SECS).collect();
    if recent.len() >= BUDGET_MAX {
        (false, recent)
    } else {
        recent.push(now);
        (true, recent)
    }
}

fn budget_file() -> PathBuf {
    app_dir().join("crash-count")
}

fn take_restart_budget() -> bool {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let history: Vec<u64> = std::fs::read_to_string(budget_file())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    let (ok, keep) = allow_restart(&history, now);
    if !ok {
        return false;
    }
    // Only restart if the new count is safely on disk; otherwise the next
    // guardian would see the old count and the limit would never bite.
    let text: String = keep.iter().map(|t| format!("{t}\n")).collect();
    let tmp = budget_file().with_extension("tmp");
    match std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, budget_file())) {
        Ok(()) => true,
        Err(e) => {
            crate::log!("guardian: cannot record restart ({e}); not restarting");
            false
        }
    }
}

/// The guardian's own limit for giving the taskbar back: a stuck explorer
/// must not stop its crash / hang handling (audit A03). The call keeps
/// running on its helper and finishes when explorer answers.
const RESTORE_DEADLINE: Duration = Duration::from_secs(5);

/// How long the guardian stays to see a late restore through before it
/// exits (its helper thread ends with the process).
const FINAL_RESTORE_WAIT: Duration = Duration::from_secs(600);

fn restore_taskbar() {
    if !taskbar::restore_within(RESTORE_DEADLINE) {
        crate::log!("guardian: explorer did not answer the taskbar restore in time; going on");
    }
}

/// Waits between the guardian's final restore tries: 1, 2, 4, ... s, at
/// most RETRY_MAX (try `n` = 1 is the first retry).
pub fn retry_delay(n: u32) -> Duration {
    const RETRY_MAX: Duration = Duration::from_secs(30);
    Duration::from_secs(1u64 << n.saturating_sub(1).min(5)).min(RETRY_MAX)
}

/// The dock is gone (quit, crash, kill, hang kill): the taskbar must be
/// back. The windows are always shown again (that never hides anything).
/// A hide the dock did not undo (its marker is still on disk) is restored
/// and checked; while the marker stays (restore not confirmed), the
/// guardian keeps trying with a growing pause, for FINAL_RESTORE_WAIT at
/// most, then leaves the marker for the next start (or --restore-taskbar).
/// True only when the taskbar is confirmed back (no marker left).
fn final_restore() -> bool {
    let left = taskbar::marker_present();
    if left {
        crate::log!("guardian: the dock left the taskbar hidden; restoring it");
    }
    let start = Instant::now();
    let mut tries = 0u32;
    loop {
        restore_taskbar();
        if !finish_restore() {
            // Explorer never answered (finish_restore waited the limit).
            crate::log!("guardian: taskbar restore NOT confirmed (explorer not answering); marker kept, the next start restores it");
            return false;
        }
        if !taskbar::marker_present() {
            if left || tries > 0 {
                crate::log!("guardian: taskbar restored{}", if tries > 0 { format!(" after {} retries", tries) } else { String::new() });
            }
            return true;
        }
        // Back but left off its place (an empty band): retried a few
        // times, then left to the next start, which lets it go
        // (taskbar::RESEAT_LIMIT): never a restore loop forever.
        let reseats = taskbar::reseats_on_disk();
        if reseats >= taskbar::GUARDIAN_RESEATS {
            crate::log!("guardian: a taskbar window is still off its place after {reseats} restores; marker kept, the next start tries once more");
            return false;
        }
        tries += 1;
        let wait = retry_delay(tries);
        if start.elapsed() + wait >= FINAL_RESTORE_WAIT {
            crate::log!("guardian: taskbar restore NOT confirmed after {tries} tries in {} s; marker kept, the next start (or --restore-taskbar) restores it", start.elapsed().as_secs());
            return false;
        }
        if tries == 1 {
            crate::log!("guardian: taskbar restore not confirmed; retrying with a growing pause (up to {} s)", FINAL_RESTORE_WAIT.as_secs());
        }
        std::thread::sleep(wait);
    }
}

/// Before the guardian exits (or starts a new dock): every restore it
/// queued must have run, or it would die with this process. Waits up to
/// FINAL_RESTORE_WAIT; if explorer never answers, the taskbar marker stays
/// on disk and the next start (or --restore-taskbar) puts the state back.
fn finish_restore() -> bool {
    if taskbar::shell::idle() {
        return true;
    }
    crate::log!("guardian: waiting for explorer to take the taskbar restore");
    let done = taskbar::shell::wait_idle(FINAL_RESTORE_WAIT);
    if done {
        crate::log!("guardian: taskbar restore went through");
    } else {
        crate::log!("guardian: explorer never took the taskbar restore; the next start restores it");
    }
    done
}

fn relaunch() {
    match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn()) {
        Ok(child) => crate::log!("guardian: started a new dock (pid {})", child.id()),
        Err(e) => crate::log!("guardian: could not start a new dock: {e}"),
    }
}

/// The dock's main window, if `pid` has created it yet.
pub(crate) fn dock_window(pid: u32) -> Option<HWND> {
    let mut after: Option<HWND> = None;
    unsafe {
        while let Ok(h) = FindWindowExW(None, after, w!("NopBarWindow"), PCWSTR::null()) {
            let mut owner = 0u32;
            GetWindowThreadProcessId(h, Some(&mut owner));
            if owner == pid {
                return Some(h);
            }
            after = Some(h);
        }
    }
    None
}

fn prune_dumps(dir: &Path) -> std::io::Result<()> {
    let mut dumps = Vec::new();
    for entry in fs::read_dir(dir)?.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(DUMP_PREFIX) || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if name.ends_with(".dmp") {
            dumps.push(entry);
        } else if name.ends_with(".part")
            && entry.metadata().and_then(|m| m.modified()).ok()
                .and_then(|time| time.elapsed().ok()).is_some_and(|age| age >= DUMP_PART_AGE)
        {
            if let Err(e) = fs::remove_file(entry.path()) {
                crate::log!("guardian: could not remove stale minidump part: {e}");
            }
        }
    }
    dumps.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for entry in dumps.into_iter().skip(DUMP_LIMIT) {
        fs::remove_file(entry.path())?;
    }
    Ok(())
}

fn dump_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("NopBar").join("dumps"))
}

fn write_hang_dump(process: OwnedHandle, pid: u32, stage: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = dump_dir().ok_or("LOCALAPPDATA is missing")?;
    fs::create_dir_all(&dir)?;
    let time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros();
    let path = dir.join(format!("{DUMP_PREFIX}{time:020}-{pid}-{stage}.dmp"));
    let partial = path.with_extension("part");
    let file = OpenOptions::new().write(true).create_new(true).share_mode(0).open(&partial)?;
    let written = unsafe {
        MiniDumpWriteDump(
            HANDLE(process.as_raw_handle()), pid, HANDLE(file.as_raw_handle()),
            MiniDumpNormal | MiniDumpWithThreadInfo, None, None, None,
        )
    };
    drop(file);
    if let Err(e) = written.map_err(std::io::Error::other).and_then(|()| fs::rename(&partial, &path)) {
        let _ = fs::remove_file(&partial);
        return Err(e.into());
    }
    if let Err(e) = prune_dumps(&dir) {
        crate::log!("guardian: minidump cleanup failed: {e}");
    }
    Ok(path)
}

fn capture_hang(pid: u32, stage: &'static str) {
    let Some(writer) = DumpWriter::claim() else {
        crate::log!("guardian: skipping dock {pid} minidump ({stage}): previous minidump is still running");
        return;
    };
    let process = unsafe {
        OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid)
            .map(|handle| OwnedHandle::from_raw_handle(handle.0))
    };
    let Ok(process) = process else {
        crate::log!("guardian: could not open dock {pid} for minidump ({stage}): {}", process.unwrap_err());
        return;
    };
    let (done, result) = mpsc::sync_channel(1);
    let worker = std::thread::Builder::new().name("guardian-dump".into()).spawn(move || {
        let written = write_hang_dump(process, pid, stage);
        drop(writer);
        match written {
            Ok(path) => crate::log!("guardian: dock {pid} minidump ({stage}): {}", path.display()),
            Err(e) => crate::log!("guardian: dock {pid} minidump ({stage}) failed: {e}"),
        };
        let _ = done.send(());
    });
    match worker {
        Err(e) => crate::log!("guardian: minidump worker failed to start: {e}"),
        Ok(worker) if result.recv_timeout(DUMP_TIMEOUT).is_ok() => {
            let _ = worker.join();
        }
        Ok(_) => crate::log!("guardian: dock {pid} minidump ({stage}) exceeded {} ms", DUMP_TIMEOUT.as_millis()),
    }
}

pub fn run(pid: u32) {
    if let Some(dir) = dump_dir() {
        if let Err(e) = prune_dumps(&dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                crate::log!("guardian: minidump cleanup failed: {e}");
            }
        }
    }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, pid)
        else {
            final_restore();
            return;
        };
        // Which run of the dock this is: its tray icon cleanup must not
        // touch a newer dock's icon (dock/trayicon.rs).
        let started = crate::dock::process_started(h);
        let mut hung_since: Option<Instant> = None;
        let mut restored_for_hang = false;
        loop {
            if WaitForSingleObject(h, CHECK_EVERY_MS) == WAIT_OBJECT_0 {
                let mut code = 0u32;
                let _ = GetExitCodeProcess(h, &mut code);
                let _ = CloseHandle(h);
                crate::log!("guardian: dock {pid} exited with code {code:#x}{}", if is_crash(code) { " (crash)" } else { "" });
                crate::dock::recover_genie();
                // Our tray icon outlives a dock that did not remove it.
                crate::dock::remove_stale_tray_icon(crate::dock::TrayIconGone::Dock(pid, started));
                // A late restore must not run after a new dock hid the
                // taskbar again: see it through first.
                let restored = final_restore();
                if is_crash(code) {
                    if !restored {
                        crate::log!("guardian: not starting a new dock while the taskbar is not confirmed back");
                    } else if take_restart_budget() {
                        relaunch();
                    } else {
                        crate::log!("guardian: restart budget used up; leaving the taskbar restored");
                    }
                }
                return;
            }
            let Some(win) = dock_window(pid) else { continue };
            let mut result = 0usize;
            let answered = SendMessageTimeoutW(
                win,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                PING_TIMEOUT_MS,
                Some(&mut result),
            )
            .0 != 0;
            if answered {
                hung_since = None;
                if restored_for_hang {
                    // Our restore must be through first, or it could land
                    // after the dock hid the taskbar again.
                    if taskbar::shell::idle() {
                        crate::log!("guardian: dock responsive again; asking it to hide the taskbar");
                        let _ = PostMessageW(Some(win), crate::dock::WM_APPLY_TASKBAR, WPARAM(0), LPARAM(0));
                        restored_for_hang = false;
                    }
                }
                continue;
            }
            let since = *hung_since.get_or_insert_with(|| {
                crate::log!("guardian: dock {pid} not responding");
                let started = Instant::now();
                capture_hang(pid, "first");
                started
            });
            if !restored_for_hang && since.elapsed() >= HANG_RESTORE {
                restore_taskbar();
                restored_for_hang = true;
                crate::log!("guardian: dock hung ~15 s; taskbar restored");
            }
            if since.elapsed() >= HANG_RESTART {
                capture_hang(pid, "before-kill");
                crate::log!("guardian: dock hung ~30 s; ending it");
                let _ = TerminateProcess(h, 0xDEAD);
                WaitForSingleObject(h, 5_000);
                let _ = CloseHandle(h);
                crate::dock::recover_genie();
                // Our tray icon outlives a dock that did not remove it.
                crate::dock::remove_stale_tray_icon(crate::dock::TrayIconGone::Dock(pid, started));
                if !final_restore() {
                    crate::log!("guardian: not starting a new dock while the taskbar is not confirmed back");
                } else if take_restart_budget() {
                    relaunch();
                } else {
                    crate::log!("guardian: restart budget used up; leaving the taskbar restored");
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_retention_keeps_the_three_newest_guardian_dumps() {
        let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
        let dir = std::env::temp_dir().join(format!("nopbar-dump-{}-{time}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let names: Vec<_> = (0..5).map(|n| format!("{DUMP_PREFIX}{n:020}-42-first.dmp")).collect();
        for name in &names {
            fs::write(dir.join(name), []).unwrap();
        }
        fs::write(dir.join("other.dmp"), []).unwrap();
        let old = std::fs::File::create(dir.join(format!("{DUMP_PREFIX}old.part"))).unwrap();
        old.set_modified(UNIX_EPOCH).unwrap();
        drop(old);
        let fresh = format!("{DUMP_PREFIX}fresh.part");
        fs::write(dir.join(&fresh), []).unwrap();
        prune_dumps(&dir).unwrap();
        let mut remaining: Vec<_> = fs::read_dir(&dir).unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
        for name in &remaining {
            fs::remove_file(dir.join(name)).unwrap();
        }
        fs::remove_dir(&dir).unwrap();
        remaining.sort_unstable();
        let mut expected = [&[fresh], &names[2..], &["other.dmp".to_owned()]].concat();
        expected.sort_unstable();
        assert_eq!(remaining, expected);
    }

    #[test]
    fn writer_remains_claimed_until_the_worker_finishes() {
        let writer = DumpWriter::claim().unwrap();
        let (release, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = wait.recv();
            drop(writer);
        });
        assert!(DumpWriter::claim().is_none());
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(DumpWriter::claim().is_some());
    }

    #[test]
    fn crash_codes_vs_user_choices() {
        assert!(is_crash(0xC000_0409)); // Rust panic=abort (fail fast)
        assert!(is_crash(0xC000_0005)); // access violation
        assert!(!is_crash(0)); // quit from the menu
        assert!(!is_crash(1)); // taskkill /f, Task Manager
        assert!(!is_crash(0xFFFF_FFFF)); // .NET Process.Kill / Stop-Process
        assert!(!is_crash(0xDEAD)); // our own hang kill (restart decided separately)
    }

    #[test]
    fn final_restore_retries_back_off_and_stay_bounded() {
        let d: Vec<u64> = (1..=8).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(d, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        // Many tries fit in the window; it still ends (the loop checks it).
        let total: u64 = (1..=40).map(|n| retry_delay(n).as_secs()).sum();
        assert!(total > FINAL_RESTORE_WAIT.as_secs());
        assert_eq!(retry_delay(0), Duration::from_secs(1));
    }

    #[test]
    fn restart_budget_allows_three_per_ten_minutes() {
        let (ok, h) = allow_restart(&[], 1000);
        assert!(ok);
        let (ok, h) = allow_restart(&h, 1100);
        assert!(ok);
        let (ok, h) = allow_restart(&h, 1200);
        assert!(ok);
        let (ok, h) = allow_restart(&h, 1300);
        assert!(!ok, "fourth restart within 10 minutes must be refused");
        // A refused restart is not recorded.
        assert_eq!(h, vec![1000, 1100, 1200]);
        // At 1600 the 1000 entry is 10 minutes old, so there is room again.
        let (ok, h) = allow_restart(&h, 1600);
        assert!(ok);
        assert_eq!(h, vec![1100, 1200, 1600]);
    }
}
