//! Install-time pieces: packaged (Store) detection, start-at-login for both
//! builds, `--quit` (the installer's graceful stop before an upgrade) and
//! `--uninstall` (stop the dock, give the taskbar back, remove everything
//! NopBar and its direct-download installer wrote; docs/release.md).

use std::path::{Path, PathBuf};

use windows::core::{w, HSTRING};

/// Running from an MSIX package (the Store build).
pub fn is_packaged() -> bool {
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut len = 0u32;
    // ERROR_INSUFFICIENT_BUFFER (122) = has a package; 15700 = none.
    let r = unsafe { GetCurrentPackageFullName(&mut len, None) };
    r.0 == 122
}

/// The Store build's StartupTask (declared in the manifest). The async
/// calls run on their own thread (MTA); the caller, often a UI thread,
/// waits at most 800 ms.
fn startup_task(on: bool) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("startup-task".into()).spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let _ = tx.send(startup_task_now(on));
    });
    rx.recv_timeout(std::time::Duration::from_millis(800)).unwrap_or(false)
}

fn startup_task_now(on: bool) -> bool {
    use windows::ApplicationModel::{StartupTask, StartupTaskState};
    let run = || -> windows::core::Result<bool> {
        let task = StartupTask::GetAsync(&HSTRING::from("NopBarStartup"))?.join()?;
        if on {
            let s = task.RequestEnableAsync()?.join()?;
            Ok(s == StartupTaskState::Enabled || s == StartupTaskState::EnabledByPolicy)
        } else {
            task.Disable()?;
            Ok(true)
        }
    };
    run().unwrap_or(false)
}

/// Starts NopBar at login, or not. Store build: its StartupTask (the user
/// can also switch it in Windows settings). Otherwise: the HKCU Run key.
pub fn set_run_at_login(on: bool) -> bool {
    if is_packaged() {
        return startup_task(on);
    }
    run_key(on)
}

/// Whether NopBar really starts at sign-in now, as Windows sees it (the
/// Run value, unless Startup apps / Task Manager turned it off; the Store
/// build's StartupTask). None: could not tell.
pub fn run_at_login_state() -> Option<bool> {
    if is_packaged() {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new().name("startup-state".into()).spawn(move || {
            use windows::ApplicationModel::{StartupTask, StartupTaskState};
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            let s = StartupTask::GetAsync(&HSTRING::from("NopBarStartup")).and_then(|op| op.join()).and_then(|t| t.State());
            let _ = tx.send(s.ok().map(|s| s == StartupTaskState::Enabled || s == StartupTaskState::EnabledByPolicy));
        });
        return rx.recv_timeout(std::time::Duration::from_millis(800)).ok().flatten();
    }
    use windows::Win32::System::Registry::*;
    let run = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let approved = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");
    unsafe {
        let mut size = 0u32;
        let r = RegGetValueW(HKEY_CURRENT_USER, run, w!("NopBar"), RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ, None, None, Some(&mut size));
        if r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            return Some(false);
        }
        if r.is_err() {
            return None;
        }
        let mut data = [0u8; 12];
        let mut n = data.len() as u32;
        let r = RegGetValueW(HKEY_CURRENT_USER, approved, w!("NopBar"), RRF_RT_REG_BINARY, None, Some(data.as_mut_ptr() as *mut _), Some(&mut n));
        Some(startup_approved(if r.is_ok() { Some(&data[..n as usize]) } else { None }))
    }
}

/// Pure: Startup apps' record of a Run entry (none = never switched:
/// enabled). Its first byte is even when enabled (02, 06), odd when the
/// user turned it off (03, 07).
pub fn startup_approved(bytes: Option<&[u8]>) -> bool {
    bytes.and_then(|b| b.first()).is_none_or(|b| b & 1 == 0)
}

fn run_key(on: bool) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::*;
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .is_err()
        {
            return false;
        }
        let ok = if on {
            match std::env::current_exe() {
                Ok(exe) => {
                    let v: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(Some(0)).collect();
                    let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
                    RegSetValueExW(key, w!("NopBar"), None, REG_SZ, Some(bytes)).is_ok()
                }
                Err(_) => false,
            }
        } else {
            let r = RegDeleteValueW(key, w!("NopBar"));
            r.is_ok() || r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND
        };
        let _ = RegCloseKey(key);
        ok
    }
}

/// Other NopBar processes (guardian, settings window, a dock that would
/// not quit): anything running from this exe or from a build copy under
/// `%LOCALAPPDATA%\NopBar\bin\` (early test builds ran from there).
fn our_processes() -> Vec<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    let me = std::process::id();
    let this = std::env::current_exe().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
    let copies = std::env::var("LOCALAPPDATA").map(|l| format!(r"{}\nopbar\bin\", l.to_lowercase())).unwrap_or_default();
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            if e.th32ProcessID != me {
                if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID) {
                    let mut buf = [0u16; 1024];
                    let mut len = buf.len() as u32;
                    if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                        let path = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
                        if path == this || (!copies.is_empty() && path.starts_with(&copies)) {
                            out.push(e.th32ProcessID);
                        }
                    }
                    let _ = windows::Win32::Foundation::CloseHandle(h);
                }
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    out
}

