//! Cooperative app shutdown. Register process identities, never files or
//! services: RM must not expand the request to unrelated resource users.

use super::hung;
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc, Mutex, OnceLock, Condvar};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_FAIL_SHUTDOWN, ERROR_SUCCESS, FILETIME, HANDLE, HWND, LPARAM, WIN32_ERROR};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::RestartManager::{RmCancelCurrentTask, RmEndSession, RmRegisterResources, RmShutdown, RmStartSession, RM_UNIQUE_PROCESS};
use windows::Win32::System::Threading::{GetProcessTimes, GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetLastActivePopup, GetWindow, GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible, SetForegroundWindow, GW_OWNER};

const LIMIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Availability {
    #[default]
    None,
    Pending,
    Ready,
    Denied,
}

#[derive(Clone, Debug)]
struct Process {
    pid: u32,
    parent: u32,
    start: u64,
    exe: String,
    protected: bool,
    allowed: bool,
    gui: bool,
    aumid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Target {
    exe: String,
    aumid: Option<String>,
    descendants: bool,
}

fn under(path: &str, dir: &str) -> bool {
    !dir.is_empty() && path.starts_with(&format!("{}\\", path_key(dir).trim_end_matches('\\')))
}

fn safe_exe(exe: &str, windir: &str) -> bool {
    let key = path_key(exe);
    let name = key.rsplit('\\').next().unwrap_or("");
    const HOSTS: &[&str] = &["applicationframehost.exe", "java.exe", "javaw.exe", "python.exe", "pythonw.exe", "node.exe",
        "msedgewebview2.exe", "dllhost.exe", "rundll32.exe", "conhost.exe", "wscript.exe", "cscript.exe", "mshta.exe",
        "electron.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "wt.exe", "windowsterminal.exe", "bash.exe", "wsl.exe"];
    !key.is_empty() && !under(&key, windir) && !key.contains("\\windowsapps\\") && !HOSTS.contains(&name)
        && !hung::protected(100, Some(&key), Some(1), 101)
}

fn windows_dir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let mut buf = [0u16; 32768];
        let n = unsafe { windows::Win32::System::SystemInformation::GetWindowsDirectoryW(Some(&mut buf)) } as usize;
        if n > 0 && n < buf.len() { path_key(&String::from_utf16_lossy(&buf[..n])) } else { r"c:\windows".into() }
    })
}

fn app_folder(exe: &str, roots: &[String]) -> bool {
    let key = path_key(exe);
    let Some((folder, _)) = key.rsplit_once('\\') else { return false };
    // Never expand a bare Program Files/AppData directory into its children.
    roots.iter().any(|root| under(folder, root))
}

pub(super) fn target(exe: &str, aumid: Option<&str>, web: bool, packaged: bool) -> Option<Target> {
    if web || packaged || aumid.is_some_and(crate::webapps::is_web_app) || !safe_exe(exe, windows_dir()) { return None; }
    let roots: Vec<_> = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "LOCALAPPDATA", "APPDATA"].iter()
        .filter_map(std::env::var_os).map(|s| path_key(&s.to_string_lossy())).collect();
    Some(Target { exe: path_key(exe), aumid: aumid.filter(|s| !s.is_empty()).map(str::to_lowercase), descendants: app_folder(exe, &roots) })
}

fn matches_identity(target: &Target, p: &Process) -> bool {
    !p.protected && p.gui && safe_exe(&p.exe, windows_dir())
        && target.aumid.as_ref().is_none_or(|a| p.aumid.as_ref().is_some_and(|b| a.eq_ignore_ascii_case(b)))
}

// Read only the bounded PE headers, off the UI thread. Unknown images fail closed.
fn gui_image(exe: &str) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    (|| -> std::io::Result<bool> {
        let mut f = std::fs::File::open(exe)?;
        let mut dos = [0u8; 64];
        f.read_exact(&mut dos)?;
        if &dos[..2] != b"MZ" { return Ok(false); }
        let pe = u32::from_le_bytes(dos[60..64].try_into().unwrap());
        if !(64..=1024 * 1024).contains(&pe) { return Ok(false); }
        f.seek(SeekFrom::Start(pe as u64))?;
        let mut header = [0u8; 94];
        f.read_exact(&mut header)?;
        Ok(gui_header(&header))
    })().unwrap_or(false)
}

fn gui_header(h: &[u8; 94]) -> bool {
    &h[..4] == b"PE\0\0" && u16::from_le_bytes([h[20], h[21]]) >= 70
        && matches!(u16::from_le_bytes([h[24], h[25]]), 0x10b | 0x20b)
        && u16::from_le_bytes([h[92], h[93]]) == 2 // IMAGE_SUBSYSTEM_WINDOWS_GUI
}

