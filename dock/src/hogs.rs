//! "Using the most power": which apps kept the CPU and GPU busy over the
//! last 10 minutes, for the system stats panel (topbar/stats.rs).
//!
//! An estimate, said so in the panel's tooltip: Windows does not tell apps
//! how much power each app draws, so each app's share is its CPU time plus
//! its GPU time (both in busy seconds; all of an app's processes together,
//! by exe name) out of every app's, over a sliding 10-minute window.
//!
//! When it looks: only while the stats panel is open, every 5 s (on the
//! panel's sampler thread, crate::sysstat, which reads the processes
//! anyway). Nothing samples in the background. The panel shows the list
//! on battery only. Names and icons (disk, shell) are read on a lane of
//! their own (LANE), so the sampler never waits for the shell.
//!
//! Never part of the power list: Windows' own processes (session 0
//! services, the kernel, the shell, the compositor, security software) and
//! NopBar. The GPU list counts them (what keeps the GPU busy, honestly:
//! the compositor often does), but never offers to show or quit them.
//! "Quit" only ever asks an app's windows to close (WM_CLOSE: the app may
//! ask to save); nothing is ever terminated.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE};


/// The window the shares cover.
pub const WINDOW_MS: i64 = 10 * 60_000;
/// Readings further apart than this start over (a baseline only): what
/// happened in between is unknown, not a burst.
pub const MAX_GAP_MS: i64 = 3 * 60_000;
/// How often the open panel notes the apps.
pub const PANEL_EVERY: Duration = Duration::from_secs(5);
/// Rows the panel shows.
pub const TOP_N: usize = 3;
/// The window of the panel's GPU list: what keeps the GPU busy now, not
/// over ten minutes (six of the open panel's readings).
pub const GPU_WINDOW_MS: i64 = 30_000;
/// A GPU busy second against a CPU one (equal: both are an engine kept
/// awake; the tooltip says it is an estimate).
pub const GPU_WEIGHT: f64 = 1.0;

/// One process in a reading: its CPU and GPU time so far (100 ns).
#[derive(Clone, Debug, PartialEq)]
pub struct ProcTime {
    pub pid: u32,
    /// Creation time (FILETIME): with the pid, tells a reused pid apart.
    pub created: i64,
    /// Its exe's file name ("chrome.exe").
    pub name: String,
    pub session: u32,
    pub cpu: u64,
    pub gpu: u64,
}

/// Never on the power list and never offered "Show" or "Quit" (the GPU
/// list counts them: Tracker::add): Windows itself, the shell, security
/// software, NopBar. Services (session 0) are known by their session.
const PROTECTED: &[&str] = &[
    "system",
    "idle",
    "registry",
    "secure system",
    "memory compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
    "svchost.exe",
    "dwm.exe",
    "fontdrvhost.exe",
    "sihost.exe",
    "ctfmon.exe",
    "explorer.exe",
    "taskhostw.exe",
    "runtimebroker.exe",
    "searchhost.exe",
    "searchindexer.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "shellhost.exe",
    "textinputhost.exe",
    "applicationframehost.exe",
    "audiodg.exe",
    "msmpeng.exe",
    "nissrv.exe",
    "securityhealthsystray.exe",
    "securityhealthservice.exe",
    "smartscreen.exe",
    "lockapp.exe",
    "logonui.exe",
    "conhost.exe",
    "dllhost.exe",
    "wudfhost.exe",
    "taskmgr.exe",
    "nopbar.exe",
];

/// Is this process left out (and never closed)? `own`: NopBar's pid.
pub fn protected(name: &str, pid: u32, session: u32, own: u32) -> bool {
    if pid <= 4 || session == 0 || pid == own {
        return true;
    }
    let n = name.trim().to_ascii_lowercase();
    n.is_empty() || PROTECTED.contains(&n.as_str())
}

/// One reading's busy seconds per app, and the processes behind each.
#[derive(Clone, Debug, Default)]
struct Bucket {
    at: i64,
    secs: HashMap<String, f64>,
    /// The GPU part of `secs` alone (the panel's GPU list).
    gpu: HashMap<String, f64>,
}

/// An app in the ranking.
#[derive(Clone, Debug, PartialEq)]
pub struct Rank {
    /// The exe name, lowercase ("chrome.exe").
    pub key: String,
    /// Its share of every app's busy time, 0..1.
    pub share: f32,
    /// Its processes at the last reading: (pid, creation time).
    pub procs: Vec<(u32, i64)>,
    /// The session of its first process (0: services).
    pub session: u32,
}

/// The sliding window of readings (pure: the tests feed it numbers).
#[derive(Default)]
pub struct Tracker {
    last: HashMap<(u32, i64), (u64, u64)>,
    last_at: Option<i64>,
    buckets: VecDeque<Bucket>,
    /// Each app's processes at the last reading, and its first one's
    /// session: the power list's (the protected left out, so a service
    /// sharing an app's exe never decides the app's row) ...
    procs: HashMap<String, (Vec<(u32, i64)>, u32)>,
    /// ... and the GPU list's (every process with a name).
    procs_all: HashMap<String, (Vec<(u32, i64)>, u32)>,
}