fn terminate(pid: u32) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) else { return false };
        let ok = TerminateProcess(h, 1).is_ok() && WaitForSingleObject(h, 3000) == windows::Win32::Foundation::WAIT_OBJECT_0;
        let _ = windows::Win32::Foundation::CloseHandle(h);
        ok
    }
}

// ---------------------------------------------------------------- command line

/// The command-line modes `main` hands to this module.
#[derive(Debug, PartialEq, Eq)]
pub enum Cli {
    /// `--quit`: ask a running dock to quit, wait up to 5 s (the installer
    /// runs this before it replaces the files).
    Quit,
    WidgetsAdd,
    /// `--uninstall [--remove-data] [--quiet]`.
    Uninstall { remove_data: bool, quiet: bool },
    /// Anything else (the dock itself, `--settings`, `--guardian`, ...).
    Other,
}

/// Reads the mode from the whole argv (`args[0]` is the exe). Only the
/// first argument picks the mode; flags after it may come in any order.
pub fn parse_cli(args: &[String]) -> Cli {
    let has = |f: &str| args.iter().skip(2).any(|a| a.eq_ignore_ascii_case(f));
    match args.get(1).map(|a| a.to_ascii_lowercase()).as_deref() {
        Some("--quit") => Cli::Quit,
        Some("--widgets-add") => Cli::WidgetsAdd,
        Some("--uninstall") => Cli::Uninstall { remove_data: has("--remove-data"), quiet: has("--quiet") },
        _ => Cli::Other,
    }
}

/// How long `--quit` waits for the dock to go.
pub const QUIT_WAIT_SECS: u64 = 5;

/// `--quit`: 0 = no dock was running, or it quit in time; 1 = still running.
/// A hung dock never answers its pipe, so the whole request is bounded too.
pub fn quit_running() -> i32 {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("quit".into()).spawn(move || {
        let _ = tx.send(crate::settings::stop_running_dock(QUIT_WAIT_SECS));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(QUIT_WAIT_SECS + 1)) {
        Ok(true) => 0,
        _ => 1,
    }
}

// ---------------------------------------------------------------- uninstall

/// The direct-download installer's Apps & features entry (HKCU).
pub const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\NopBar";
/// Its Start menu shortcut, under %APPDATA% (packaging/inno/nopbar.iss).
pub const SHORTCUT: &str = "Microsoft\\Windows\\Start Menu\\Programs\\NopBar.lnk";
/// Every file the installer puts in the install folder (the [Files] of
/// packaging/inno/nopbar.iss; a test keeps the two lists equal). Only
/// these are deleted, then the folder if it is empty.
pub const INSTALLED_FILES: &[&str] = &["nopbar.exe", "nopbar-mcp.exe", "LICENSE.txt"];

/// What `--uninstall` removes, in order.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Step {
    /// Quit the dock, end every other NopBar process.
    StopNopBar,
    /// Give the taskbar back (and drop a tray icon a killed dock left).
    RestoreTaskbar,
    /// `HKCU\Software\Classes\AppUserModelId\NopBar.NopBar` (toasts).
    RemoveAumid,
    /// DesktopBackground shell verb.
    RemoveWidgetVerb,
    /// The HKCU Run value, or the Store build's StartupTask.
    RemoveRunAtLogin,
    /// NopBar's hooks in Claude Code's settings and its notify entry in
    /// Codex's config (this exe's, and any whose exe is gone:
    /// agents::connect::remove_on_uninstall), so the agents never run a
    /// program that is not there.
    RemoveAgentHooks,
    /// The installed files (moved out at once, see `remove_install_dir`),
    /// then the folder if empty.
    RemoveInstallDir(PathBuf),
    /// This is the installed copy, but its folder is not one files are
    /// removed from (a drive root, a relative path): everything of the
    /// installer's stays registered, and the uninstall reports it.
    KeepInstallDir(PathBuf),
    /// The installer's Start menu shortcut (only after the files are gone).
    RemoveShortcut(PathBuf),
    /// The installer's Apps & features entry ([`UNINSTALL_KEY`]), likewise.
    RemoveUninstallKey,
    /// Settings and local data (`--remove-data` only).
    RemoveData(PathBuf),
}