fn path_key(path: &str) -> String {
    path.replace('/', "\\").trim_start_matches(r"\\?\").to_lowercase()
}

fn availability(target: &Target, processes: &[Process]) -> Availability {
    let mut denied = false;
    for p in processes.iter().filter(|p| matches_identity(target, p) && path_key(&p.exe) == target.exe) {
        if p.allowed {
            return Availability::Ready;
        }
        denied = true;
    }
    if denied {
        Availability::Denied
    } else {
        Availability::None
    }
}

/// Descendants must stay inside the app directory at every generation.
/// The parent's start time also rules out a recycled Toolhelp parent PID.
fn select_targets(target: &Target, processes: &[Process]) -> Vec<Process> {
    let exe = &target.exe;
    let Some((folder, _)) = exe.rsplit_once('\\') else { return Vec::new() };
    let prefix = format!("{folder}\\");
    let mut selected: Vec<Process> = processes.iter().filter(|p| matches_identity(target, p) && p.allowed && path_key(&p.exe) == *exe).cloned().collect();
    if !target.descendants { return selected; }
    loop {
        let children: Vec<Process> = processes
            .iter()
            .filter(|p| {
                matches_identity(target, p) && p.allowed && path_key(&p.exe).starts_with(&prefix) && !selected.iter().any(|s| s.pid == p.pid) && selected.iter().any(|s| s.pid == p.parent && s.start <= p.start)
            })
            .cloned()
            .collect();
        if children.is_empty() {
            break;
        }
        selected.extend(children);
    }
    selected
}

struct OwnedHandle(HANDLE);
// Owned kernel handles can be transferred to the service's wait thread.
unsafe impl Send for OwnedHandle {}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Facts come from one handle; keep it for RM and any subsequent window
/// lookup, so the process object cannot be mistaken for a reused PID.
fn inspect(pid: u32, parent: u32) -> Option<(Process, OwnedHandle)> {
    unsafe {
        let h = OwnedHandle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        let mut exit = 0;
        if GetExitCodeProcess(h.0, &mut exit).is_err() || exit != 259 {
            return None;
        }
        let mut buf = [0u16; 32768];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).ok()?;
        let exe = String::from_utf16_lossy(&buf[..len as usize]);
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        GetProcessTimes(h.0, &mut c, &mut e, &mut k, &mut u).ok()?;
        let session = hung::session_of(pid)?;
        let protected = hung::protected(pid, Some(&exe), Some(session), std::process::id()) || Some(session) != hung::session_of(std::process::id());
        let allowed = !protected; // RM registration, not termination rights, decides availability.
        let gui = safe_exe(&exe, windows_dir()) && gui_image(&exe);
        let mut aumid_buf = [0u16; 512];
        let mut aumid_len = aumid_buf.len() as u32;
        let aumid_code = windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId(h.0, &mut aumid_len, Some(PWSTR(aumid_buf.as_mut_ptr())));
        let aumid = (aumid_code == ERROR_SUCCESS && aumid_len > 1 && aumid_len <= 512)
            .then(|| String::from_utf16_lossy(&aumid_buf[..aumid_len as usize - 1]));
        Some((
            Process {
                pid,
                parent,
                start: ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64,
                exe,
                protected,
                allowed,
                gui,
                aumid,
            },
            h,
        ))
    }
}

#[derive(Clone)]
struct Entry {
    pid: u32,
    parent: u32,
    name: String,
}

fn snapshot() -> Vec<Entry> {
    unsafe {
        let Ok(h) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Vec::new() };
        let h = OwnedHandle(h);
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut ok = Process32FirstW(h.0, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push(Entry {
                pid: e.th32ProcessID,
                parent: e.th32ParentProcessID,
                name: String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase(),
            });
            ok = Process32NextW(h.0, &mut e).is_ok();
        }
        out
    }
}

fn roots(exe: &str, entries: &[Entry]) -> Vec<Process> {
    let key = path_key(exe);
    let name = key.rsplit('\\').next().unwrap_or("");
    entries
        .iter()
        .filter(|e| e.name == name)
        .filter_map(|e| inspect(e.pid, e.parent).map(|(p, _)| p))
        .filter(|p| path_key(&p.exe) == key)
        .collect()
}

struct Cached {
    value: Availability,
    generation: u64,
    watch: Option<crate::service::Id>,
}
#[derive(Default)]
struct ProbeCache {
    values: HashMap<Target, Cached>,
    pending: HashSet<Target>,
}
fn cache() -> &'static Mutex<ProbeCache> {
    static CACHE: OnceLock<Mutex<ProbeCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ProbeCache::default()))
}

struct PendingGuard(Target);
impl PendingGuard {
    fn begin(target: &Target) -> Option<Self> {
        cache().lock().unwrap_or_else(|e| e.into_inner()).pending.insert(target.clone()).then(|| Self(target.clone()))
    }
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        cache().lock().unwrap_or_else(|e| e.into_inner()).pending.remove(&self.0);
    }
}

fn invalidate(target: &Target, generation: Option<u64>) {
    let old = {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        if c.values.get(target).is_some_and(|v| generation.is_none_or(|g| g == v.generation)) { c.values.remove(target) } else { None }
    };
    if let Some(id) = old.and_then(|v| v.watch) { crate::service::poke(id); }
}

