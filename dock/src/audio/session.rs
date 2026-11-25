//! Per-app volume: the wheel on a running app's dock icon changes that
//! app's audio session volume, and the dock asks (only while the pointer is
//! on it) which apps are making sound, for the speaker badge. The Live
//! Island's card reads and sets the volume of the app playing (matched by
//! its media session's app id, `media_match`).
//!
//! One worker thread (COM MTA) does all Core Audio work. It starts on the
//! first request and ends after IDLE of no requests, so nothing runs while
//! the dock is left alone. Requests are coalesced (wheel steps for an app
//! add up while the worker is busy) and dropped while a call is stuck past
//! STUCK, so a hung audio service never queues work or blocks the UI. The
//! answers go to a bounded list and one posted message tells the dock.
//!
//! Sessions are found on every active render endpoint (not only the
//! default: an app may play elsewhere) and kept for SESSIONS_FOR; a new
//! session (IAudioSessionNotification::OnSessionCreated) only marks the
//! list stale, it is read again on the next request.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Ref, Result, GUID, PWSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND, LPARAM, S_OK, WPARAM};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eRender, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl, IAudioSessionControl2, IAudioSessionManager2,
    IAudioSessionNotification, IAudioSessionNotification_Impl, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the dock when answers are waiting (take them with `take`).
pub const WM_SOUND: u32 = WM_APP + 76;

/// Percent per wheel notch.
pub const STEP_PCT: i32 = 4;
/// One notch of a classic wheel (WHEEL_DELTA).
const WHEEL_DELTA: i32 = 120;
/// Peak level above which a session counts as making sound.
pub const PEAK_THRESHOLD: f32 = 0.01;
/// Parent links followed from a session's process to an app.
const MAX_DEPTH: usize = 4;
/// The worker ends after this long without requests.
const IDLE: Duration = Duration::from_secs(30);
/// Session list reused this long (unless a new session appeared).
const SESSIONS_FOR: Duration = Duration::from_secs(3);
/// Endpoint list (and session managers) reused this long.
const ENDPOINTS_FOR: Duration = Duration::from_secs(10);
/// A request running longer than this is stuck: new ones are dropped.
const STUCK: Duration = Duration::from_secs(2);
/// Wheel steps not started within this long are dropped (the moment
/// passed; the dock would not show their answer either).
const STEP_LATE: Duration = Duration::from_millis(1500);
/// Bound on waiting answers.
const MAX_REPLIES: usize = 32;
/// Our event context for SetMasterVolume / SetMute.
const EVENT_CTX: GUID = GUID::from_u128(0x6e6f_7062_6172_4175_6469_6f53_7465_7000);

// ---- pure parts ------------------------------------------------------------------

/// Turns wheel deltas into whole percent steps. High-resolution wheels and
/// touchpads send fractions of a notch: they add up, STEP_PCT per full
/// notch, and the remainder waits for the next delta. A turn the other way
/// starts over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WheelAcc {
    /// Remainder, in delta units times STEP_PCT.
    acc: i32,
}

impl WheelAcc {
    /// Adds a delta (120 = one notch up) and returns the percent to move.
    pub fn feed(&mut self, delta: i32) -> i32 {
        if delta == 0 {
            return 0;
        }
        if (self.acc > 0) != (delta > 0) {
            self.acc = 0;
        }
        self.acc += delta * STEP_PCT;
        let out = self.acc / WHEEL_DELTA;
        self.acc -= out * WHEEL_DELTA;
        out
    }

    pub fn reset(&mut self) {
        self.acc = 0;
    }
}

/// Whole percent for a 0..1 level.
pub fn percent(level: f32) -> u8 {
    if !level.is_finite() {
        return 0;
    }
    (level.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// The level after moving `delta` percent from `level` (0..1), in percent.
pub fn step_level(level: f32, delta: i32) -> u8 {
    (percent(level) as i32 + delta).clamp(0, 100) as u8
}

/// What the hover label says: "42%" (in the UI language's pattern), or
/// "Muted".
pub fn level_text(p: u8, muted: bool) -> String {
    if muted {
        crate::i18n::t("control.muted").to_string()
    } else {
        crate::i18n::tf("control.percent", &[("p", &p.to_string())])
    }
}

/// What the worker knows about a process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcInfo {
    pub parent: u32,
    /// Lowercase full path, when readable.
    pub exe: Option<String>,
    /// Packaged apps' AUMID.
    pub aumid: Option<String>,
    /// Creation time (FILETIME ticks), 0 = unknown.
    pub created: u64,
}

pub type Procs = HashMap<u32, ProcInfo>;

/// A running app on the dock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// The dock item's path (its key everywhere in the dock).
    pub key: String,
    /// Processes owning its windows.
    pub pids: Vec<u32>,
    /// Lowercase exe path.
    pub exe: Option<String>,
    pub aumid: Option<String>,
}

fn file_name(p: &str) -> &str {
    p.rsplit(['\\', '/']).next().unwrap_or(p)
}