/// What the plan depends on, read from the PC by [`UninstallEnv::current`].
#[derive(Debug, Clone, Default)]
pub struct UninstallEnv {
    /// This exe.
    pub exe: PathBuf,
    /// %APPDATA%.
    pub appdata: Option<PathBuf>,
    /// The data folders `--remove-data` deletes.
    pub data_dirs: Vec<PathBuf>,
    /// The installer's `InstallLocation`, when it registered one.
    pub install_location: Option<PathBuf>,
    /// `<install_location>\nopbar.exe` exists.
    pub installed_exe_exists: bool,
}

impl UninstallEnv {
    pub fn current(exe: PathBuf) -> Self {
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
        let local = std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("NopBar"));
        let install_location = install_location();
        let installed_exe_exists = install_location.as_ref().is_some_and(|d| d.join("nopbar.exe").is_file());
        UninstallEnv {
            exe,
            appdata,
            data_dirs: [Some(crate::config::app_dir()), local].into_iter().flatten().collect(),
            install_location,
            installed_exe_exists,
        }
    }
}

/// Lowercase, backslashes, no trailing separator: for comparing folders.
fn norm(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// A folder the installer could have made: absolute (drive or UNC), not a
/// root, no `.`/`..`. Only the installer's own files are deleted from it
/// and the folder only when empty, so depth does not matter (`C:\NopBar`
/// from `/DIR=` is fine).
fn deletable_dir(dir: &Path) -> bool {
    use std::path::Component;
    let parts: Vec<_> = dir.components().collect();
    matches!(parts.first(), Some(Component::Prefix(_)))
        && matches!(parts.get(1), Some(Component::RootDir))
        && parts.len() >= 3
        && parts.iter().skip(2).all(|c| matches!(c, Component::Normal(_)))
}

/// What `--uninstall` will remove, in order. The installer's pieces go only
/// when this is the installed copy, or when the installed copy is already
/// gone (leftovers), so a portable or dev build's uninstall never breaks a
/// direct-download install. The files go first: the shortcut and the
/// Apps & features entry are removed only after them (at run time: only if
/// that worked), so a failed uninstall can be run again from Apps & features.
pub fn uninstall_plan(env: &UninstallEnv, remove_data: bool) -> Vec<Step> {
    // The agents' hooks first: no new hook process of this exe starts
    // while the running ones are ended and the files removed.
    let mut plan = vec![Step::RemoveAgentHooks, Step::StopNopBar, Step::RestoreTaskbar, Step::RemoveAumid, Step::RemoveWidgetVerb, Step::RemoveRunAtLogin];
    if let Some(loc) = &env.install_location {
        let here = env.exe.parent().is_some_and(|d| norm(d) == norm(loc));
        if here && !deletable_dir(loc) {
            // Its files cannot be removed: keep the registration.
            plan.push(Step::KeepInstallDir(loc.clone()));
        } else if here || !env.installed_exe_exists {
            if here {
                plan.push(Step::RemoveInstallDir(loc.clone()));
            }
            if let Some(appdata) = &env.appdata {
                plan.push(Step::RemoveShortcut(appdata.join(SHORTCUT)));
            }
            plan.push(Step::RemoveUninstallKey);
        }
    }
    if remove_data {
        plan.extend(env.data_dirs.iter().cloned().map(Step::RemoveData));
    }
    plan
}

/// The installer's `InstallLocation` value (HKCU), if any.
pub(crate) fn install_location() -> Option<PathBuf> {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY), w!("InstallLocation"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    if r.is_err() {
        return None;
    }
    let len = (size as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..len]);
    let s = s.trim_end_matches('\0').trim();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// After an automatic update of the installed copy in `install`: Apps &
/// features shows the new version (only when the installer registered
/// that very folder).
pub fn set_display_version(install: &Path, version: &str) {
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    if install_location().is_none_or(|loc| norm(&loc) != norm(install)) {
        return;
    }
    let v: Vec<u16> = version.encode_utf16().chain(Some(0)).collect();
    let r = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY), w!("DisplayVersion"), REG_SZ.0, Some(v.as_ptr().cast()), (v.len() * 2) as u32) };
    if r.is_err() {
        crate::log!("update: Apps & features version not updated ({r:?})");
    }
}

fn remove_uninstall_key() -> bool {
    use windows::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};
    let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY)) };
    r.is_ok() || r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND
}

// ---------------------------------------------------------------- install folder removal

/// An open handle to a file or folder itself (never what a reparse point
/// points to), closed on drop.
struct Pinned(windows::Win32::Foundation::HANDLE);

impl Drop for Pinned {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Which file it is (volume + file index) and its attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    volume: u32,
    index: u64,
    attributes: u32,
}

impl FileIdentity {
    fn is_dir(&self) -> bool {
        self.attributes & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY.0 != 0
    }
    fn is_reparse(&self) -> bool {
        self.attributes & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }
    fn same_file(&self, other: &FileIdentity) -> bool {
        self.volume == other.volume && self.index == other.index
    }
}