impl Tracker {
    /// A reading at `at` (Unix ms). What each process used since the last
    /// reading goes in; a process new since then counts from now (its
    /// whole past would read as a burst), as does everything after a gap.
    ///
    /// Two lists, two filters: the power list leaves the protected out
    /// (nothing of theirs is ranked there); the GPU list counts every
    /// process with a name, protected or not, in its ranking and in its
    /// whole (the compositor busy is the GPU busy). What may be shown or
    /// quit is decided per row (`guarded`), never by leaving one out.
    pub fn add(&mut self, at: i64, procs: &[ProcTime], own: u32) {
        let fresh = self.last_at.is_none_or(|l| at <= l || at - l > MAX_GAP_MS);
        let mut b = Bucket { at, secs: HashMap::new(), gpu: HashMap::new() };
        let mut now: HashMap<(u32, i64), (u64, u64)> = HashMap::with_capacity(procs.len());
        let mut by_app: HashMap<String, (Vec<(u32, i64)>, u32)> = HashMap::new();
        let mut by_app_all: HashMap<String, (Vec<(u32, i64)>, u32)> = HashMap::new();
        for p in procs {
            if p.name.trim().is_empty() {
                continue;
            }
            let guarded = protected(&p.name, p.pid, p.session, own);
            let key = p.name.to_ascii_lowercase();
            now.insert((p.pid, p.created), (p.cpu, p.gpu));
            by_app_all.entry(key.clone()).or_insert_with(|| (Vec::new(), p.session)).0.push((p.pid, p.created));
            if !guarded {
                by_app.entry(key.clone()).or_insert_with(|| (Vec::new(), p.session)).0.push((p.pid, p.created));
            }
            if fresh {
                continue;
            }
            let Some(&(c0, g0)) = self.last.get(&(p.pid, p.created)) else { continue };
            let gpu = p.gpu.saturating_sub(g0) as f64;
            if gpu > 0.0 {
                *b.gpu.entry(key.clone()).or_default() += gpu / 10_000_000.0;
            }
            if guarded {
                continue;
            }
            let used = p.cpu.saturating_sub(c0) as f64 + gpu * GPU_WEIGHT;
            if used > 0.0 {
                *b.secs.entry(key).or_default() += used / 10_000_000.0;
            }
        }
        self.last = now;
        self.last_at = Some(at);
        self.procs = by_app;
        self.procs_all = by_app_all;
        if !fresh {
            self.buckets.push_back(b);
        }
        self.prune(at);
    }

    /// Drops readings older than the window.
    fn prune(&mut self, now: i64) {
        while self.buckets.front().is_some_and(|b| b.at <= now - WINDOW_MS) {
            self.buckets.pop_front();
        }
    }

    /// The apps that used the most over the window ending at `now`, best
    /// first (ties by name), at most `n`, with their share. Apps that are
    /// gone keep their part of the window, without processes.
    pub fn ranking(&self, now: i64, n: usize) -> Vec<Rank> {
        self.ranked(now, n, WINDOW_MS, false)
    }

    /// The apps that kept the GPU busiest over the last GPU_WINDOW_MS, as
    /// `ranking`: each one's share of every process's GPU time (Windows'
    /// own and NopBar included: see add).
    pub fn ranking_gpu(&self, now: i64, n: usize) -> Vec<Rank> {
        self.ranked(now, n, GPU_WINDOW_MS, true)
    }

    fn ranked(&self, now: i64, n: usize, window: i64, gpu: bool) -> Vec<Rank> {
        let mut sum: HashMap<&str, f64> = HashMap::new();
        for b in self.buckets.iter().filter(|b| b.at > now - window) {
            for (k, v) in if gpu { &b.gpu } else { &b.secs } {
                *sum.entry(k.as_str()).or_default() += v;
            }
        }
        let total: f64 = sum.values().sum();
        if total <= 0.0 {
            return Vec::new();
        }
        let mut out: Vec<Rank> = sum
            .into_iter()
            .filter(|(_, v)| *v > 0.0)
            .map(|(k, v)| {
                let (procs, session) = (if gpu { &self.procs_all } else { &self.procs }).get(k).cloned().unwrap_or_default();
                Rank { key: k.to_string(), share: (v / total) as f32, procs, session }
            })
            .collect();
        out.sort_by(|a, b| b.share.partial_cmp(&a.share).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.key.cmp(&b.key)));
        out.truncate(n);
        out
    }

    /// Readings in the window (for the panel's "measuring" state).
    pub fn has_data(&self) -> bool {
        !self.buckets.is_empty()
    }
}

/// "pid_1234_luid_..._engtype_3D" -> 1234.
pub fn gpu_pid(instance: &str) -> Option<u32> {
    let rest = instance.strip_prefix("pid_")?;
    let end = rest.find('_').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ---------------------------------------------------------------- the shared list

/// A row the panel shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Hog {
    pub key: String,
    /// Its name as shown (the exe's own product name, else its file name).
    pub name: String,
    pub share: f32,
    /// Its icon (premultiplied BGRA, square side), if read.
    pub icon: Option<Arc<(u32, Vec<u8>)>>,
    pub procs: Vec<(u32, i64)>,
    /// It has a window to show or close.
    pub window: bool,
}

/// An app's name and icon, read once (on the icon lane).
#[derive(Clone, Debug)]
struct Info {
    name: String,
    icon: Option<Arc<(u32, Vec<u8>)>>,
    /// Its exe could not be read (protected, gone) at this time (now_ms):
    /// the plain name stands in, and it is tried again INFO_RETRY_MS later,
    /// not at every reading.
    missed: Option<i64>,
}

/// An app whose exe could not be read is tried again this long after.
const INFO_RETRY_MS: i64 = 30_000;

/// Pure: a kept answer still stands at `now` (one read for real: always;
/// a miss: until INFO_RETRY_MS on).
fn info_fresh(missed: Option<i64>, now: i64) -> bool {
    missed.is_none_or(|at| now >= at && now - at < INFO_RETRY_MS)
}

