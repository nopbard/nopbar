//! System stats for the stats panel (topbar/stats.rs, opened from NopBar's
//! notification icon): CPU (total and per core), memory, GPU, disk,
//! network, battery, uptime and the busiest processes.
//!
//! Everything is read on one worker thread (`Sampler`) that lives only
//! while the stats panel is open, once a second; never more than one in
//! the process (one held up past a close is re-armed by the next opening,
//! never joined by a second). The cheap reads
//! (GetSystemTimes, memory, power) always run; GPU and disk (PDH "GPU
//! Engine" 3D engines, "PhysicalDisk") and the panel's other extras
//! (cores, network, processes) only while it is open. The thread posts a
//! message when a new snapshot is ready and stops (exits) when nobody
//! wants it.
//!
//! The math (CPU % from time deltas, counter wrap, the sparkline buffer,
//! the process ranking) is pure and tested here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{FILETIME, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

// ---- pure math -------------------------------------------------------------

/// GetSystemTimes in 100 ns units. Kernel time includes idle time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuTimes {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
}

/// Busy share (0..=100) between two GetSystemTimes readings; None when no
/// time passed (or the counters went backwards).
pub fn cpu_percent(prev: CpuTimes, cur: CpuTimes) -> Option<f32> {
    let idle = cur.idle.checked_sub(prev.idle)?;
    let total = cur.kernel.checked_sub(prev.kernel)? + cur.user.checked_sub(prev.user)?;
    if total == 0 {
        return None;
    }
    let busy = total.saturating_sub(idle);
    Some((busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0) as f32)
}

/// How far a counter moved from `prev` to `cur`. A `bits`-wide counter that
/// wrapped past its maximum counts the way round; any other step back is a
/// reset (an adapter came back) and counts as nothing.
pub fn counter_delta(prev: u64, cur: u64, bits: u32) -> u64 {
    if cur >= prev {
        return cur - prev;
    }
    let max = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
    // A real wrap starts close to the top: within 1/16 of the range.
    if prev > max || prev < max - max / 16 {
        return 0;
    }
    (max - prev) + cur + 1
}

/// Bytes per second for `delta` bytes over `secs`.
pub fn rate(delta: u64, secs: f64) -> f64 {
    if secs <= 0.0 { 0.0 } else { delta as f64 / secs }
}

/// How far back a sparkline goes (ms).
pub const SPARK_SPAN_MS: u64 = 60_000;
/// Two samples further apart than this (ms) are not joined: time with no
/// reading (the sampler stopped, or was held up) is a gap, never squeezed
/// out. A tick is a second.
pub const SPARK_STEP_MS: u64 = 2_500;

/// Milliseconds on a clock of this process that never goes back (the
/// sparklines' time).
pub fn clock_ms() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The last `span` ms of one reading, by time, oldest first: a sample
/// every tick (at its `clock_ms`), None where it could not be read. An
/// unread tick, or a stretch with no tick, breaks the line; samples older
/// than the span go, whatever came since.
#[derive(Clone, Debug, PartialEq)]
pub struct Spark {
    samples: std::collections::VecDeque<(u64, Option<f32>)>,
    span: u64,
}

impl Spark {
    pub fn new(span_ms: u64) -> Self {
        Self { samples: std::collections::VecDeque::new(), span: span_ms.max(1) }
    }
    /// The reading of the tick at `at` (ms), None if it could not be read.
    /// One older than the last is ignored (the clock never goes back).
    pub fn push(&mut self, at: u64, v: Option<f32>) {
        if self.samples.back().is_some_and(|s| at < s.0) {
            return;
        }
        self.samples.push_back((at, v.filter(|v| v.is_finite())));
        while self.samples.front().is_some_and(|s| s.0 + self.span < at) {
            self.samples.pop_front();
        }
    }
    #[cfg(test)]
    pub fn span(&self) -> u64 {
        self.span
    }
    /// The samples of the span up to `now`, oldest first.
    fn window(&self, now: u64) -> impl Iterator<Item = (u64, Option<f32>)> + '_ {
        self.samples.iter().copied().filter(move |s| s.0 <= now && s.0 + self.span >= now)
    }
    /// The values read in the span up to `now` (for the scale: a gap is
    /// no value, an old peak gone is no peak).
    pub fn live(&self, now: u64) -> Vec<f32> {
        self.window(now).filter_map(|s| s.1).collect()
    }
    /// The line up to `now`: runs of readings next to each other, each
    /// (age ms, value), oldest first. A gap starts a new run.
    pub fn runs(&self, now: u64) -> Vec<Vec<(u64, f32)>> {
        let mut out: Vec<Vec<(u64, f32)>> = Vec::new();
        let mut last: Option<u64> = None;
        for (at, v) in self.window(now) {
            match v {
                Some(v) => {
                    let joined = last.is_some_and(|l| at - l <= SPARK_STEP_MS);
                    if !joined || out.is_empty() {
                        out.push(Vec::new());
                    }
                    if let Some(run) = out.last_mut() {
                        run.push((now - at, v));
                    }
                    last = Some(at);
                }
                None => last = None,
            }
        }
        out
    }
}

/// One process in a snapshot: its CPU time (user + kernel, 100 ns).
#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: usize,
    /// Creation time: with the pid, tells a reused pid from the old process.
    pub created: i64,
    pub name: String,
    pub time: u64,
    /// Its session (0: services, never an app of the user's).
    pub session: u32,
    /// Private bytes (its commit charge): another measure, never shown as
    /// its "Memory".
    pub private: u64,
    /// Private working set (WorkingSetPrivateSize: Task Manager's
    /// "Memory" column); None when the system did not fill it in. A 0
    /// read is a real 0 (all of it paged out).
    pub private_ws: Option<u64>,
}

impl Proc {
    /// The memory Task Manager shows for it: its private working set (None
    /// when that is not known; never its private bytes instead).
    pub fn memory(&self) -> Option<u64> {
        self.private_ws
    }
}

/// An app in the panel's lists: every process of one exe together (by its
/// file name; two apps sharing an exe name are one row, as in crate::hogs).
#[derive(Clone, Debug, PartialEq)]
pub struct AppUse {
    /// The exe name, lowercase ("chrome.exe").
    pub key: String,
    /// Its processes now: (pid, creation time). A reused pid is another
    /// process, and is told apart by the time.
    pub procs: Vec<(u32, i64)>,
    /// The session of its first process (0: services).
    pub session: u32,
    /// CPU, % of the whole machine, averaged over CPU_AVG_TICKS readings.
    pub cpu: f32,
    /// Memory as Task Manager's "Memory" column counts it (Proc::memory),
    /// its processes summed; one not known adds nothing.
    pub memory: u64,
}

/// A list's CPU share is averaged over this many readings (three seconds):
/// a one-second spike does not reorder the list.
pub const CPU_AVG_TICKS: usize = 3;
/// Apps a snapshot carries per list: more than the panel's rows, so the
/// panel can keep its rows' order between rankings and still have their
/// values.
pub const APPS_N: usize = 6;

/// The CPU time (100 ns) each app used between two snapshots, by exe name
/// (lowercase). A process new since `prev` counts from zero (its whole
/// time would be a spike); a reused pid is a new process.
pub fn app_times(prev: &[Proc], cur: &[Proc]) -> HashMap<String, u64> {
    let before: HashMap<(usize, i64), u64> = prev.iter().map(|p| ((p.pid, p.created), p.time)).collect();
    let mut out: HashMap<String, u64> = HashMap::new();
    for p in cur.iter().filter(|p| p.pid != 0) {
        let Some(was) = before.get(&(p.pid, p.created)) else { continue };
        let used = p.time.saturating_sub(*was);
        if used > 0 {
            *out.entry(p.name.to_lowercase()).or_default() += used;
        }
    }
    out
}

/// The last CPU_AVG_TICKS readings' app times (app_times) with the wall
/// time each covers (100 ns).
#[derive(Clone, Debug, Default)]
pub struct CpuWindow {
    ticks: std::collections::VecDeque<(u64, HashMap<String, u64>)>,
}