/// Which target (index) a session from process `pid` belongs to, by the
/// strongest evidence on its verified parent chain (itself first, at most
/// MAX_DEPTH links up):
/// 1. the closest process that owns a target's windows (so a WebView2
///    child counts for its host, and a game started by a launcher counts
///    for the game: its own windows come first);
/// 2. a packaged app's AUMID; a process that is a packaged app no dock
///    item runs belongs to nobody (never matched by exe after that);
/// 3. last resort, the exe path, and only when exactly one target has it
///    (a browser's audio process after the app restarted).
///
/// The chain stops at explorer (the shell starts nearly everything), at
/// unknown processes, and at a parent that is not provably older than its
/// child (ids are reused; unknown creation times prove nothing).
pub fn owner(pid: u32, targets: &[Target], procs: &Procs) -> Option<usize> {
    let chain = chain(pid, procs);
    if let Some(t) = chain.iter().find_map(|p| targets.iter().position(|t| t.pids.contains(p))) {
        return Some(t);
    }
    for p in &chain {
        if let Some(a) = procs.get(p).and_then(|i| i.aumid.as_deref()) {
            return targets.iter().position(|t| t.aumid.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(a)));
        }
    }
    for p in &chain {
        let Some(e) = procs.get(p).and_then(|i| i.exe.as_deref()) else { continue };
        let mut hits = targets.iter().enumerate().filter(|(_, t)| t.exe.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(e)));
        match (hits.next(), hits.next()) {
            (Some((t, _)), None) => return Some(t),
            (Some(_), Some(_)) => return None,
            _ => {}
        }
    }
    None
}

/// The session's process and its verified parents (see `owner`).
fn chain(pid: u32, procs: &Procs) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut cur = pid;
    for depth in 0..=MAX_DEPTH {
        if cur == 0 || out.contains(&cur) {
            break;
        }
        let info = procs.get(&cur);
        if depth > 0 && info.and_then(|i| i.exe.as_deref()).is_some_and(|e| file_name(e).eq_ignore_ascii_case("explorer.exe")) {
            break;
        }
        out.push(cur);
        let Some(i) = info else { break };
        let older = procs.get(&i.parent).is_some_and(|p| p.created != 0 && i.created != 0 && p.created < i.created);
        if !older {
            break;
        }
        cur = i.parent;
    }
    out
}

/// The app ids browsers give their windows and media sessions (lowercase,
/// per channel), and the exe that plays their sound. Firefox's id is a
/// hash of its install folder: the release one's is known.
const BROWSER_IDS: [(&str, &str); 20] = [
    ("chrome", "chrome.exe"),
    ("chromebeta", "chrome.exe"),
    ("chromedev", "chrome.exe"),
    ("chromecanary", "chrome.exe"),
    ("msedge", "msedge.exe"),
    ("msedgebeta", "msedge.exe"),
    ("msedgedev", "msedge.exe"),
    ("msedgecanary", "msedge.exe"),
    ("brave", "brave.exe"),
    ("bravesoftware.brave", "brave.exe"),
    ("bravesoftware.brave.beta", "brave.exe"),
    ("bravesoftware.brave.nightly", "brave.exe"),
    ("bravesoftware.bravebeta", "brave.exe"),
    ("bravesoftware.bravenightly", "brave.exe"),
    ("opera", "opera.exe"),
    ("operagx", "opera.exe"),
    ("vivaldi", "vivaldi.exe"),
    ("firefox", "firefox.exe"),
    ("308046b0af4a39cb", "firefox.exe"),
    ("firefoxdeveloperedition", "firefox.exe"),
];
/// An installed web app's id: "<browser id>._crx_<app>" (it plays in
/// that browser).
const WEB_APP: &str = "._crx_";

/// A path compared as Windows does: lowercase, one kind of slash.
fn norm_path(p: &str) -> String {
    p.trim().replace('/', "\\").to_lowercase()
}

/// Does an audio session's process (its exe path, its packaged AUMID)
/// belong to the app of a media session, whose SourceAppUserModelId is
/// `app`?
/// - a packaged app's id ("Pkg_hash!App"): only that AUMID;
/// - a path: only that exe (the whole path: another copy elsewhere is
///   another app);
/// - "Spotify.exe": an exe of that name;
/// - a browser's id (BROWSER_IDS, or one of its installed web apps): its
///   exe;
/// - any other plain name ("Spotify"): the exe of that name. A dotted id
///   nobody knows matches nothing (no guessing at its parts).
pub fn media_match(app: &str, exe: Option<&str>, aumid: Option<&str>) -> bool {
    let id = app.trim();
    if id.is_empty() {
        return false;
    }
    if id.contains('!') {
        return aumid.is_some_and(|a| a.eq_ignore_ascii_case(id));
    }
    let Some(exe) = exe else { return false };
    if id.contains(['\\', '/']) {
        return norm_path(id) == norm_path(exe);
    }
    let name = file_name(exe);
    let lower = id.to_ascii_lowercase();
    if lower.ends_with(".exe") {
        return lower.len() > 4 && name.eq_ignore_ascii_case(id);
    }
    let browser = match lower.find(WEB_APP) {
        Some(at) => &lower[..at],
        None => lower.as_str(),
    };
    if let Some((_, want)) = BROWSER_IDS.iter().find(|(b, _)| *b == browser) {
        return name.eq_ignore_ascii_case(want);
    }
    if lower.contains('.') {
        return false;
    }
    let stem = name.len().checked_sub(4).filter(|&n| name.is_char_boundary(n) && name[n..].eq_ignore_ascii_case(".exe")).map_or(name, |n| &name[..n]);
    stem.eq_ignore_ascii_case(&lower)
}

/// Processes whose details the matcher may need: the session processes and
/// their parent chains (from `parents`, pid -> parent pid), each once.
pub fn chain_pids(sessions: &[u32], parents: &HashMap<u32, u32>) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for &s in sessions {
        let mut cur = s;
        for _ in 0..=MAX_DEPTH {
            if cur == 0 || out.contains(&cur) {
                break;
            }
            out.push(cur);
            match parents.get(&cur) {
                Some(&p) => cur = p,
                None => break,
            }
        }
    }
    out
}

// ---- requests and answers ---------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// An app's volume after a step.
    Level { key: String, percent: u8, muted: bool },
    /// The app has no audio session (the wheel does nothing for it).
    Silent { key: String },
    /// The apps making sound right now.
    Playing(Vec<String>),
}