/// Most apps whose name and icon are kept.
const MAX_INFO: usize = 64;
/// Icon side read (px; drawn smaller).
const ICON_PX: i32 = 32;

static TRACKER: Mutex<Option<Tracker>> = Mutex::new(None);
static LIST: Mutex<Vec<Hog>> = Mutex::new(Vec::new());
/// The apps keeping the GPU busiest (list_gpu), as LIST.
static LIST_GPU: Mutex<Vec<Hog>> = Mutex::new(Vec::new());
/// The pids that own an app window, as of the last resolved reading
/// (describe: which listed apps can be shown or asked to quit).
static WINDOWS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
static INFO: Mutex<Option<HashMap<String, Info>>> = Mutex::new(None);

fn now_ms() -> i64 {
    crate::focus_timer::now_ms()
}

/// A reading goes in. `resolve`: list the top apps now with their windows
/// (the panel's sampler). Their names and icons are read on the icon lane
/// (LANE), never here: until one is read the row shows its exe's name and
/// the default glyph, and list() fills it in once it is.
pub fn record(sample: Vec<ProcTime>, resolve: bool) {
    let now = now_ms();
    let own = std::process::id();
    let ranked = {
        let mut g = TRACKER.lock().unwrap_or_else(|e| e.into_inner());
        let t = g.get_or_insert_with(Tracker::default);
        t.add(now, &sample, own);
        (t.ranking(now, TOP_N), t.ranking_gpu(now, TOP_N))
    };
    if !resolve {
        return;
    }
    let windows = window_pids();
    let rows = |ranked: Vec<Rank>| -> Vec<Hog> {
        ranked
            .into_iter()
            .map(|r| {
                let on_screen = r.procs.iter().any(|(pid, _)| windows.contains(pid));
                row_of(r, on_screen)
            })
            .collect()
    };
    let (power, gpu) = (rows(ranked.0), rows(ranked.1));
    *LIST.lock().unwrap_or_else(|e| e.into_inner()) = power;
    *LIST_GPU.lock().unwrap_or_else(|e| e.into_inner()) = gpu;
    *WINDOWS.lock().unwrap_or_else(|e| e.into_inner()) = Some(windows);
}

/// `list` with every name and icon the lane has read since it was made.
fn named(mut list: Vec<Hog>) -> Vec<Hog> {
    if let Some(m) = INFO.lock().ok().as_ref().and_then(|g| g.as_ref()) {
        for h in &mut list {
            if let Some(i) = m.get(&h.key) {
                h.name = i.name.clone();
                h.icon = i.icon.clone();
            }
        }
    }
    list
}

/// The rows for the panel now (empty while measuring), with every name and
/// icon the lane has read since they were listed.
pub fn list() -> Vec<Hog> {
    named(LIST.lock().map(|l| l.clone()).unwrap_or_default())
}

/// The apps keeping the GPU busiest over the last GPU_WINDOW_MS (their
/// share of the listed apps' GPU time), as `list`.
pub fn list_gpu() -> Vec<Hog> {
    named(LIST_GPU.lock().map(|l| l.clone()).unwrap_or_default())
}

/// An app of the panel's CPU and memory lists (crate::sysstat: every
/// process of one exe, `key` its lowercase name) as a row: its name and
/// icon as far as the lane has read them (never waited for), and whether
/// it has a window to show or ask to close, as of the last resolved
/// reading. A protected one (Windows' own, a service: `session` 0, NopBar)
/// is listed honestly but never offered either: `window` is false.
pub fn describe(key: &str, procs: &[(u32, i64)], session: u32) -> Hog {
    let on_screen = WINDOWS.lock().ok().is_some_and(|g| g.as_ref().is_some_and(|w| procs.iter().any(|(pid, _)| w.contains(pid))));
    row_of(Rank { key: key.to_string(), share: 0.0, procs: procs.to_vec(), session }, on_screen)
}

/// Pure: an app never offered "Show" or "Quit" (listed all the same):
/// NopBar (any of its processes, whatever its exe is called), Windows'
/// own, a service (by its first process's session).
fn guarded(key: &str, procs: &[(u32, i64)], session: u32, own: u32) -> bool {
    let lead = procs.first().map(|p| p.0).unwrap_or(0);
    procs.iter().any(|p| p.0 == own) || protected(key, lead, session, own)
}

/// An app as a row of any list: its name and icon as far as the lane has
/// read them, and a window to show or close when it has one on screen
/// (`on_screen`) and is not guarded.
fn row_of(r: Rank, on_screen: bool) -> Hog {
    let lead = r.procs.first().map(|p| p.0).unwrap_or(0);
    // The kernel's and the services' exes cannot be read: not asked for.
    let info = if lead <= 4 || r.session == 0 { Info { name: plain_name(&r.key), icon: None, missed: None } } else { info_soon(&r) };
    let window = on_screen && !guarded(&r.key, &r.procs, r.session, std::process::id());
    Hog { key: r.key, name: info.name, share: r.share, icon: info.icon, procs: r.procs, window }
}

/// Something was measured over the window (else the panel says
/// "Measuring…").
pub fn measured() -> bool {
    TRACKER.lock().ok().and_then(|g| g.as_ref().map(|t| t.has_data())).unwrap_or(false)
}