struct ExitWatch {
    target: Target,
    generation: u64,
    handle: OwnedHandle,
}
impl crate::service::Watch for ExitWatch {
    fn name(&self) -> &'static str { "full quit process exit" }
    fn handles(&self) -> Vec<HANDLE> { vec![self.handle.0] }
    fn wake(&mut self, _: crate::service::Wake) -> crate::service::Next {
        let current = cache().lock().unwrap_or_else(|e| e.into_inner()).values.get(&self.target).is_some_and(|v| v.generation == self.generation);
        if !current { return crate::service::Next::Done; }
        if unsafe { WaitForSingleObject(self.handle.0, 0) } != windows::Win32::Foundation::WAIT_TIMEOUT {
            invalidate(&self.target, Some(self.generation));
            crate::service::Next::Done
        } else { crate::service::Next::Wait }
    }
}
impl Drop for ExitWatch {
    fn drop(&mut self) { invalidate(&self.target, Some(self.generation)); }
}

fn publish(target: &Target, value: Availability, processes: &[Process]) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let generation = NEXT.fetch_add(1, Ordering::Relaxed);
    invalidate(target, None);
    cache().lock().unwrap_or_else(|e| e.into_inner()).values.insert(target.clone(), Cached { value, generation, watch: None });
    // One eligible live root suffices to prove presence. Its exit invalidates
    // the result; the next probe discovers any other remaining instances.
    if let Some(p) = processes.iter().find(|p| matches_identity(target, p)) {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, p.pid) }.ok().map(OwnedHandle);
        if let Some(handle) = handle {
            let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
            let valid = unsafe { GetProcessTimes(handle.0, &mut c, &mut e, &mut k, &mut u) }.is_ok()
                && ((c.dwHighDateTime as u64) << 32 | c.dwLowDateTime as u64) == p.start;
            if valid {
                if let Some(id) = crate::service::add(Box::new(ExitWatch { target: target.clone(), generation, handle })) {
                    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(v) = cache.values.get_mut(target).filter(|v| v.generation == generation) { v.watch = Some(id); }
                    return;
                }
            }
        }
        // No reliable exit notification: don't retain an unbounded result.
        invalidate(target, Some(generation));
    }
}

fn probe_result(target: &Target, processes: &[Process]) -> Availability {
    let value = availability(target, processes);
    if value != Availability::Ready { return value; }
    let eligible: Vec<_> = processes.iter().filter(|p| matches_identity(target, p) && p.allowed).cloned().collect();
    match register(&eligible) {
        Ok(_session) => Availability::Ready,
        Err(_) => Availability::Denied,
    }
}

/// Cached presence has no TTL: a kernel process-exit notification invalidates
/// it. Unknown results reserve a disabled row before the popup is opened.
pub(super) fn probe(target: &Target, known: Vec<u32>) -> Availability {
    if let Some(value) = cache().lock().unwrap_or_else(|e| e.into_inner()).values.get(target).map(|v| v.value) {
        if matches!(value, Availability::Ready | Availability::Denied) { return value; }
    }
    let Some(guard) = PendingGuard::begin(target) else { return Availability::Pending };
    let target = target.clone();
    let _ = std::thread::Builder::new().name("full quit probe".into()).spawn(move || {
        let _guard = guard;
        let mut seen = HashSet::new();
        let mut found: Vec<_> = known.into_iter().filter(|pid| seen.insert(*pid)).filter_map(|pid| inspect(pid, 0).map(|(p, _)| p))
            .filter(|p| path_key(&p.exe) == target.exe).collect();
        if availability(&target, &found) != Availability::Ready { found = roots(&target.exe, &snapshot()); }
        let value = probe_result(&target, &found);
        publish(&target, value, &found);
    });
    Availability::Pending
}

pub(super) fn resolved(target: &Target) -> Option<Availability> {
    let c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.pending.contains(target) { None } else { Some(c.values.get(target).map_or(Availability::None, |v| v.value)) }
}

struct WarmGate { busy: AtomicBool, last: Mutex<Option<Instant>> }
struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }
impl WarmGate {
    fn begin(&self, now: Instant) -> Option<BusyGuard<'_>> {
        if self.busy.swap(true, Ordering::AcqRel) { return None; }
        let guard = BusyGuard(&self.busy);
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(5)) { return None; }
        *last = Some(now);
        Some(guard)
    }
}

pub(super) fn should_warm(pinned: bool, windows: usize, tray: usize) -> bool { pinned && (windows == 0 || tray > 0) }

pub(super) fn warm(targets: Vec<Target>) {
    static GATE: WarmGate = WarmGate { busy: AtomicBool::new(false), last: Mutex::new(None) };
    if targets.is_empty() { return; }
    let Some(guard) = GATE.begin(Instant::now()) else { return };
    let _ = std::thread::Builder::new().name("full quit warm".into()).spawn(move || {
        let _guard = guard;
        let targets: HashSet<_> = targets.into_iter().filter(|t| {
            !cache().lock().unwrap_or_else(|e| e.into_inner()).values.get(t).is_some_and(|v| matches!(v.value, Availability::Ready | Availability::Denied))
        }).collect();
        // Reserve every lookup before the shared snapshot. A per-menu probe
        // cannot finish and then be overwritten by this older snapshot.
        let pending: Vec<_> = targets.into_iter().filter_map(|target| PendingGuard::begin(&target).map(|guard| (target, guard))).collect();
        if pending.is_empty() { return; }
        let entries = snapshot();
        for (target, _pending) in pending {
            let found = roots(&target.exe, &entries);
            publish(&target, probe_result(&target, &found), &found);
        }
    });
}