/// The Live Island's answer: the volume of the media session's app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppVolume {
    /// The media session's app id it was asked for.
    pub app: String,
    /// (percent, muted); None = no audio session of that app (the card
    /// shows no slider).
    pub level: Option<(u8, bool)>,
    /// The request's number (app_volume's): an answer to a request older
    /// than the newest change is not shown.
    pub seq: u64,
}

/// One request of the island's card, waiting for the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaReq {
    pub app: String,
    /// Set to this percent; None: read only.
    pub set: Option<u8>,
    pub seq: u64,
    /// Where its answer is posted: (window, message).
    pub to: (isize, u32),
}

/// Requests waiting at most (a card asks for one app at a time).
const MAX_MEDIA_REQS: usize = 8;
/// Answers waiting at most.
const MAX_MEDIA_REPLIES: usize = 16;

/// Queues `r` among the requests not started yet. A write replaces only an
/// older write for the same app (a drag sends many: the newest value
/// counts); a read replaces only an older read for the same app. A read
/// never drops a write, and nothing of another app is dropped (unless
/// MAX_MEDIA_REQS are waiting: then the oldest read goes, never a write).
pub fn queue_media(q: &mut Vec<MediaReq>, r: MediaReq) {
    let same = |o: &MediaReq| o.app.eq_ignore_ascii_case(&r.app) && o.set.is_some() == r.set.is_some();
    q.retain(|o| !same(o));
    q.push(r);
    while q.len() > MAX_MEDIA_REQS {
        match q.iter().position(|o| o.set.is_none()) {
            Some(i) => {
                q.remove(i);
            }
            None => break,
        }
    }
}

/// The order the worker runs them: writes first (in the order asked), then
/// reads, so a read asked after a write answers with the written value.
pub fn media_order(mut q: Vec<MediaReq>) -> Vec<MediaReq> {
    q.sort_by_key(|r| r.set.is_none());
    q
}

static MEDIA_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct Pending {
    /// The running apps, newest snapshot.
    targets: Vec<Target>,
    /// Wheel steps per app, added up, and when the last one came.
    steps: Vec<(String, i32, Instant)>,
    peaks: bool,
    /// The island's card: read or set its media app's volume (queue_media).
    media: Vec<MediaReq>,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.steps.is_empty() && !self.peaks && self.media.is_empty()
    }
}

struct State {
    pending: Pending,
    /// Wakes the worker; None when no worker runs.
    wake: Option<Sender<()>>,
    /// A wake is queued and not taken yet (at most one is).
    woken: bool,
    hwnd: isize,
    replies: Vec<Reply>,
    /// WM_SOUND is on its way and not taken yet.
    posted: bool,
    /// The worker is inside a request since then.
    busy_since: Option<Instant>,
    stuck_logged: bool,
    /// The card's answers not taken yet.
    media_replies: Vec<AppVolume>,
}

static STATE: Mutex<State> = Mutex::new(State {
    pending: Pending { targets: Vec::new(), steps: Vec::new(), peaks: false, media: Vec::new() },
    wake: None,
    woken: false,
    hwnd: 0,
    replies: Vec::new(),
    posted: false,
    busy_since: None,
    stuck_logged: false,
    media_replies: Vec::new(),
});
/// A session was created since the list was read.
static SESSIONS_DIRTY: AtomicBool = AtomicBool::new(true);

fn submit(hwnd: HWND, targets: Vec<Target>, f: impl FnOnce(&mut Pending)) {
    submit_with(|s| {
        s.hwnd = hwnd.0 as isize;
        s.pending.targets = targets;
        f(&mut s.pending);
    });
}

/// Queues a request (`f` writes it) and wakes the worker, starting it if
/// none runs.
fn submit_with(f: impl FnOnce(&mut State)) {
    let Ok(mut s) = STATE.lock() else { return };
    if s.busy_since.is_some_and(|t| t.elapsed() > STUCK) {
        if !std::mem::replace(&mut s.stuck_logged, true) {
            crate::log!("sound: the audio worker is stuck in a call; requests are dropped until it returns");
        }
        return;
    }
    f(&mut s);
    if s.wake.is_some() && s.woken {
        return;
    }
    let woke = s.wake.as_ref().is_some_and(|w| w.send(()).is_ok());
    if woke {
        s.woken = true;
        return;
    }
    let (tx, rx) = channel::<()>();
    let started = std::thread::Builder::new().name("app-volume".into()).spawn(move || worker(rx));
    match started {
        Ok(_) => {
            let _ = tx.send(());
            s.wake = Some(tx);
            s.woken = true;
            // Setting up COM counts as busy: a hung first call is noticed.
            s.busy_since = Some(Instant::now());
        }
        Err(e) => crate::log!("sound: cannot start the audio worker: {e}"),
    }
}

/// Move app `key`'s volume by `delta` percent (wheel). `targets` are all
/// running apps (to tell whose session is whose).
pub fn step(hwnd: HWND, key: &str, delta: i32, targets: Vec<Target>) {
    if delta == 0 {
        return;
    }
    let now = Instant::now();
    submit(hwnd, targets, |p| match p.steps.iter_mut().find(|(k, _, _)| k == key) {
        Some((_, d, at)) => {
            *d += delta;
            *at = now;
        }
        None => p.steps.push((key.to_string(), delta, now)),
    });
}

/// Ask which of `targets` are making sound.
pub fn poll_playing(hwnd: HWND, targets: Vec<Target>) {
    if targets.is_empty() {
        return;
    }
    submit(hwnd, targets, |p| p.peaks = true);
}

