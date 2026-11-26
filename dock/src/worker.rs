//! Background threads. The UI thread never blocks on other processes or on
//! the shell: requests go to workers over channels, and every result comes
//! back on one event queue that is announced with a single posted message
//! ("post-once": at most one WM_EVENTS is in flight; the UI drains the whole
//! queue when it arrives).
//!
//! Worker threads exist only while there is work (crate::idle); long waits
//! (config.json, explorer, the settings pipe) are watches on the shared
//! service thread (crate::service).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, PostMessageW, WM_APP};

use crate::apps::{self, AppWindow};
use crate::icons::{self, Icon};
use crate::idle::{IdleQueue, Jobs};

/// Posted to the dock window when the event queue has something in it.
pub const WM_EVENTS: u32 = WM_APP + 4;

/// Per dock window: a WM_EVENTS is on its way (each dock window has its
/// own event queue; see start_view).
static EVENTS_POSTED: Mutex<Vec<(isize, std::sync::Arc<AtomicBool>)>> = Mutex::new(Vec::new());

fn posted_flag(hwnd: isize) -> std::sync::Arc<AtomicBool> {
    let mut v = EVENTS_POSTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, f)) = v.iter().find(|(h, _)| *h == hwnd) {
        return f.clone();
    }
    let f = std::sync::Arc::new(AtomicBool::new(false));
    v.push((hwnd, f.clone()));
    f
}

/// A dock window is gone: its flag goes too.
pub fn forget_events(hwnd: isize) {
    EVENTS_POSTED.lock().unwrap_or_else(|e| e.into_inner()).retain(|(h, _)| *h != hwnd);
}
/// explorer PID currently being waited on (0 = none).
static WATCHED_PID: AtomicU32 = AtomicU32::new(0);
/// Dock window + event sender, so explorer watching does not need the
/// (possibly borrowed) Dock and works even inside a modal menu loop.
static WATCH_CTX: OnceLock<(isize, Mutex<EventTx>)> = OnceLock::new();

/// `AppWindow` with the handle as an integer so it can cross threads.
pub struct WinRaw {
    pub hwnd: isize,
    pub exe: Option<String>,
    pub aumid: Option<String>,
    pub pid: u32,
    pub started: u64,
}

impl WinRaw {
    pub fn to_app(&self) -> AppWindow {
        AppWindow { hwnd: HWND(self.hwnd as *mut _), exe: self.exe.clone(), aumid: self.aumid.clone(), pid: self.pid, started: self.started }
    }
}

pub enum Event {
    /// Window list as of request `seq` (requests are coalesced, so the UI may
    /// skip numbers; it must ignore anything older than what it applied).
    Snapshot { seq: u64, wins: Vec<WinRaw> },
    /// Icon for `key` at `size` px, or None if extraction failed.
    /// `look`: the dock's look it was asked for (icons::reply_stale);
    /// `drawn`: one of the dock's own tiles (icons::Source::Drawn);
    /// `small`: pictures prepared for the smaller sides, using a drawn
    /// tile at that size or a size-specific frame of the bundled icon.
    Icon { key: String, size: i32, look: icons::Appearance, drawn: bool, icon: Option<Icon>, small: Vec<Icon> },
    /// The explorer process we were watching (this pid) exited.
    ExplorerGone(u32),
    /// Blurred wallpaper for the mica material (None if unavailable).
    Wallpaper { mon: (i32, i32, i32, i32), img: Option<Icon> },
    /// config.json was changed by someone else (already parsed off-thread).
    /// The u64 is config::write_seq() at the time the file was read: if the
    /// dock saved after that, the event is stale.
    ConfigChanged(Box<crate::config::Config>, u64),
    /// The dock's own edits up to request `gen` were saved (see
    /// Workers::save_config): `cfg` is what the file now holds and `seq`
    /// write_seq() right after that write.
    ConfigSaved { gen: u64, cfg: Box<crate::config::Config>, seq: u64 },
    /// A settings-pipe request that needs the dock's state; answer on `reply`.
    Ipc { req: String, reply: Sender<String>, at: std::time::Instant },
    /// A stack's folder listed, with icons.
    Stack(Box<crate::stacks::Listing>),
    /// Pinned paths checked for being folders: (path, is a folder).
    PinKinds(Vec<(String, bool)>),
    /// Files dropped on a stack were moved / copied / linked into its
    /// folder (`ok`: all of them, nothing cancelled or failed).
    StackDropped { folder: String, ok: bool },
}

/// Work for the shell-image thread (STA COM).
enum ImageJob {
    Icon { key: String, size: i32, sides: Vec<u32>, look: icons::Appearance },
}

pub struct Workers {
    /// Window-list scans, icon loads, mica snapshots and config saves each
    /// have a queue whose thread exists only while there is work
    /// (crate::idle).
    tracker: IdleQueue<u64>,
    icons: IdleQueue<ImageJob>,
    /// Mica jobs run apart from icons: a slow desktop snapshot must never
    /// hold up icon loading.
    mica: IdleQueue<(i32, i32, i32, i32)>,
    /// Config changes made from the dock's own UI, saved off the UI thread.
    saver: IdleQueue<ConfigSave>,
    /// Stack listings and folder checks run on their own short-lived
    /// threads (crate::stacks); results come back on the event queue.
    hwnd: isize,
    events_tx: EventTx,
    seq: u64,
}

/// Caps short-lived helper threads of one kind (audit A05). A helper stuck
/// in a call that never returns (Explorer COM, the WLAN service, a shell
/// extension) keeps its slot; new work of that kind is refused instead of
/// piling up more stuck threads.
pub struct HelperCap {
    live: std::sync::atomic::AtomicU32,
    max: u32,
}

/// One running helper; dropping it (the thread ending) frees the slot.
pub struct HelperSlot(&'static HelperCap);

impl HelperCap {
    pub const fn new(max: u32) -> Self {
        Self { live: std::sync::atomic::AtomicU32::new(0), max }
    }
    /// A slot for a new helper, or None while `max` are still running.
    pub fn try_take(&'static self) -> Option<HelperSlot> {
        let mut n = self.live.load(Ordering::SeqCst);
        while n < self.max {
            match self.live.compare_exchange(n, n + 1, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return Some(HelperSlot(self)),
                Err(now) => n = now,
            }
        }
        None
    }
}

impl Drop for HelperSlot {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Tries to post WM_EVENTS this many times (a full message queue or a busy
/// moment) before leaving it to the next event (audit A15).
const NOTIFY_TRIES: u64 = 3;

fn notify(hwnd: isize) {
    let posted = posted_flag(hwnd);
    if posted.swap(true, Ordering::SeqCst) {
        return;
    }
    for i in 0..NOTIFY_TRIES {
        if unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_EVENTS, WPARAM(0), LPARAM(0)) }.is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50 * (i + 1)));
    }
    // Never left "posted" without a message on its way: the next event
    // posts again, and if none comes a retry thread keeps trying, so queued
    // events are never stranded.
    posted.store(false, Ordering::SeqCst);
    crate::log!("worker: could not tell the dock about new events; retrying");
    retry_notify(hwnd);
}

/// A retry thread is running (at most one).
static RETRYING: AtomicBool = AtomicBool::new(false);
/// Waits between retries; the last one repeats until a post goes through.
const RETRY_MS: [u64; 5] = [250, 1000, 2000, 5000, 15000];