/// An app's name and icon if the lane has read them, else its exe's name
/// and no icon for now, and a read of them queued (once: the lane drops a
/// key it already has).
fn info_soon(r: &Rank) -> Info {
    let now = now_ms();
    if let Some(i) = INFO.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&r.key).cloned())) {
        if info_fresh(i.missed, now) {
            return i;
        }
    }
    let job = r.clone();
    LANE.submit(format!("info:{}", r.key), Box::new(move || {
        info_of(&job);
    }));
    Info { name: plain_name(&r.key), icon: None, missed: None }
}

/// "chrome.exe" -> "chrome": a row's name until its own is read.
fn plain_name(key: &str) -> String {
    key.trim_end_matches(".exe").to_string()
}

/// Reads an app's name and icon (the lane: disk and shell) and keeps them.
fn info_of(r: &Rank) -> Info {
    let now = now_ms();
    if let Some(i) = INFO.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&r.key).cloned())) {
        if info_fresh(i.missed, now) {
            return i;
        }
    }
    let path = r.procs.iter().find_map(|(pid, _)| exe_path(*pid));
    let name = match &path {
        Some(p) => crate::apps::display_name(&crate::apps::AppWindow { hwnd: HWND::default(), exe: Some(p.to_lowercase()), aumid: None, pid: 0, started: 0 }),
        None => plain_name(&r.key),
    };
    let icon_key = if r.procs.iter().any(|(pid, _)| *pid == std::process::id()) {
        Some(crate::icons::NOPBAR_ICON_KEY)
    } else {
        path.as_deref()
    };
    let icon = icon_key.and_then(load_icon);
    // A miss is kept too, with its time (a closed app may come back; a
    // protected one is not asked about every second).
    let info = Info { name, icon, missed: path.is_none().then_some(now) };
    let mut g = INFO.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if m.len() >= MAX_INFO {
        m.clear();
    }
    m.insert(r.key.clone(), info.clone());
    info
}

/// An app's icon as the stats panel draws it: its side (px) and its
/// premultiplied BGRA pixels, square.
pub type AppIcon = Arc<(u32, Vec<u8>)>;

/// An exe's icon as the panel draws it.
pub(crate) fn load_icon(path: &str) -> Option<AppIcon> {
    crate::icons::load(path, ICON_PX).filter(|i| i.width == i.height && i.pixels.len() == (i.width * i.height * 4) as usize).map(|i| Arc::new((i.width, i.pixels)))
}

// ---------------------------------------------------------------- the icon lane

/// Icons and names (the shell, the disk) are read here, never on the
/// sampler: a read that hangs holds up only this lane. At most LANE_WORKERS
/// jobs run at once (one: a stuck one keeps the rest queued, never piles up
/// threads), at most LANE_QUEUE wait, and a key already queued or running
/// is not queued again. Nothing ever waits for a job.
static LANE: Lane = Lane::new("hogs-icons", LANE_WORKERS, LANE_QUEUE, true);
const LANE_WORKERS: usize = 1;
const LANE_QUEUE: usize = 16;

type Job = Box<dyn FnOnce() + Send>;

/// A bounded helper: jobs by key, run in order on at most `workers`
/// threads, each started when there is work and gone when there is none.
struct Lane {
    name: &'static str,
    workers: usize,
    queue_max: usize,
    /// COM (single-threaded) on its threads: the shell's icons want it.
    com: bool,
    state: Mutex<LaneState>,
}

struct LaneState {
    queue: VecDeque<(String, Job)>,
    /// Keys queued or running.
    keys: Vec<String>,
    /// Threads alive.
    running: usize,
}

impl Lane {
    const fn new(name: &'static str, workers: usize, queue_max: usize, com: bool) -> Lane {
        Lane { name, workers, queue_max, com, state: Mutex::new(LaneState { queue: VecDeque::new(), keys: Vec::new(), running: 0 }) }
    }

    /// Queues `job` under `key`, unless that key is queued or running or
    /// the queue is full (the caller asks again later). Never waits for a
    /// job. True if queued.
    fn submit(&'static self, key: String, job: Job) -> bool {
        let start = {
            let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if g.keys.contains(&key) || g.queue.len() >= self.queue_max {
                return false;
            }
            g.keys.push(key.clone());
            g.queue.push_back((key, job));
            let start = g.running < self.workers;
            if start {
                g.running += 1;
            }
            start
        };
        if start && std::thread::Builder::new().name(self.name.into()).spawn(move || self.work()).is_err() {
            // No thread: the job waits for the next submit's.
            self.state.lock().unwrap_or_else(|e| e.into_inner()).running -= 1;
        }
        true
    }

    fn work(&'static self) {
        let com = self.com && unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) }.is_ok();
        loop {
            let (key, job) = {
                let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
                match g.queue.pop_front() {
                    Some(j) => j,
                    None => {
                        // Under the lock: a submit meanwhile sees no thread and starts one.
                        g.running -= 1;
                        break;
                    }
                }
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            self.state.lock().unwrap_or_else(|e| e.into_inner()).keys.retain(|k| *k != key);
        }
        if com {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

fn exe_path(pid: u32) -> Option<String> {
    exe_path_of(pid, None)
}

/// Process `pid`'s full image path; with `created`, only if it is still
/// the process created then (a reused pid is another one).
fn exe_path_of(pid: u32, created: Option<i64>) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let same = created.is_none_or(|c| {
            let (mut made, mut exit, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
            GetProcessTimes(h, &mut made, &mut exit, &mut kernel, &mut user).is_ok() && ((made.dwHighDateTime as i64) << 32 | made.dwLowDateTime as i64) == c
        });
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = same && QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Pids that own an app window (as the taskbar would list it).
fn window_pids() -> HashSet<u32> {
    window_list().into_iter().map(|(_, pid)| pid).collect()
}

/// Every app window with its process id.
fn window_list() -> Vec<(HWND, u32)> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<(HWND, u32)>);
        if crate::apps::is_app_window(hwnd) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            out.push((hwnd, pid));
        }
        BOOL(1)
    }
    let mut out: Vec<(HWND, u32)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// The app's windows now: owned by its processes (the same ones: a reused
