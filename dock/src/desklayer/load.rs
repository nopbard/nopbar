//! How much the widgets ask of the PC.
//!
//! Measured, not guessed: each widget kind reports what it holds (its
//! surfaces and decoded pictures, in bytes) and the CPU it took is counted
//! where it runs: the image pieces on the dock's thread (spans around their window procedures, exclusive of what they
//! call into: crate::desklayer::load::span), the notes on their own thread
//! (its CPU time). The top bar and the dock are spanned the same way, so
//! the system stats panel can split NopBar's use into dock / top bar /
//! widgets.
//!
//! A level (light / normal / heavy) comes from those numbers against the
//! PC (physical memory, logical cores): the same use is lighter on a big
//! PC. Nothing here runs by itself: numbers are read when Settings or the
//! stats panel asks.

use std::sync::atomic::{AtomicU64, Ordering};

/// The widget kinds (Settings rows, in this order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Notes,
    Pieces,
}

impl Kind {
    pub const ALL: [Kind; 2] = [Kind::Notes, Kind::Pieces];

    /// Config / settings id.
    pub fn id(self) -> &'static str {
        match self {
            Kind::Notes => "notes",
            Kind::Pieces => "pieces",
        }
    }

    pub fn from_id(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.id() == s)
    }

    fn slot(self) -> usize {
        self as usize
    }
}

/// What a PC has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Spec {
    /// Physical memory, bytes.
    pub ram: u64,
    /// Logical processors.
    pub cores: u32,
}

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

impl Spec {
    /// A PC where a few MB and a little CPU matter: under 7 GiB of memory
    /// (a 4 or 6 GB PC; an 8 GB one reports a little under 8) or at most
    /// four logical processors.
    pub fn low_end(&self) -> bool {
        (self.ram > 0 && self.ram < 7 * GIB) || (self.cores > 0 && self.cores <= 4)
    }

    /// This PC.
    pub fn now() -> Spec {
        use windows::Win32::System::SystemInformation::{GetSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO};
        let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        let ram = if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() { m.ullTotalPhys } else { 0 };
        let mut si = SYSTEM_INFO::default();
        unsafe { GetSystemInfo(&mut si) };
        Spec { ram, cores: si.dwNumberOfProcessors }
    }
}

/// How heavy something is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Light,
    Normal,
    Heavy,
}

impl Level {
    pub fn word(self) -> &'static str {
        match self {
            Level::Light => "light",
            Level::Normal => "normal",
            Level::Heavy => "heavy",
        }
    }

    pub fn from_word(w: &str) -> Option<Level> {
        [Level::Light, Level::Normal, Level::Heavy].into_iter().find(|l| l.word() == w)
    }
}

/// Pure: the level of `mem` bytes held and `cpu` % of the whole machine
/// (averaged) on `spec`. Memory: light up to 1/1000 of the PC's memory
/// (16 MB on 16 GB, at least 8 MB), normal up to four times that. CPU:
/// light up to 0.2 %, normal up to 1 % (half that on a low-end PC). The
/// heavier of the two.
pub fn level(mem: u64, cpu: f32, spec: Spec) -> Level {
    let low = spec.low_end();
    let unit = (spec.ram / 1000).max(8 * MIB);
    let by_mem = if mem <= unit {
        Level::Light
    } else if mem <= unit * 4 {
        Level::Normal
    } else {
        Level::Heavy
    };
    let k = if low { 0.5 } else { 1.0 };
    let cpu = if cpu.is_finite() { cpu.max(0.0) } else { 0.0 };
    let by_cpu = if cpu <= 0.2 * k {
        Level::Light
    } else if cpu <= 1.0 * k {
        Level::Normal
    } else {
        Level::Heavy
    };
    by_mem.max(by_cpu)
}

/// Pure: CPU % of the whole machine for `busy` (100 ns) over `wall`
/// (100 ns) on `cores` logical processors.
pub fn cpu_pct(busy: u64, wall: u64, cores: u32) -> f32 {
    if wall == 0 || cores == 0 {
        return 0.0;
    }
    (busy as f64 / (wall as f64 * cores as f64) * 100.0) as f32
}

/// One widget kind's use.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Usage {
    /// Windows of it on the desktop.
    pub count: u32,
    /// Bytes held (surfaces, decoded pictures).
    pub mem: u64,
    /// CPU % of the whole machine since NopBar started.
    pub cpu: f32,
}