fn retry_notify(hwnd: isize) {
    if RETRYING.swap(true, Ordering::SeqCst) {
        return;
    }
    let posted = posted_flag(hwnd);
    let spawned = std::thread::Builder::new().name("events-retry".into()).spawn(move || {
        for i in 0.. {
            std::thread::sleep(Duration::from_millis(RETRY_MS[i.min(RETRY_MS.len() - 1)]));
            if posted.swap(true, Ordering::SeqCst) {
                break; // a newer event got its message through
            }
            if unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_EVENTS, WPARAM(0), LPARAM(0)) }.is_ok() {
                break;
            }
            posted.store(false, Ordering::SeqCst);
            if unsafe { !IsWindow(Some(HWND(hwnd as *mut _))).as_bool() } {
                break; // the dock window is gone: nobody to tell
            }
        }
        RETRYING.store(false, Ordering::SeqCst);
    });
    if spawned.is_err() {
        RETRYING.store(false, Ordering::SeqCst);
    }
}

/// Called by the UI before draining dock window `hwnd`'s queue: anything
/// sent after this posts again.
pub fn events_taken(hwnd: isize) {
    posted_flag(hwnd).store(false, Ordering::SeqCst);
}

/// Most events the queue holds (audit A15). Coalescing keeps it far below
/// this in practice (one window list, one config, one wallpaper per
/// monitor, one icon per key and size). Past it only results that can be
/// asked for again go, oldest first (icons, which the UI re-requests, and
/// stack listings, which time out); window lists, config and recovery
/// events and settings requests are never dropped (they are few: each kind
/// coalesces to one, or one per client).
const MAX_QUEUED: usize = 1024;

/// May this event go when the queue is full?
fn droppable(ev: &Event) -> bool {
    matches!(ev, Event::Icon { .. } | Event::Stack(_))
}

struct Queue {
    items: std::collections::VecDeque<Event>,
    /// Icons dropped from a full queue: the UI asks for them again.
    lost_icons: Vec<(String, i32)>,
    /// The receiver is gone: sends fail, so producer threads end.
    closed: bool,
    /// Already logged that events were dropped (until the queue drains).
    overflowed: bool,
}

/// The sending end of the worker event queue (cloned per producer).
#[derive(Clone)]
pub struct EventTx(std::sync::Arc<Mutex<Queue>>);

/// The UI's end: everything queued so far, in order.
pub struct EventRx(std::sync::Arc<Mutex<Queue>>);

fn event_queue() -> (EventTx, EventRx) {
    let q = std::sync::Arc::new(Mutex::new(Queue { items: Default::default(), lost_icons: Vec::new(), closed: false, overflowed: false }));
    (EventTx(q.clone()), EventRx(q))
}

/// Folds `ev` into an equivalent queued event if there is one: only the
/// newest window list / config / stack listing matters, and a newer result
/// for the same icon or monitor replaces the older one. Gives the event
/// back when it has to be queued on its own.
fn coalesce(items: &mut std::collections::VecDeque<Event>, ev: Event) -> Option<Event> {
    for old in items.iter_mut() {
        match (&mut *old, &ev) {
            (Event::Snapshot { seq: a, .. }, Event::Snapshot { seq: b, .. })
            | (Event::ConfigChanged(_, a), Event::ConfigChanged(_, b)) => {
                if *b >= *a {
                    *old = ev;
                }
                return None;
            }
            (Event::ConfigSaved { gen: a, .. }, Event::ConfigSaved { gen: b, .. }) => {
                if *b >= *a {
                    *old = ev;
                }
                return None;
            }
            (Event::Stack(a), Event::Stack(b)) => {
                if b.seq >= a.seq {
                    *old = ev;
                }
                return None;
            }
            (Event::Icon { key: a, size: sa, .. }, Event::Icon { key: b, size: sb, .. }) if a == b && sa == sb => {
                *old = ev;
                return None;
            }
            (Event::Wallpaper { mon: a, .. }, Event::Wallpaper { mon: b, .. }) if a == b => {
                *old = ev;
                return None;
            }
            (Event::ExplorerGone(a), Event::ExplorerGone(b)) if a == b => return None,
            (Event::PinKinds(a), Event::PinKinds(_)) => {
                let Event::PinKinds(b) = ev else { unreachable!() };
                for (path, folder) in b {
                    match a.iter_mut().find(|(p, _)| p.eq_ignore_ascii_case(&path)) {
                        Some(e) => e.1 = folder,
                        None => a.push((path, folder)),
                    }
                }
                return None;
            }
            _ => {}
        }
    }
    Some(ev)
}

/// Queues `ev` (coalesced, capped). False when nobody drains any more.
fn push(q: &mut Queue, ev: Event) -> bool {
    if q.closed {
        return false;
    }
    if let Some(ev) = coalesce(&mut q.items, ev) {
        q.items.push_back(ev);
    }
    if q.items.len() > MAX_QUEUED {
        if let Some(i) = q.items.iter().position(droppable) {
            if let Some(Event::Icon { key, size, .. }) = q.items.remove(i) {
                if !q.lost_icons.iter().any(|(k, s)| *k == key && *s == size) {
                    q.lost_icons.push((key, size));
                }
            }
            if !std::mem::replace(&mut q.overflowed, true) {
                crate::log!("worker: event queue full; dropping icon/stack results to be asked for again");
            }
        }
    }
    true
}

impl EventTx {
    /// Like mpsc's send: Err when the receiver is gone.
    pub fn send(&self, ev: Event) -> Result<(), ()> {
        let Ok(mut q) = self.0.lock() else { return Err(()) };
        if push(&mut q, ev) {
            Ok(())
        } else {
            Err(())
        }
    }
}

impl EventRx {
    /// Takes everything queued so far.
    pub fn try_iter(&self) -> std::collections::vec_deque::IntoIter<Event> {
        let Ok(mut q) = self.0.lock() else { return std::collections::VecDeque::new().into_iter() };
        q.overflowed = false;
        std::mem::take(&mut q.items).into_iter()
    }

    /// Icons whose results were dropped from a full queue (to ask again).
    pub fn take_lost_icons(&self) -> Vec<(String, i32)> {
        self.0.lock().map(|mut q| std::mem::take(&mut q.lost_icons)).unwrap_or_default()
    }
}

impl Drop for EventRx {
    fn drop(&mut self) {
        if let Ok(mut q) = self.0.lock() {
            q.closed = true;
            q.items.clear();
        }
    }
}

/// How long an idle worker thread waits for more before it ends.
const WORKER_IDLE: Duration = Duration::from_secs(30);

pub fn start(hwnd: isize) -> (Workers, EventRx) {
    let (events_tx, events_rx) = event_queue();
    let tx = events_tx.clone();
    let tracker = IdleQueue::new("tracker", WORKER_IDLE, 0, move |jobs| tracker(hwnd, jobs, &tx));
    let tx = events_tx.clone();
    let icons = IdleQueue::new("icons", WORKER_IDLE, 0, move |jobs| icon_worker(hwnd, jobs, &tx));
    let tx = events_tx.clone();
    let mica = IdleQueue::new("mica", WORKER_IDLE, 0, move |jobs| mica_worker(hwnd, jobs, &tx));
    let tx = events_tx.clone();
    let saver = IdleQueue::new("config-save", WORKER_IDLE, 0, move |jobs| config_saver(hwnd, jobs, &tx));
    // Long waits share the service thread.
    start_config_watch(hwnd, events_tx.clone());
    ipc::listen(hwnd, events_tx.clone());
    // The AI agents' hooks have a pipe of their own (crate::agents).
    crate::agents::listen();
    let stack_events = events_tx.clone();
    let _ = WATCH_CTX.set((hwnd, Mutex::new(events_tx)));
    (Workers { tracker, icons, mica, saver, hwnd, events_tx: stack_events, seq: 0 }, events_rx)
}