impl CpuWindow {
    pub fn push(&mut self, elapsed: u64, used: HashMap<String, u64>) {
        if elapsed == 0 {
            return;
        }
        self.ticks.push_back((elapsed, used));
        while self.ticks.len() > CPU_AVG_TICKS {
            self.ticks.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.ticks.clear();
    }

    /// Each app's share of the whole machine (%) over the window, on
    /// `cpus` logical processors. Empty before the first reading.
    pub fn percent(&self, cpus: u32) -> HashMap<String, f32> {
        let whole: f64 = self.ticks.iter().map(|t| t.0 as f64).sum::<f64>() * cpus as f64;
        if whole <= 0.0 {
            return HashMap::new();
        }
        let mut sum: HashMap<&str, u64> = HashMap::new();
        for (_, used) in &self.ticks {
            for (k, v) in used {
                *sum.entry(k.as_str()).or_default() += v;
            }
        }
        sum.into_iter().map(|(k, v)| (k.to_string(), (v as f64 / whole * 100.0).clamp(0.0, 100.0) as f32)).collect()
    }
}

/// Every app in `cur`: its processes together, with its CPU share from
/// `pct` (CpuWindow::percent; none: 0) and its memory summed.
pub fn apps(cur: &[Proc], pct: &HashMap<String, f32>) -> Vec<AppUse> {
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<AppUse> = Vec::new();
    for p in cur.iter().filter(|p| p.pid != 0) {
        let key = p.name.to_lowercase();
        let i = match at.get(&key) {
            Some(i) => *i,
            None => {
                out.push(AppUse { cpu: pct.get(&key).copied().unwrap_or(0.0), key: key.clone(), procs: Vec::new(), session: p.session, memory: 0 });
                at.insert(key, out.len() - 1);
                out.len() - 1
            }
        };
        out[i].procs.push((p.pid as u32, p.created));
        out[i].memory += p.memory().unwrap_or(0);
    }
    out
}

/// The `n` apps using the most CPU (those using any), best first; ties by
/// name. Summed first, cut after: an app of many small processes ranks by
/// their sum.
pub fn top_by_cpu(all: &[AppUse], n: usize) -> Vec<AppUse> {
    let mut v: Vec<AppUse> = all.iter().filter(|a| a.cpu > 0.0).cloned().collect();
    v.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.key.cmp(&b.key)));
    v.truncate(n);
    v
}

/// The `n` apps using the most memory, best first; ties by name.
pub fn top_by_memory(all: &[AppUse], n: usize) -> Vec<AppUse> {
    let mut v: Vec<AppUse> = all.iter().filter(|a| a.memory > 0).cloned().collect();
    v.sort_by(|a, b| b.memory.cmp(&a.memory).then_with(|| a.key.cmp(&b.key)));
    v.truncate(n);
    v
}

/// What NopBar itself uses, summed over its processes (own_procs).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Own {
    /// CPU, % of the whole machine (None until two readings).
    pub cpu: Option<f32>,
    /// Memory as Task Manager's "Memory" column counts it (Proc::memory:
    /// the private working sets); None when one of them is not known (a
    /// part would read as the whole).
    pub memory: Option<u64>,
    /// How many processes that is.
    pub procs: usize,
}

/// NopBar's own processes in `cur`: every process running the same image
/// as `me` (the dock) in its session. The guardian, Settings and the
/// updater are all copies of the one exe, started by the dock or by each
/// other, so the image (not the parent) is what ties them together.
pub fn own_procs(cur: &[Proc], me: usize) -> Vec<&Proc> {
    let Some(dock) = cur.iter().find(|p| p.pid == me) else { return Vec::new() };
    cur.iter().filter(|p| p.session == dock.session && p.name.eq_ignore_ascii_case(&dock.name)).collect()
}

/// What NopBar's processes (own_procs of `me`) use in `cur`: their memory
/// (Proc::memory), and their CPU since `prev` (`elapsed` wall time in 100 ns, on
/// `cpus` logical processors) as a share of the whole machine. `prev_at`:
/// when `prev` was read (FILETIME, as Proc::created). A process started
/// since then counts all its time (it is all inside this interval); one
/// missing from `prev` though older (not read then) has no baseline and
/// counts nothing. A reused pid is told apart by its creation time. None
/// when `me` is not in `cur`.
pub fn own_usage(prev: &[Proc], prev_at: i64, cur: &[Proc], me: usize, elapsed: u64, cpus: u32) -> Option<Own> {
    let mine = own_procs(cur, me);
    if mine.is_empty() {
        return None;
    }
    let memory = mine.iter().map(|p| p.memory()).sum();
    let cpu = (!prev.is_empty() && elapsed > 0 && cpus > 0).then(|| {
        let before: HashMap<(usize, i64), u64> = prev.iter().map(|p| ((p.pid, p.created), p.time)).collect();
        let used: u64 = mine
            .iter()
            .map(|p| match before.get(&(p.pid, p.created)) {
                Some(was) => p.time.saturating_sub(*was),
                None if p.created > prev_at => p.time,
                None => 0,
            })
            .sum();
        (used as f64 / (elapsed as f64 * cpus as f64) * 100.0).clamp(0.0, 100.0) as f32
    });
    Some(Own { cpu, memory, procs: mine.len() })
}

/// "12.3 KB/s" / "4.5 MB/s" for bytes per second.
pub fn rate_text(bps: f64) -> String {
    let kb = bps / 1024.0;
    if kb < 1000.0 {
        format!("{kb:.1} KB/s")
    } else {
        format!("{:.1} MB/s", kb / 1024.0)
    }
}

/// Whole days, hours and minutes in `ms`.
pub fn dhm(ms: u64) -> (u64, u64, u64) {
    let m = ms / 60_000;
    (m / 1440, m / 60 % 24, m % 60)
}

fn ft(f: FILETIME) -> u64 {
    (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
}

// ---- readers (any thread) --------------------------------------------------

pub fn read_cpu_times() -> Option<CpuTimes> {
    let (mut i, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { windows::Win32::System::Threading::GetSystemTimes(Some(&mut i), Some(&mut k), Some(&mut u)) }.ok()?;
    Some(CpuTimes { idle: ft(i), kernel: ft(k), user: ft(u) })
}

/// The system time now (FILETIME, as a process's creation time).
fn file_time_now() -> i64 {
    let f = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
    ft(f) as i64
}

/// Our own process's CPU time (100 ns), for the cost log.
pub fn own_cpu_time() -> u64 {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    match unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) } {
        Ok(()) => ft(k) + ft(u),
        Err(_) => 0,
    }
}

/// (used, total) physical memory in bytes.
pub fn read_memory() -> Option<(u64, u64)> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok()?;
    Some((m.ullTotalPhys.saturating_sub(m.ullAvailPhys), m.ullTotalPhys))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Power {
    /// Battery percent, if there is a battery.
    pub battery: Option<u8>,
    pub on_ac: bool,
    /// Seconds left on battery, if Windows knows.
    pub secs_left: Option<u32>,
    /// Energy (battery) saver is on.
    pub saver: bool,
}

pub fn read_power() -> Power {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut s = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut s) }.is_err() {
        return Power { on_ac: true, ..Default::default() };
    }
    let has = s.BatteryFlag != 128 && s.BatteryLifePercent <= 100;
    Power {
        battery: has.then_some(s.BatteryLifePercent),
        on_ac: s.ACLineStatus != 0,
        secs_left: (has && s.ACLineStatus == 0 && s.BatteryLifeTime != u32::MAX).then_some(s.BatteryLifeTime),
        saver: s.SystemStatusFlag & 1 != 0,
    }
}

pub fn uptime_ms() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