/// One row of the widgets card.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub kind: Kind,
    pub usage: Usage,
    pub level: Level,
}

/// What Settings shows: the summary line and a row per kind.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// Widget windows on the desktop, every kind.
    pub count: u32,
    pub level: Level,
    pub mem: u64,
    pub cpu: f32,
    pub spec: Spec,
    pub rows: Vec<Row>,
}

/// Pure: the card from each kind's use on `spec`.
pub fn status(uses: &[(Kind, Usage)], spec: Spec) -> Status {
    let rows: Vec<Row> = uses.iter().map(|(k, u)| Row { kind: *k, usage: *u, level: level(u.mem, u.cpu, spec) }).collect();
    let mem = uses.iter().map(|(_, u)| u.mem).sum();
    let cpu = uses.iter().map(|(_, u)| u.cpu).sum();
    let count = uses.iter().map(|(_, u)| u.count).sum();
    Status { count, level: level(mem, cpu, spec), mem, cpu, spec, rows }
}

impl Status {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "count": self.count,
            "level": self.level.word(),
            "mem": self.mem,
            "cpu": self.cpu,
            "ram": self.spec.ram,
            "cores": self.spec.cores,
            "rows": self.rows.iter().map(|r| serde_json::json!({
                "id": r.kind.id(), "count": r.usage.count, "mem": r.usage.mem, "cpu": r.usage.cpu, "level": r.level.word(),
            })).collect::<Vec<_>>(),
        })
    }

    pub fn from_json(v: &serde_json::Value) -> Option<Status> {
        let u = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_u64());
        let f = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_f64()).map(|x| x as f32);
        let rows = v
            .get("rows")?
            .as_array()?
            .iter()
            .filter_map(|r| {
                Some(Row {
                    kind: Kind::from_id(r.get("id")?.as_str()?)?,
                    usage: Usage { count: u(r, "count")? as u32, mem: u(r, "mem")?, cpu: f(r, "cpu")? },
                    level: Level::from_word(r.get("level")?.as_str()?)?,
                })
            })
            .collect();
        Some(Status {
            count: u(v, "count")? as u32,
            level: Level::from_word(v.get("level")?.as_str()?)?,
            mem: u(v, "mem")?,
            cpu: f(v, "cpu")?,
            spec: Spec { ram: u(v, "ram")?, cores: u(v, "cores")? as u32 },
            rows,
        })
    }
}

// ---------------------------------------------------------------- meters

/// CPU time (100 ns) a widget's helper threads took (reading the
/// history, decoding thumbnails, keeping pinned content, preparing a drag).
static EXTRA: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Counts the calling (helper) thread's CPU time until dropped to `kind`.
pub struct Charge(Kind, u64);

pub fn charge(kind: Kind) -> Charge {
    Charge(kind, current_thread_time())
}

impl Drop for Charge {
    fn drop(&mut self) {
        let d = current_thread_time().saturating_sub(self.1);
        EXTRA[self.0.slot()].fetch_add(d, Ordering::Relaxed);
    }
}

fn current_thread_time() -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    if unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) }.is_ok() {
        filetime(k) + filetime(u)
    } else {
        0
    }
}

/// Bytes held and windows shown, per kind (reported by the widgets).
static MEM: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static COUNT: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// What a widget kind holds now (bytes: its drawings and decoded pictures,
/// shown or hidden) and how many windows it shows.
pub fn report(kind: Kind, mem: u64, count: u32) {
    MEM[kind.slot()].store(mem, Ordering::Relaxed);
    COUNT[kind.slot()].store(count as u64, Ordering::Relaxed);
}

/// Pure: bytes of a `w`x`h` px BGRA surface.
pub fn surface_bytes(w: i32, h: i32) -> u64 {
    (w.max(0) as u64) * (h.max(0) as u64) * 4
}

/// Where CPU time on the dock's thread is counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Dock,
    TopBar,
    Widget(Kind),
}

impl Part {
    fn slot(self) -> usize {
        match self {
            Part::Dock => 0,
            Part::TopBar => 1,
            Part::Widget(k) => 2 + k.slot(),
        }
    }
}