/// Workers for a dock on another display (crate::dock views): icons,
/// wallpaper and stacks for its own window. The window list, config
/// watching and saving and the settings pipe stay with the primary dock
/// (those queues exist but are never used, so they never start a thread).
pub fn start_view(hwnd: isize) -> (Workers, EventRx) {
    let (events_tx, events_rx) = event_queue();
    let tx = events_tx.clone();
    let tracker = IdleQueue::new("tracker", WORKER_IDLE, 0, move |jobs| tracker(hwnd, jobs, &tx));
    let tx = events_tx.clone();
    let icons = IdleQueue::new("icons", WORKER_IDLE, 0, move |jobs| icon_worker(hwnd, jobs, &tx));
    let tx = events_tx.clone();
    let mica = IdleQueue::new("mica", WORKER_IDLE, 0, move |jobs| mica_worker(hwnd, jobs, &tx));
    let tx = events_tx.clone();
    let saver = IdleQueue::new("config-save", WORKER_IDLE, 0, move |jobs| config_saver(hwnd, jobs, &tx));
    (Workers { tracker, icons, mica, saver, hwnd, events_tx, seq: 0 }, events_rx)
}

impl Workers {
    pub fn request_refresh(&mut self) {
        self.seq += 1;
        let _ = self.tracker.send(self.seq);
    }

    /// `key` is the launch path (.lnk / .exe / shell:AppsFolder\...).
    /// `look`: the dock's theme (packaged apps' logos come in light and
    /// dark forms: icons::pick_logo; drawn tiles take its colors).
    /// `sides`: the pictures the dock makes of it (device px; a drawn tile
    /// is drawn at the smaller ones it looks different at).
    pub fn request_icon(&self, key: &str, size: i32, sides: Vec<u32>, look: icons::Appearance) {
        let _ = self.icons.send(ImageJob::Icon { key: key.to_string(), size, sides, look });
    }

    /// List a stack's folder (Event::Stack).
    /// A newer request makes an older one stop; a stuck one is abandoned.
    pub fn request_stack(&self, seq: u64, folder: &str, grid_px: u32, list_px: u32) {
        let (hwnd, tx) = (self.hwnd, self.events_tx.clone());
        crate::stacks::spawn_listing(seq, folder.to_string(), grid_px, list_px, move |l| {
            if tx.send(Event::Stack(Box::new(l))).is_ok() {
                notify(hwnd);
            }
        });
    }

    /// Which pinned paths are folders (Event::PinKinds).
    pub fn check_pin_kinds(&self, paths: Vec<String>) {
        let (hwnd, tx) = (self.hwnd, self.events_tx.clone());
        let ok = crate::stacks::check_kinds(paths, move |r| {
            if tx.send(Event::PinKinds(r)).is_ok() {
                notify(hwnd);
            }
        });
        if !ok {
            crate::log!("stacks: pinned folders not checked (helpers busy)");
        }
    }

    /// Files dropped on a stack: move / copy / link them into its folder
    /// with Windows' copy engine on a thread of its own (Event::StackDropped).
    pub fn drop_into_stack(&self, files: Vec<String>, folder: String, effect: u32) {
        let (hwnd, tx) = (self.hwnd, self.events_tx.clone());
        let f2 = folder.clone();
        crate::stacks::spawn_transfer(files, folder, effect, move |ok| {
            if tx.send(Event::StackDropped { folder: f2, ok }).is_ok() {
                notify(hwnd);
            }
        });
    }

    /// Saves a change made from the dock's own UI (audit A13: no config
    /// file I/O on the UI thread). The UI has already shown `job.shown`.
    pub fn save_config(&self, job: ConfigSave) {
        SAVES_PENDING.fetch_add(1, Ordering::SeqCst);
        if let Err(job) = self.saver.send(job) {
            // No saver thread: do it here rather than lose the change.
            SAVES_PENDING.fetch_sub(1, Ordering::SeqCst);
            let (cfg, seq) = run_save(&job);
            if self.events_tx.send(Event::ConfigSaved { gen: job.gen, cfg: Box::new(cfg), seq }).is_ok() {
                notify(self.hwnd);
            }
        }
    }

    /// Mica material: ask for the blurred wallpaper (Event::Wallpaper).
    pub fn request_wallpaper(&self, mon: (i32, i32, i32, i32)) {
        let _ = self.mica.send(mon);
    }
}

/// Waits (on the service thread, zero idle cost) for explorer `pid` to exit
/// and then reports ExplorerGone. One watcher per pid. If the process cannot
/// be opened it retries briefly; if it turns out to be gone already it still
/// reports ExplorerGone, so the UI goes looking for the new explorer.
pub fn watch_explorer(pid: u32) {
    let Some((hwnd, tx)) = WATCH_CTX.get() else { return };
    if pid == 0 || WATCHED_PID.swap(pid, Ordering::SeqCst) == pid {
        return;
    }
    let tx = tx.lock().map(|t| t.clone());
    let Ok(tx) = tx else {
        let _ = WATCHED_PID.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
        return;
    };
    let w = ExplorerWatch { pid, hwnd: *hwnd, tx, process: 0, tries: 0 };
    if crate::service::add(Box::new(w)).is_none() {
        let _ = WATCHED_PID.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

struct ExplorerWatch {
    pid: u32,
    hwnd: isize,
    tx: EventTx,
    /// The opened process (0 = not yet).
    process: isize,
    tries: u32,
}

impl ExplorerWatch {
    /// Only report if nobody started watching a newer explorer meanwhile.
    fn gone(&self) {
        if WATCHED_PID.load(Ordering::SeqCst) == self.pid {
            // Straight from this thread (the UI may be busy): the tray spy
            // must be gone before the next explorer starts.
            crate::tray::explorer_gone();
        }
        if WATCHED_PID.compare_exchange(self.pid, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok()
            && self.tx.send(Event::ExplorerGone(self.pid)).is_ok()
        {
            notify(self.hwnd);
        }
    }

    fn open(&mut self) -> crate::service::Next {
        use crate::service::Next;
        if let Ok(h) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, self.pid) } {
            self.process = h.0 as isize;
            return Next::Wait;
        }
        if crate::taskbar::explorer_pid() != self.pid {
            // Already gone.
            self.gone();
            return Next::Done;
        }
        self.tries += 1;
        if self.tries < 5 {
            return Next::Sleep(Duration::from_millis(200));
        }
        // Alive but not openable: stop claiming to watch it; the
        // TaskbarCreated broadcast still covers a restart.
        let _ = WATCHED_PID.compare_exchange(self.pid, 0, Ordering::SeqCst, Ordering::SeqCst);
        Next::Done
    }
}

impl crate::service::Watch for ExplorerWatch {
    fn name(&self) -> &'static str {
        "explorer watch"
    }
    fn handles(&self) -> Vec<HANDLE> {
        if self.process == 0 {
            Vec::new()
        } else {
            vec![HANDLE(self.process as *mut _)]
        }
    }
    fn wake(&mut self, why: crate::service::Wake) -> crate::service::Next {
        use crate::service::{Next, Wake};
        match why {
            Wake::Signaled(_) => {
                self.gone();
                Next::Done
            }
            _ if self.process == 0 => self.open(),
            // Pokes / timers while waiting: keep waiting.
            _ => Next::Wait,
        }
    }
}

impl Drop for ExplorerWatch {
    fn drop(&mut self) {
        if self.process != 0 {
            unsafe {
                let _ = CloseHandle(HANDLE(self.process as *mut _));
            }
        }
    }
}