fn discover(target: &Target, deadline: Instant) -> Vec<Process> {
    let entries = snapshot();
    let mut all = roots(&target.exe, &entries);
    let mut visited: HashSet<u32> = all.iter().map(|p| p.pid).collect();
    loop {
        let selected = select_targets(target, &all);
        let children: Vec<_> = entries.iter().filter(|e| !visited.contains(&e.pid) && selected.iter().any(|p| p.pid == e.parent)).cloned().collect();
        if children.is_empty() || Instant::now() >= deadline {
            return selected;
        }
        for e in children {
            visited.insert(e.pid);
            if let Some((p, _)) = inspect(e.pid, e.parent) {
                all.push(p);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Ended,
    Refused,
    Hung,
    TimedOut,
    Failed(u32),
}

fn outcome(code: WIN32_ERROR, timed_out: bool, alive: bool, hung: bool) -> Outcome {
    if !alive {
        Outcome::Ended
    } else if hung && (timed_out || code == ERROR_FAIL_SHUTDOWN) {
        Outcome::Hung
    } else if code == ERROR_FAIL_SHUTDOWN {
        Outcome::Refused
    } else if timed_out {
        Outcome::TimedOut
    } else {
        Outcome::Failed(code.0)
    }
}

struct Session(u32);
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = RmEndSession(self.0);
        }
    }
}

fn register(processes: &[Process]) -> Result<Arc<Session>, u32> {
    unsafe {
        let mut id = 0;
        let mut key = [0u16; 33];
        let code = RmStartSession(&mut id, None, PWSTR(key.as_mut_ptr()));
        if code != ERROR_SUCCESS { return Err(code.0); }
        let session = Arc::new(Session(id));
        let unique: Vec<_> = processes.iter().map(|p| RM_UNIQUE_PROCESS {
            dwProcessId: p.pid,
            ProcessStartTime: FILETIME { dwLowDateTime: p.start as u32, dwHighDateTime: (p.start >> 32) as u32 },
        }).collect();
        let code = RmRegisterResources(id, None, Some(&unique), None);
        if code != ERROR_SUCCESS { return Err(code.0); }
        Ok(session)
    }
}

fn cancel_on_timeout(deadline: Instant, wait: mpsc::Receiver<()>, timed_out: &AtomicBool, stopping: &AtomicBool, mut cancel: impl FnMut()) {
    loop {
        match wait.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => return,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let expired = Instant::now() >= deadline;
        if expired || stopping.load(Ordering::Acquire) {
            timed_out.store(expired, Ordering::Release);
            cancel();
        }
        let delay = if expired { Duration::from_millis(50) } else { deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(50)) };
        match wait.recv_timeout(delay) {
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            _ => return,
        }
    }
}

fn dialog_candidate(h: HWND, pids: &[u32]) -> bool {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let mut class = [0u16; 32];
        let n = GetClassNameW(h, &mut class).max(0) as usize;
        let dialog = String::from_utf16_lossy(&class[..n]) == "#32770";
        let owner = GetWindow(h, GW_OWNER).unwrap_or_default();
        let active_owned = !owner.is_invalid() && GetLastActivePopup(owner) == h;
        foreground_allowed(Duration::ZERO, pids.contains(&pid), dialog, active_owned, IsWindowVisible(h).as_bool(), !IsHungAppWindow(h).as_bool())
    }
}

fn foreground_allowed(elapsed: Duration, target: bool, dialog: bool, active_owned: bool, visible: bool, responsive: bool) -> bool {
    elapsed < Duration::from_secs(3) && target && (dialog || active_owned) && visible && responsive
}

struct Windows {
    pids: Vec<u32>,
    responsive: Vec<isize>,
    hung: bool,
}
unsafe extern "system" fn collect_window(h: HWND, param: LPARAM) -> BOOL {
    let facts = &mut *(param.0 as *mut Windows);
    let mut pid = 0;
    GetWindowThreadProcessId(h, Some(&mut pid));
    if facts.pids.contains(&pid) {
        if IsHungAppWindow(h).as_bool() {
            facts.hung = true;
        } else if dialog_candidate(h, &facts.pids) {
            facts.responsive.push(h.0 as isize);
        }
    }
    BOOL(1)
}