const PARTS: usize = 4;
/// Thread cycles counted to each part (dock's thread only).
static CYCLES: [AtomicU64; PARTS] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
/// The dock's thread (0 until init).
static MAIN: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Spans open on this thread (innermost last) and the cycle count when
    /// the innermost one last took over.
    static STACK: std::cell::RefCell<(Vec<Part>, u64)> = const { std::cell::RefCell::new((Vec::new(), 0)) };
}

#[link(name = "kernel32")]
extern "system" {
    /// kernel32's QueryThreadCycleTime (the windows crate keeps it behind
    /// a feature this crate does not otherwise need).
    fn QueryThreadCycleTime(thread: windows::Win32::Foundation::HANDLE, cycles: *mut u64) -> i32;
}

fn cycles_now() -> u64 {
    let mut c = 0u64;
    unsafe { QueryThreadCycleTime(windows::Win32::System::Threading::GetCurrentThread(), &mut c) };
    c
}

/// Pure: exclusive attribution. With spans `stack` open, `now - since`
/// cycles go to the innermost one (nothing open: nothing counted).
fn attribute(stack: &[Part], since: u64, now: u64, add: &mut dyn FnMut(Part, u64)) {
    if let Some(p) = stack.last() {
        add(*p, now.saturating_sub(since));
    }
}

/// Counts the time until it is dropped to `part` (minus spans opened
/// inside it). Only on the dock's thread; elsewhere it does nothing.
pub struct Span(bool);

pub fn span(part: Part) -> Span {
    if MAIN.load(Ordering::Relaxed) != unsafe { windows::Win32::System::Threading::GetCurrentThreadId() } as u64 {
        return Span(false);
    }
    let now = cycles_now();
    STACK.with(|s| {
        let mut s = s.borrow_mut();
        let since = s.1;
        attribute(&s.0, since, now, &mut |p, c| {
            CYCLES[p.slot()].fetch_add(c, Ordering::Relaxed);
        });
        s.0.push(part);
        s.1 = now;
    });
    Span(true)
}

impl Drop for Span {
    fn drop(&mut self) {
        if !self.0 {
            return;
        }
        let now = cycles_now();
        STACK.with(|s| {
            let mut s = s.borrow_mut();
            let since = s.1;
            attribute(&s.0, since, now, &mut |p, c| {
                CYCLES[p.slot()].fetch_add(c, Ordering::Relaxed);
            });
            s.0.pop();
            s.1 = now;
        });
    }
}

/// The dock's thread: spans count from here on. Call once, on it.
pub fn init_main() {
    MAIN.store(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() } as u64, Ordering::SeqCst);
}

fn filetime(f: windows::Win32::Foundation::FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// CPU time (100 ns, kernel + user) of thread `tid` of this process.
fn thread_time(tid: u32) -> u64 {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{GetThreadTimes, OpenThread, THREAD_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, tid) else { return 0 };
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let ok = GetThreadTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok();
        let _ = CloseHandle(h);
        if ok {
            filetime(k) + filetime(u)
        } else {
            0
        }
    }
}

fn thread_cycles(tid: u32) -> u64 {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenThread, THREAD_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, tid) else { return 0 };
        let mut c = 0u64;
        QueryThreadCycleTime(h, &mut c);
        let _ = CloseHandle(h);
        c
    }
}

/// Pure: the dock thread's `time` (100 ns) shared out by cycles: what
/// `part` cycles of `total` cycles took.
pub fn share(time: u64, part: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    ((time as u128 * part.min(total) as u128) / total as u128) as u64
}

/// CPU time (100 ns) the dock's thread spent in `part` since NopBar started.
fn main_time_of(part: Part) -> u64 {
    let tid = MAIN.load(Ordering::Relaxed) as u32;
    if tid == 0 {
        return 0;
    }
    share(thread_time(tid), CYCLES[part.slot()].load(Ordering::Relaxed), thread_cycles(tid))
}

/// The notes' windows of this process: (count, bytes of their surfaces,
/// their thread). The notes live on their own thread (crate::notes).
fn notes_seen() -> (u32, u64, Option<u32>) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetClientRect, GetWindowThreadProcessId};
    let me = std::process::id();
    let (mut n, mut mem, mut tid) = (0u32, 0u64, None);
    let mut after = None;
    for _ in 0..512 {
        let Ok(w) = (unsafe { FindWindowExW(None, after, windows::core::w!("NopBar.Note"), windows::core::PCWSTR::null()) }) else { break };
        after = Some(w);
        let mut pid = 0u32;
        let t = unsafe { GetWindowThreadProcessId(w, Some(&mut pid)) };
        if pid != me {
            continue;
        }
        let mut r = RECT::default();
        let _ = unsafe { GetClientRect(w, &mut r) };
        n += 1;
        mem += surface_bytes(r.right - r.left, r.bottom - r.top);
        tid = Some(t);
    }
    (n, mem, tid)
}