/// NtQuerySystemInformation into a reused buffer (grown as needed).
fn query_system(class: i32, buf: &mut Vec<u64>) -> Option<usize> {
    use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS};
    const MISMATCH: i32 = 0xC000_0004_u32 as i32;
    if buf.is_empty() {
        buf.resize(8 * 1024, 0);
    }
    for _ in 0..4 {
        let mut need = 0u32;
        let bytes = (buf.len() * 8) as u32;
        let st = unsafe { NtQuerySystemInformation(SYSTEM_INFORMATION_CLASS(class), buf.as_mut_ptr() as *mut _, bytes, &mut need) };
        if st.0 >= 0 {
            return Some((need as usize).min(bytes as usize));
        }
        if st.0 != MISMATCH {
            return None;
        }
        // Room for what it asked plus a margin (processes come and go).
        let want = (need as usize).max(bytes as usize * 2) + 64 * 1024;
        if want > 64 << 20 {
            return None;
        }
        buf.resize(want.div_ceil(8), 0);
    }
    None
}

/// Per logical processor (this processor group): (idle, kernel, user).
fn read_cores(buf: &mut Vec<u64>) -> Vec<CpuTimes> {
    // SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: idle, kernel, user,
    // 2 reserved i64, a u32 (+4 padding) = 48 bytes.
    let Some(len) = query_system(8, buf) else { return Vec::new() };
    let n = len / 48;
    (0..n.min(buf.len() / 6)).map(|i| CpuTimes { idle: buf[i * 6], kernel: buf[i * 6 + 1], user: buf[i * 6 + 2] }).collect()
}

/// Every process with its CPU time (SYSTEM_PROCESS_INFORMATION, 64-bit
/// layout read by offset).
#[cfg(target_pointer_width = "64")]
pub(crate) fn read_procs(buf: &mut Vec<u64>) -> Vec<Proc> {
    let Some(len) = query_system(5, buf) else { return Vec::new() };
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len() * 8) };
    let base = bytes.as_ptr() as usize;
    let rd32 = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap_or([0; 4]));
    let rd64 = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap_or([0; 8]));
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 104 <= len {
        let next = rd32(at) as usize;
        let created = rd64(at + 32) as i64;
        let time = rd64(at + 40).saturating_add(rd64(at + 48));
        let name_len = u16::from_le_bytes([bytes[at + 56], bytes[at + 57]]) as usize;
        let name_ptr = rd64(at + 64) as usize;
        let pid = rd64(at + 80) as usize;
        let session = rd32(at + 100);
        // PrivatePageCount (SIZE_T at 200; the entry is 256 bytes before
        // its threads).
        let private = if at + 208 <= len { rd64(at + 200) } else { 0 };
        // WorkingSetPrivateSize (LARGE_INTEGER at 8): Task Manager's
        // "Memory"; 0 is a real 0. Negative (never seen) reads as not known.
        let private_ws = u64::try_from(rd64(at + 8) as i64).ok();
        // The name lives inside the buffer; anything else is not trusted.
        let name = if name_ptr >= base && name_ptr + name_len <= base + len && name_len % 2 == 0 {
            let o = name_ptr - base;
            let units: Vec<u16> = bytes[o..o + name_len].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&units)
        } else if pid == 4 {
            "System".to_string()
        } else {
            String::new()
        };
        if pid != 0 && !name.is_empty() {
            out.push(Proc { pid, created, name, time, session, private, private_ws });
        }
        if next == 0 {
            break;
        }
        at += next;
    }
    out
}

#[cfg(not(target_pointer_width = "64"))]
pub(crate) fn read_procs(_buf: &mut Vec<u64>) -> Vec<Proc> {
    Vec::new()
}

/// Bytes received / sent so far per up, physical interface (by LUID).
fn read_net() -> HashMap<u64, (u64, u64)> {
    use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
    let mut out = HashMap::new();
    unsafe {
        let mut t: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut t).0 != 0 || t.is_null() {
            return out;
        }
        let n = (*t).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), n);
        for r in rows {
            let hardware = r.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
            // 24 = software loopback; 1 = IfOperStatusUp.
            if hardware && r.Type != 24 && r.OperStatus.0 == 1 {
                out.insert(r.InterfaceLuid.Value, (r.InOctets, r.OutOctets));
            }
        }
        FreeMibTable(t as *const _);
    }
    out
}

/// PDH counters (GPU 3D engines, disk idle time), opened only when wanted.
struct Pdh {
    query: windows::Win32::System::Performance::PDH_HQUERY,
    gpu: Option<windows::Win32::System::Performance::PDH_HCOUNTER>,
    disk: Option<windows::Win32::System::Performance::PDH_HCOUNTER>,
}

impl Pdh {
    /// A query with every requested counter, or None (nothing left open:
    /// a query missing a counter it was asked for is closed, and the
    /// caller tries again later).
    fn open(gpu: bool, disk: bool) -> Option<Pdh> {
        use windows::core::HSTRING;
        use windows::Win32::System::Performance::*;
        unsafe {
            let mut q = PDH_HQUERY::default();
            if PdhOpenQueryW(None, 0, &mut q) != 0 {
                return None;
            }
            // Owned from here: dropping it closes the query.
            let mut p = Pdh { query: q, gpu: None, disk: None };
            let add = |path: &str| {
                let mut c = PDH_HCOUNTER::default();
                (PdhAddEnglishCounterW(q, &HSTRING::from(path), 0, &mut c) == 0).then_some(c)
            };
            if gpu {
                p.gpu = Some(add(r"\GPU Engine(*engtype_3D)\Utilization Percentage")?);
            }
            if disk {
                p.disk = Some(add(r"\PhysicalDisk(_Total)\% Idle Time")?);
            }
            // The first collection only sets the baseline.
            if PdhCollectQueryData(q) != 0 {
                return None;
            }
            Some(p)
        }
    }

    /// Collects, then (GPU %, disk busy %).
    fn read(&self) -> (Option<f32>, Option<f32>) {
        use windows::Win32::System::Performance::*;
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return (None, None);
            }
        }
        let gpu = self.gpu.and_then(|c| array_sum(c).map(|v| v.clamp(0.0, 100.0) as f32));
        let disk = self.disk.and_then(|c| unsafe {
            let mut v = PDH_FMT_COUNTERVALUE::default();
            (PdhGetFormattedCounterValue(c, PDH_FMT(PDH_FMT_DOUBLE.0 | 0x8000), None, &mut v) == 0 && v.CStatus <= 1)
                .then(|| (100.0 - v.Anonymous.doubleValue).clamp(0.0, 100.0) as f32)
        });
        (gpu, disk)
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Performance::PdhCloseQuery(self.query);
        }
    }
}

/// Sum of a wildcard counter's instances.
fn array_sum(c: windows::Win32::System::Performance::PDH_HCOUNTER) -> Option<f64> {
    use windows::Win32::System::Performance::*;
    unsafe {
        let fmt = PDH_FMT(PDH_FMT_DOUBLE.0 | 0x8000);
        let (mut size, mut count) = (0u32, 0u32);
        let r = PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut count, None);
        if r != PDH_MORE_DATA || size == 0 {
            // No instances (no GPU work at all) reads as 0.
            return (r == 0).then_some(0.0);
        }
        let item = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let mut buf: Vec<u64> = vec![0; (size as usize).div_ceil(8) + 1];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        if PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut count, Some(items)) != 0 {
            return None;
        }
        let n = (count as usize).min(buf.len() * 8 / item);
        let s = std::slice::from_raw_parts(items, n);
        Some(s.iter().filter(|i| i.FmtValue.CStatus <= 1).map(|i| i.FmtValue.Anonymous.doubleValue.max(0.0)).sum())
    }
}

/// A PDH query that could not be opened (a counter missing, the service
/// busy) is tried again this long after.
const PDH_RETRY: Duration = Duration::from_secs(15);