fn tracker(hwnd: isize, jobs: &mut Jobs<u64>, tx: &EventTx) {
    // Uninitialize only what we initialized (the thread ends when idle).
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    while let Some(mut seq) = jobs.next() {
        // Coalesce a burst of requests into one scan.
        while let Some(s) = jobs.try_next() {
            seq = seq.max(s);
        }
        let wins = apps::enumerate(HWND(hwnd as *mut _));
        apps::warm_names(&wins);
        let wins = wins
            .into_iter()
            .map(|w| WinRaw { hwnd: w.hwnd.0 as isize, exe: w.exe, aumid: w.aumid, pid: w.pid, started: w.started })
            .collect();
        if tx.send(Event::Snapshot { seq, wins }).is_err() {
            break;
        }
        notify(hwnd);
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

/// The icon cache was swept this run (once, after the startup icons).
static SWEPT: AtomicBool = AtomicBool::new(false);

fn icon_worker(hwnd: isize, jobs: &mut Jobs<ImageJob>, tx: &EventTx) {
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    while let Some(job) = jobs.next() {
        let ev = match job {
            ImageJob::Icon { key, size, sides, look } => {
                // Shell objects change their icon with their state, and the
                // dock's drawn tiles (Launchpad pin, trash, folder stacks)
                // with light / dark and high contrast: always load fresh
                // (drawing is quicker than reading anyway).
                // NopBar's bundled frames also bypass the disk cache.
                // `size` is the largest the dock draws it; the source is the
                // largest real frame there is, whatever the size (the dock
                // draws it to each size it needs with a high-quality filter).
                let bundled = icons::is_own_app_icon(&key);
                let own = !bundled && icons::drawn_key(&key);
                let cache = if key.starts_with("::") || own || bundled {
                    None
                } else {
                    let tag = icons::source_tag(&key, size, look.light);
                    icons::cache_path(&key, &tag)
                };
                let fresh = cache.as_deref().filter(|p| icons::cache_fresh(&key, p));
                let mut drawn = false;
                let mut small = Vec::new();
                let icon = match fresh.and_then(icons::read_cache) {
                    Some(i) => {
                        icons::log_source(&key, &i, "cached");
                        Some(i)
                    }
                    None if bundled => icons::own_app_pictures(size.max(1) as u32, &sides).map(|(i, s)| {
                        icons::log_source(&key, &i, "Bundled");
                        small = s;
                        i
                    }),
                    // The dock's own tiles: also drawn at the smaller
                    // sides they look different at.
                    None => match own.then(|| icons::drawn_pictures(&key, size, &sides, look)).flatten() {
                        Some((i, s)) => {
                            icons::log_source(&key, &i, &format!("{:?}", icons::Source::Drawn));
                            drawn = true;
                            small = s;
                            Some(i)
                        }
                        None => icons::load_source(&key, size, look).map(|(i, src)| {
                            icons::log_source(&key, &i, &format!("{src:?}"));
                            drawn = src == icons::Source::Drawn;
                            if let Some(p) = cache.as_deref().filter(|_| !drawn) {
                                icons::write_cache(p, &i);
                            }
                            i
                        }),
                    },
                };
                Event::Icon { key, size, look, drawn, icon, small }
            }
        };
        if tx.send(ev).is_err() {
            break;
        }
        notify(hwnd);
        // Once per run, when the startup icons are out (audit A16).
        if !SWEPT.swap(true, Ordering::SeqCst) {
            icons::sweep_cache();
        }
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

/// A change-notification handle, closed with its own API on every path
/// (audit A17: CloseHandle is not the right call for it).
struct ChangeWatch(windows::Win32::Foundation::HANDLE);

impl Drop for ChangeWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Storage::FileSystem::FindCloseChangeNotification(self.0);
        }
    }
}

/// Waits before re-registering a failed watch: 1 s, 2 s, ... up to 5 min.
fn watch_backoff(failures: u32) -> Duration {
    Duration::from_secs((1u64 << failures.min(9)).min(300))
}

/// One edit made from the dock's own UI, kept by the UI until its save is
/// reported, and re-applied over any config read from disk meanwhile.
pub type ConfigEdit = std::sync::Arc<dyn Fn(&mut crate::config::Config) + Send + Sync>;

/// A change from the dock's own UI: applied to the newest config on disk
/// (so an outside edit that has not reached the dock is kept), then saved,
/// all under config's write lock (one read-modify-write at a time, across
/// threads and processes).
pub struct ConfigSave {
    /// The UI's request number (not the disk write order).
    pub gen: u64,
    pub edit: ConfigEdit,
    /// Used when the file is missing, unreadable or read-only.
    pub base: crate::config::Config,
}

/// Saves queued or being written.
static SAVES_PENDING: AtomicU32 = AtomicU32::new(0);

/// Waits (at most `max`) for queued config saves to land: before exiting or
/// at the end of the session, a change made a moment ago is not lost.
pub fn wait_for_saves(max: Duration) {
    let until = std::time::Instant::now() + max;
    while SAVES_PENDING.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Merges and writes one change: the saved config and write_seq() after it.
fn run_save(job: &ConfigSave) -> (crate::config::Config, u64) {
    let (latest, saved, seq) = crate::config::update(&job.base, |c| (job.edit)(c));
    match saved {
        // An open settings window shows pins the dock changed (dragged on,
        // off or around) without being reopened.
        Ok(()) => crate::settings::notify_config_saved(),
        Err(e) => crate::log!("config: dock change not saved ({e})"),
    }
    (latest, seq)
}

fn config_saver(hwnd: isize, jobs: &mut Jobs<ConfigSave>, tx: &EventTx) {
    while let Some(job) = jobs.next() {
        let (cfg, seq) = run_save(&job);
        // Always reported: the UI drops its copy of every edit up to `gen`.
        let sent = tx.send(Event::ConfigSaved { gen: job.gen, cfg: Box::new(cfg), seq });
        SAVES_PENDING.fetch_sub(1, Ordering::SeqCst);
        if sent.is_err() {
            break;
        }
        notify(hwnd);
    }
}

/// Watches the app folder from the service thread, which only waits on the
/// change notification and keeps time. Everything that touches the disk
/// (making the folder, registering the notification, reading and parsing
/// config.json) runs on the `config-read` worker, which exists only while
/// there is something to do, so a slow disk (a redirected network AppData)
/// cannot hold up the other watches. When the text changed and it is not
/// our own save, the parsed config goes to the UI. A notification that
/// fails (folder deleted and recreated, network hiccup) is registered again
/// after a growing pause instead of ending live updates.
struct ConfigWatch {
    /// The change notification (0 = none yet / failed); set by the reader.
    h: std::sync::Arc<std::sync::atomic::AtomicIsize>,
    /// Registrations that failed in a row (for the backoff).
    failures: u32,
    /// A registration was asked for and has not answered yet.
    arming: bool,
    reader: IdleQueue<ConfigJob>,
}

enum ConfigJob {
    /// Register the change notification, then read (the file may have
    /// changed while there was none).
    Arm,
    Read,
}

/// How long the config reader waits for more before it ends.
const CONFIG_READER_IDLE: Duration = Duration::from_secs(5);
/// The config watch on the service thread (for the reader's pokes).
static CONFIG_WATCH: Mutex<Option<crate::service::Id>> = Mutex::new(None);

fn poke_config_watch() {
    if let Some(id) = *CONFIG_WATCH.lock().unwrap_or_else(|e| e.into_inner()) {
        crate::service::poke(id);
    }
}

fn start_config_watch(hwnd: isize, tx: EventTx) {
    let h = std::sync::Arc::new(std::sync::atomic::AtomicIsize::new(0));
    let slot = h.clone();
    // Baseline = what the dock actually loaded, not what is on disk now.
    let last = Mutex::new(crate::config::loaded_hash());
    let reader = IdleQueue::new("config-read", CONFIG_READER_IDLE, 0, move |jobs: &mut Jobs<ConfigJob>| {
        while let Some(job) = jobs.next() {
            let mut arm = matches!(job, ConfigJob::Arm);
            // A burst of requests is one read (and at most one registration).
            while let Some(j) = jobs.try_next() {
                arm |= matches!(j, ConfigJob::Arm);
            }
            if arm {
                arm_config_folder(&slot);
                poke_config_watch();
            }
            let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
            read_config(hwnd, &tx, &mut last);
        }
    });
    // Held while adding: a reader that answers at once waits for the id.
    let mut g = CONFIG_WATCH.lock().unwrap_or_else(|e| e.into_inner());
    *g = crate::service::add(Box::new(ConfigWatch { h, failures: 0, arming: false, reader }));
}

/// Registers the change notification on the app folder (on the reader).
fn arm_config_folder(slot: &std::sync::atomic::AtomicIsize) {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{
        FindFirstChangeNotificationW, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    };
    if slot.load(Ordering::SeqCst) != 0 {
        return;
    }
    let dir = crate::config::app_dir();
    let _ = std::fs::create_dir_all(&dir);
    let opened = unsafe {
        FindFirstChangeNotificationW(&HSTRING::from(dir.as_os_str()), false, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME)
    };
    match opened {
        Ok(h) => slot.store(h.0 as isize, Ordering::SeqCst),
        Err(_) => crate::log!("config-watch: cannot watch {}; retrying", dir.display()),
    }
}

/// Reads config.json and reports a change (on the reader).
fn read_config(hwnd: isize, tx: &EventTx, last: &mut u64) {
    let seq = crate::config::write_seq();
    let Some(text) = crate::config::read_text() else { return };
    let hash = crate::config::text_hash(&text);
    if hash == *last {
        return;
    }
    *last = hash;
    if crate::config::is_own_write(&text) {
        return;
    }
    match crate::config::from_json(&text) {
        Ok(cfg) => {
            if tx.send(Event::ConfigChanged(Box::new(cfg), seq)).is_ok() {
                notify(hwnd);
            }
        }
        Err(e) => crate::log!("config-watch: ignoring unreadable edit ({e})"),
    }
}

impl ConfigWatch {
    fn arm(&mut self) -> crate::service::Next {
        self.arming = true;
        let _ = self.reader.send(ConfigJob::Arm);
        crate::service::Next::Wait
    }

    /// Drops a failed notification and waits a growing pause.
    fn failed(&mut self) -> crate::service::Next {
        let h = self.h.swap(0, Ordering::SeqCst);
        if h != 0 {
            drop(ChangeWatch(windows::Win32::Foundation::HANDLE(h as *mut _)));
            crate::log!("config-watch: watch on {} failed; registering it again", crate::config::app_dir().display());
        }
        let wait = watch_backoff(self.failures);
        self.failures += 1;
        crate::service::Next::Sleep(wait)
    }
}

impl crate::service::Watch for ConfigWatch {
    fn name(&self) -> &'static str {
        "config watch"
    }
    fn handles(&self) -> Vec<windows::Win32::Foundation::HANDLE> {
        match self.h.load(Ordering::SeqCst) {
            0 => Vec::new(),
            h => vec![windows::Win32::Foundation::HANDLE(h as *mut _)],
        }
    }
    fn wake(&mut self, why: crate::service::Wake) -> crate::service::Next {
        use crate::service::{Next, Wake};
        use windows::Win32::Storage::FileSystem::FindNextChangeNotification;
        let h = self.h.load(Ordering::SeqCst);
        match why {
            Wake::Start => self.arm(),
            // The reader answered a registration.
            Wake::Poked if self.arming => {
                self.arming = false;
                if h == 0 {
                    self.failed()
                } else {
                    self.failures = 0;
                    Next::Wait
                }
            }
            Wake::Poked => Next::Wait,
            // Editors write in bursts (truncate, write, rename): settle first.
            Wake::Signaled(_) => Next::Sleep(Duration::from_millis(200)),
            Wake::Timer if h == 0 => self.arm(),
            Wake::Timer => {
                if unsafe { FindNextChangeNotification(windows::Win32::Foundation::HANDLE(h as *mut _)) }.is_err() {
                    return self.failed();
                }
                let _ = self.reader.send(ConfigJob::Read);
                Next::Wait
            }
        }
    }
}

impl Drop for ConfigWatch {
    fn drop(&mut self) {
        let h = self.h.swap(0, Ordering::SeqCst);
        if h != 0 {
            drop(ChangeWatch(windows::Win32::Foundation::HANDLE(h as *mut _)));
        }
    }
}

/// Named-pipe server for the settings app: `\\.\pipe\NopBar.<session>`,
/// one JSON object per line each way, up to MAX_CLIENTS clients at once.
pub mod ipc {
    use super::*;
    use windows::core::HSTRING;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{
        GetLastError, LocalFree, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, INFINITE};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile, PIPE_ACCESS_DUPLEX};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;

    pub const PROTO: u32 = 1;
    /// Connections served at once; a newcomer beyond this ends the
    /// longest-idle one (audit A07).
    const MAX_CLIENTS: u32 = 4;

    /// Security descriptor that admits only SYSTEM and the current user
    /// (the default pipe DACL also lets other local users read).
    pub(crate) unsafe fn owner_only_sd() -> Option<PSECURITY_DESCRIPTOR> {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut _), len, &mut len);
        let _ = CloseHandle(token);
        got.ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid).ok()?;
        let sid_text = sid.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(sid.0 as *mut _)));
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{})", sid_text?);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None)
            .ok()?;
        // Kept for the life of the process: every pipe instance uses it.
        Some(sd)
    }

    pub fn pipe_name() -> String {
        let mut session = 0u32;
        unsafe {
            let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
        }
        format!(r"\\.\pipe\NopBar.{session}")
    }

    /// Answers one request line. Requests that need the dock's state are
    /// forwarded to the UI thread and waited on for at most 1 s.
    pub fn handle_line(line: &str, ask_ui: &dyn Fn(&str) -> Option<String>) -> String {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return serde_json::json!({"type": "Error", "msg": format!("bad json: {e}")}).to_string(),
        };
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "Hello" => serde_json::json!({"type": "Hello", "app": "NopBar", "proto": PROTO}).to_string(),
            "Ping" => serde_json::json!({"type": "Pong"}).to_string(),
            "WidgetsAdd" => ask_ui("WidgetsAdd")
                .unwrap_or_else(|| serde_json::json!({"type": "Error", "msg": "dock busy"}).to_string()),
            "GetState" => ask_ui("GetState")
                .unwrap_or_else(|| serde_json::json!({"type": "Error", "msg": "dock busy"}).to_string()),
            // The settings app: try a config live, keep it, or go back.
            "Preview" | "Commit" | "Revert" | "Quit" | "SuspendKeys" | "NewNote" => ask_ui(line)
                .unwrap_or_else(|| serde_json::json!({"type": "Error", "msg": "dock busy"}).to_string()),
            // Settings > Dock > Identify: each display's number on it for a
            // moment (its own thread; nothing waits for it).
            "Identify" => {
                crate::displays::identify();
                serde_json::json!({"type": "Ok"}).to_string()
            }
            // Settings' "Restart to update", the toast's Install now,
            // --update-now: the update planner does it (crate::autoupdate).
            "InstallUpdate" => {
                if crate::autoupdate::eligible().is_none() {
                    return serde_json::json!({"type": "Error", "msg": "this copy does not update itself"}).to_string();
                }
                crate::autoupdate::request(v.get("check").and_then(|c| c.as_bool()).unwrap_or(false));
                serde_json::json!({"type": "Ok"}).to_string()
            }
            other => serde_json::json!({"type": "Error", "msg": format!("unsupported in this build: {other:?}")})
                .to_string(),
        }
    }

    /// A partial request line (bytes but no newline yet) must be finished
    /// within this long (audit A07: a client that sends half a line and
    /// stops no longer holds its slot).
    const LINE_DEADLINE: Duration = Duration::from_secs(10);
    /// An answer must be taken by the client within this long.
    const WRITE_DEADLINE: Duration = Duration::from_secs(5);
    /// Longest request line accepted.
    const MAX_LINE: usize = 1 << 20;

    /// One connected client: when it last did something, and the event that
    /// ends its connection early (set to make room for a new client).
    struct Slot {
        id: u32,
        last: std::time::Instant,
        cancel: isize,
    }
    static SLOTS: Mutex<Vec<Slot>> = Mutex::new(Vec::new());

    /// Which client gives up its slot when all are taken and a new one
    /// arrives: the one that has been quiet the longest (a settings window
    /// left open for hours keeps working until someone actually needs the
    /// room; a client stuck since connecting goes first).
    fn idlest(slots: &[(u32, std::time::Instant)]) -> Option<u32> {
        slots.iter().min_by_key(|(_, last)| *last).map(|(id, _)| *id)
    }

    /// How long the next read may wait: forever between requests (only
    /// eviction ends an idle client); within a request, what is left of
    /// LINE_DEADLINE counted from its first byte, however the bytes trickle
    /// in (stability review). Err: that deadline has passed.
    fn read_deadline(request_started: Option<std::time::Instant>, now: std::time::Instant) -> Result<Option<Duration>, ()> {
        match request_started {
            None => Ok(None),
            Some(t) => match LINE_DEADLINE.checked_sub(now.duration_since(t)) {
                Some(left) if !left.is_zero() => Ok(Some(left)),
                _ => Err(()),
            },
        }
    }

    fn touch(id: u32) {
        if let Ok(mut s) = SLOTS.lock() {
            if let Some(slot) = s.iter_mut().find(|s| s.id == id) {
                slot.last = std::time::Instant::now();
            }
        }
    }

    /// Manual-reset event, closed on drop.
    struct IoEvent(HANDLE);
    // An event handle may be waited on, set and closed from any thread.
    unsafe impl Send for IoEvent {}
    impl IoEvent {
        fn new() -> Option<Self> {
            unsafe { CreateEventW(None, true, false, None).ok().map(IoEvent) }
        }
    }
    impl Drop for IoEvent {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Waits for overlapped I/O `ov` on `pipe` until it finishes, `cancel`
    /// is set, or `deadline` passes. Bytes moved, or None (failed, timed
    /// out, cancelled: the I/O is cancelled and fully finished before this
    /// returns, so the buffer is free again).
    unsafe fn finish_io(
        started: windows::core::Result<()>,
        pipe: HANDLE,
        ov: &mut OVERLAPPED,
        cancel: HANDLE,
        deadline: Option<Duration>,
    ) -> Option<u32> {
        let mut n = 0u32;
        if started.is_err() && GetLastError() != ERROR_IO_PENDING {
            return None;
        }
        let ms = deadline.map_or(INFINITE, |d| d.as_millis().min(u32::MAX as u128 - 1) as u32);
        let r = WaitForMultipleObjects(&[ov.hEvent, cancel], false, ms);
        if r == WAIT_OBJECT_0 {
            return GetOverlappedResult(pipe, ov, &mut n, false).ok().map(|_| n);
        }
        let _ = CancelIoEx(pipe, Some(ov));
        let _ = GetOverlappedResult(pipe, ov, &mut n, true);
        None
    }

    unsafe fn write_line(pipe: HANDLE, text: &str, ev: &IoEvent, cancel: HANDLE) -> bool {
        let mut data = text.as_bytes().to_vec();
        data.push(b'\n');
        let mut off = 0;
        while off < data.len() {
            let _ = ResetEvent(ev.0);
            let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
            let started = WriteFile(pipe, Some(&data[off..]), None, Some(&mut ov));
            match finish_io(started, pipe, &mut ov, cancel, Some(WRITE_DEADLINE)) {
                Some(n) if n > 0 => off += n as usize,
                _ => return false, // not taking its answers: drop it
            }
        }
        true
    }

    static NEXT_CONN: AtomicU32 = AtomicU32::new(1);

    /// Settings requests carry the connection they came from, so one client
    /// cannot end another's preview.
    fn tag_conn(line: &str, conn: u32) -> (String, bool) {
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(line) else { return (line.to_string(), false) };
        let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if !matches!(t.as_str(), "Preview" | "Commit" | "Revert" | "Quit" | "SuspendKeys" | "NewNote") {
            return (line.to_string(), false);
        }
        if let Some(o) = v.as_object_mut() {
            o.insert("conn".into(), conn.into());
        }
        // Both leave something behind if the window goes away: a Revert
        // on disconnect undoes a preview and resumes the shortcuts.
        (v.to_string(), t == "Preview" || t == "SuspendKeys")
    }

    /// Serves one client until it leaves, stalls past a deadline or is
    /// evicted (`cancel`); true if it ever started a preview.
    unsafe fn serve(pipe: HANDLE, conn: u32, cancel: HANDLE, ask_ui: &dyn Fn(&str) -> Option<String>) -> bool {
        let mut previewed = false;
        let mut pending: Vec<u8> = Vec::new();
        // When the request now arriving sent its first byte.
        let mut request_started: Option<std::time::Instant> = None;
        let mut buf = [0u8; 4096];
        let Some(ev) = IoEvent::new() else { return false };
        loop {
            let Ok(deadline) = read_deadline(request_started, std::time::Instant::now()) else {
                return previewed; // a request not finished in time
            };
            let _ = ResetEvent(ev.0);
            let mut ov = OVERLAPPED { hEvent: ev.0, ..Default::default() };
            let started = ReadFile(pipe, Some(&mut buf), None, Some(&mut ov));
            let n = match finish_io(started, pipe, &mut ov, cancel, deadline) {
                Some(n) if n > 0 => n,
                _ => return previewed, // went away, stalled or evicted
            };
            // (Partial bytes are not activity: only finished requests make
            // a client "recently used" for eviction.)
            let now = std::time::Instant::now();
            request_started.get_or_insert(now);
            pending.extend_from_slice(&buf[..n as usize]);
            if pending.len() > MAX_LINE {
                return previewed; // no newline in 1 MB: not our client
            }
            while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line[..line.len() - 1]).trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let (line, p) = tag_conn(&line, conn);
                previewed |= p;
                if !write_line(pipe, &handle_line(&line, ask_ui), &ev, cancel) {
                    return previewed;
                }
                touch(conn);
                // The rest (if any) is the next request, begun just now.
                request_started = Some(now);
            }
            if pending.is_empty() {
                request_started = None;
            }
        }
    }

    /// Takes a client slot for `conn`, making room when all MAX_CLIENTS are
    /// connected: ends the longest-idle one and waits (briefly) for it to
    /// go. Checking and taking happen under one lock, so two newcomers can
    /// never both take the last slot. Runs on the newcomer's own thread.
    fn claim_slot(conn: u32, cancel: isize) -> bool {
        for _ in 0..100 {
            {
                let Ok(mut s) = SLOTS.lock() else { return false };
                if (s.len() as u32) < MAX_CLIENTS {
                    s.push(Slot { id: conn, last: std::time::Instant::now(), cancel });
                    return true;
                }
                let list: Vec<(u32, std::time::Instant)> = s.iter().map(|s| (s.id, s.last)).collect();
                if let Some(slot) = idlest(&list).and_then(|id| s.iter().find(|s| s.id == id)) {
                    // Set under the lock: a client removes its slot (under
                    // the same lock) before closing this event.
                    unsafe {
                        let _ = SetEvent(HANDLE(slot.cancel as *mut _));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Serves a connected client on a thread of its own, so one that stalls
    /// cannot block the others; deadlines and eviction end stuck ones.
    unsafe fn spawn_client(pipe: HANDLE, hwnd: isize, tx: EventTx) {
        let drop_pipe = |pipe: HANDLE| {
            let _ = DisconnectNamedPipe(pipe);
            let _ = CloseHandle(pipe);
        };
        let raw = pipe.0 as isize;
        let spawned = std::thread::Builder::new().name("ipc-client".into()).spawn(move || {
            let pipe = HANDLE(raw as *mut _);
            let Some(cancel) = IoEvent::new() else {
                drop_pipe(pipe);
                return;
            };
            let conn = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
            if !claim_slot(conn, cancel.0 .0 as isize) {
                crate::log!("ipc: no room for a new client");
                drop_pipe(pipe);
                return;
            }
            let ask_ui = |req: &str| -> Option<String> {
                let (reply_tx, reply_rx) = channel();
                tx.send(Event::Ipc { req: req.to_string(), reply: reply_tx, at: std::time::Instant::now() }).ok()?;
                notify(hwnd);
                reply_rx.recv_timeout(Duration::from_secs(1)).ok()
            };
            // A settings window that went away without saving (closed,
            // crashed, evicted) must not leave its preview behind.
            if serve(pipe, conn, cancel.0, &ask_ui) {
                let _ = ask_ui(&format!(r#"{{"type":"Revert","why":"disconnect","conn":{conn}}}"#));
            }
            drop_pipe(pipe);
            if let Ok(mut s) = SLOTS.lock() {
                s.retain(|s| s.id != conn);
            }
            drop(cancel);
        });
        if spawned.is_err() {
            drop_pipe(HANDLE(raw as *mut _));
        }
    }

    /// Starts listening for settings windows: a watch on the service
    /// thread waits for the next client (overlapped ConnectNamedPipe);
    /// each connected client gets a thread of its own while it stays.
    pub fn listen(hwnd: isize, tx: EventTx) {
        let Some(sd) = (unsafe { owner_only_sd() }) else {
            // Fail closed: no pipe rather than a pipe others can open.
            crate::log!("ipc: could not build the pipe's security descriptor; settings pipe disabled");
            return;
        };
        let Some(ev) = IoEvent::new() else {
            crate::log!("ipc: no event to wait on; settings pipe disabled");
            return;
        };
        let l = Listener {
            hwnd,
            tx,
            name: HSTRING::from(pipe_name()),
            sd: sd.0 as isize,
            ov: Box::new(OVERLAPPED { hEvent: ev.0, ..Default::default() }),
            _ev: ev,
            pipe: 0,
            failures: 0,
        };
        crate::service::add(Box::new(l));
    }

    /// The pipe instance waiting for the next client.
    struct Listener {
        hwnd: isize,
        tx: EventTx,
        name: HSTRING,
        /// Owner-only security descriptor (kept for the life of the process).
        sd: isize,
        /// Boxed: the kernel writes to it while a connect is pending.
        ov: Box<OVERLAPPED>,
        /// The OVERLAPPED's event (closed last, after any pending connect).
        _ev: IoEvent,
        /// The instance being listened on (0 = none).
        pipe: isize,
        failures: u32,
    }

    // The raw handles and the descriptor are only used on the service thread.
    unsafe impl Send for Listener {}

    impl Listener {
        /// Makes a pipe instance and starts waiting for a client on it.
        unsafe fn listen(&mut self) -> crate::service::Next {
            use crate::service::Next;
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.sd as *mut _,
                bInheritHandle: false.into(),
            };
            // A client that connects and leaves at once: take the next one.
            for _ in 0..8 {
                // One instance more than the clients served, so a newcomer
                // can connect (and make room) while all slots are taken.
                let pipe = CreateNamedPipeW(
                    &self.name,
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    MAX_CLIENTS + 1,
                    64 * 1024,
                    64 * 1024,
                    0,
                    Some(&sa),
                );
                if pipe == INVALID_HANDLE_VALUE {
                    if GetLastError() == ERROR_PIPE_BUSY {
                        // All instances are taken; wait for one to free up.
                        return Next::Sleep(Duration::from_secs(1));
                    }
                    self.failures += 1;
                    crate::log!("ipc: CreateNamedPipe failed ({:?})", GetLastError());
                    if self.failures >= 5 {
                        return Next::Done;
                    }
                    return Next::Sleep(Duration::from_secs(2));
                }
                self.failures = 0;
                let _ = ResetEvent(self.ov.hEvent);
                if ConnectNamedPipe(pipe, Some(&mut *self.ov)).is_ok() {
                    spawn_client(pipe, self.hwnd, self.tx.clone());
                    continue;
                }
                match GetLastError() {
                    ERROR_IO_PENDING => {
                        self.pipe = pipe.0 as isize;
                        return Next::Wait;
                    }
                    ERROR_PIPE_CONNECTED => spawn_client(pipe, self.hwnd, self.tx.clone()),
                    _ => {
                        let _ = DisconnectNamedPipe(pipe);
                        let _ = CloseHandle(pipe);
                    }
                }
            }
            Next::Sleep(Duration::from_secs(1))
        }
    }

    impl crate::service::Watch for Listener {
        fn name(&self) -> &'static str {
            "settings pipe"
        }
        fn handles(&self) -> Vec<HANDLE> {
            if self.pipe == 0 {
                Vec::new()
            } else {
                vec![self.ov.hEvent]
            }
        }
        fn wake(&mut self, why: crate::service::Wake) -> crate::service::Next {
            use crate::service::{Next, Wake};
            unsafe {
                match why {
                    Wake::Signaled(_) => {
                        let pipe = HANDLE(std::mem::replace(&mut self.pipe, 0) as *mut _);
                        let mut n = 0u32;
                        if GetOverlappedResult(pipe, &*self.ov, &mut n, false).is_ok() {
                            spawn_client(pipe, self.hwnd, self.tx.clone());
                        } else {
                            let _ = DisconnectNamedPipe(pipe);
                            let _ = CloseHandle(pipe);
                        }
                        self.listen()
                    }
                    _ if self.pipe == 0 => self.listen(),
                    _ => Next::Wait,
                }
            }
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            unsafe {
                if self.pipe != 0 {
                    let pipe = HANDLE(self.pipe as *mut _);
                    // Cancel the pending connect before the OVERLAPPED goes.
                    let _ = CancelIoEx(pipe, Some(&*self.ov));
                    let mut n = 0u32;
                    let _ = GetOverlappedResult(pipe, &*self.ov, &mut n, true);
                    let _ = CloseHandle(pipe);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn no_ui(_: &str) -> Option<String> {
            None
        }

        #[test]
        fn hello_and_ping() {
            assert!(handle_line(r#"{"type":"Hello","ver":1}"#, &no_ui).contains("\"proto\":1"));
            assert_eq!(handle_line(r#"{"type":"Ping"}"#, &no_ui), r#"{"type":"Pong"}"#);
        }

        #[test]
        fn notes_entry_reaches_the_ui_with_its_settings_connection() {
            let (request, remember) = tag_conn(r#"{"type":"NewNote"}"#, 17);
            assert!(!remember);
            let reply = handle_line(&request, &|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(value["type"], "NewNote");
                assert_eq!(value["conn"], 17);
                Some(r#"{"type":"Ok"}"#.into())
            });
            assert_eq!(reply, r#"{"type":"Ok"}"#);
            assert!(handle_line(&request, &no_ui).contains("dock busy"));
        }

        #[test]
        fn errors_are_replies_not_panics() {
            assert!(handle_line("{nope", &no_ui).contains("bad json"));
            assert!(handle_line(r#"{"type":"Preview"}"#, &no_ui).contains("dock busy"));
            // Recording a shortcut reaches the dock, and its connection is
            // remembered (a disconnect resumes the shortcuts).
            assert!(handle_line(r#"{"type":"SuspendKeys","on":true}"#, &no_ui).contains("dock busy"));
            let (tagged, remember) = tag_conn(r#"{"type":"SuspendKeys","on":true}"#, 7);
            assert!(remember && tagged.contains("\"conn\":7"), "{tagged}");
            assert!(handle_line(r#"{"type":"GetState"}"#, &no_ui).contains("dock busy"));
        }

        #[test]
        fn newcomer_ends_the_quietest_client() {
            let now = std::time::Instant::now();
            let s = Duration::from_secs(1);
            assert_eq!(idlest(&[]), None);
            assert_eq!(idlest(&[(1, now), (2, now - 5 * s), (3, now - s)]), Some(2));
        }

        #[test]
        fn a_request_has_one_deadline_from_its_first_byte() {
            let t = std::time::Instant::now();
            assert_eq!(read_deadline(None, t), Ok(None), "idle between requests: no limit");
            assert_eq!(read_deadline(Some(t), t), Ok(Some(LINE_DEADLINE)));
            // Bytes trickling in do not extend it: only what is left remains.
            assert_eq!(read_deadline(Some(t), t + Duration::from_secs(9)), Ok(Some(Duration::from_secs(1))));
            assert_eq!(read_deadline(Some(t), t + LINE_DEADLINE), Err(()));
            assert_eq!(read_deadline(Some(t), t + Duration::from_secs(60)), Err(()));
        }

        #[test]
        fn getstate_uses_ui_answer() {
            let ui = |_: &str| Some(r#"{"type":"State"}"#.to_string());
            assert_eq!(handle_line(r#"{"type":"GetState"}"#, &ui), r#"{"type":"State"}"#);
        }
    }
}

fn mica_worker(hwnd: isize, jobs: &mut Jobs<(i32, i32, i32, i32)>, tx: &EventTx) {
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    while let Some(mut mon) = jobs.next() {
        // Only the newest screen layout matters.
        while let Some(m) = jobs.try_next() {
            mon = m;
        }
        let r = windows::Win32::Foundation::RECT { left: mon.0, top: mon.1, right: mon.2, bottom: mon.3 };
        let img = crate::wallpaper::mica_image(r);
        if tx.send(Event::Wallpaper { mon, img }).is_err() {
            break;
        }
        notify(hwnd);
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

#[cfg(test)]
mod helper_cap_tests {
    use super::*;

    #[test]
    fn stuck_helpers_are_not_joined_by_more() {
        static CAP: HelperCap = HelperCap::new(2);
        let a = CAP.try_take().expect("first");
        let b = CAP.try_take().expect("second");
        assert!(CAP.try_take().is_none(), "two still running: refused");
        drop(a);
        let c = CAP.try_take().expect("a slot came free");
        assert!(CAP.try_take().is_none());
        drop(b);
        drop(c);
        assert!(CAP.try_take().is_some());
    }
}
#[cfg(test)]
mod queue_tests {
    use super::*;

    fn icon(key: &str, size: i32) -> Event {
        Event::Icon { key: key.into(), size, look: Default::default(), drawn: false, icon: None, small: Vec::new() }
    }

    fn kinds(rx: &EventRx) -> Vec<String> {
        rx.try_iter()
            .map(|e| match e {
                Event::Snapshot { seq, .. } => format!("snap{seq}"),
                Event::Icon { key, size, .. } => format!("icon{key}@{size}"),
                Event::PinKinds(k) => format!("kinds{}", k.len()),
                Event::ExplorerGone(pid) => format!("gone{pid}"),
                _ => "other".into(),
            })
            .collect()
    }

    #[test]
    fn newest_of_a_kind_replaces_the_queued_one() {
        let (tx, rx) = event_queue();
        tx.send(Event::Snapshot { seq: 1, wins: vec![] }).unwrap();
        tx.send(icon("a", 32)).unwrap();
        tx.send(Event::Snapshot { seq: 3, wins: vec![] }).unwrap();
        tx.send(Event::Snapshot { seq: 2, wins: vec![] }).unwrap(); // older: ignored
        tx.send(icon("a", 32)).unwrap();
        tx.send(icon("a", 48)).unwrap();
        tx.send(Event::ExplorerGone(7)).unwrap();
        tx.send(Event::ExplorerGone(7)).unwrap();
        tx.send(Event::PinKinds(vec![("A".into(), true)])).unwrap();
        tx.send(Event::PinKinds(vec![("a".into(), false), ("b".into(), true)])).unwrap();
        assert_eq!(kinds(&rx), ["snap3", "icona@32", "icona@48", "gone7", "kinds2"]);
        assert!(kinds(&rx).is_empty(), "drained");
    }

    #[test]
    fn a_full_queue_never_drops_critical_events() {
        let (tx, rx) = event_queue();
        tx.send(Event::ExplorerGone(1)).unwrap();
        tx.send(Event::Snapshot { seq: 1, wins: vec![] }).unwrap();
        for i in 0..MAX_QUEUED + 5 {
            tx.send(icon(&i.to_string(), 32)).unwrap();
        }
        let got = kinds(&rx);
        assert_eq!(&got[..2], ["gone1", "snap1"], "recovery and window list kept; icons went instead");
        assert_eq!(got.len(), MAX_QUEUED);
    }

    #[test]
    fn the_queue_is_capped_and_closes_with_its_reader() {
        let (tx, rx) = event_queue();
        for i in 0..MAX_QUEUED + 10 {
            tx.send(icon(&i.to_string(), 32)).unwrap();
        }
        let got = kinds(&rx);
        assert_eq!(got.len(), MAX_QUEUED);
        assert_eq!(got[0], "icon10@32", "the oldest went first");
        let lost = rx.take_lost_icons();
        assert_eq!(lost.len(), 10, "and are asked for again");
        assert_eq!(lost[0], ("0".to_string(), 32));
        drop(rx);
        assert!(tx.send(Event::ExplorerGone(1)).is_err());
    }
}

#[cfg(test)] mod widget_shell_tests {
 #[test] fn review_13_widgets_ipc_forwards_to_ui_without_settings_ownership(){
  let seen=std::cell::RefCell::new(Vec::new());
  let reply=super::ipc::handle_line(r#"{"type":"WidgetsAdd"}"#,&|s|{seen.borrow_mut().push(s.to_string());Some(r#"{"type":"Ok"}"#.into())});
  assert_eq!(reply,r#"{"type":"Ok"}"#);assert_eq!(&*seen.borrow(),&["WidgetsAdd"]);
  assert!(super::ipc::handle_line(r#"{"type":"WidgetsAdd"}"#,&|_|None).contains("dock busy"));
 }
}