/// Invoked only with explicit targets. Used by the isolated child fixture
/// tests too, without discovery or foreground activation.
fn shutdown_controlled(targets: Vec<Process>, deadline: Instant, foreground: bool, stopping: Arc<AtomicBool>) -> Outcome {
    let requested = deadline.checked_sub(LIMIT).unwrap_or_else(Instant::now);
    let mut held = Vec::new();
    for p in targets {
        if Instant::now() >= deadline || stopping.load(Ordering::Acquire) {
            return Outcome::TimedOut;
        }
        if let Some((now, h)) = inspect(p.pid, p.parent) {
            if now.start == p.start && path_key(&now.exe) == path_key(&p.exe) && !now.protected && now.allowed && now.gui && now.aumid == p.aumid {
                held.push((now, h));
            }
        }
    }
    if held.is_empty() {
        return Outcome::Ended;
    }
    unsafe {
        let processes: Vec<_> = held.iter().map(|(p, _)| p.clone()).collect();
        let session = match register(&processes) { Ok(s) => s, Err(code) => return Outcome::Failed(code) };
        let id = session.0;
        if Instant::now() >= deadline || stopping.load(Ordering::Acquire) {
            return Outcome::TimedOut;
        }
        let (done, wait) = mpsc::channel::<()>();
        let cancel = session.clone();
        let timed_out = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = timed_out.clone();
        // Both threads retain the session: RmEndSession cannot race a
        // cancellation, even when shutdown returns at the deadline.
        let cancel_thread = std::thread::spawn(move || {
            cancel_on_timeout(deadline, wait, &flag, &stopping, || { let _ = RmCancelCurrentTask(cancel.0); });
        });
        let code = RmShutdown(id, 0, None);
        let _ = done.send(());
        let _ = cancel_thread.join();
        let pids: Vec<_> = held
            .iter()
            .filter(|(_, h)| { let mut code = 0; GetExitCodeProcess(h.0, &mut code).is_ok() && code == 259 })
            .map(|(p, _)| p.pid)
            .collect();
        let alive = !pids.is_empty();
        let mut wins = Windows {
            pids,
            responsive: Vec::new(),
            hung: false,
        };
        let _ = EnumWindows(Some(collect_window), LPARAM(&mut wins as *mut _ as isize));
        let result = outcome(code, timed_out.load(std::sync::atomic::Ordering::Acquire), alive, wins.hung);
        if foreground && result == Outcome::Refused && requested.elapsed() < Duration::from_secs(3) {
            if let Some(&h) = wins.responsive.first() {
                let h = HWND(h as *mut _);
                if dialog_candidate(h, &wins.pids) && requested.elapsed() < Duration::from_secs(3) {
                    let _ = SetForegroundWindow(h);
                }
            }
        }
        result
    }
}

#[derive(Default)]
struct ActiveState { closing: bool, jobs: HashMap<Target, Arc<AtomicBool>> }
#[derive(Default)]
struct Runtime { state: Mutex<ActiveState>, done: Condvar }
struct JobGuard { runtime: Arc<Runtime>, target: Target }
impl Drop for JobGuard {
    fn drop(&mut self) {
        self.runtime.state.lock().unwrap_or_else(|e| e.into_inner()).jobs.remove(&self.target);
        self.runtime.done.notify_all();
    }
}
impl Runtime {
    fn begin(self: &Arc<Self>, target: &Target) -> Option<(JobGuard, Arc<AtomicBool>)> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing || state.jobs.contains_key(target) { return None; }
        let stop = Arc::new(AtomicBool::new(false));
        state.jobs.insert(target.clone(), stop.clone());
        Some((JobGuard { runtime: self.clone(), target: target.clone() }, stop))
    }
    fn stop(&self, limit: Duration) -> usize {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closing = true;
        for stop in state.jobs.values() { stop.store(true, Ordering::Release); }
        let (state, _) = self.done.wait_timeout_while(state, limit, |s| !s.jobs.is_empty()).unwrap_or_else(|e| e.into_inner());
        state.jobs.len()
    }
}
fn runtime() -> &'static Arc<Runtime> {
    static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
    RUNTIME.get_or_init(|| Arc::new(Runtime::default()))
}

pub(super) fn finish_pending() {
    let left = runtime().stop(Duration::from_secs(2));
    if left > 0 { crate::log!("dock: {left} full quit worker(s) did not return from RM within the exit budget"); }
}

pub(super) fn request(target: Target) {
    let Some((guard, stop)) = runtime().begin(&target) else { return };
    let _ = std::thread::Builder::new().name("full quit shutdown".into()).spawn(move || {
        let _guard = guard;
        let deadline = Instant::now() + LIMIT;
        let targets = discover(&target, deadline);
        let result = shutdown_controlled(targets.clone(), deadline, true, stop);
        crate::log!("dock: cooperative quit: {result:?}");
        invalidate(&target, None);
        if let Outcome::Failed(_) = result {
            publish(&target, Availability::Denied, &targets);
        }
    });
}