fn process_start_and_now() -> (u64, u64) {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        (filetime(c), filetime(GetSystemTimeAsFileTime()))
    }
}

/// CPU time (100 ns) each widget kind took since NopBar started.
fn widget_times() -> [u64; 2] {
    let notes = notes_seen().2.map(thread_time).unwrap_or(0);
    let extra = |k: Kind| EXTRA[k.slot()].load(Ordering::Relaxed);
    [notes + extra(Kind::Notes), main_time_of(Part::Widget(Kind::Pieces)) + extra(Kind::Pieces)]
}

/// Settings' widgets card, measured now.
pub fn measure() -> Status {
    let spec = Spec::now();
    let (start, now) = process_start_and_now();
    let wall = now.saturating_sub(start);
    let times = widget_times();
    let (notes_n, notes_mem, _) = notes_seen();
    let uses: Vec<(Kind, Usage)> = Kind::ALL
        .iter()
        .map(|&k| {
            let (count, mem) = match k {
                Kind::Notes => (notes_n, notes_mem),
                _ => (COUNT[k.slot()].load(Ordering::Relaxed) as u32, MEM[k.slot()].load(Ordering::Relaxed)),
            };
            (k, Usage { count, mem, cpu: cpu_pct(times[k.slot()], wall, spec.cores) })
        })
        .collect();
    status(&uses, spec)
}

pub fn status_json() -> serde_json::Value {
    measure().to_json()
}

/// CPU time (100 ns) since NopBar started: (top bar, widgets). The dock's
/// share is the process's total less these (the stats panel splits its
/// NopBar row with two readings of this).
pub fn parts_time() -> (u64, u64) {
    (main_time_of(Part::TopBar), widget_times().iter().sum())
}