/// pid is checked by its creation time), never a protected process's.
fn windows_of(h: &Hog) -> Vec<Owned> {
    let own = std::process::id();
    window_list()
        .into_iter()
        .filter_map(|(w, pid)| {
            let &(_, created) = h.procs.iter().find(|(p, _)| *p == pid)?;
            let o = Owned { hwnd: w, pid, created, key: h.key.clone() };
            o.still(own).then_some(o)
        })
        .collect()
}

/// A window with the process it must still belong to (pid and creation
/// time) and that process's app (exe name, lowercase).
#[derive(Clone, Debug)]
struct Owned {
    hwnd: HWND,
    pid: u32,
    created: i64,
    key: String,
}

impl Owned {
    /// Still that window of that process of that app, and not a protected
    /// one: checked right before anything is sent (a handle or pid reused
    /// meanwhile by another app fails here).
    fn still(&self, own: u32) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::IsWindow;
        if !unsafe { IsWindow(Some(self.hwnd)) }.as_bool() {
            return false;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(self.hwnd, Some(&mut pid)) };
        if pid != self.pid || crate::apps::process_started(pid).is_none_or(|c| c as i64 != self.created) {
            return false;
        }
        let Some(exe) = exe_path(pid) else { return false };
        let name = exe.rsplit(['\\', '/']).next().unwrap_or("").to_ascii_lowercase();
        name == self.key && !protected(&name, pid, 1, own)
    }
}

/// "Show": brings the app's front-most window up (UI thread), verified
/// again just before.
pub fn show(h: &Hog) {
    let own = std::process::id();
    if let Some(w) = windows_of(h).into_iter().find(|w| w.still(own)) {
        crate::dock::bring_to_front(w.hwnd);
    }
}

/// "Quit": asks every window of the app to close (WM_CLOSE, posted; the
/// app may ask to save). Never terminates anything. Each window is checked
/// again right before its message (same process, same app, not
/// protected); windows of an app running with higher rights are skipped
/// (Windows would drop the message).
///
/// Windows sent to the tray (crate::traywin) are included on purpose: the
/// user asked this app to quit, and the list is read fresh by process
/// right now, so no stale handle is involved. The tray's own rule ("NopBar
/// never closes a tucked window") is about our window menus, where the
/// target is a remembered handle.
pub fn quit(h: &Hog) {
    let own = std::process::id();
    let mut asked = 0;
    for w in windows_of(h) {
        if crate::apps::is_locked(w.pid) || !w.still(own) {
            continue;
        }
        unsafe {
            let _ = PostMessageW(Some(w.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        asked += 1;
    }
    crate::log!("hogs: asked {asked} window(s) of {} to close", h.key);
}

// ---------------------------------------------------------------- reading

/// GPU time per process: "\GPU Engine(*)\Running Time" (100 ns, raw:
/// one collection per reading, no rate to keep).
struct GpuQuery {
    query: windows::Win32::System::Performance::PDH_HQUERY,
    counter: windows::Win32::System::Performance::PDH_HCOUNTER,
}

impl GpuQuery {
    fn open() -> Option<GpuQuery> {
        use windows::core::HSTRING;
        use windows::Win32::System::Performance::*;
        unsafe {
            let mut q = PDH_HQUERY::default();
            if PdhOpenQueryW(None, 0, &mut q) != 0 {
                return None;
            }
            let mut c = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(q, &HSTRING::from(r"\GPU Engine(*)\Running Time"), 0, &mut c) != 0 {
                let _ = PdhCloseQuery(q);
                return None;
            }
            Some(GpuQuery { query: q, counter: c })
        }
    }

    /// Running time so far per pid (all its engines).
    fn read(&self) -> HashMap<u32, u64> {
        use windows::Win32::System::Performance::*;
        let mut out = HashMap::new();
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return out;
            }
            let (mut size, mut count) = (0u32, 0u32);
            let r = PdhGetRawCounterArrayW(self.counter, &mut size, &mut count, None);
            if r != PDH_MORE_DATA || size == 0 {
                return out;
            }
            let item = std::mem::size_of::<PDH_RAW_COUNTER_ITEM_W>();
            let mut buf: Vec<u64> = vec![0; (size as usize).div_ceil(8) + 1];
            let items = buf.as_mut_ptr() as *mut PDH_RAW_COUNTER_ITEM_W;
            if PdhGetRawCounterArrayW(self.counter, &mut size, &mut count, Some(items)) != 0 {
                return out;
            }
            let n = (count as usize).min(buf.len() * 8 / item);
            for i in std::slice::from_raw_parts(items, n) {
                if i.RawValue.CStatus > 1 || i.szName.is_null() {
                    continue;
                }
                let Ok(name) = i.szName.to_string() else { continue };
                if let Some(pid) = gpu_pid(&name) {
                    *out.entry(pid).or_default() += i.RawValue.FirstValue.max(0) as u64;
                }
            }
        }
        out
    }
}

impl Drop for GpuQuery {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Performance::PdhCloseQuery(self.query);
        }
    }
}