#[cfg(test)]
fn shutdown(targets: Vec<Process>, deadline: Instant, foreground: bool) -> Outcome {
    shutdown_controlled(targets, deadline, foreground, Arc::new(AtomicBool::new(false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(exe: &str) -> Target { Target { exe: path_key(exe), aumid: None, descendants: true } }

    fn p(pid: u32, parent: u32, exe: &str) -> Process {
        Process {
            pid,
            parent,
            start: pid as u64,
            exe: exe.into(),
            protected: false,
            allowed: true,
            gui: true,
            aumid: None,
        }
    }

    #[test]
    fn full_quit_shared_hosts_system_and_web_items_are_excluded() {
        for host in ["ApplicationFrameHost", "java", "javaw", "python", "pythonw", "node", "msedgewebview2", "dllhost", "rundll32", "conhost", "wscript", "cscript", "mshta", "electron", "cmd", "powershell", "pwsh"] {
            assert!(!safe_exe(&format!(r"D:\Apps\{host}.exe"), r"Z:\Windows"), "{host}");
        }
        assert!(!safe_exe(r"z:\WINDOWS\System32\custom.exe", r"Z:\Windows"));
        assert!(!safe_exe(r"C:\Program Files\WindowsApps\Calc\app.exe", r"Z:\Windows"));
        assert!(safe_exe(r"Z:\WindowsOther\custom.exe", r"Z:\Windows"));
        assert!(target(r"C:\Apps\chrome.exe", None, true, false).is_none());
        assert!(target(r"C:\Apps\chrome.exe", Some("Chrome._crx_cmkncekebbplodngbpllndjkfo"), false, false).is_none());
        assert!(target(r"C:\Apps\calc.exe", None, false, true).is_none());
    }

    #[test]
    fn full_quit_identity_and_console_filter_apply_to_roots_and_children() {
        let mut target = t(r"C:\Apps\Chat\chat.exe");
        target.aumid = Some("Vendor.Chat".into());
        let mut root = p(10, 0, &target.exe);
        assert_eq!(availability(&target, &[root.clone()]), Availability::None);
        root.aumid = Some("vendor.chat".into());
        let mut other = p(11, 0, &target.exe);
        other.aumid = Some("Vendor.Other".into());
        let mut console = root.clone(); console.pid = 12; console.gui = false;
        let mut child = p(13, 10, r"C:\Apps\Chat\helper.exe"); child.aumid = other.aumid.clone();
        let all = [root, other, console, child];
        assert_eq!(select_targets(&target, &all).iter().map(|p| p.pid).collect::<Vec<_>>(), [10]);
        assert_eq!(availability(&target, &all[1..]), Availability::None);
    }

    #[test]
    fn full_quit_descendants_require_a_dedicated_user_app_folder() {
        let roots = vec![r"C:\Program Files".into(), r"C:\Users\Test\AppData\Local".into()];
        assert!(app_folder(r"C:\Program Files\Vendor\app.exe", &roots));
        assert!(app_folder(r"C:\Users\Test\AppData\Local\Chat\app.exe", &roots));
        for exe in [r"C:\Program Files\app.exe", r"C:\Program FilesOther\app.exe", r"C:\app.exe", r"C:\Tools\app.exe"] {
            assert!(!app_folder(exe, &roots));
        }
        let mut target = t(r"C:\Tools\app.exe"); target.descendants = false;
        assert_eq!(select_targets(&target, &[p(10, 0, &target.exe), p(11, 10, r"C:\Tools\helper.exe")]).len(), 1);
    }

    #[test]
    fn full_quit_pe_requires_a_valid_gui_subsystem() {
        let mut h = [0u8; 94];
        h[..4].copy_from_slice(b"PE\0\0"); h[20] = 70;
        for magic in [0x10bu16, 0x20b] {
            h[24..26].copy_from_slice(&magic.to_le_bytes()); h[92] = 2;
            assert!(gui_header(&h));
            h[92] = 3; assert!(!gui_header(&h));
        }
        h[92] = 2; h[20] = 0; assert!(!gui_header(&h));
        assert!(!gui_image(r"C:\nonexistent-full-quit-image.exe"));
    }

    #[test]
    fn full_quit_foreground_only_targets_recent_responsive_dialogs() {
        let recent = Duration::from_secs(1);
        assert!(foreground_allowed(recent, true, true, false, true, true));
        assert!(foreground_allowed(recent, true, false, true, true, true));
        assert!(!foreground_allowed(recent, true, false, false, true, true));
        assert!(!foreground_allowed(recent, false, true, true, true, true));
        assert!(!foreground_allowed(recent, true, true, true, false, true));
        assert!(!foreground_allowed(recent, true, true, true, true, false));
        assert!(!foreground_allowed(Duration::from_secs(3), true, true, true, true, true));
    }

    #[test]
    fn full_quit_pending_and_busy_guards_recover_after_panics() {
        let target = t(r"C:\Tests\panic-probe.exe");
        let result = std::panic::catch_unwind(|| {
            let _guard = PendingGuard::begin(&target).unwrap();
            assert_eq!(resolved(&target), None);
            panic!("simulated lookup failure");
        });
        assert!(result.is_err());
        assert_eq!(resolved(&target), Some(Availability::None));
        assert!(PendingGuard::begin(&target).is_some());
        let gate = WarmGate { busy: AtomicBool::new(false), last: Mutex::new(None) };
        let now = Instant::now();
        assert!(std::panic::catch_unwind(|| { let _guard = gate.begin(now).unwrap(); panic!("warm failure"); }).is_err());
        assert!(!gate.busy.load(Ordering::Acquire));
        assert!(gate.begin(now + Duration::from_secs(4)).is_none());
        assert!(gate.begin(now + Duration::from_secs(5)).is_some());
    }

    #[test]
    fn full_quit_warms_only_pinned_windowless_or_tray_items() {
        assert!(should_warm(true, 0, 0));
        assert!(should_warm(true, 1, 1));
        assert!(!should_warm(true, 1, 0));
        assert!(!should_warm(false, 0, 1));
        assert!(!should_warm(false, 0, 0));
    }

    #[test]
    fn full_quit_cached_presence_survives_until_its_own_exit_notification() {
        let target = t(r"C:\Tests\cache-generation.exe");
        cache().lock().unwrap().values.insert(target.clone(), Cached { value: Availability::Ready, generation: 99, watch: None });
        assert_eq!(probe(&target, vec![]), Availability::Ready);
        invalidate(&target, Some(98));
        assert_eq!(resolved(&target), Some(Availability::Ready));
        invalidate(&target, Some(99));
        assert_eq!(resolved(&target), Some(Availability::None));
    }

    #[test]
    fn full_quit_exit_cancels_workers_and_waits_for_cleanup() {
        let runtime = Arc::new(Runtime::default());
        let target = t(r"C:\Tests\exit.exe");
        let (guard, stop) = runtime.begin(&target).unwrap();
        assert!(runtime.begin(&target).is_none());
        let cleaned = Arc::new(AtomicBool::new(false));
        let done = cleaned.clone();
        let worker = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) { std::thread::sleep(Duration::from_millis(1)); }
            done.store(true, Ordering::Release);
            drop(guard);
        });
        assert_eq!(runtime.stop(Duration::from_secs(1)), 0);
        assert!(cleaned.load(Ordering::Acquire));
        assert!(runtime.begin(&target).is_none());
        worker.join().unwrap();
        let runtime = Arc::new(Runtime::default());
        let (guard, stop) = runtime.begin(&target).unwrap();
        assert_eq!(runtime.stop(Duration::ZERO), 1);
        assert!(stop.load(Ordering::Acquire));
        drop(guard);
        assert_eq!(runtime.stop(Duration::ZERO), 0);
    }

    #[test]
    fn full_quit_exit_requests_cancel_before_the_ten_second_deadline() {
        let (done, wait) = mpsc::channel();
        let timed_out = AtomicBool::new(false);
        cancel_on_timeout(Instant::now() + LIMIT, wait, &timed_out, &AtomicBool::new(true), || { done.send(()).unwrap(); });
        assert!(!timed_out.load(Ordering::Acquire));
    }

    #[test]
    fn full_quit_cancellation_stops_after_completion_or_disconnect() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for completed in [true, false] {
            let (done, wait) = mpsc::channel();
            if completed { done.send(()).unwrap(); }
            drop(done);
            let timed_out = AtomicBool::new(false);
            cancel_on_timeout(Instant::now(), wait, &timed_out, &AtomicBool::new(false), || panic!("completed tasks must not be cancelled"));
            assert!(!timed_out.load(Ordering::Acquire));
        }
    }

    #[test]
    fn full_quit_cancellation_retries_until_shutdown_returns() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (done, wait) = mpsc::channel();
        let timed_out = AtomicBool::new(false);
        let mut calls = 0;
        cancel_on_timeout(Instant::now(), wait, &timed_out, &AtomicBool::new(false), || {
            assert!(timed_out.load(Ordering::Acquire));
            calls += 1;
            if calls == 2 { done.send(()).unwrap(); }
        });
        assert_eq!(calls, 2);
    }

    #[test]
    fn full_quit_targets_stay_in_app_tree_and_directory() {
        let exe = r"C:\Apps\Chat\chat.exe";
        let mut protected = p(16, 10, r"C:\Apps\Chat\nopbar.exe");
        protected.protected = true;
        let mut old_child = p(17, 10, r"C:\Apps\Chat\old.exe");
        old_child.start = 1;
        let mut denied = p(18, 10, r"C:\Apps\Chat\admin.exe");
        denied.allowed = false;
        let all = vec![
            p(13, 12, r"C:\Apps\Chat\bin\renderer.exe"),
            p(10, 0, exe),
            p(11, 0, r"c:/apps/chat/CHAT.exe"),
            p(12, 10, r"C:\Apps\Chat\helper.exe"),
            p(14, 10, r"C:\Apps\ChatOther\other.exe"),
            p(15, 14, r"C:\Apps\Chat\back.exe"),
            protected,
            old_child,
            denied,
            p(19, 0, r"C:\Apps\Chat\unrelated.exe"),
        ];
        let mut ids: Vec<_> = select_targets(&t(exe), &all).iter().map(|p| p.pid).collect();
        ids.sort_unstable();
        assert_eq!(ids, [10, 11, 12, 13]);
        assert!(select_targets(&t(""), &all).is_empty());
    }

    #[test]
    fn full_quit_presence_and_permissions() {
        let exe = r"C:\Chat\chat.exe";
        assert_eq!(availability(&t(exe), &[]), Availability::None);
        let mut process = p(10, 0, exe);
        assert_eq!(availability(&t(exe), &[process.clone()]), Availability::Ready);
        process.allowed = false;
        assert_eq!(availability(&t(exe), &[process.clone()]), Availability::Denied);
        assert_eq!(availability(&t(exe), &[process.clone(), p(11, 0, exe)]), Availability::Ready);
        process.protected = true;
        assert_eq!(availability(&t(exe), &[process]), Availability::None);
        assert_eq!(availability(&t(exe), &[p(20, 0, r"D:\Chat\chat.exe")]), Availability::None);
    }

    #[test]
    fn full_quit_outcomes_never_escalate() {
        assert_eq!(outcome(ERROR_SUCCESS, false, false, false), Outcome::Ended);
        assert_eq!(outcome(ERROR_FAIL_SHUTDOWN, false, true, false), Outcome::Refused);
        assert_eq!(outcome(ERROR_FAIL_SHUTDOWN, false, true, true), Outcome::Hung);
        assert_eq!(outcome(ERROR_SUCCESS, true, true, true), Outcome::Hung);
        assert_eq!(outcome(ERROR_SUCCESS, true, true, false), Outcome::TimedOut);
        assert_eq!(outcome(WIN32_ERROR(5), false, true, false), Outcome::Failed(5));
    }

    // No discovery by path here: only the PID returned by our own spawn
    // can be registered, and production revalidates its creation time.
    struct Fixture(std::process::Child);
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.0.stdin.take(); // EOF asks this fixture to close itself.
            let _ = self.0.wait(); // It also has its own 20s exit timer.
        }
    }

    fn fixture(mode: &str) -> (Fixture, mpsc::Receiver<String>) {
        use std::io::BufRead;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        let test = std::env::current_exe().unwrap();
        let exe = test.parent().unwrap().parent().unwrap().join("examples/full_quit_fixture.exe");
        assert!(exe.is_file(), "first build: cargo build -p dock --example full_quit_fixture");
        let child = Command::new(exe).arg(mode).creation_flags(0x08000000).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut child = Fixture(child);
        let stdout = child.0.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "READY");
        (child, rx)
    }

    #[test]
    #[ignore = "only spawns the dedicated hidden fixture; build --example full_quit_fixture first"]
    fn full_quit_rm_accepts_owned_child_only() {
        let (mut child, messages) = fixture("accept");
        let (mut bystander, bystander_messages) = fixture("veto");
        let (p, _h) = inspect(child.0.id(), std::process::id()).unwrap();
        let began = Instant::now();
        probe(&t(&p.exe), vec![p.pid]);
        println!("known-PID menu probe: {:?}", began.elapsed());
        wait_for_probe(&t(&p.exe));
        invalidate(&t(&p.exe), None);
        let began = Instant::now();
        probe(&t(&p.exe), Vec::new());
        println!("windowless menu probe: {:?}", began.elapsed());
        wait_for_probe(&t(&p.exe));
        let target = t(&p.exe);
        publish(&target, Availability::Ready, &[p.clone()]);
        let result = shutdown(vec![p], Instant::now() + LIMIT, false);
        assert_eq!(messages.recv_timeout(Duration::from_secs(2)).unwrap(), "QUERY");
        assert_eq!(messages.recv_timeout(Duration::from_secs(2)).unwrap(), "END");
        let until = Instant::now() + Duration::from_secs(2);
        while resolved(&target) != Some(Availability::None) && Instant::now() < until { std::thread::sleep(Duration::from_millis(1)); }
        assert_eq!(resolved(&target), Some(Availability::None));
        assert_eq!(result, Outcome::Ended);
        assert!(child.0.wait().unwrap().success());
        assert!(bystander.0.try_wait().unwrap().is_none(), "unregistered owned fixture stays alive");
        assert!(bystander_messages.try_recv().is_err(), "RM must not expand to another process with the same executable");
    }

    fn wait_for_probe(exe: &Target) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while resolved(exe).is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(resolved(exe), Some(Availability::Ready));
    }

    #[test]
    #[ignore = "only spawns the dedicated hidden fixture; build --example full_quit_fixture first"]
    fn full_quit_rm_veto_keeps_owned_child_alive() {
        let (mut child, messages) = fixture("veto");
        let (p, _h) = inspect(child.0.id(), std::process::id()).unwrap();
        let mut stale = p.clone();
        stale.start += 1;
        assert_eq!(shutdown(vec![stale], Instant::now() + LIMIT, false), Outcome::Ended);
        assert!(child.0.try_wait().unwrap().is_none(), "stale identities never reach RM");
        assert!(messages.try_recv().is_err(), "no shutdown message for a stale identity");
        let mut wrong_path = p.clone();
        wrong_path.exe.push_str(".other");
        assert_eq!(shutdown(vec![wrong_path], Instant::now() + LIMIT, false), Outcome::Ended);
        assert_eq!(shutdown(vec![p.clone()], Instant::now(), false), Outcome::TimedOut);
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(messages.try_recv().is_err(), "wrong paths and expired deadlines never reach RM");
        let result = shutdown(vec![p], Instant::now() + LIMIT, false);
        assert_eq!(messages.recv_timeout(Duration::from_secs(2)).unwrap(), "QUERY");
        assert_eq!(result, Outcome::Refused);
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(!messages.try_iter().any(|m| m == "END"));
    }
}