// ---- the snapshot ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// Total CPU busy % (None until two readings).
    pub cpu: Option<f32>,
    /// The last minute of CPU, memory (%), GPU and disk, a sample a tick.
    pub history: Spark,
    pub mem_history: Spark,
    pub gpu_history: Spark,
    pub disk_history: Spark,
    /// When it was read (clock_ms).
    pub at: u64,
    /// (used, total) bytes.
    pub memory: Option<(u64, u64)>,
    pub gpu: Option<f32>,
    pub power: Power,
    /// Panel only (empty / None otherwise):
    pub cores: Vec<f32>,
    pub disk: Option<f32>,
    /// (down, up) bytes per second.
    pub net: Option<(f64, f64)>,
    /// The apps using the most CPU (each with its % of the machine over
    /// the last CPU_AVG_TICKS readings) and the most memory (bytes), best
    /// first, APPS_N at most: rows as crate::hogs lists them (name, icon,
    /// whether there is a window to show or close).
    pub apps_cpu: Vec<(crate::hogs::Hog, f32)>,
    pub apps_mem: Vec<(crate::hogs::Hog, u64)>,
    /// What NopBar itself uses, and its CPU over the last minute.
    pub own: Option<Own>,
    pub own_history: Spark,
    pub uptime_ms: u64,
    /// Snapshots taken since the sampler started.
    pub seq: u64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            cpu: None,
            history: Spark::new(SPARK_SPAN_MS),
            mem_history: Spark::new(SPARK_SPAN_MS),
            gpu_history: Spark::new(SPARK_SPAN_MS),
            disk_history: Spark::new(SPARK_SPAN_MS),
            at: 0,
            memory: None,
            gpu: None,
            power: Power { on_ac: true, ..Default::default() },
            cores: Vec::new(),
            disk: None,
            net: None,
            apps_cpu: Vec::new(),
            apps_mem: Vec::new(),
            own: None,
            own_history: Spark::new(SPARK_SPAN_MS),
            uptime_ms: 0,
            seq: 0,
        }
    }
}

impl Snapshot {
    /// Memory in use, %.
    pub fn memory_percent(&self) -> Option<f32> {
        self.memory.filter(|m| m.1 > 0).map(|(u, t)| (u as f64 / t as f64 * 100.0) as f32)
    }
}

/// What the sampler should read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Want {
    /// Read at all (the panel is open).
    pub run: bool,
    /// The panel's extras (GPU, disk, network, cores, processes).
    pub full: bool,
}

/// What the sampler is asked, under one lock.
struct Gate {
    want: Want,
    /// The thread is alive (one at most, process-wide).
    running: bool,
    /// Which opening of the panel it serves (Sampler::on): a reading
    /// begun for an earlier one is never published, and the thread starts
    /// over (no deltas, no histories) for a new one.
    gen: u64,
}

/// A reading a second.
const TICK: Duration = Duration::from_secs(1);

/// What the thread runs: `run`, or a stand-in in the tests.
type Body = Arc<dyn Fn(&Shared) + Send + Sync>;

struct Shared {
    gate: Mutex<Gate>,
    wake: Condvar,
    snap: Mutex<Snapshot>,
    hwnd: AtomicIsize,
    msg: AtomicU32,
    body: Body,
    /// How long the thread rests between readings (TICK).
    period: Duration,
}

impl Shared {
    fn new(body: Body, period: Duration) -> Arc<Shared> {
        Arc::new(Shared {
            gate: Mutex::new(Gate { want: Want::default(), running: false, gen: 0 }),
            wake: Condvar::new(),
            snap: Mutex::new(Snapshot::default()),
            hwnd: AtomicIsize::new(0),
            msg: AtomicU32::new(0),
            body,
            period,
        })
    }

    fn gate(&self) -> std::sync::MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The process's one sampler state: every panel opening uses it, so a
    /// thread still alive from the last one (held up in a reading) is
    /// re-armed for the next instead of a second one starting.
    fn global() -> Arc<Shared> {
        static ONE: std::sync::OnceLock<Arc<Shared>> = std::sync::OnceLock::new();
        ONE.get_or_init(|| Shared::new(Arc::new(run), TICK)).clone()
    }
}

/// The worker thread's handle. At most one thread runs in the process,
/// whatever opens and closes meanwhile; dropping this stops it (it exits
/// within a moment, or once a reading it is held up in returns; nothing
/// ever waits for it).
pub struct Sampler {
    shared: Arc<Shared>,
    want: Want,
    gen: u64,
}

impl Sampler {
    /// A sampler that posts `msg` to `hwnd` after every snapshot. Idle
    /// (no thread) until `set` asks for something.
    pub fn new(hwnd: HWND, msg: u32) -> Self {
        Self::on(Shared::global(), hwnd, msg)
    }

    /// A new opening on `shared`: fresh numbers, its own window; whatever
    /// an earlier one asked is dropped.
    fn on(shared: Arc<Shared>, hwnd: HWND, msg: u32) -> Self {
        let gen = {
            let mut g = shared.gate();
            g.gen += 1;
            g.want = Want::default();
            *shared.snap.lock().unwrap_or_else(|e| e.into_inner()) = Snapshot::default();
            shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
            shared.msg.store(msg, Ordering::SeqCst);
            g.gen
        };
        shared.wake.notify_all();
        Self { shared, want: Want::default(), gen }
    }

    /// Starts, changes or pauses the reading. A change takes effect at once
    /// (the panel opening gets its numbers without waiting a second).
    pub fn set(&mut self, w: Want) {
        if w == self.want {
            return;
        }
        self.want = w;
        let start = {
            let mut g = self.shared.gate();
            if g.gen != self.gen {
                // Another opening since: not ours to steer.
                return;
            }
            g.want = w;
            // One thread at a time: one alive (paused, or held up in a
            // reading) keeps going instead.
            let start = w.run && !g.running;
            if start {
                g.running = true;
            }
            start
        };
        self.shared.wake.notify_all();
        if start {
            let shared = self.shared.clone();
            let r = std::thread::Builder::new().name("sysstat".into()).spawn(move || {
                let body = shared.body.clone();
                body(&shared)
            });
            if r.is_err() {
                self.shared.gate().running = false;
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.snap.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        {
            let mut g = self.shared.gate();
            if g.gen == self.gen {
                g.want = Want::default();
                self.shared.hwnd.store(0, Ordering::SeqCst);
            }
        }
        self.shared.wake.notify_all();
    }
}

/// What the thread does next (decided under the gate).
enum Turn {
    /// Read this much for its opening.
    Read(Want),
    /// A new opening: start over for it.
    New(u64),
    /// Nobody wants anything: release the reader, then `stop` (the
    /// thread stays the one running until its reader is gone).
    Stop,
}

fn turn(shared: &Shared, gen: u64) -> Turn {
    let g = shared.gate();
    if !g.want.run {
        Turn::Stop
    } else if g.gen != gen {
        Turn::New(g.gen)
    } else {
        Turn::Read(g.want)
    }
}

/// After the reader is released: the opening asked for meanwhile (served
/// by this thread, with a fresh reader), else the thread is gone (marked
/// under the lock: a start from now on sees no thread and spawns one).
fn stop(shared: &Shared) -> Option<u64> {
    let mut g = shared.gate();
    if g.want.run {
        Some(g.gen)
    } else {
        g.running = false;
        None
    }
}

/// The thread's loop: for each opening a fresh reader (`fresh`), then a
/// tick (`tick`: the last snapshot in, the new one out) a second, or as
/// soon as what is wanted changes; exits once nobody wants anything. A
/// tick held up past a new opening is not published.
fn drive<R>(shared: &Shared, mut fresh: impl FnMut() -> R, mut tick: impl FnMut(&mut R, Want, Snapshot) -> Snapshot) {
    let mut gen = shared.gate().gen;
    let mut reader = fresh();
    loop {
        let want = match turn(shared, gen) {
            Turn::Stop => {
                // Its queries close here (PdhCloseQuery, maybe slowly) while
                // the thread is still the one running: an opening meanwhile
                // is taken up below, never given a second thread.
                drop(reader);
                match stop(shared) {
                    None => return,
                    Some(g) => {
                        gen = g;
                        reader = fresh();
                        continue;
                    }
                }
            }
            Turn::New(g) => {
                gen = g;
                reader = fresh();
                continue;
            }
            Turn::Read(w) => w,
        };
        let s = shared.snap.lock().map(|s| s.clone()).unwrap_or_default();
        let s = tick(&mut reader, want, s);
        let h = {
            let g = shared.gate();
            if g.gen != gen {
                continue;
            }
            *shared.snap.lock().unwrap_or_else(|e| e.into_inner()) = s;
            shared.hwnd.load(Ordering::SeqCst)
        };
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), shared.msg.load(Ordering::SeqCst), WPARAM(0), LPARAM(0));
            }
        }
        // A second, or less if what is wanted (or the opening) changes.
        let g = shared.gate();
        let _ = shared.wake.wait_timeout_while(g, shared.period, |g| g.want == want && g.gen == gen);
    }
}