/// Read (`set` None) or set the volume of the audio sessions of media app
/// `app` (a media session's SourceAppUserModelId). The answer posts `msg`
/// to `hwnd`; take it with `take_app_volume`. Returns the request's
/// number (rising across all requests; 0: not asked).
pub fn app_volume(hwnd: HWND, msg: u32, app: &str, set: Option<u8>) -> u64 {
    if app.trim().is_empty() {
        return 0;
    }
    let seq = MEDIA_SEQ.fetch_add(1, Ordering::SeqCst);
    let r = MediaReq { app: app.to_string(), set: set.map(|p| p.min(100)), seq, to: (hwnd.0 as isize, msg) };
    submit_with(|s| queue_media(&mut s.pending.media, r));
    seq
}

/// The card's answers that came since the last take (oldest first).
pub fn take_app_volume() -> Vec<AppVolume> {
    STATE.lock().map(|mut s| std::mem::take(&mut s.media_replies)).unwrap_or_default()
}

fn answer_app(r: AppVolume, to: (isize, u32)) {
    let Ok(mut s) = STATE.lock() else { return };
    if s.media_replies.len() >= MAX_MEDIA_REPLIES {
        s.media_replies.remove(0);
    }
    s.media_replies.push(r);
    let (h, msg) = to;
    if h != 0 {
        // Posted every time (rare): a card that went keeps no flag stuck.
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// The answers so far (the next answer posts WM_SOUND again).
pub fn take() -> Vec<Reply> {
    match STATE.lock() {
        Ok(mut s) => {
            s.posted = false;
            std::mem::take(&mut s.replies)
        }
        Err(_) => Vec::new(),
    }
}

fn answer(r: Reply) {
    let Ok(mut s) = STATE.lock() else { return };
    // Newer replaces older of the same kind for the same app.
    s.replies.retain(|o| !matches!((o, &r), (Reply::Playing(_), Reply::Playing(_))) && !same_app(o, &r));
    if s.replies.len() >= MAX_REPLIES {
        s.replies.remove(0);
    }
    s.replies.push(r);
    if !s.posted && s.hwnd != 0 {
        let ok = unsafe { PostMessageW(Some(HWND(s.hwnd as *mut _)), WM_SOUND, WPARAM(0), LPARAM(0)) }.is_ok();
        s.posted = ok;
    }
}

fn same_app(a: &Reply, b: &Reply) -> bool {
    let key = |r: &Reply| match r {
        Reply::Level { key, .. } | Reply::Silent { key } => Some(key.clone()),
        Reply::Playing(_) => None,
    };
    key(a).is_some() && key(a) == key(b)
}

// ---- the worker ------------------------------------------------------------------

#[implement(IAudioSessionNotification)]
struct NewSession;

impl IAudioSessionNotification_Impl for NewSession_Impl {
    fn OnSessionCreated(&self, _: Ref<IAudioSessionControl>) -> Result<()> {
        // Lazily: the next request reads the sessions again.
        SESSIONS_DIRTY.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct Session {
    pid: u32,
    ctl: IAudioSessionControl2,
    vol: Option<ISimpleAudioVolume>,
    meter: Option<IAudioMeterInformation>,
}

struct Audio {
    enumerator: IMMDeviceEnumerator,
    notify: IAudioSessionNotification,
    managers: Vec<IAudioSessionManager2>,
    managers_at: Option<Instant>,
    sessions: Vec<Session>,
    procs: Procs,
    sessions_at: Option<Instant>,
}

impl Audio {
    unsafe fn new() -> Option<Self> {
        let enumerator: IMMDeviceEnumerator = match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            Ok(e) => e,
            Err(e) => {
                crate::log!("sound: no device enumerator: {e}");
                return None;
            }
        };
        Some(Audio {
            enumerator,
            notify: NewSession.into(),
            managers: Vec::new(),
            managers_at: None,
            sessions: Vec::new(),
            procs: Procs::new(),
            sessions_at: None,
        })
    }

    unsafe fn drop_managers(&mut self) {
        for m in self.managers.drain(..) {
            let _ = m.UnregisterSessionNotification(&self.notify);
        }
        self.managers_at = None;
    }

    /// Session managers of every active output device.
    unsafe fn managers(&mut self) {
        if self.managers_at.is_some_and(|t| t.elapsed() < ENDPOINTS_FOR) {
            return;
        }
        self.drop_managers();
        if let Ok(list) = self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
            for i in 0..list.GetCount().unwrap_or(0) {
                let Ok(dev) = list.Item(i) else { continue };
                let Ok(m) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                // Notifications start once the sessions were enumerated.
                let _ = m.GetSessionEnumerator();
                let _ = m.RegisterSessionNotification(&self.notify);
                self.managers.push(m);
            }
        }
        self.managers_at = Some(Instant::now());
        SESSIONS_DIRTY.store(true, Ordering::SeqCst);
    }

    /// The sessions (and their processes), read again when stale. True if
    /// they were read just now.
    unsafe fn sessions(&mut self, fresh: bool) -> bool {
        self.managers();
        let stale = fresh || SESSIONS_DIRTY.load(Ordering::SeqCst) || self.sessions_at.is_none_or(|t| t.elapsed() >= SESSIONS_FOR);
        if !stale {
            return false;
        }
        SESSIONS_DIRTY.store(false, Ordering::SeqCst);
        self.sessions.clear();
        for m in &self.managers {
            let Ok(en) = m.GetSessionEnumerator() else { continue };
            for i in 0..en.GetCount().unwrap_or(0) {
                let Ok(c) = en.GetSession(i) else { continue };
                let Ok(ctl) = c.cast::<IAudioSessionControl2>() else { continue };
                if ctl.IsSystemSoundsSession() == S_OK || ctl.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                    continue;
                }
                // Fails for sessions spanning several processes.
                let Ok(pid) = ctl.GetProcessId() else { continue };
                if pid == 0 {
                    continue;
                }
                let vol = ctl.cast::<ISimpleAudioVolume>().ok();
                let meter = ctl.cast::<IAudioMeterInformation>().ok();
                self.sessions.push(Session { pid, ctl, vol, meter });
            }
        }
        let pids: Vec<u32> = self.sessions.iter().map(|s| s.pid).collect();
        self.procs = read_procs(&pids);
        self.sessions_at = Some(Instant::now());
        true
    }

    /// Process details read again now (the session list may be cached).
    unsafe fn refresh_procs(&mut self) {
        let pids: Vec<u32> = self.sessions.iter().map(|s| s.pid).collect();
        self.procs = read_procs(&pids);
    }

    unsafe fn step(&mut self, targets: &[Target], key: &str, delta: i32) -> Reply {
        let Some(t) = targets.iter().position(|t| t.key == key) else { return Reply::Silent { key: key.into() } };
        let read = self.sessions(false);
        if !read {
            // Volume changes match against live processes, never against
            // a cached parent whose id may belong to someone else by now.
            self.refresh_procs();
        }
        let mut mine: Vec<usize> = self.owned(targets, t);
        if mine.is_empty() && !read {
            // Started since the list was read?
            self.sessions(true);
            mine = self.owned(targets, t);
        }
        let vols: Vec<&ISimpleAudioVolume> = mine.iter().filter_map(|&i| self.sessions[i].vol.as_ref()).collect();
        if vols.is_empty() {
            return Reply::Silent { key: key.into() };
        }
        let now = vols.iter().filter_map(|v| v.GetMasterVolume().ok()).fold(0.0f32, f32::max);
        let p = step_level(now, delta);
        for v in &vols {
            let _ = v.SetMasterVolume(p as f32 / 100.0, &EVENT_CTX);
            if delta > 0 {
                // Turning it up brings a muted app back.
                let _ = v.SetMute(false, &EVENT_CTX);
            }
        }
        let muted = vols.iter().all(|v| v.GetMute().is_ok_and(|m| m.as_bool()));
        Reply::Level { key: key.into(), percent: p, muted }
    }

    /// The volume of media app `app`'s sessions (the loudest of them), set
    /// first to `set` percent if asked (turning it up unmutes, as the
    /// wheel does). None: it has no session.
    unsafe fn media_volume(&mut self, app: &str, set: Option<u8>) -> Option<(u8, bool)> {
        let read = self.sessions(false);
        if !read {
            self.refresh_procs();
        }
        let mut mine = self.media_owned(app);
        if mine.is_empty() && !read {
            self.sessions(true);
            mine = self.media_owned(app);
        }
        let vols: Vec<&ISimpleAudioVolume> = mine.iter().filter_map(|&i| self.sessions[i].vol.as_ref()).collect();
        if vols.is_empty() {
            return None;
        }
        let now = percent(vols.iter().filter_map(|v| v.GetMasterVolume().ok()).fold(0.0f32, f32::max));
        let level = match set {
            Some(p) => {
                for v in &vols {
                    let _ = v.SetMasterVolume(p as f32 / 100.0, &EVENT_CTX);
                    if p > now {
                        let _ = v.SetMute(false, &EVENT_CTX);
                    }
                }
                p
            }
            None => now,
        };
        let muted = vols.iter().all(|v| v.GetMute().is_ok_and(|m| m.as_bool()));
        Some((level, muted))
    }

    fn media_owned(&self, app: &str) -> Vec<usize> {
        (0..self.sessions.len())
            .filter(|&i| {
                let p = self.procs.get(&self.sessions[i].pid);
                media_match(app, p.and_then(|p| p.exe.as_deref()), p.and_then(|p| p.aumid.as_deref()))
            })
            .collect()
    }

    fn owned(&self, targets: &[Target], t: usize) -> Vec<usize> {
        (0..self.sessions.len()).filter(|&i| owner(self.sessions[i].pid, targets, &self.procs) == Some(t)).collect()
    }

    unsafe fn playing(&mut self, targets: &[Target]) -> Vec<String> {
        let _ = self.sessions(false);
        let mut out: HashSet<String> = HashSet::new();
        for s in &self.sessions {
            let active = s.ctl.GetState().is_ok_and(|st| st == AudioSessionStateActive);
            let loud = active && s.meter.as_ref().and_then(|m| m.GetPeakValue().ok()).is_some_and(|p| p > PEAK_THRESHOLD);
            if loud {
                if let Some(t) = owner(s.pid, targets, &self.procs) {
                    out.insert(targets[t].key.clone());
                }
            }
        }
        out.into_iter().collect()
    }
}

fn worker(rx: std::sync::mpsc::Receiver<()>) {
    // Uninitialize only what we initialized.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut audio = unsafe { Audio::new() };
    if let Ok(mut s) = STATE.lock() {
        s.busy_since = None;
    }
    loop {
        match rx.recv_timeout(IDLE) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {
                // Nothing came: end, unless a request slipped in.
                let Ok(mut s) = STATE.lock() else { break };
                if s.pending.is_empty() {
                    s.wake = None;
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let (steps, peaks, targets, media) = match STATE.lock() {
            Ok(mut s) => {
                s.busy_since = Some(Instant::now());
                s.woken = false;
                (std::mem::take(&mut s.pending.steps), std::mem::replace(&mut s.pending.peaks, false), s.pending.targets.clone(), std::mem::take(&mut s.pending.media))
            }
            Err(_) => break,
        };
        for r in media_order(media) {
            let level = audio.as_mut().and_then(|a| unsafe { a.media_volume(&r.app, r.set) });
            answer_app(AppVolume { app: r.app, level, seq: r.seq }, r.to);
        }
        if let Some(a) = audio.as_mut() {
            for (key, delta, at) in &steps {
                if at.elapsed() > STEP_LATE {
                    continue;
                }
                let r = unsafe { a.step(&targets, key, *delta) };
                answer(r);
            }
            if peaks {
                let keys = unsafe { a.playing(&targets) };
                answer(Reply::Playing(keys));
            }
        } else {
            for (key, _, _) in &steps {
                answer(Reply::Silent { key: key.clone() });
            }
        }
        if let Ok(mut s) = STATE.lock() {
            s.busy_since = None;
            s.stuck_logged = false;
        }
    }
    if let Some(mut a) = audio.take() {
        unsafe { a.drop_managers() };
        drop(a);
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

/// Parents of every process, and details of the session processes and
/// their parent chains.
unsafe fn read_procs(sessions: &[u32]) -> Procs {
    let mut parents: HashMap<u32, u32> = HashMap::new();
    if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            parents.insert(e.th32ProcessID, e.th32ParentProcessID);
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let mut out = Procs::new();
    for pid in chain_pids(sessions, &parents) {
        let mut info = ProcInfo { parent: parents.get(&pid).copied().unwrap_or(0), ..Default::default() };
        if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                info.exe = Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase());
            }
            let (mut c, mut x, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
            if GetProcessTimes(h, &mut c, &mut x, &mut k, &mut u).is_ok() {
                info.created = ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64;
            }
            info.aumid = process_aumid(h);
            let _ = CloseHandle(h);
        }
        out.insert(pid, info);
    }
    out
}

unsafe fn process_aumid(h: windows::Win32::Foundation::HANDLE) -> Option<String> {
    use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    let r = GetApplicationUserModelId(h, &mut len, Some(PWSTR(buf.as_mut_ptr())));
    if r.0 != 0 || len == 0 {
        return None;
    }
    // len counts the terminating null.
    let n = (len as usize).saturating_sub(1).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_notches_and_fractions() {
        let mut w = WheelAcc::default();
        assert_eq!(w.feed(120), STEP_PCT);
        assert_eq!(w.feed(-120), -STEP_PCT);
        assert_eq!(w.feed(240), 2 * STEP_PCT);
        // A quarter notch is one percent; four of them make a notch.
        let mut w = WheelAcc::default();
        let quarter: Vec<i32> = (0..4).map(|_| w.feed(30)).collect();
        assert_eq!(quarter, vec![1, 1, 1, 1]);
        // Smaller fractions wait until they add up to a percent.
        let mut w = WheelAcc::default();
        assert_eq!(w.feed(20), 0);
        assert_eq!(w.feed(20), 1);
        assert_eq!(w.feed(10), 0);
        assert_eq!(w.feed(10), 1);
        // Turning back drops the leftover of the other direction.
        let mut w = WheelAcc::default();
        assert_eq!(w.feed(20), 0);
        assert_eq!(w.feed(-20), 0);
        assert_eq!(w.feed(-10), -1);
        w.reset();
        assert_eq!(w.feed(0), 0);
        // A full turn of tiny deltas adds up to exactly what notches give.
        let mut w = WheelAcc::default();
        let sum: i32 = (0..120).map(|_| w.feed(1)).sum();
        assert_eq!(sum, STEP_PCT);
    }

    #[test]
    fn levels_step_and_clamp() {
        assert_eq!(step_level(0.42, 4), 46);
        assert_eq!(step_level(0.98, 4), 100);
        assert_eq!(step_level(0.02, -4), 0);
        assert_eq!(step_level(0.375, 4), 42, "rounds to whole percent first");
        assert_eq!(step_level(f32::NAN, 4), 4);
        assert_eq!(percent(0.006), 1);
        assert_eq!(percent(0.004), 0);
        assert_eq!(percent(1.5), 100);
        assert_eq!(percent(-1.0), 0);
    }

    #[test]
    fn percent_formatting() {
        let l = crate::i18n::test_lock("en");
        assert_eq!(level_text(42, false), "42%");
        assert_eq!(level_text(0, false), "0%");
        assert_eq!(level_text(42, true), "Muted");
        drop(l);
        let _l = crate::i18n::test_lock("ko");
        assert_eq!(level_text(100, false), "100%");
        assert_eq!(level_text(7, true), crate::i18n::t("control.muted"));
    }

    fn p(parent: u32, exe: &str, created: u64) -> ProcInfo {
        ProcInfo { parent, exe: Some(exe.to_lowercase()), aumid: None, created }
    }

    fn t(key: &str, pids: &[u32], exe: &str) -> Target {
        Target { key: key.into(), pids: pids.to_vec(), exe: Some(exe.to_lowercase()), aumid: None }
    }

    #[test]
    fn sessions_match_their_app() {
        let explorer = r"c:\windows\explorer.exe";
        let chrome = r"c:\program files\google\chrome\application\chrome.exe";
        let teams = r"c:\program files\windowsapps\msteams\ms-teams.exe";
        let webview = r"c:\program files (x86)\microsoft\edgewebview\application\msedgewebview2.exe";
        let steam = r"c:\program files (x86)\steam\steam.exe";
        let game = r"c:\program files (x86)\steam\steamapps\common\game\game.exe";
        let player = r"c:\tools\player.exe";
        let mut procs = Procs::new();
        procs.insert(1, p(0, explorer, 1));
        procs.insert(10, p(1, chrome, 10)); // browser (windows)
        procs.insert(11, p(10, chrome, 11)); // its audio service
        procs.insert(20, p(1, teams, 20)); // Teams (windows)
        procs.insert(21, p(20, webview, 21)); // WebView2 under it
        procs.insert(22, p(21, webview, 22)); // ... and its audio child
        procs.insert(30, p(1, steam, 30)); // launcher (windows)
        procs.insert(31, p(30, game, 31)); // game (own windows)
        procs.insert(40, p(1, player, 40)); // no dock item, started by the shell
        let targets = vec![
            t("explorer", &[1], explorer),
            t("chrome", &[10], chrome),
            t("teams", &[20], teams),
            t("steam", &[30], steam),
            t("game", &[31], game),
        ];
        // Its own process.
        assert_eq!(owner(10, &targets, &procs), Some(1));
        // Same exe, no windows of its own.
        assert_eq!(owner(11, &targets, &procs), Some(1));
        // A different exe under the app (two links up).
        assert_eq!(owner(22, &targets, &procs), Some(2));
        // A child with its own dock item is that item, not the launcher.
        assert_eq!(owner(31, &targets, &procs), Some(4));
        // Explorer starts everything: an orphan player is nobody's.
        assert_eq!(owner(40, &targets, &procs), None);
        // ... but Explorer's own sessions are Explorer's.
        assert_eq!(owner(1, &targets, &procs), Some(0));
        // Unknown process, no match.
        assert_eq!(owner(99, &targets, &procs), None);
        assert_eq!(owner(0, &targets, &procs), None);
    }

    #[test]
    fn matching_by_exe_alone_and_by_aumid() {
        let spotify = r"c:\users\me\appdata\roaming\spotify\spotify.exe";
        let mut procs = Procs::new();
        // The app was restarted: its windows' pid is new, the session's
        // process only shares the exe (case differs).
        procs.insert(50, ProcInfo { parent: 7, exe: Some(spotify.to_uppercase()), aumid: None, created: 5 });
        let targets = vec![t("spotify", &[60], spotify)];
        assert_eq!(owner(50, &targets, &procs), Some(0));
        // A packaged app: its windows belong to ApplicationFrameHost, the
        // session's process is known by its AUMID.
        let aumid = "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic";
        procs.insert(70, ProcInfo { parent: 8, exe: Some(r"c:\program files\windowsapps\zune\music.exe".into()), aumid: Some(aumid.into()), created: 9 });
        let packaged = vec![Target { key: "music".into(), pids: vec![71], exe: None, aumid: Some(aumid.to_lowercase()) }];
        assert_eq!(owner(70, &packaged, &procs), Some(0));
    }

    #[test]
    fn stale_parents_and_loops_are_not_followed() {
        let app = r"c:\app\app.exe";
        let helper = r"c:\other\helper.exe";
        let mut procs = Procs::new();
        // The helper's recorded parent id now belongs to a newer process
        // (the real parent exited and the id was reused by the app).
        procs.insert(5, p(9, app, 200));
        procs.insert(6, p(5, helper, 100));
        let targets = vec![t("app", &[5], app)];
        assert_eq!(owner(6, &targets, &procs), None);
        // Unknown creation times prove nothing: not followed.
        procs.insert(6, p(5, helper, 0));
        assert_eq!(owner(6, &targets, &procs), None);
        procs.insert(6, p(5, helper, 300));
        assert_eq!(owner(6, &targets, &procs), Some(0));
        // A parent loop ends (a process cannot be older than itself's child).
        let mut procs = Procs::new();
        procs.insert(1, p(2, helper, 5));
        procs.insert(2, p(1, helper, 4));
        assert_eq!(owner(1, &targets, &procs), None);
        // Too deep.
        let mut procs = Procs::new();
        for i in 100..110u32 {
            procs.insert(i, p(i + 1, helper, 1000 - i as u64));
        }
        procs.insert(110, p(0, app, 1));
        assert_eq!(owner(100, &[t("app", &[110], app)], &procs), None);
        assert_eq!(owner(106, &[t("app", &[110], app)], &procs), Some(0));
    }

    #[test]
    fn window_owners_beat_exe_paths() {
        // Two dock items run the same host exe (e.g. two Electron apps on
        // one runtime). B's audio child has that exe too, but its parent is
        // B: it is B's, not the first item with the exe.
        let host = r"c:\runtime\host.exe";
        let mut procs = Procs::new();
        procs.insert(100, p(1, host, 10));
        procs.insert(200, p(1, host, 20));
        procs.insert(201, p(200, host, 30));
        procs.insert(300, p(999, host, 40)); // parent unknown
        let targets = vec![t("a", &[100], host), t("b", &[200], host)];
        assert_eq!(owner(201, &targets, &procs), Some(1));
        assert_eq!(owner(100, &targets, &procs), Some(0));
        // Exe alone and two items have it: nobody's (never the first one).
        assert_eq!(owner(300, &targets, &procs), None);
        // Exe alone, one item has it: last resort.
        assert_eq!(owner(300, &targets[..1], &procs), Some(0));
        // A packaged app no dock item runs is nobody's, even with that exe.
        procs.insert(400, ProcInfo { parent: 999, exe: Some(host.into()), aumid: Some("Other_x!App".into()), created: 50 });
        assert_eq!(owner(400, &targets[..1], &procs), None);
    }

    #[test]
    fn chains_are_collected_once() {
        let parents: HashMap<u32, u32> = [(3, 2), (2, 1), (1, 0), (5, 2), (7, 7)].into_iter().collect();
        assert_eq!(chain_pids(&[3, 5, 7], &parents), vec![3, 2, 1, 5, 7]);
        assert!(chain_pids(&[], &parents).is_empty());
    }

    #[test]
    fn answers_replace_older_ones_for_the_same_app() {
        let a = Reply::Level { key: "a".into(), percent: 1, muted: false };
        let b = Reply::Silent { key: "a".into() };
        let c = Reply::Level { key: "c".into(), percent: 1, muted: false };
        assert!(same_app(&a, &b));
        assert!(!same_app(&a, &c));
        assert!(!same_app(&Reply::Playing(vec![]), &Reply::Playing(vec![])));
    }

    #[test]
    fn media_sessions_find_their_audio_sessions() {
        let chrome = Some(r"c:\program files\google\chrome\application\chrome.exe");
        let edge = Some(r"c:\program files (x86)\microsoft\edge\application\msedge.exe");
        let firefox = Some(r"c:\program files\mozilla firefox\firefox.exe");
        let spotify = Some(r"c:\users\me\appdata\roaming\spotify\spotify.exe");
        // Browsers' app ids against their audio processes.
        assert!(media_match("Chrome", chrome, None));
        assert!(media_match("chrome", chrome, None));
        assert!(media_match("Chrome._crx_abcdef", chrome, None), "an installed web app runs in Chrome");
        assert!(media_match("MSEdge", edge, None));
        assert!(!media_match("MSEdge", chrome, None));
        assert!(!media_match("Chrome", edge, None));
        assert!(media_match("308046B0AF4A39CB", firefox, None));
        assert!(!media_match("308046B0AF4A39CB", chrome, None));
        // An exe name, or a path to it.
        assert!(media_match("Spotify.exe", spotify, None));
        assert!(media_match(r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe", spotify, None));
        assert!(!media_match("Spotify.exe", chrome, None));
        // Packaged: only by AUMID (never by an exe of the same name).
        let pkg = "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify";
        assert!(media_match(pkg, spotify, Some("spotifyab.spotifymusic_zpdnekdrzrea0!spotify")));
        assert!(!media_match(pkg, spotify, None));
        assert!(!media_match(pkg, spotify, Some("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic")));
        // Nothing to go by.
        assert!(!media_match("", chrome, None));
        assert!(!media_match("Chrome", None, None));
        assert!(!media_match(".exe", chrome, None));
        assert!(!media_match("Chrom", chrome, None), "whole names only");
    }

    #[test]
    fn browsers_and_paths_are_matched_exactly() {
        let brave = Some(r"c:\program files\bravesoftware\brave-browser\application\brave.exe");
        let player_a = Some(r"c:\players\a\player.exe");
        let player_b = Some(r"d:\players\b\player.exe");
        // Brave's ids (all channels) play in brave.exe, never in an exe
        // named after the part before the dot.
        for id in ["Brave", "BraveSoftware.Brave", "BraveSoftware.Brave.Beta", "BraveSoftware.Brave.Nightly", "BraveSoftware.BraveBeta"] {
            assert!(media_match(id, brave, None), "{id}");
        }
        assert!(!media_match("BraveSoftware.Brave", Some(r"c:\x\bravesoftware.exe"), None));
        assert!(media_match("ChromeBeta", Some(r"c:\program files\google\chrome beta\application\chrome.exe"), None));
        assert!(media_match("MSEdge._crx_abc", Some(r"c:\program files (x86)\microsoft\edge\application\msedge.exe"), None));
        assert!(!media_match("Opera._crx_abc", brave, None));
        // An id nobody knows with a dot in it matches nothing.
        assert!(!media_match("Vendor.Player", Some(r"c:\vendor\vendor.exe"), None));
        // A plain name: its exe.
        assert!(media_match("player", player_a, None));
        // A path: that copy only.
        assert!(media_match(r"C:\Players\A\player.exe", player_a, None));
        assert!(media_match("c:/players/a/PLAYER.EXE", player_a, None), "slashes and case as Windows reads them");
        assert!(!media_match(r"C:\Players\A\player.exe", player_b, None), "another copy is another app");
        assert!(!media_match(r"C:\Players\A\player.exe", None, None));
    }

    fn req(app: &str, set: Option<u8>, seq: u64) -> MediaReq {
        MediaReq { app: app.into(), set, seq, to: (1, 2) }
    }

    #[test]
    fn volume_requests_never_drop_a_write() {
        let mut q = Vec::new();
        queue_media(&mut q, req("A", Some(80), 1));
        // A read (the card opened again) keeps the write.
        queue_media(&mut q, req("A", None, 2));
        assert_eq!(q, [req("A", Some(80), 1), req("A", None, 2)]);
        // A newer write for the same app replaces the older one.
        queue_media(&mut q, req("a", Some(85), 3));
        assert_eq!(q, [req("A", None, 2), req("a", Some(85), 3)]);
        // Another app's write is kept too; a newer read replaces a read.
        queue_media(&mut q, req("B", Some(10), 4));
        queue_media(&mut q, req("A", None, 5));
        assert_eq!(q, [req("a", Some(85), 3), req("B", Some(10), 4), req("A", None, 5)]);
        // Writes run first: a read answers with what was written.
        let order: Vec<u64> = media_order(vec![req("A", None, 5), req("A", Some(1), 6), req("B", Some(2), 7)]).iter().map(|r| r.seq).collect();
        assert_eq!(order, [6, 7, 5]);
        // Full: the oldest read goes, never a write.
        let mut q: Vec<MediaReq> = (0..MAX_MEDIA_REQS as u64 - 1).map(|i| req(&format!("w{i}"), Some(1), i)).collect();
        queue_media(&mut q, req("r", None, 99));
        assert_eq!(q.len(), MAX_MEDIA_REQS);
        queue_media(&mut q, req("r2", None, 100));
        assert_eq!(q.len(), MAX_MEDIA_REQS);
        assert!(!q.iter().any(|r| r.seq == 99), "the older read went");
        assert_eq!(q.iter().filter(|r| r.set.is_some()).count(), MAX_MEDIA_REQS - 1, "every write is kept");
        queue_media(&mut q, req("w9", Some(1), 101));
        assert_eq!(q.iter().filter(|r| r.set.is_some()).count(), MAX_MEDIA_REQS, "a write over a full queue pushes a read out");
    }
}