/// Opens `path` itself (FILE_FLAG_OPEN_REPARSE_POINT: a link is opened as
/// the link). `share_delete` false keeps anyone from renaming or deleting
/// it while the handle is open.
fn pin(path: &Path, share_delete: bool) -> Result<(Pinned, FileIdentity), String> {
    use windows::Win32::Storage::FileSystem::*;
    let mut share = FILE_SHARE_READ | FILE_SHARE_WRITE;
    if share_delete {
        share |= FILE_SHARE_DELETE;
    }
    unsafe {
        let h = CreateFileW(
            &HSTRING::from(path.as_os_str()),
            FILE_READ_ATTRIBUTES.0,
            share,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        let pinned = Pinned(h);
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        GetFileInformationByHandle(h, &mut info).map_err(|e| format!("{}: {e}", path.display()))?;
        let id = FileIdentity {
            volume: info.dwVolumeSerialNumber,
            index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            attributes: info.dwFileAttributes,
        };
        Ok((pinned, id))
    }
}

/// A regular file (not a folder, not a link or junction).
fn plain_file(path: &Path) -> Result<FileIdentity, String> {
    let (_h, id) = pin(path, true)?;
    if id.is_dir() || id.is_reparse() {
        return Err(format!("{}: not a plain file, left alone", path.display()));
    }
    Ok(id)
}

/// A name for the private folder the installed files are moved into.
fn quarantine_name() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("nopbar-uninstall-{}-{nanos}", std::process::id())
}

/// The cmd.exe arguments that, run *inside* the quarantine folder, retry
/// deleting the moved nopbar.exe until this process has exited (up to
/// ~20 s), then remove the (by then empty) quarantine folder. No path is
/// on the command line, only the fixed exe name and the quarantine name
/// (ASCII letters, digits, `-`), and delayed expansion is off (/v:off), so
/// nothing in the user's paths can change what is deleted. None for any
/// other name.
pub fn cleanup_command(quarantine: &str) -> Option<String> {
    if quarantine.is_empty() || !quarantine.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    Some(format!(
        "/d /v:off /s /c \"(for /l %i in (1,1,20) do @(if exist nopbar.exe (ping -n 2 127.0.0.1 >nul & del /a:-l /f /q nopbar.exe 2>nul))) & cd .. & rmdir {quarantine}\""
    ))
}

/// Removes the installed files now, safely, and the folder if that leaves
/// it empty:
/// 1. the folder is pinned open (it cannot be renamed, swapped or deleted
///    meanwhile) and must be a real folder, not a junction or link;
/// 2. every installed file must be a regular file, and nopbar.exe must be
///    this very exe (same file ID): a newer install in its place is left
///    alone;
/// 3. they are moved into a fresh private folder (in %TEMP%, or next to the
///    install folder on another volume) and their IDs checked again there;
///    from then on nothing touches the install path, so a reinstall that
///    lands there a second later is safe;
/// 4. LICENSE.txt is deleted, the install folder removed if empty;
/// 5. the moved exe (still running: it cannot delete itself) is deleted by
///    a hidden cmd.exe working inside the private folder once this process
///    has exited; if that cannot start, it is marked for deletion at
///    restart (needs admin; otherwise it stays in %TEMP%).
/// Ok = nothing of the installer's is left in the install folder.
fn remove_install_dir(dir: &Path, this_exe: &Path) -> Result<String, String> {
    let (dir_pin, dir_id) = pin(dir, false)?;
    if !dir_id.is_dir() || dir_id.is_reparse() {
        return Err(format!("{}: not a plain folder, left alone", dir.display()));
    }
    let mut ids = Vec::new();
    for f in INSTALLED_FILES {
        let p = dir.join(f);
        match plain_file(&p) {
            Ok(id) => ids.push((*f, id)),
            Err(_) if !p.exists() && std::fs::symlink_metadata(&p).is_err() => {}
            Err(e) => return Err(e),
        }
    }
    let me = plain_file(this_exe)?;
    if let Some((_, exe)) = ids.iter().find(|(f, _)| *f == "nopbar.exe") {
        if !exe.same_file(&me) {
            return Err(format!("{}: a different nopbar.exe (newer install?), left alone", dir.display()));
        }
    }
    // A fresh private folder on the same volume (a rename cannot cross volumes).
    let name = quarantine_name();
    let mut quarantine = std::env::temp_dir().join(&name);
    std::fs::create_dir(&quarantine).map_err(|e| format!("{}: {e}", quarantine.display()))?;
    let mut moved = Vec::new();
    for (f, id) in &ids {
        let from = dir.join(f);
        let mut r = std::fs::rename(&from, quarantine.join(f));
        if moved.is_empty() && matches!(&r, Err(e) if e.raw_os_error() == Some(17)) {
            // ERROR_NOT_SAME_DEVICE: %TEMP% is on another volume.
            let _ = std::fs::remove_dir(&quarantine);
            quarantine = dir.parent().ok_or("no parent folder")?.join(&name);
            std::fs::create_dir(&quarantine).map_err(|e| format!("{}: {e}", quarantine.display()))?;
            r = std::fs::rename(&from, quarantine.join(f));
        }
        r.map_err(|e| format!("{}: {e}", from.display()))?;
        // What was moved must be what was checked.
        if plain_file(&quarantine.join(f)).ok().as_ref().is_some_and(|q| q.same_file(id)) {
            moved.push(*f);
        } else {
            return Err(format!("{}: changed while moving, left in {}", f, quarantine.display()));
        }
    }
    for f in moved.iter().filter(|f| **f != "nopbar.exe") {
        let _ = std::fs::remove_file(quarantine.join(f));
    }
    // The folder: only if empty (anything the user put there stays).
    drop(dir_pin);
    let folder = match std::fs::remove_dir(dir) {
        Ok(()) => "folder removed".to_string(),
        Err(e) => format!("folder kept ({e})"),
    };
    let exe_note = if moved.contains(&"nopbar.exe") {
        schedule_exe_deletion(&quarantine, &name)
    } else {
        let _ = std::fs::remove_dir(&quarantine);
        "no exe".into()
    };
    Ok(format!("files moved out, {folder}, {exe_note}"))
}