/// The real thread: the system's numbers.
fn run(shared: &Shared) {
    drive(shared, Reading::new, Reading::tick);
}

/// What the thread keeps from one tick to the next, for one opening.
struct Reading {
    cpu_prev: Option<CpuTimes>,
    cores_prev: Vec<CpuTimes>,
    procs_prev: Vec<Proc>,
    /// The apps' CPU time over the last readings (the lists' average).
    cpu_window: CpuWindow,
    procs_at: Instant,
    /// When procs_prev was read (FILETIME, as Proc::created).
    procs_prev_ft: i64,
    net_prev: Option<(HashMap<u64, (u64, u64)>, Instant)>,
    pdh: Option<(Pdh, bool, bool)>,
    /// When opening the PDH query last failed (tried again after PDH_RETRY).
    pdh_retry: Option<Instant>,
    pdh_fails: u32,
    buf: Vec<u64>,
    /// The power list (crate::hogs) while the panel is open: its GPU query
    /// and when it last noted the apps.
    hogs: Option<crate::hogs::Reader>,
    hogs_at: Option<Instant>,
    cpus: u32,
    want: Want,
    me: usize,
}

impl Reading {
    fn new() -> Reading {
        Reading {
            cpu_prev: None,
            cores_prev: Vec::new(),
            procs_prev: Vec::new(),
            cpu_window: CpuWindow::default(),
            procs_at: Instant::now(),
            procs_prev_ft: 0,
            net_prev: None,
            pdh: None,
            pdh_retry: None,
            pdh_fails: 0,
            buf: Vec::new(),
            hogs: None,
            hogs_at: None,
            cpus: std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1),
            want: Want::default(),
            me: std::process::id() as usize,
        }
    }

    /// One reading into `s` (the last snapshot). Never waits for the
    /// shell: icons and app names come from crate::hogs's lane.
    fn tick(&mut self, w: Want, mut s: Snapshot) -> Snapshot {
        let Reading { cpu_prev, cores_prev, procs_prev, cpu_window, procs_at, procs_prev_ft, net_prev, pdh, pdh_retry, pdh_fails, buf, hogs, hogs_at, cpus, want, me } = self;
        let (cpus, me) = (*cpus, *me);
        let mut opened = false;
        if w.full && !want.full {
            // The panel just opened: its deltas start now.
            opened = true;
            cores_prev.clear();
            procs_prev.clear();
            cpu_window.clear();
            *net_prev = None;
        }
        if !w.full {
            // Nobody wants the speed: the next start counts from then.
            *net_prev = None;
        }
        *want = w;
        let want = w;
        if opened {
            // NopBar's own line starts over with the panel: no value from
            // an earlier opening.
            s.own = None;
            s.own_history = Spark::new(SPARK_SPAN_MS);
        }
        // Every tick goes into the histories, read or not (a gap), at its
        // own time: a late or missed tick never squeezes the minute.
        let tick = clock_ms();
        s.at = tick;
        let mut cpu_now = None;
        if let Some(now) = read_cpu_times() {
            if let Some(p) = *cpu_prev {
                cpu_now = cpu_percent(p, now);
                if let Some(v) = cpu_now {
                    s.cpu = Some(v);
                }
            }
            *cpu_prev = Some(now);
        }
        s.history.push(tick, cpu_now);
        s.memory = read_memory();
        s.mem_history.push(tick, s.memory_percent());
        s.power = read_power();
        s.uptime_ms = uptime_ms();
        // PDH only when GPU or disk numbers are wanted (reopened when that changes).
        let (need_gpu, need_disk) = (want.full, want.full);
        let mut fresh = false;
        if !need_gpu && !need_disk {
            *pdh = None;
        } else if pdh.as_ref().is_some_and(|p| p.1 != need_gpu || p.2 != need_disk) {
            // Other counters wanted: open again now.
            *pdh = None;
            *pdh_retry = None;
        }
        if (need_gpu || need_disk) && pdh.is_none() && pdh_retry.is_none_or(|at| at.elapsed() >= PDH_RETRY) {
            *pdh = Pdh::open(need_gpu, need_disk).map(|p| (p, need_gpu, need_disk));
            *pdh_retry = if pdh.is_none() { Some(Instant::now()) } else { None };
            fresh = pdh.is_some();
        }
        // A query opened just now has only its baseline; a failed read is
        // unknown (shown as such), never the last good value.
        let (gpu, disk) = if fresh { (None, None) } else { pdh.as_ref().map(|p| p.0.read()).unwrap_or((None, None)) };
        // A query that keeps failing is closed and opened again later.
        let failed = !fresh && pdh.is_some() && ((need_gpu && gpu.is_none()) || (need_disk && disk.is_none()));
        *pdh_fails = if failed { *pdh_fails + 1 } else { 0 };
        if *pdh_fails >= 5 {
            *pdh = None;
            *pdh_retry = Some(Instant::now());
            *pdh_fails = 0;
        }
        s.gpu = if need_gpu { gpu } else { None };
        s.disk = if need_disk { disk } else { None };
        s.gpu_history.push(tick, s.gpu);
        s.disk_history.push(tick, s.disk);
        if want.full {
            // Network speed.
            let now = Instant::now();
            let net = read_net();
            if let Some((old, at)) = net_prev.as_ref() {
                let secs = (now - *at).as_secs_f64();
                let (mut down, mut up) = (0u64, 0u64);
                for (luid, (i, o)) in &net {
                    if let Some((pi, po)) = old.get(luid) {
                        down += counter_delta(*pi, *i, 64);
                        up += counter_delta(*po, *o, 64);
                    }
                }
                s.net = Some((rate(down, secs), rate(up, secs)));
            }
            *net_prev = Some((net, now));
        } else {
            s.net = None;
        }
        if want.full {
            let cores = read_cores(buf);
            if cores.len() == cores_prev.len() {
                s.cores = cores.iter().zip(cores_prev.iter()).map(|(c, p)| cpu_percent(*p, *c).unwrap_or(0.0)).collect();
            }
            *cores_prev = cores;
            let now = Instant::now();
            let now_ft = file_time_now();
            let procs = read_procs(buf);
            if hogs_at.is_none_or(|t| t.elapsed() >= crate::hogs::PANEL_EVERY) {
                *hogs_at = Some(Instant::now());
                let r = hogs.get_or_insert_with(crate::hogs::Reader::new);
                let sample = r.sample(&procs);
                crate::hogs::record(sample, true);
            }
            let elapsed = (now - *procs_at).as_nanos() as u64 / 100;
            if !procs_prev.is_empty() {
                cpu_window.push(elapsed, app_times(procs_prev, &procs));
            }
            // The lists: every process of an exe as one app. Their names,
            // icons and windows come from crate::hogs (its lane reads the
            // shell; nothing here waits for it).
            let all = apps(&procs, &cpu_window.percent(cpus));
            let row = |a: &AppUse| crate::hogs::describe(&a.key, &a.procs, a.session);
            s.apps_cpu = top_by_cpu(&all, APPS_N).iter().map(|a| (row(a), a.cpu)).collect();
            s.apps_mem = top_by_memory(&all, APPS_N).iter().map(|a| (row(a), a.memory)).collect();
            // NopBar's own use, from the same list (nothing more is read).
            s.own = own_usage(procs_prev, *procs_prev_ft, &procs, me, elapsed, cpus);
            s.own_history.push(tick, s.own.and_then(|o| o.cpu));
            *procs_prev = procs;
            *procs_at = now;
            *procs_prev_ft = now_ft;
        } else {
            // The panel closed: its GPU query goes with it.
            *hogs = None;
            *hogs_at = None;
            s.cores.clear();
            s.apps_cpu.clear();
            s.apps_mem.clear();
            cpu_window.clear();
            s.own = None;
            s.own_history = Spark::new(SPARK_SPAN_MS);
        }
        s.seq += 1;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_percent_from_time_deltas() {
        let a = CpuTimes { idle: 1000, kernel: 2000, user: 1000 };
        // 1000 kernel (of which 750 idle) + 1000 user = 2000; busy 1250.
        let b = CpuTimes { idle: 1750, kernel: 3000, user: 2000 };
        assert_eq!(cpu_percent(a, b), Some(62.5));
        // All idle.
        let c = CpuTimes { idle: 2000, kernel: 3000, user: 1000 };
        assert_eq!(cpu_percent(a, c), Some(0.0));
        // Fully busy.
        let d = CpuTimes { idle: 1000, kernel: 2500, user: 1500 };
        assert_eq!(cpu_percent(a, d), Some(100.0));
        // No time passed, or counters went back.
        assert_eq!(cpu_percent(a, a), None);
        assert_eq!(cpu_percent(b, a), None);
    }

    #[test]
    fn network_rates_with_counter_wrap() {
        assert_eq!(counter_delta(100, 350, 64), 250);
        // A 32-bit counter wrapping past its top.
        assert_eq!(counter_delta(u32::MAX as u64 - 9, 20, 32), 30);
        // A 64-bit one too.
        assert_eq!(counter_delta(u64::MAX - 4, 5, 64), 10);
        // A reset (adapter back, counter from zero) is no traffic.
        assert_eq!(counter_delta(1_000_000, 10, 32), 0);
        assert_eq!(counter_delta(5_000_000_000, 10, 64), 0);
        // Out of range for its width: a reset too.
        assert_eq!(counter_delta(u64::MAX, 10, 32), 0);
        assert_eq!(rate(2048, 2.0), 1024.0);
        assert_eq!(rate(2048, 0.0), 0.0);
        assert_eq!(rate_text(512.0), "0.5 KB/s");
        assert_eq!(rate_text(2.5 * 1024.0 * 1024.0), "2.5 MB/s");
        assert_eq!(dhm(((2 * 24 + 3) * 60 + 4) * 60_000 + 59_000), (2, 3, 4));
    }

    #[test]
    fn sparkline_keeps_the_last_minute_by_time() {
        let mut s = Spark::new(3_000);
        assert!(s.runs(0).is_empty());
        for (t, v) in [(0, 1.0), (1_000, 2.0), (2_000, 3.0)] {
            s.push(t, Some(v));
        }
        assert_eq!(s.runs(2_000), vec![vec![(2_000, 1.0), (1_000, 2.0), (0, 3.0)]]);
        s.push(3_000, Some(4.0));
        s.push(4_000, Some(5.0));
        assert_eq!(s.live(4_000), vec![2.0, 3.0, 4.0, 5.0], "older than the span: gone");
        // A late look also leaves out what aged out meanwhile.
        assert_eq!(s.live(6_000), vec![4.0, 5.0]);
        s.push(3_500, Some(9.0));
        assert_eq!(s.live(4_000), vec![2.0, 3.0, 4.0, 5.0], "the clock never goes back");
        assert_eq!(Spark::new(0).span(), 1);
        assert_eq!(Snapshot::default().history.span(), SPARK_SPAN_MS);
    }

    /// GPU at 80 %, then its counter fails for 90 s: the old line ages
    /// out (it never stays as if current), the scale forgets its peak, and
    /// the line after the gap is not joined to the one before.
    #[test]
    fn a_missing_reading_is_a_gap_and_old_values_expire() {
        let mut s = Spark::new(SPARK_SPAN_MS);
        let mut t = 0;
        for _ in 0..60 {
            s.push(t, Some(80.0));
            t += 1_000;
        }
        for _ in 0..90 {
            s.push(t, None);
            t += 1_000;
        }
        assert!(s.runs(t).is_empty(), "nothing read in the last minute: no line");
        assert!(s.live(t).is_empty(), "no old peak in the scale");
        s.push(t, Some(10.0));
        s.push(t + 1_000, Some(12.0));
        assert_eq!(s.runs(t + 1_000), vec![vec![(1_000, 10.0), (0, 12.0)]]);
        // One unread tick in the middle breaks the line in two.
        let mut g = Spark::new(SPARK_SPAN_MS);
        for (i, v) in [Some(1.0), Some(2.0), None, Some(3.0), Some(4.0)].into_iter().enumerate() {
            g.push(i as u64 * 1_000, v);
        }
        assert_eq!(g.runs(4_000), vec![vec![(4_000, 1.0), (3_000, 2.0)], vec![(1_000, 3.0), (0, 4.0)]]);
        // No tick at all for a while (the sampler held up): a gap too.
        let mut h = Spark::new(SPARK_SPAN_MS);
        h.push(0, Some(5.0));
        h.push(1_000, Some(6.0));
        h.push(1_000 + SPARK_STEP_MS + 1, Some(7.0));
        assert_eq!(h.runs(1_000 + SPARK_STEP_MS + 1).len(), 2);
    }

    fn proc_of(pid: usize, created: i64, name: &str, time: u64, ws: u64) -> Proc {
        Proc { pid, created, name: name.into(), time, session: 1, private: 0, private_ws: Some(ws) }
    }

    /// The same exe many times over is one row, summed before the cut; a
    /// reused pid and a process new since the last reading count nothing.
    #[test]
    fn apps_group_their_processes_before_they_are_ranked() {
        let p = proc_of;
        let prev = vec![p(10, 1, "Browser.exe", 1_000, 0), p(11, 1, "browser.exe", 0, 0), p(12, 1, "browser.exe", 0, 0), p(20, 1, "big.exe", 0, 0), p(13, 1, "old.exe", 0, 0), p(14, 1, "idle.exe", 0, 0)];
        let cur = vec![
            p(10, 1, "Browser.exe", 1_600, 100), // +600
            p(11, 1, "browser.exe", 500, 200),   // +500
            p(12, 1, "browser.exe", 400, 300),   // +400: 1500 together
            p(20, 1, "big.exe", 1_000, 50),      // +1000: the busiest single process
            p(13, 9, "new.exe", 9_000, 10),      // pid reused: a new process, not counted
            p(14, 1, "idle.exe", 0, 5),
            p(15, 1, "fresh.exe", 50, 5), // new since prev
            p(0, 0, "Idle", 99_999, 0),
        ];
        let used = app_times(&prev, &cur);
        assert_eq!((used.get("browser.exe"), used.get("big.exe"), used.get("new.exe"), used.get("fresh.exe")), (Some(&1_500), Some(&1_000), None, None));
        // 1000 units elapsed on 4 CPUs = 4000 units of CPU.
        let mut w = CpuWindow::default();
        assert!(w.percent(4).is_empty(), "nothing read yet");
        w.push(1_000, used);
        let all = apps(&cur, &w.percent(4));
        let top = top_by_cpu(&all, 5);
        assert_eq!(top.iter().map(|a| (a.key.as_str(), a.cpu)).collect::<Vec<_>>(), [("browser.exe", 37.5), ("big.exe", 25.0)], "summed, then cut; idle apps left out");
        assert_eq!(top[0].procs, [(10, 1), (11, 1), (12, 1)], "its processes ride along");
        assert_eq!(top_by_cpu(&all, 1).len(), 1);
        // Memory: the working sets summed, ties by name.
        let mem = top_by_memory(&all, 3);
        assert_eq!(mem.iter().map(|a| (a.key.as_str(), a.memory)).collect::<Vec<_>>(), [("browser.exe", 600), ("big.exe", 50), ("new.exe", 10)]);
        assert_eq!(all.iter().find(|a| a.key == "new.exe").map(|a| &a.procs), Some(&vec![(13u32, 9i64)]), "the reused pid as the process it is now");
        // A working set not known adds nothing (never the commit instead).
        let blind = [Proc { private_ws: None, private: 999, ..p(30, 1, "x.exe", 0, 0) }];
        assert_eq!(apps(&blind, &HashMap::new())[0].memory, 0);
    }

    /// The lists' CPU share is the last three readings together.
    #[test]
    fn the_cpu_share_is_averaged_over_three_readings() {
        let one = |v: u64| HashMap::from([("a.exe".to_string(), v)]);
        let mut w = CpuWindow::default();
        w.push(1_000, one(4_000)); // 100 % of 4 CPUs
        w.push(1_000, one(0));
        assert_eq!(w.percent(4).get("a.exe"), Some(&50.0));
        w.push(2_000, one(0));
        assert_eq!(w.percent(4).get("a.exe"), Some(&25.0), "by wall time, not by reading");
        w.push(1_000, HashMap::new());
        assert_eq!(w.percent(4).get("a.exe"), Some(&0.0), "the spike slid out");
        w.push(0, one(9_999));
        assert_eq!(w.percent(4).get("a.exe"), Some(&0.0), "no time passed: not a reading");
        w.clear();
        assert!(w.percent(4).is_empty());
        assert!(CpuWindow::default().percent(0).is_empty());
    }

    #[test]
    fn own_usage_sums_every_nopbar_process() {
        const MB: u64 = 1 << 20;
        // `ws`: the private working set (Task Manager's "Memory"); the
        // commit (private bytes) is always larger and is not what shows.
        let p = |pid: usize, created: i64, name: &str, time: u64, session: u32, ws: u64| Proc { pid, created, name: name.into(), time, session, private: ws + 20 * MB, private_ws: Some(ws) };
        // The previous reading was at time 10; the processes below were
        // created at 1 (before it).
        const AT: i64 = 10;
        // The dock (100), its guardian (101) and Settings (102), plus
        // another user's copy (session 2) and an unrelated app.
        let prev = vec![
            p(100, 1, "nopbar.exe", 10_000, 1, 80 * MB),
            p(101, 1, "nopbar.exe", 500, 1, 3 * MB),
            p(102, 1, "NopBar.EXE", 0, 1, 40 * MB),
            p(200, 1, "nopbar.exe", 0, 2, 70 * MB),
            p(300, 1, "app.exe", 0, 1, 500 * MB),
        ];
        let cur = vec![
            p(100, 1, "nopbar.exe", 10_600, 1, 82 * MB), // +600
            p(101, 1, "nopbar.exe", 500, 1, 3 * MB),     // idle
            p(102, 1, "NopBar.EXE", 200, 1, 45 * MB),    // +200, any case
            // Older than the previous reading but not in it (not read
            // then): no baseline, no CPU. Its working set is a real 0
            // (paged out): 0, not its 20 MB of commit.
            p(103, 5, "nopbar.exe", 9_000, 1, 0),
            p(200, 1, "nopbar.exe", 4_000, 2, 70 * MB), // another session: not ours
            p(300, 1, "app.exe", 9_000, 1, 500 * MB),
        ];
        // 1000 units elapsed on 4 CPUs = 4000; ours used 800 of it.
        let own = own_usage(&prev, AT, &cur, 100, 1_000, 4).expect("the dock is listed");
        assert_eq!(own.cpu, Some(20.0));
        assert_eq!(own.memory, Some((82 + 3 + 45) * MB), "working sets only; a 0 adds 0");
        assert_eq!(own.procs, 4);
        assert_eq!(own_procs(&cur, 100).len(), 4);
        // The first reading: memory at once, CPU once there is a delta.
        let first = own_usage(&[], 0, &cur, 100, 1_000, 4).unwrap();
        assert_eq!((first.cpu, first.memory, first.procs), (None, own.memory, 4));
        // Proc::memory on its own: a 0 is 0, unknown is unknown.
        let ws = p(1, 1, "a.exe", 0, 1, 5 * MB);
        assert_eq!(ws.memory(), Some(5 * MB));
        assert_eq!(Proc { private_ws: Some(0), ..ws.clone() }.memory(), Some(0));
        assert_eq!(Proc { private_ws: None, ..ws }.memory(), None, "never the commit instead");
        // One process's working set not known: the sum is not known
        // either (no part shown as the whole, no other measure mixed in).
        let blind = vec![p(100, 1, "nopbar.exe", 10_600, 1, 82 * MB), Proc { private_ws: None, ..p(101, 1, "nopbar.exe", 500, 1, 0) }];
        assert_eq!(own_usage(&prev, AT, &blind, 100, 1_000, 4).unwrap().memory, None);
        assert_eq!(own_usage(&prev, AT, &cur, 100, 0, 4).unwrap().cpu, None, "no time passed");
        // A reused pid created before the previous reading: unknown, 0.
        let reused = vec![p(100, 9, "nopbar.exe", 50_000, 1, MB)];
        assert_eq!(own_usage(&prev, AT, &reused, 100, 1_000, 4).unwrap().cpu, Some(0.0));
        assert_eq!(own_usage(&prev, AT, &cur, 999, 1_000, 4), None);
        // Never past the whole machine.
        let busy = vec![p(100, 1, "nopbar.exe", 90_000, 1, MB)];
        assert_eq!(own_usage(&prev, AT, &busy, 100, 1_000, 4).unwrap().cpu, Some(100.0));
    }

    /// A NopBar process started since the previous reading (Settings, the
    /// updater) counts all its CPU time: it all falls in this interval.
    #[test]
    fn own_usage_counts_a_new_process_from_its_start() {
        let p = |pid: usize, created: i64, time: u64| Proc { pid, created, name: "nopbar.exe".into(), time, session: 1, private: 0, private_ws: Some(0) };
        let prev = vec![p(100, 1, 10_000)];
        // 4 CPUs, 1000 elapsed: the dock +600, a new one (created at 15,
        // after the reading at 10) used 2000: 2600 / 4000 = 65 %.
        let cur = vec![p(100, 1, 10_600), p(104, 15, 2_000)];
        assert_eq!(own_usage(&prev, 10, &cur, 100, 1_000, 4).unwrap().cpu, Some(65.0));
        // The dock's pid reused by a new process since then: its time
        // from its start, not a delta against the old one's.
        let reborn = vec![p(100, 1, 10_600), p(104, 15, 2_000)];
        let prev_old = vec![p(100, 1, 10_000), p(104, 2, 50_000)];
        assert_eq!(own_usage(&prev_old, 10, &reborn, 100, 1_000, 4).unwrap().cpu, Some(65.0));
        // Created before the reading but missing from it: no baseline.
        let missed = vec![p(100, 1, 10_600), p(104, 5, 2_000)];
        assert_eq!(own_usage(&prev, 10, &missed, 100, 1_000, 4).unwrap().cpu, Some(15.0));
    }

    /// Waits (a bounded while) for `ok`; false if it never came.
    fn until(ok: impl Fn() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        ok()
    }

    /// A stand-in sampler thread: the real loop (drive) with a tick that
    /// can be held up (`held`) as a reading stuck in the shell would be.
    struct Fake {
        started: std::sync::atomic::AtomicUsize,
        alive: std::sync::atomic::AtomicUsize,
        most: std::sync::atomic::AtomicUsize,
        /// Openings it started over for (fresh readers).
        sessions: std::sync::atomic::AtomicUsize,
        ticks: std::sync::atomic::AtomicUsize,
        held: Mutex<bool>,
        release: Condvar,
        /// Readers whose release began (Lease::drop).
        releasing: std::sync::atomic::AtomicUsize,
        /// A reader's release held up (`releasing`), as PdhCloseQuery may be.
        held_drop: Mutex<bool>,
        release_drop: Condvar,
    }

    /// The stand-in reader: its drop can be held up (`held_drop`).
    struct Lease(Arc<Fake>);

    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.releasing.fetch_add(1, Ordering::SeqCst);
            let g = self.0.held_drop.lock().unwrap();
            drop(self.0.release_drop.wait_while(g, |h| *h).unwrap());
        }
    }

    fn fake() -> (Arc<Fake>, Arc<Shared>) {
        use std::sync::atomic::AtomicUsize;
        let f = Arc::new(Fake {
            started: AtomicUsize::new(0),
            alive: AtomicUsize::new(0),
            most: AtomicUsize::new(0),
            sessions: AtomicUsize::new(0),
            ticks: AtomicUsize::new(0),
            held: Mutex::new(false),
            release: Condvar::new(),
            releasing: AtomicUsize::new(0),
            held_drop: Mutex::new(false),
            release_drop: Condvar::new(),
        });
        let ff = f.clone();
        let shared = Shared::new(Arc::new(move |sh: &Shared| {
            ff.started.fetch_add(1, Ordering::SeqCst);
            let n = ff.alive.fetch_add(1, Ordering::SeqCst) + 1;
            ff.most.fetch_max(n, Ordering::SeqCst);
            drive(
                sh,
                || {
                    ff.sessions.fetch_add(1, Ordering::SeqCst);
                    Lease(ff.clone())
                },
                |_, _, mut s| {
                    ff.ticks.fetch_add(1, Ordering::SeqCst);
                    let g = ff.held.lock().unwrap();
                    drop(ff.release.wait_while(g, |h| *h).unwrap());
                    s.seq += 1;
                    s
                },
            );
            ff.alive.fetch_sub(1, Ordering::SeqCst);
        }), Duration::from_millis(5));
        (f, shared)
    }

    fn hold(f: &Fake, on: bool) {
        *f.held.lock().unwrap() = on;
        f.release.notify_all();
    }

    fn hold_drop(f: &Fake, on: bool) {
        *f.held_drop.lock().unwrap() = on;
        f.release_drop.notify_all();
    }

    const RUN: Want = Want { run: true, full: true };

    /// The panel closed, and the sampler's reader is slow to let go (its
    /// PDH queries closing): reopened (and paused, resumed) meanwhile, no
    /// second thread starts; once the reader is gone the same thread
    /// serves the new opening with a fresh one, then exits on close.
    #[test]
    fn a_sampler_releasing_its_reader_is_still_the_one() {
        let (f, shared) = fake();
        hold_drop(&f, true);
        let mut first = Sampler::on(shared.clone(), HWND::default(), 0);
        first.set(RUN);
        assert!(until(|| first.snapshot().seq >= 1), "reads while open");
        drop(first);
        assert!(until(|| f.releasing.load(Ordering::SeqCst) == 1), "closed: the thread is releasing its reader");
        for _ in 0..5 {
            let mut s = Sampler::on(shared.clone(), HWND::default(), 0);
            s.set(RUN);
            s.set(Want::default());
            s.set(RUN);
            drop(s);
        }
        let mut next = Sampler::on(shared.clone(), HWND::default(), 0);
        next.set(RUN);
        assert_eq!(f.started.load(Ordering::SeqCst), 1, "no second thread while the reader is released");
        assert_eq!(f.alive.load(Ordering::SeqCst), 1, "the first is still in its release");
        assert_eq!(next.snapshot().seq, 0, "a fresh opening");
        hold_drop(&f, false);
        assert!(until(|| next.snapshot().seq >= 1), "the same thread serves the new opening");
        assert_eq!(f.sessions.load(Ordering::SeqCst), 2, "a fresh reader for it");
        assert_eq!((f.started.load(Ordering::SeqCst), f.most.load(Ordering::SeqCst)), (1, 1));
        drop(next);
        assert!(until(|| f.alive.load(Ordering::SeqCst) == 0), "closed: the thread goes");
        assert_eq!(f.releasing.load(Ordering::SeqCst), 2);
        assert_eq!(f.started.load(Ordering::SeqCst), 1);
    }

    /// The panel closed while its sampler was stuck in a reading, then
    /// opened and closed again and again (and the session locked and
    /// unlocked meanwhile): still the one thread, never a second; once
    /// the reading returns it serves the latest opening (its stale reading
    /// not published), and exits when that closes. Nothing joins it.
    #[test]
    fn a_stuck_sampler_is_re_armed_never_stacked() {
        let (f, shared) = fake();
        hold(&f, true);
        let mut first = Sampler::on(shared.clone(), HWND::default(), 0);
        first.set(RUN);
        assert!(until(|| f.ticks.load(Ordering::SeqCst) == 1), "the thread is in its reading");
        drop(first);
        for _ in 0..20 {
            let mut s = Sampler::on(shared.clone(), HWND::default(), 0);
            s.set(RUN);
            // Locked, then unlocked.
            s.set(Want::default());
            s.set(RUN);
            drop(s);
        }
        let mut last = Sampler::on(shared.clone(), HWND::default(), 0);
        last.set(RUN);
        assert_eq!(f.started.load(Ordering::SeqCst), 1, "no second thread while the first is stuck");
        assert_eq!(last.snapshot().seq, 0, "a fresh opening");
        hold(&f, false);
        assert!(until(|| last.snapshot().seq >= 1), "the same thread serves the latest opening");
        assert_eq!(f.sessions.load(Ordering::SeqCst), 2, "started over once, for the latest opening");
        assert_eq!((f.started.load(Ordering::SeqCst), f.most.load(Ordering::SeqCst)), (1, 1));
        drop(last);
        assert!(until(|| f.alive.load(Ordering::SeqCst) == 0), "closed: the thread goes");
        assert_eq!(f.started.load(Ordering::SeqCst), 1);
    }

    /// Open, pause (locked / display off), resume, close, open again: the
    /// thread starts on open, goes on pause and close, and there is never
    /// more than one; a sampler of an earlier opening steers nothing.
    #[test]
    fn the_sampler_starts_on_open_and_stops_on_close() {
        let (f, shared) = fake();
        let mut a = Sampler::on(shared.clone(), HWND::default(), 0);
        assert_eq!(f.started.load(Ordering::SeqCst), 0, "idle until asked");
        a.set(RUN);
        assert!(until(|| a.snapshot().seq >= 2), "reads while open");
        a.set(Want::default());
        assert!(until(|| f.alive.load(Ordering::SeqCst) == 0), "paused: gone");
        a.set(RUN);
        assert!(until(|| f.started.load(Ordering::SeqCst) == 2 && a.snapshot().seq >= 3), "resumed");
        drop(a);
        assert!(until(|| f.alive.load(Ordering::SeqCst) == 0), "closed: gone");
        let stale = Sampler::on(shared.clone(), HWND::default(), 0);
        let mut b = Sampler::on(shared.clone(), HWND::default(), 0);
        b.set(RUN);
        assert!(until(|| b.snapshot().seq >= 1));
        drop(stale);
        let seq = b.snapshot().seq;
        assert!(until(|| b.snapshot().seq > seq), "an old sampler dropping does not stop the new one");
        drop(b);
        assert!(until(|| f.alive.load(Ordering::SeqCst) == 0));
        assert_eq!(f.most.load(Ordering::SeqCst), 1, "never two at once");
    }

    #[test]
    fn memory_percent() {
        let s = Snapshot { memory: Some((4, 16)), ..Default::default() };
        assert_eq!(s.memory_percent(), Some(25.0));
        assert_eq!(Snapshot { memory: Some((0, 0)), ..Default::default() }.memory_percent(), None);
    }
}

/// Called by existing power broadcasts, not by the stats sampler.
pub fn companion_power(){static EDGES:std::sync::Mutex<crate::companion::cues::PowerEdges>=std::sync::Mutex::new(crate::companion::cues::PowerEdges::new());let p=read_power();let cues=EDGES.lock().unwrap_or_else(|e|e.into_inner()).observe(p.battery,p.on_ac);for cue in cues{crate::companion::cue(cue);}}