/// Pure: NopBar's CPU `total` % split by the CPU times of its parts since
/// the last reading (`d_total`: the process's, `d_bar`, `d_widgets`):
/// (dock, top bar, widgets) %, summing to `total`.
pub fn split(total: f32, d_total: u64, d_bar: u64, d_widgets: u64) -> (f32, f32, f32) {
    if d_total == 0 || !total.is_finite() {
        return (total.max(0.0), 0.0, 0.0);
    }
    let bar = d_bar.min(d_total);
    let widgets = d_widgets.min(d_total - bar);
    let f = |x: u64| total * x as f32 / d_total as f32;
    let (b, w) = (f(bar), f(widgets));
    ((total - b - w).max(0.0), b, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIG: Spec = Spec { ram: 32 * GIB, cores: 16 };
    const SMALL: Spec = Spec { ram: 4 * GIB, cores: 4 };

    #[test]
    fn low_end_pcs_are_told_by_memory_or_cores() {
        assert!(!BIG.low_end());
        assert!(SMALL.low_end());
        assert!(Spec { ram: 6 * GIB, cores: 8 }.low_end());
        assert!(!Spec { ram: 8 * GIB - 200 * MIB, cores: 8 }.low_end(), "an 8 GB PC reports a little less");
        assert!(Spec { ram: 16 * GIB, cores: 2 }.low_end());
        assert!(!Spec::default().low_end(), "nothing known: not assumed low-end");
    }

    #[test]
    fn levels_weigh_use_against_the_pc() {
        assert_eq!(level(0, 0.0, BIG), Level::Light);
        // 20 MB: light on 32 GB (32 MB unit), normal on 4 GB (8 MB unit).
        assert_eq!(level(20 * MIB, 0.0, BIG), Level::Light);
        assert_eq!(level(20 * MIB, 0.0, SMALL), Level::Normal);
        assert_eq!(level(40 * MIB, 0.0, SMALL), Level::Heavy);
        assert_eq!(level(200 * MIB, 0.0, BIG), Level::Heavy);
        // CPU: the heavier of the two counts; a low-end PC is stricter.
        assert_eq!(level(0, 0.15, BIG), Level::Light);
        assert_eq!(level(0, 0.15, SMALL), Level::Normal);
        assert_eq!(level(0, 0.8, BIG), Level::Normal);
        assert_eq!(level(0, 0.8, SMALL), Level::Heavy);
        assert_eq!(level(MIB, 3.0, BIG), Level::Heavy);
        assert_eq!(level(0, f32::NAN, BIG), Level::Light);
    }

    #[test]
    fn cpu_is_a_share_of_the_whole_machine() {
        // 1 s busy over 100 s on 4 cores: 0.25 %.
        assert!((cpu_pct(10_000_000, 1_000_000_000, 4) - 0.25).abs() < 1e-4);
        assert_eq!(cpu_pct(5, 0, 4), 0.0);
        assert_eq!(share(1000, 25, 100), 250);
        assert_eq!(share(1000, 200, 100), 1000, "never more than the whole");
        assert_eq!(share(1000, 5, 0), 0);
    }

    #[test]
    fn the_card_sums_every_kind() {
        let uses = [
            (Kind::Notes, Usage { count: 3, mem: MIB, cpu: 0.0 }),
            (Kind::Pieces, Usage { count: 2, mem: 60 * MIB, cpu: 0.0 }),
        ];
        let s = status(&uses, BIG);
        assert_eq!(s.count, 5);
        assert_eq!(s.mem, 61 * MIB);
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.rows[0].level, Level::Light);
        assert_eq!(s.rows[1].level, Level::Normal);
        assert_eq!(s.level, Level::Normal);
        // Through the pipe and back.
        let back = Status::from_json(&s.to_json()).unwrap();
        assert_eq!(back.count, s.count);
        assert_eq!(back.rows, s.rows);
        assert_eq!(back.spec, BIG);
        assert!(Status::from_json(&serde_json::json!({"count": 1})).is_none());
    }

    #[test]
    fn spans_count_exclusively_to_the_innermost() {
        let mut got: Vec<(Part, u64)> = Vec::new();
        attribute(&[Part::TopBar, Part::Widget(Kind::Pieces)], 100, 160, &mut |p, c| got.push((p, c)));
        attribute(&[], 0, 50, &mut |p, c| got.push((p, c)));
        attribute(&[Part::Dock], 200, 150, &mut |p, c| got.push((p, c)));
        assert_eq!(got, vec![(Part::Widget(Kind::Pieces), 60), (Part::Dock, 0)]);
    }

    #[test]
    fn helper_threads_count_to_their_widget() {
        let before = EXTRA[Kind::Pieces.slot()].load(Ordering::Relaxed);
        std::thread::spawn(|| {
            let _c = charge(Kind::Pieces);
            // Busy for a while (at least one tick of the thread clock).
            let t = std::time::Instant::now();
            let mut x = 0u64;
            while t.elapsed() < std::time::Duration::from_millis(60) {
                x = x.wrapping_add(std::hint::black_box(1));
            }
            std::hint::black_box(x);
        })
        .join()
        .unwrap();
        assert!(EXTRA[Kind::Pieces.slot()].load(Ordering::Relaxed) > before);
    }

    #[test]
    fn spans_off_the_dock_thread_count_nothing() {
        // This test thread is not the dock's.
        let before: u64 = CYCLES.iter().map(|c| c.load(Ordering::Relaxed)).sum();
        {
            let _s = span(Part::TopBar);
            let _ = (0..1000).sum::<u64>();
        }
        let after: u64 = CYCLES.iter().map(|c| c.load(Ordering::Relaxed)).sum();
        assert_eq!(before, after);
    }

    #[test]
    fn nopbar_cpu_splits_into_its_parts() {
        let (d, b, w) = split(1.0, 1000, 250, 250);
        assert!((d - 0.5).abs() < 1e-5 && (b - 0.25).abs() < 1e-5 && (w - 0.25).abs() < 1e-5);
        assert_eq!(split(0.4, 0, 0, 0), (0.4, 0.0, 0.0));
        // Parts never sum past the whole.
        let (d, b, w) = split(2.0, 100, 80, 80);
        assert!((d + b + w - 2.0).abs() < 1e-5 && d >= 0.0);
    }

    #[test]
    fn surfaces_count_their_bytes() {
        assert_eq!(surface_bytes(10, 20), 800);
        assert_eq!(surface_bytes(-5, 20), 0);
    }
}