fn schedule_exe_deletion(quarantine: &Path, name: &str) -> String {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if let Some(args) = cleanup_command(name) {
        let cmd = std::env::var_os("SystemRoot").map(|r| PathBuf::from(r).join("System32\\cmd.exe")).unwrap_or_else(|| "cmd.exe".into());
        let started = std::process::Command::new(cmd).raw_arg(&args).current_dir(quarantine).creation_flags(CREATE_NO_WINDOW).spawn().is_ok();
        if started {
            return format!("exe deletion scheduled in {}", quarantine.display());
        }
    }
    unsafe {
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};
        let exe = quarantine.join("nopbar.exe");
        let _ = MoveFileExW(&HSTRING::from(exe.as_os_str()), None, MOVEFILE_DELAY_UNTIL_REBOOT);
        let _ = MoveFileExW(&HSTRING::from(quarantine.as_os_str()), None, MOVEFILE_DELAY_UNTIL_REBOOT);
    }
    format!("exe left in {} (deleted at restart where allowed)", quarantine.display())
}

/// `--uninstall [--remove-data] [--quiet]`: run by Apps & features (the
/// direct-download installer's entry) or by hand. It never shows UI (`--quiet` is
/// accepted for Apps & features / winget conventions); the outcome goes to
/// `%TEMP%\nopbar-uninstall.log`. Settings are deleted only when asked
/// explicitly, and only once every NopBar process is gone. Returns the
/// process exit code (0 = everything done).
pub fn uninstall(remove_data: bool) -> i32 {
    let env = std::env::current_exe().map(UninstallEnv::current).unwrap_or_default();
    let plan = uninstall_plan(&env, remove_data);
    let mut failed = Vec::new();
    let mut removed = Vec::new();
    let (mut quit, mut gone) = (false, false);
    // False once the installed files could not be removed: then the
    // shortcut and Apps & features entry stay, to uninstall again later.
    let mut files_gone = true;
    for step in &plan {
        if !files_gone && matches!(step, Step::RemoveShortcut(_) | Step::RemoveUninstallKey) {
            failed.push(format!("{step:?} skipped: the installed files are still there"));
            continue;
        }
        match step {
            Step::StopNopBar => {
                // Ask the dock to quit (it gives the taskbar back itself);
                // give the guardian a moment to see it go. Whatever is left
                // (guardian, settings window, a hung dock) is ended.
                quit = crate::settings::stop_running_dock(8);
                std::thread::sleep(std::time::Duration::from_millis(1500));
                for pid in our_processes() {
                    if !terminate(pid) {
                        failed.push(format!("process {pid} still running"));
                    }
                }
                gone = crate::settings::stop_running_dock(1) && our_processes().is_empty();
                if !gone {
                    failed.push("dock still running".into());
                } else if let Err(e) = crate::companion::chat::credentials::purge() {
                    failed.push(e.into());
                }
            }
            Step::RestoreTaskbar => {
                // After nothing can hide it again. Not confirmed back while
                // a hide of ours is on record: try once more, then stop the
                // uninstall here, keeping the files, registration and data
                // (the saved state, the marker, --restore-taskbar) until it
                // is; uninstalling again later finishes the job.
                let back = |ok: bool| ok || !crate::taskbar::marker_present();
                let mut ok = back(crate::taskbar::restore_before_exit(std::time::Duration::from_secs(30)));
                if !ok {
                    ok = back(crate::taskbar::restore_before_exit(std::time::Duration::from_secs(30)));
                }
                crate::dock::remove_stale_tray_icon(crate::dock::TrayIconGone::Any);
                if !ok {
                    failed.push("the taskbar could not be restored; uninstall stopped (nothing else removed), run it again".into());
                    break;
                }
            }
            Step::RemoveAumid => crate::updates::unregister_aumid(),
            Step::RemoveWidgetVerb => {
                if !crate::desklayer::shell::remove_current(){failed.push("widget shell menu not removed".into());}
            },
            Step::RemoveRunAtLogin => {
                if !set_run_at_login(false) {
                    failed.push("start-at-login not removed".into());
                }
            }
            Step::RemoveAgentHooks => {
                failed.extend(crate::companion::chat::connect::uninstall());
                let (n, errors) = crate::agents::connect::remove_on_uninstall(&crate::agents::connect::Places::of_this_pc(),
                );
                if n > 0 {
                    removed.push(format!("agent hooks ({n} file(s))"));
                }
                failed.extend(errors);
            }
            Step::KeepInstallDir(dir) => {
                files_gone = false;
                failed.push(format!("{}: not a folder NopBar removes files from", dir.display()));
            }
            Step::RemoveInstallDir(dir) => {
                let r = if gone { remove_install_dir(dir, &env.exe) } else { Err(format!("{} kept: NopBar did not stop", dir.display())) };
                match r {
                    Ok(how) => removed.push(format!("{}: {how}", dir.display())),
                    Err(e) => {
                        files_gone = false;
                        failed.push(e);
                    }
                }
            }
            Step::RemoveShortcut(p) => match std::fs::remove_file(p) {
                Ok(()) => removed.push(p.display().to_string()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => failed.push(format!("{}: {e}", p.display())),
            },
            Step::RemoveUninstallKey => {
                if remove_uninstall_key() {
                    removed.push(format!("HKCU\\{UNINSTALL_KEY}"));
                } else {
                    failed.push("Apps & features entry not removed".into());
                }
            }
            Step::RemoveData(dir) => {
                if !gone {
                    failed.push(format!("{} kept: NopBar did not stop", dir.display()));
                } else if dir.exists() {
                    match std::fs::remove_dir_all(dir) {
                        Ok(()) => removed.push(dir.display().to_string()),
                        Err(e) => failed.push(format!("{}: {e}", dir.display())),
                    }
                }
            }
        }
    }
    let report = format!("NopBar uninstall: plan={plan:?}, quit={quit}, all stopped={gone}, removed={removed:?}, problems={failed:?}");
    println!("{report}");
    // Leave a trace the user can read (the data folder may be gone).
    let _ = std::fs::write(std::env::temp_dir().join("nopbar-uninstall.log"), &report);
    if failed.is_empty() { 0 } else { 1 }
}

/// The product used to be called NopDock: carry its data, start-at-login
/// and startup shortcuts over to NopBar once (only when NopBar has none).
pub fn migrate_from_nopdock() {
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var).map(std::path::PathBuf::from) {
            let (old, new) = (base.join("NopDock"), base.join("NopBar"));
            if old.is_dir() && !new.exists() {
                let _ = std::fs::rename(&old, &new);
            }
        }
    }
    if remove_run_value(w!("NopDock")) {
        let _ = set_run_at_login(true);
    }
    if let Some(dir) = std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join(r"Microsoft\Windows\Start Menu\Programs\Startup")) {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(rest) = name.strip_prefix("NopDock - ") {
                let _ = std::fs::rename(e.path(), dir.join(format!("NopBar - {rest}")));
            }
        }
    }
}