/// What a reading thread keeps: the GPU query. (Names and icons are read
/// on the icon lane, which sets up its own COM.)
pub struct Reader {
    gpu: Option<GpuQuery>,
}

impl Reader {
    pub fn new() -> Reader {
        Reader { gpu: GpuQuery::open() }
    }

    /// The processes (from crate::sysstat) with their GPU time.
    pub fn sample(&mut self, procs: &[crate::sysstat::Proc]) -> Vec<ProcTime> {
        let gpu = self.gpu.as_ref().map(|g| g.read()).unwrap_or_default();
        procs
            .iter()
            .map(|p| ProcTime { pid: p.pid as u32, created: p.created, name: p.name.clone(), session: p.session, cpu: p.time, gpu: gpu.get(&(p.pid as u32)).copied().unwrap_or(0) })
            .collect()
    }
}

// ---------------------------------------------------------------- when it shows

/// The panel shows the list: on battery only.
pub fn shown(on_battery: bool) -> bool {
    on_battery
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 10_000_000;

    fn p(pid: u32, name: &str, cpu_s: u64, gpu_s: u64) -> ProcTime {
        ProcTime { pid, created: 7, name: name.into(), session: 1, cpu: cpu_s * S, gpu: gpu_s * S }
    }

    #[test]
    fn shares_rank_cpu_and_gpu_time() {
        let mut t = Tracker::default();
        t.add(0, &[p(10, "Chrome.exe", 100, 0), p(11, "chrome.exe", 50, 10), p(20, "game.exe", 5, 500), p(30, "notepad.exe", 1, 0)], 999);
        assert!(t.ranking(0, 3).is_empty(), "the first reading is a baseline");
        t.add(5_000, &[p(10, "Chrome.exe", 103, 0), p(11, "chrome.exe", 51, 11), p(20, "game.exe", 6, 504), p(30, "notepad.exe", 1, 0)], 999);
        let r = t.ranking(5_000, 3);
        // game: 1 + 4 = 5 s; chrome (two processes, one app): 3 + 1 + 1 = 5 s:
        // a tie goes by name; notepad used nothing and is not listed.
        assert_eq!(r.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["chrome.exe", "game.exe"]);
        assert!((r[0].share - 0.5).abs() < 1e-6 && (r[1].share - 0.5).abs() < 1e-6);
        let mut pids = r[0].procs.clone();
        pids.sort();
        assert_eq!(pids, [(10, 7), (11, 7)]);
        // At most n.
        assert_eq!(t.ranking(5_000, 1).len(), 1);
    }

    #[test]
    fn the_window_slides_and_gaps_start_over() {
        let mut t = Tracker::default();
        t.add(0, &[p(101, "a.exe", 0, 0), p(102, "b.exe", 0, 0)], 999);
        // a.exe busy early on, b.exe later.
        t.add(60_000, &[p(101, "a.exe", 30, 0), p(102, "b.exe", 0, 0)], 999);
        for m in 2..=11 {
            t.add(m * 60_000, &[p(101, "a.exe", 30, 0), p(102, "b.exe", (m as u64 - 1) * 2, 0)], 999);
        }
        // At 11 min a's minute (at 1 min) is still... out: the window is
        // (1 min, 11 min].
        let r = t.ranking(11 * 60_000, 3);
        assert_eq!(r[0].key, "b.exe");
        assert_eq!(r.len(), 1, "a.exe's busy minute slid out of the window");
        // At 10 min it was still in.
        let mut early = Tracker::default();
        early.add(0, &[p(101, "a.exe", 0, 0)], 999);
        early.add(60_000, &[p(101, "a.exe", 30, 0)], 999);
        assert_eq!(early.ranking(60_000 + WINDOW_MS - 1, 3).len(), 1);
        assert!(early.ranking(60_000 + WINDOW_MS, 3).is_empty());
        // A long gap is a new baseline, not a burst.
        let mut gap = Tracker::default();
        gap.add(0, &[p(101, "a.exe", 0, 0)], 999);
        gap.add(MAX_GAP_MS + 1, &[p(101, "a.exe", 500, 0)], 999);
        assert!(gap.ranking(MAX_GAP_MS + 1, 3).is_empty() && !gap.has_data());
        // A process that is new (or a reused pid) counts from its first
        // reading.
        let mut same = Tracker::default();
        same.add(0, &[p(101, "a.exe", 10, 0)], 999);
        same.add(5_000, &[p(101, "a.exe", 900, 0)], 999);
        assert_eq!(same.ranking(5_000, 3).len(), 1, "the same process counts");
        let mut reuse = Tracker::default();
        reuse.add(0, &[p(101, "a.exe", 10, 0)], 999);
        let mut again = p(101, "a.exe", 900, 0);
        again.created = 8;
        reuse.add(5_000, &[again], 999);
        assert!(reuse.ranking(5_000, 3).is_empty());
        // Counters going back count nothing.
        let mut back = Tracker::default();
        back.add(0, &[p(101, "a.exe", 10, 10)], 999);
        back.add(5_000, &[p(101, "a.exe", 5, 5)], 999);
        assert!(back.ranking(5_000, 3).is_empty());
    }

    #[test]
    fn protected_processes_are_never_listed() {
        let own = 4242;
        assert!(protected("System", 4, 0, own));
        assert!(protected("svchost.exe", 900, 0, own), "services by session");
        assert!(protected("anything.exe", 900, 0, own));
        assert!(protected("explorer.exe", 900, 1, own), "the shell");
        assert!(protected("DWM.EXE", 900, 1, own), "case does not matter");
        assert!(protected("MsMpEng.exe", 900, 1, own));
        assert!(protected("mine.exe", own, 1, own), "NopBar itself");
        assert!(protected("", 900, 1, own));
        assert!(!protected("chrome.exe", 900, 1, own));
        let mut t = Tracker::default();
        t.add(0, &[p(900, "csrss.exe", 0, 0), p(901, "code.exe", 0, 0), ProcTime { session: 0, ..p(902, "updater.exe", 0, 0) }], own);
        t.add(5_000, &[p(900, "csrss.exe", 50, 0), p(901, "code.exe", 1, 0), ProcTime { session: 0, ..p(902, "updater.exe", 40, 0) }], own);
        let r = t.ranking(5_000, 3);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].key.as_str(), r[0].share), ("code.exe", 1.0));
    }

    #[test]
    fn gpu_instances_name_their_process() {
        assert_eq!(gpu_pid("pid_1234_luid_0x00000000_0x0000D1D5_phys_0_eng_0_engtype_3D"), Some(1234));
        assert_eq!(gpu_pid("pid_7"), Some(7));
        assert_eq!(gpu_pid("luid_0x0_pid_7"), None);
        assert_eq!(gpu_pid("pid_x_luid"), None);
    }

    /// Waits (a bounded while) for `ok`; false if it never came.
    fn until(ok: impl Fn() -> bool) -> bool {
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < end {
            if ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        ok()
    }

    /// A gate a test job waits at (as a hung shell call would).
    #[derive(Default)]
    struct Gate {
        held: Mutex<bool>,
        open: std::sync::Condvar,
    }

    impl Gate {
        fn held() -> Arc<Gate> {
            Arc::new(Gate { held: Mutex::new(true), open: std::sync::Condvar::new() })
        }
        fn pass(&self) {
            drop(self.open.wait_while(self.held.lock().unwrap(), |h| *h).unwrap());
        }
        fn release(&self) {
            *self.held.lock().unwrap() = false;
            self.open.notify_all();
        }
    }

    fn lane() -> &'static Lane {
        Box::leak(Box::new(Lane::new("test-lane", LANE_WORKERS, LANE_QUEUE, false)))
    }

    /// Jobs held up in the shell: never more than LANE_WORKERS (one) at
    /// once, never more than LANE_QUEUE waiting, a key already queued is
    /// not queued twice, and nothing that submits ever waits.
    #[test]
    fn the_icon_lane_runs_one_job_at_a_time() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let lane = lane();
        let gate = Gate::held();
        let (now, most, done) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let job = || -> Job {
            let (gate, now, most, done) = (gate.clone(), now.clone(), most.clone(), done.clone());
            Box::new(move || {
                most.fetch_max(now.fetch_add(1, SeqCst) + 1, SeqCst);
                gate.pass();
                now.fetch_sub(1, SeqCst);
                done.fetch_add(1, SeqCst);
            })
        };
        assert!(lane.submit("k0".into(), job()));
        assert!(until(|| now.load(SeqCst) == 1), "the first job is running (and stuck)");
        for i in 1..=LANE_QUEUE {
            assert!(lane.submit(format!("k{i}"), job()), "queued behind it");
        }
        assert!(!lane.submit("k3".into(), job()), "already queued");
        assert!(!lane.submit("k0".into(), job()), "already running");
        assert!(!lane.submit("more".into(), job()), "the queue is full");
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!((now.load(SeqCst), lane.state.lock().unwrap().running), (1, 1), "one at a time while stuck");
        gate.release();
        assert!(until(|| done.load(SeqCst) == LANE_QUEUE + 1));
        assert!(until(|| lane.state.lock().unwrap().running == 0), "idle: its thread goes");
        assert_eq!(most.load(SeqCst), LANE_WORKERS);
        // Free again: a key that ran can be asked for again.
        assert!(lane.submit("k0".into(), Box::new(|| {})));
    }

    /// The GPU list: GPU time alone, over its own short window; an app
    /// busy on the CPU only is not in it.
    #[test]
    fn the_gpu_list_ranks_gpu_time_alone_over_its_own_window() {
        let mut t = Tracker::default();
        t.add(0, &[p(10, "game.exe", 0, 0), p(20, "build.exe", 0, 0), p(30, "video.exe", 0, 0)], 999);
        t.add(5_000, &[p(10, "game.exe", 1, 3), p(20, "build.exe", 40, 0), p(30, "video.exe", 0, 1)], 999);
        let g = t.ranking_gpu(5_000, 3);
        assert_eq!(g.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["game.exe", "video.exe"]);
        assert!((g[0].share - 0.75).abs() < 1e-6 && (g[1].share - 0.25).abs() < 1e-6);
        // The power list still counts both (the CPU-bound build leads it).
        assert_eq!(t.ranking(5_000, 3)[0].key, "build.exe");
        // The GPU went quiet: past its window the list is empty, though the
        // ten minutes still remember.
        t.add(10_000, &[p(10, "game.exe", 1, 3), p(20, "build.exe", 41, 0), p(30, "video.exe", 0, 1)], 999);
        assert_eq!(t.ranking_gpu(5_000 + GPU_WINDOW_MS - 1, 3).len(), 2);
        assert!(t.ranking_gpu(5_000 + GPU_WINDOW_MS, 3).is_empty());
        assert!(!t.ranking(5_000 + GPU_WINDOW_MS, 3).is_empty());
    }

    /// An app's exe also running as a service: the power list's row is the
    /// app's own processes (session 1), whatever order the reading lists
    /// them in, so it may still be shown and asked to quit.
    #[test]
    fn a_service_sharing_an_apps_exe_never_decides_its_power_row() {
        let own = 4242;
        let svc = |cpu| ProcTime { session: 0, ..p(60, "helper.exe", cpu, 0) };
        for service_first in [true, false] {
            let at = |cpu_app: u64| {
                let (a, b) = (svc(0), p(70, "helper.exe", cpu_app, 0));
                if service_first { vec![a, b] } else { vec![b, a] }
            };
            let mut t = Tracker::default();
            t.add(0, &at(0), own);
            t.add(5_000, &at(1), own);
            let r = t.ranking(5_000, 3);
            assert_eq!(r.len(), 1);
            assert_eq!((r[0].procs.clone(), r[0].session), (vec![(70, 7)], 1), "service first: {service_first}");
            assert!(!guarded(&r[0].key, &r[0].procs, r[0].session, own), "service first: {service_first}");
        }
    }

    /// The GPU list answers for the GPU as it is: NopBar, the compositor
    /// and services count in its ranking and in its whole, and are never
    /// offered Show or Quit; the power list still leaves them out.
    #[test]
    fn the_gpu_list_counts_windows_own_and_nopbar_but_never_offers_them() {
        let own = 4242;
        let at = |gpu: [u64; 4]| vec![p(own, "mine.exe", 0, gpu[0]), p(50, "dwm.exe", 0, gpu[1]), ProcTime { session: 0, ..p(60, "svchost.exe", 0, gpu[2]) }, p(70, "game.exe", 3, gpu[3])];
        let mut t = Tracker::default();
        t.add(0, &at([0; 4]), own);
        t.add(5_000, &at([1, 6, 2, 1]), own);
        let g = t.ranking_gpu(5_000, 4);
        // Out of all ten seconds: the compositor 60%, the service 20%, the
        // game and NopBar 10% each (a tie goes by name).
        assert_eq!(g.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["dwm.exe", "svchost.exe", "game.exe", "mine.exe"]);
        for (r, share) in g.iter().zip([0.6, 0.2, 0.1, 0.1]) {
            assert!((r.share - share).abs() < 1e-6, "{}: {}", r.key, r.share);
        }
        assert_eq!(g.iter().map(|x| (x.procs.clone(), x.session)).collect::<Vec<_>>(), [(vec![(50, 7)], 1), (vec![(60, 7)], 0), (vec![(70, 7)], 1), (vec![(own, 7)], 1)]);
        // Listed, but only the game may be shown or asked to quit.
        for r in &g {
            assert_eq!(guarded(&r.key, &r.procs, r.session, own), r.key != "game.exe", "{}", r.key);
        }
        // (A service's row is made without the icon lane.) On screen or not,
        // it has no window to offer.
        let svc = g[1].clone();
        assert!(!row_of(svc, true).window);
        // The power list keeps its own filter: the game alone.
        let r = t.ranking(5_000, 4);
        assert_eq!((r.len(), r[0].key.as_str(), r[0].share), (1, "game.exe", 1.0));
        // The review's case: the compositor 9 s, an app 1 s. The app is
        // 10% (not 100%, not the culprit); the compositor alone busy still
        // lists it.
        let mut t = Tracker::default();
        let two = |dwm: u64, app: u64| [p(50, "dwm.exe", 0, dwm), p(70, "game.exe", 0, app)];
        t.add(0, &two(0, 0), own);
        t.add(5_000, &two(9, 1), own);
        let g = t.ranking_gpu(5_000, 3);
        assert_eq!((g[0].key.as_str(), g[1].key.as_str()), ("dwm.exe", "game.exe"));
        assert!((g[0].share - 0.9).abs() < 1e-6 && (g[1].share - 0.1).abs() < 1e-6);
        t.add(10_000, &two(14, 1), own);
        let g = t.ranking_gpu(10_000, 3);
        assert_eq!(g[0].key, "dwm.exe");
        // Nameless processes count for neither.
        let mut t = Tracker::default();
        t.add(0, &[p(80, " ", 0, 0)], own);
        t.add(5_000, &[p(80, " ", 0, 9)], own);
        assert!(t.ranking_gpu(5_000, 3).is_empty());
    }

    /// An exe that could not be read is not asked about at every reading.
    #[test]
    fn a_missed_app_is_tried_again_later_not_every_second() {
        assert!(info_fresh(None, 5), "read for real: kept");
        assert!(info_fresh(Some(1_000), 1_000 + INFO_RETRY_MS - 1));
        assert!(!info_fresh(Some(1_000), 1_000 + INFO_RETRY_MS), "tried again");
        assert!(!info_fresh(Some(1_000), 500), "a clock set back: tried again");
    }

    /// The CPU and memory lists' rows: NopBar and Windows' own are listed
    /// but never offered Show or Quit.
    #[test]
    fn a_protected_app_is_described_without_a_window() {
        let me = std::process::id();
        *WINDOWS.lock().unwrap() = Some([me, 4321].into_iter().collect());
        // (Session 0 here: nothing is asked of the icon lane in a test.)
        assert!(!describe("mine.exe", &[(me, 1)], 0).window, "NopBar itself, whatever its exe is called");
        assert!(!describe("svc.exe", &[(4321, 1)], 0).window, "a service");
        assert!(protected("explorer.exe", 4321, 1, me) && protected("nopbar.exe", 4321, 1, me), "the shell and NopBar by name");
        let d = describe("system", &[(4, 0)], 0);
        assert_eq!((d.name.as_str(), d.window, d.procs.len()), ("system", false, 1));
    }
}