/// Deletes a Run value; true if it was there.
fn remove_run_value(name: windows::core::PCWSTR) -> bool {
    use windows::Win32::System::Registry::*;
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"), None, KEY_QUERY_VALUE | KEY_SET_VALUE, &mut key).is_err() {
            return false;
        }
        let had = RegQueryValueExW(key, name, None, None, None, None).is_ok();
        if had {
            let _ = RegDeleteValueW(key, name);
        }
        let _ = RegCloseKey(key);
        had
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_apps_switch_is_read() {
        assert!(startup_approved(None), "never switched: on");
        assert!(startup_approved(Some(&[2, 0, 0, 0])));
        assert!(startup_approved(Some(&[6, 0])));
        assert!(!startup_approved(Some(&[3, 0, 0, 0])), "turned off in Startup apps");
        assert!(!startup_approved(Some(&[7])));
        assert!(startup_approved(Some(&[])));
    }

    fn args(a: &[&str]) -> Vec<String> {
        std::iter::once("nopbar.exe").chain(a.iter().copied()).map(String::from).collect()
    }

    #[test]
    fn cli_modes() {
        assert_eq!(parse_cli(&args(&["--quit"])), Cli::Quit);
        assert_eq!(parse_cli(&args(&["--QUIT"])), Cli::Quit);
        assert_eq!(parse_cli(&args(&["--uninstall"])), Cli::Uninstall { remove_data: false, quiet: false });
        assert_eq!(parse_cli(&args(&["--uninstall", "--quiet", "--remove-data"])), Cli::Uninstall { remove_data: true, quiet: true });
        assert_eq!(parse_cli(&args(&["--uninstall", "--remove-data"])), Cli::Uninstall { remove_data: true, quiet: false });
        // Flags only count after the mode; a mode later on does not count.
        assert_eq!(parse_cli(&args(&["--remove-data", "--uninstall"])), Cli::Other);
        assert_eq!(parse_cli(&args(&["--settings", "--quit"])), Cli::Other);
        assert_eq!(parse_cli(&args(&[])), Cli::Other);
        assert_eq!(parse_cli(&[]), Cli::Other);
    }

    const LOC: &str = r"C:\Users\u\AppData\Local\Programs\NopBar";

    fn env(exe: &str, loc: Option<&str>, installed: bool) -> UninstallEnv {
        UninstallEnv {
            exe: PathBuf::from(exe),
            appdata: Some(PathBuf::from(r"C:\Users\u\AppData\Roaming")),
            data_dirs: vec![PathBuf::from(r"C:\Users\u\AppData\Roaming\NopBar"), PathBuf::from(r"C:\Users\u\AppData\Local\NopBar")],
            install_location: loc.map(PathBuf::from),
            installed_exe_exists: installed,
        }
    }

    const BASE: [Step; 6] = [Step::RemoveAgentHooks, Step::StopNopBar, Step::RestoreTaskbar, Step::RemoveAumid, Step::RemoveWidgetVerb, Step::RemoveRunAtLogin];

    #[test]
    fn installed_copy_removes_files_first_then_registration() {
        let e = env(r"c:\users\u\appdata\local\programs\nopbar\NOPBAR.EXE", Some(&format!(r"{LOC}\")), true);
        let mut want = BASE.to_vec();
        want.push(Step::RemoveInstallDir(PathBuf::from(format!(r"{LOC}\"))));
        want.push(Step::RemoveShortcut(PathBuf::from(r"C:\Users\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\NopBar.lnk")));
        want.push(Step::RemoveUninstallKey);
        assert_eq!(uninstall_plan(&e, false), want);
        // Data only on request, and last.
        let plan = uninstall_plan(&e, true);
        assert_eq!(plan.len(), want.len() + 2);
        assert_eq!(plan[want.len()..], [Step::RemoveData(e.data_dirs[0].clone()), Step::RemoveData(e.data_dirs[1].clone())]);
    }

    #[test]
    fn other_builds_leave_a_working_install_alone() {
        // Portable copy, dev build: the direct-download install still works.
        for exe in [r"C:\Users\u\AppData\Local\NopBar\bin\0.1.0\nopbar.exe", r"D:\src\nopbar\target\release\nopbar.exe"] {
            assert_eq!(uninstall_plan(&env(exe, Some(LOC), true), false), BASE.to_vec());
        }
        // No installer at all.
        assert_eq!(uninstall_plan(&env(r"D:\x\nopbar.exe", None, false), false), BASE.to_vec());
    }

    #[test]
    fn leftovers_of_a_deleted_install_are_cleaned() {
        let plan = uninstall_plan(&env(r"D:\x\nopbar.exe", Some(LOC), false), false);
        assert!(plan.contains(&Step::RemoveUninstallKey));
        assert!(plan.iter().any(|s| matches!(s, Step::RemoveShortcut(_))));
        // Not this exe's folder: never touched.
        assert!(!plan.iter().any(|s| matches!(s, Step::RemoveInstallDir(_) | Step::KeepInstallDir(_))));
    }

    #[test]
    fn odd_folders_keep_everything_registered() {
        // The installed copy, but no files are removed from these: the
        // shortcut and Apps & features entry stay too.
        for loc in [r"C:\", r"C:\Users\..\Windows", r"Programs\NopBar"] {
            let exe = format!(r"{}\nopbar.exe", loc.trim_end_matches('\\'));
            let plan = uninstall_plan(&env(&exe, Some(loc), true), false);
            let mut want = BASE.to_vec();
            want.push(Step::KeepInstallDir(PathBuf::from(loc)));
            assert_eq!(plan, want, "{loc}");
        }
        // Same rule as the installer: any folder below a root (/DIR=C:\NopBar too).
        for ok in [LOC, r"D:\Apps\NopBar", r"C:\NopBar", r"\\server\share\NopBar"] {
            assert!(deletable_dir(Path::new(ok)), "{ok}");
        }
    }

    #[test]
    fn cleanup_command_names_no_path() {
        let c = cleanup_command("nopbar-uninstall-123-456").unwrap();
        assert_eq!(
            c,
            r#"/d /v:off /s /c "(for /l %i in (1,1,20) do @(if exist nopbar.exe (ping -n 2 127.0.0.1 >nul & del /a:-l /f /q nopbar.exe 2>nul))) & cd .. & rmdir nopbar-uninstall-123-456""#
        );
        assert!(!c.contains('\\'), "no paths");
        assert!(!c.contains("/s /q"), "rmdir must not be recursive");
        for bad in ["", "a b", "x!y!", "a%b", "a\"b", r"..\x", "a&b", "a^b", "a|b", "a(b)", "김"] {
            assert_eq!(cleanup_command(bad), None, "{bad:?}");
        }
    }

    /// A scratch folder under target\ (never outside the repo).
    fn scratch(name: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(r"..\target\test-scratch").join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_different_exe_in_the_folder_is_left_alone() {
        // A newer install landed there: its nopbar.exe is not this exe.
        let d = scratch("newer");
        let dir = d.join("NopBar");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("nopbar.exe"), b"newer").unwrap();
        std::fs::write(dir.join("LICENSE.txt"), b"mit").unwrap();
        std::fs::write(d.join("me.exe"), b"older").unwrap();
        let r = remove_install_dir(&dir, &d.join("me.exe"));
        assert!(r.as_ref().is_err_and(|e| e.contains("different nopbar.exe")), "{r:?}");
        assert!(dir.join("nopbar.exe").is_file() && dir.join("LICENSE.txt").is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_plain_files_are_touched() {
        // LICENSE.txt is a folder (a junction is refused the same way: both
        // are not plain files): nothing is moved or deleted.
        let d = scratch("plain");
        let dir = d.join("NopBar");
        std::fs::create_dir_all(dir.join("LICENSE.txt")).unwrap();
        std::fs::write(dir.join("LICENSE.txt").join("keep.txt"), b"keep").unwrap();
        std::fs::write(dir.join("nopbar.exe"), b"exe").unwrap();
        let r = remove_install_dir(&dir, &dir.join("nopbar.exe"));
        assert!(r.as_ref().is_err_and(|e| e.contains("not a plain file")), "{r:?}");
        assert!(dir.join("LICENSE.txt").join("keep.txt").is_file() && dir.join("nopbar.exe").is_file());
        // The folder itself must be a plain folder too.
        let r = remove_install_dir(&dir.join("nopbar.exe"), &dir.join("nopbar.exe"));
        assert!(r.as_ref().is_err_and(|e| e.contains("not a plain folder")), "{r:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The installer script and this module must agree on what is installed
    /// and on the AppUserModelID of the shortcut (toasts use it).
    #[test]
    fn installer_script_matches() {
        let iss = include_str!("../../packaging/inno/nopbar.iss");
        let mut dest: Vec<String> = iss
            .lines()
            .filter(|l| l.trim_start().starts_with("Source:"))
            .map(|l| {
                let name = l.split("DestName:").nth(1).expect("every [Files] line names its DestName");
                name.split(';').next().unwrap().trim().trim_matches('"').to_string()
            })
            .collect();
        dest.sort();
        let mut ours: Vec<String> = INSTALLED_FILES.iter().map(|s| s.to_string()).collect();
        ours.sort();
        assert_eq!(dest, ours);
        assert!(iss.contains(&format!("AppUserModelID: \"{}\"", crate::updates::AUMID)));
        assert!(iss.lines().any(|line| line == "ChangesAssociations=yes"), "Explorer refreshes its icon cache after install");
        assert!(iss.contains(r#"Name: "{userprograms}\NopBar""#), "shortcut path must match SHORTCUT");
        assert!(iss.contains(&format!("#define UninstallKey \"{UNINSTALL_KEY}\"")));
        assert!(iss.contains(r#"""{app}\nopbar.exe"" --uninstall"#));
    }
}

#[cfg(test)] mod widget_shell_tests {
 use super::*;
 #[test] fn review_13_widgets_cli_is_a_request_only_mode(){
  let args=|s:&[&str]|std::iter::once("nopbar.exe").chain(s.iter().copied()).map(str::to_string).collect::<Vec<_>>();
  assert_eq!(parse_cli(&args(&["--widgets-add"])),Cli::WidgetsAdd);
  assert_eq!(parse_cli(&args(&["--WIDGETS-ADD"])),Cli::WidgetsAdd);
  assert_eq!(parse_cli(&args(&["--settings","--widgets-add"])),Cli::Other);
 }
}

#[cfg(test)] mod widget_uninstall_tests {
 use super::*;
 #[test] fn review_13_uninstall_and_purge_plans_remove_widget_verb(){
  for data in [false,true] {
   let plan=uninstall_plan(&UninstallEnv::default(),data);
   assert_eq!(plan.iter().filter(|s|**s==Step::RemoveWidgetVerb).count(),1);
   assert!(plan.iter().position(|s|*s==Step::RestoreTaskbar)<plan.iter().position(|s|*s==Step::RemoveWidgetVerb));
  }
 }
}
