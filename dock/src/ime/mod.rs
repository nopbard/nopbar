//! Input modes: the top bar's input indicator (가 / A, あ / A, 中 / 英, or
//! the layout's short name) and the per-app input memory (an app left in
//! Hangul comes back in Hangul).
//!
//! Only documented IMM techniques, no key injection: the focused window's
//! default IME window answers WM_IME_CONTROL (IMC_GET/SETCONVERSIONMODE,
//! IMC_GET/SETOPENSTATUS), and a layout switch is a
//! WM_INPUTLANGCHANGEREQUEST to the focused window. Every cross-process
//! call runs on one short-lived worker thread (crate::idle) with
//! SendMessageTimeout, so a hung app never holds the UI thread.
//!
//! Costs nothing unless used: with the indicator not on show and neither
//! the memory nor a rule on, foreground changes queue no work and no IME
//! WinEvent hook is set. The indicator is refreshed on foreground changes,
//! the IME WinEvents, and a cheap poll only while the top bar shows it.
//!
//! Safety: a change is made only to the exact input target that was read
//! (same top-level window in front, same focused window, thread and
//! layout), checked again right before each message; settings changes and
//! newer foreground changes cancel pending work (generations). Windows
//! above our rights (UIPI) and windows not identified yet are never
//! changed; a fullscreen game or presentation is never touched; a change
//! the user makes while a restore waits, or right after it, is their
//! choice (no fighting); nothing about what is typed is read or logged,
//! only the mode.

pub mod logic;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, GetKeyboardLayout, GetKeyboardLayoutList, HKL, VK_CAPITAL};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{InputCfg, InputRule};
use crate::idle::{IdleQueue, Jobs};
use logic::{AppIdent, Before, ImeState, Restored, Script, Shown};

/// Posted to the top bar (see `attach`) when what the cell shows changed.
pub const WM_IME_STATUS: u32 = WM_APP + 46;

/// WM_IME_CONTROL's commands (imm.h).
const IMC_GETCONVERSIONMODE: usize = 0x0001;
const IMC_SETCONVERSIONMODE: usize = 0x0002;
const IMC_GETOPENSTATUS: usize = 0x0005;
const IMC_SETOPENSTATUS: usize = 0x0006;
/// How long an IME window gets to answer (it is the app's UI thread).
const ASK_MS: u32 = 150;
/// The restore waits this long after an activation: the app settles its
/// focus (and its own IME state) first.
const RESTORE_DELAY: Duration = Duration::from_millis(80);
/// The worker thread ends this long after its last job (it stays while
/// the indicator polls).
const WORKER_IDLE: Duration = Duration::from_secs(5);
/// Most apps whose mode is kept (this session only); past it the memory
/// starts over.
const MAX_REMEMBERED: usize = 512;
/// Apps left between two foreground jobs whose mode is still noted.
const MAX_LEAVING: usize = 8;

/// The foreground app as the engine knows it.
#[derive(Clone, Debug, Default)]
struct Fg {
    hwnd: isize,
    /// Who it is and whether it is locked were read (on the worker). Until
    /// then nothing is changed in it.
    resolved: bool,
    ident: Option<AppIdent>,
    locked: bool,
    /// A restore is due: what is seen meanwhile is not recorded yet.
    pending: bool,
    restored: Option<Restored>,
    /// The user switched back right after our restore: hands off.
    overridden: bool,
    /// The native mode last seen in it (None: not an IME layout / unknown).
    last: Option<bool>,
}

struct Engine {
    cfg: InputCfg,
    /// Bumped on every settings change: pending applies check it.
    cfg_gen: u64,
    /// The top bar listening (0 = none).
    bar: isize,
    /// The top bar shows the indicator right now.
    visible: bool,
    remembered: HashMap<String, bool>,
    fg: Fg,
    /// Bumped on every foreground change.
    gen: u64,
    /// The newest foreground change not taken by the worker yet, and the
    /// apps left since it last ran (coalesced: the latest always wins,
    /// none is dropped by a full queue).
    next_fg: Option<(u64, isize)>,
    leaving: Vec<Fg>,
    /// A click on the indicator: (generation, window) it was made for.
    toggle_req: Option<(u64, isize)>,
    /// The layouts menu's target: (top-level window, focused window).
    menu_target: Option<(isize, isize)>,
    shown: Shown,
    caps: bool,
    /// Installed keyboard layouts (HKL), read on the UI thread.
    layouts: Vec<isize>,
}

fn engine() -> MutexGuard<'static, Engine> {
    static E: OnceLock<Mutex<Engine>> = OnceLock::new();
    E.get_or_init(|| {
        Mutex::new(Engine {
            cfg: InputCfg::default(),
            cfg_gen: 0,
            bar: 0,
            visible: false,
            remembered: HashMap::new(),
            fg: Fg::default(),
            gen: 0,
            next_fg: None,
            leaving: Vec::new(),
            toggle_req: None,
            menu_target: None,
            shown: Shown::default(),
            caps: false,
            layouts: Vec::new(),
        })
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Work for the worker. Each kind is queued at most once at a time; what
/// it acts on is taken from the engine when it runs (latest wins).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Job {
    Foreground,
    Poll,
    Toggle,
}

static QUEUE: OnceLock<IdleQueue<Job>> = OnceLock::new();
static FG_QUEUED: AtomicBool = AtomicBool::new(false);
static POLL_QUEUED: AtomicBool = AtomicBool::new(false);
static TOGGLE_QUEUED: AtomicBool = AtomicBool::new(false);
static VISIBLE: AtomicBool = AtomicBool::new(false);
static HOOK: AtomicIsize = AtomicIsize::new(0);

fn flag(job: Job) -> &'static AtomicBool {
    match job {
        Job::Foreground => &FG_QUEUED,
        Job::Poll => &POLL_QUEUED,
        Job::Toggle => &TOGGLE_QUEUED,
    }
}

/// Queues `job` unless it is queued already. Three kinds at most are ever
/// waiting, so the queue never refuses one for being full; a worker that
/// cannot start leaves the flag clear (the next call tries again; what it
/// acts on stays in the engine).
fn send(job: Job) {
    if flag(job).swap(true, Ordering::SeqCst) {
        return;
    }
    let q = QUEUE.get_or_init(|| {
        IdleQueue::new("ime", WORKER_IDLE, 0, |jobs: &mut Jobs<Job>| {
            while let Some(j) = jobs.next() {
                unsafe { run(j) };
            }
        })
    });
    if q.send(job).is_err() {
        flag(job).store(false, Ordering::SeqCst);
    }
}

// ---- UI thread ------------------------------------------------------------------

/// Settings changed (and at startup). Pending applies made under the old
/// settings are cancelled (cfg_gen) unless the new ones want the same.
pub fn apply_config(cfg: &InputCfg) {
    let (bar, active) = {
        let mut e = engine();
        if e.cfg == *cfg {
            return;
        }
        if e.cfg.remember && !cfg.remember {
            // Switched off: nothing kept (Remember rules learn again).
            e.remembered.clear();
        }
        e.cfg = cfg.clone();
        e.cfg_gen += 1;
        (e.bar, active(&e))
    };
    refresh_layouts();
    post_bar(bar);
    if active {
        // Identifies the app in front if that was skipped while idle.
        poll();
    }
}

/// The top bar `bar` hears about changes (the indicator's visibility is
/// told by `set_visible`). A new bar takes over.
pub fn attach(bar: HWND) {
    engine().bar = bar.0 as isize;
    refresh_layouts();
    // The bar decides on show or not (and so set_visible) when it hears.
    post_bar(bar.0 as isize);
}

/// The top bar is gone: no listener, nothing shown.
pub fn detach() {
    engine().bar = 0;
    set_visible(false);
}

/// Whether the top bar shows the indicator now (the cell is wanted, the
/// bar is not hidden for a fullscreen app). The IME WinEvent hook exists
/// only while it does; becoming visible reads the mode once.
pub fn set_visible(on: bool) {
    {
        let mut e = engine();
        if e.visible == on {
            return;
        }
        e.visible = on;
    }
    VISIBLE.store(on, Ordering::SeqCst);
    if on {
        if HOOK.load(Ordering::SeqCst) == 0 {
            let h = unsafe {
                SetWinEventHook(
                    EVENT_OBJECT_IME_SHOW,
                    EVENT_OBJECT_IME_CHANGE,
                    None,
                    Some(ime_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            HOOK.store(h.0 as isize, Ordering::SeqCst);
        }
        poll();
    } else {
        let h = HOOK.swap(0, Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = UnhookWinEvent(HWINEVENTHOOK(h as *mut _));
            }
        }
    }
}

/// IME windows come and go while composing: at most one poll per this
/// long from them (the bar's own poll covers the rest).
const EVENT_GAP_MS: u64 = 250;
static LAST_EVENT_POLL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

unsafe extern "system" fn ime_event(_hook: HWINEVENTHOOK, _event: u32, _hwnd: HWND, _obj: i32, _child: i32, _thread: u32, _time: u32) {
    if !VISIBLE.load(Ordering::SeqCst) {
        return;
    }
    let now = now_ms() + EVENT_GAP_MS;
    if now.saturating_sub(LAST_EVENT_POLL.load(Ordering::SeqCst)) >= EVENT_GAP_MS {
        LAST_EVENT_POLL.store(now, Ordering::SeqCst);
        poll();
    }
}

fn post_bar(bar: isize) {
    if bar != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(bar as *mut _)), WM_IME_STATUS, WPARAM(0), LPARAM(0));
        }
    }
}

/// Anything to do at all: the indicator is on show, or the memory or a
/// rule may act. False costs nothing (no jobs, no queries).
fn active(e: &Engine) -> bool {
    e.visible || e.cfg.remember || !e.cfg.rules.is_empty()
}

/// The app in front changed to top-level window `hwnd` (never one of ours:
/// the dock filters those). Repeats for the same window do nothing.
pub fn foreground(hwnd: HWND) {
    let go = {
        let mut e = engine();
        let h = hwnd.0 as isize;
        if h == 0 || e.fg.hwnd == h {
            return;
        }
        e.gen += 1;
        let gen = e.gen;
        let prev = std::mem::replace(&mut e.fg, Fg { hwnd: h, ..Default::default() });
        e.menu_target = None;
        if !active(&e) {
            false
        } else {
            if prev.hwnd != 0 {
                if e.leaving.len() >= MAX_LEAVING {
                    e.leaving.remove(0);
                }
                e.leaving.push(prev);
            }
            e.next_fg = Some((gen, h));
            true
        }
    };
    refresh_layouts();
    if go {
        send(Job::Foreground);
    }
}

/// Reads the mode again (coalesced; the top bar's poll and IME events).
pub fn poll() {
    if active(&engine()) {
        send(Job::Poll);
    }
}

/// The window in front has an IME that answered and that we may change
/// (else a click on the indicator opens its menu).
pub fn can_toggle() -> bool {
    let e = engine();
    e.shown.native().is_some() && !e.shown.locked && e.fg.resolved && !e.fg.locked
}

/// The indicator was clicked: native <-> Latin in the window in front
/// now (the click's target: a later foreground change cancels it).
pub fn toggle() {
    {
        let mut e = engine();
        if e.fg.hwnd == 0 {
            return;
        }
        e.toggle_req = Some((e.gen, e.fg.hwnd));
    }
    send(Job::Toggle);
}

/// Caps Lock (read on the UI thread, which pumps input). True if it
/// changed.
pub fn read_caps() -> bool {
    let on = unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0;
    let mut e = engine();
    let changed = e.caps != on;
    e.caps = on;
    changed
}

/// What the indicator shows: (what the window types with, Caps Lock).
pub fn shown() -> (Shown, bool) {
    let e = engine();
    (e.shown.clone(), e.caps)
}

/// The cell is wanted: "always", or "auto" with an IME (Korean, Japanese,
/// Chinese) or more than one layout installed.
pub fn indicator_wanted() -> bool {
    let e = engine();
    match e.cfg.indicator_mode() {
        1 => true,
        2 => false,
        _ => e.layouts.len() > 1 || e.layouts.iter().any(|&h| logic::script_of(h as usize as u16) != Script::Other),
    }
}

fn refresh_layouts() {
    let mut buf = [HKL::default(); 32];
    let n = unsafe { GetKeyboardLayoutList(Some(&mut buf)) }.max(0) as usize;
    let list: Vec<isize> = buf[..n.min(buf.len())].iter().map(|h| h.0 as isize).collect();
    engine().layouts = list;
}

/// The installed layouts for the indicator's menu: (HKL, name, in use by
/// the focused window in front), and whether that window can be changed.
/// The focused window is kept as the menu's target (`switch_layout`).
pub fn layouts() -> (Vec<(isize, String, bool)>, bool) {
    refresh_layouts();
    let (list, fg, can) = {
        let e = engine();
        (e.layouts.clone(), e.fg.hwnd, e.fg.resolved && !e.fg.locked && !e.shown.locked)
    };
    // Non-blocking reads (no message to the app).
    let (focus, current) = unsafe {
        let top = HWND(fg as *mut _);
        let focus = if fg != 0 { focus_of(top) } else { HWND::default() };
        let tid = if fg != 0 { GetWindowThreadProcessId(focus, None) } else { 0 };
        (focus.0 as isize, if tid == 0 { 0 } else { GetKeyboardLayout(tid).0 as isize })
    };
    engine().menu_target = (fg != 0 && focus != 0).then_some((fg, focus));
    let mut out: Vec<(isize, String, bool)> = Vec::new();
    for h in list {
        let mut name = language_name(h as usize as u16);
        // Two layouts of one language (US and Dvorak): told apart.
        if out.iter().any(|(_, n, _)| *n == name) {
            name = format!("{name} ({:04X})", (h as usize >> 16) & 0xFFFF);
        }
        out.push((h, name, h == current));
    }
    (out, can && fg != 0 && focus != 0)
}

/// Switches the menu's target (the window that had the keyboard when the
/// menu opened) to layout `hkl`: its own request (the app may refuse, as
/// with the language bar). Nothing if the app in front changed, or the
/// window is gone, no longer in it, or locked.
pub fn switch_layout(hkl: isize) {
    let target = {
        let mut e = engine();
        let t = e.menu_target.take();
        t.filter(|(top, _)| e.fg.hwnd == *top && e.fg.resolved && !e.fg.locked)
    };
    let Some((top, focus)) = target else { return };
    unsafe {
        let f = HWND(focus as *mut _);
        if !IsWindow(Some(f)).as_bool() || GetAncestor(f, GA_ROOT).0 as isize != top {
            return;
        }
        let _ = PostMessageW(Some(f), WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(hkl));
    }
    poll_soon();
}

/// A poll a moment later (after the app handled a request).
fn poll_soon() {
    let _ = crate::service::after("ime poll", Duration::from_millis(120), poll);
}

// ---- worker ---------------------------------------------------------------------

/// The window that has the keyboard in `fg`'s app: the foreground
/// thread's focus while `fg` is in front (UWP apps type into a window of
/// another thread), else `fg`'s own thread's focus.
unsafe fn focus_of(fg: HWND) -> HWND {
    let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    let front = GetAncestor(GetForegroundWindow(), GA_ROOT) == fg;
    let tid = if front { 0 } else { GetWindowThreadProcessId(fg, None) };
    if (front || tid != 0) && GetGUIThreadInfo(tid, &mut gti).is_ok() && !gti.hwndFocus.is_invalid() {
        return gti.hwndFocus;
    }
    fg
}

/// The exact input target a mode was read from (and may be set on).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Target {
    top: isize,
    focus: isize,
    tid: u32,
    hkl: isize,
}

unsafe fn target_of(top: HWND) -> Option<Target> {
    if top.is_invalid() || !IsWindow(Some(top)).as_bool() {
        return None;
    }
    let focus = focus_of(top);
    let tid = GetWindowThreadProcessId(focus, None);
    if tid == 0 {
        return None;
    }
    Some(Target { top: top.0 as isize, focus: focus.0 as isize, tid, hkl: GetKeyboardLayout(tid).0 as isize })
}

/// `t` is still the input target: its window in front, the same focused
/// window, thread and layout.
unsafe fn still(t: &Target) -> bool {
    let top = HWND(t.top as *mut _);
    GetAncestor(GetForegroundWindow(), GA_ROOT) == top && target_of(top) == Some(*t)
}

/// One WM_IME_CONTROL to IME window `ime` (bounded wait).
unsafe fn ask(ime: HWND, cmd: usize, lp: isize) -> Option<usize> {
    let mut r = 0usize;
    let ok = SendMessageTimeoutW(ime, WM_IME_CONTROL, WPARAM(cmd), LPARAM(lp), SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT, ASK_MS, Some(&mut r));
    (ok.0 != 0).then_some(r)
}

unsafe fn ime_state(target: HWND) -> Option<ImeState> {
    let ime = ImmGetDefaultIMEWnd(target);
    if ime.is_invalid() {
        return None;
    }
    let open = ask(ime, IMC_GETOPENSTATUS, 0)? != 0;
    let conv = ask(ime, IMC_GETCONVERSIONMODE, 0)? as u32;
    Some(ImeState { open, conv })
}

/// The short name of language `lang` ("EN", "KO").
fn short_name(lang: u16) -> String {
    let mut buf = [0u16; 85];
    let n = unsafe { windows::Win32::Globalization::LCIDToLocaleName(lang as u32, Some(&mut buf), 0) };
    let name = if n > 1 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { String::new() };
    logic::short_from_locale(&name)
}

/// Language `lang` as Windows names it ("Korean (Korea)", in its UI language).
pub fn language_name(lang: u16) -> String {
    use windows::Win32::Globalization::{GetLocaleInfoEx, LCIDToLocaleName, LOCALE_SLOCALIZEDDISPLAYNAME};
    let mut buf = [0u16; 85];
    let n = unsafe { LCIDToLocaleName(lang as u32, Some(&mut buf), 0) };
    if n <= 1 {
        return format!("{lang:04X}");
    }
    let mut out = [0u16; 128];
    let m = unsafe { GetLocaleInfoEx(windows::core::PCWSTR(buf.as_ptr()), LOCALE_SLOCALIZEDDISPLAYNAME, Some(&mut out)) };
    if m > 1 {
        String::from_utf16_lossy(&out[..m as usize - 1])
    } else {
        String::from_utf16_lossy(&buf[..n as usize - 1])
    }
}

/// What window `top` types with now, and the target it was read from. A
/// locked window: its layout only.
unsafe fn query(top: HWND, locked: bool) -> (Shown, Option<Target>) {
    let Some(t) = target_of(top) else { return (Shown::default(), None) };
    let lang = (t.hkl as usize & 0xFFFF) as u16;
    let focus = HWND(t.focus as *mut _);
    let ime = if logic::script_of(lang) != Script::Other && !locked { ime_state(focus) } else { None };
    (Shown { lang, short: short_name(lang), ime, locked }, Some(t))
}

/// Puts target `t` in native (`want`) or Latin input, from state `s` read
/// from it. `ok` is asked right before each message (and the target is
/// checked again): anything changed, nothing more is sent. True if all
/// was sent.
unsafe fn set_native(t: &Target, s: &Shown, want: bool, ok: &dyn Fn() -> bool) -> bool {
    let Some(cur) = s.ime else { return false };
    let (open, conv) = logic::plan(s.script(), cur, want);
    if open.is_none() && conv.is_none() {
        return false;
    }
    let ime = ImmGetDefaultIMEWnd(HWND(t.focus as *mut _));
    if ime.is_invalid() {
        return false;
    }
    let ready = || ok() && still(t);
    if let Some(o) = open {
        if !ready() {
            return false;
        }
        let _ = ask(ime, IMC_SETOPENSTATUS, o as isize);
    }
    if let Some(c) = conv {
        if !ready() {
            return false;
        }
        let _ = ask(ime, IMC_SETCONVERSIONMODE, c as isize);
    }
    true
}

/// A fullscreen game, a D3D app or a presentation is up: hands off.
fn fullscreen_busy() -> bool {
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN};
    matches!(unsafe { SHQueryUserNotificationState() }, Ok(s) if s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE)
}

/// Who window `h` is, and whether it runs above our rights.
fn identify(h: HWND) -> (Option<AppIdent>, bool) {
    let ident = crate::apps::window_info(h).map(|w| AppIdent::new(w.aumid.as_deref(), w.exe.as_deref())).filter(|i| i.key().is_some());
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    let locked = pid == 0 || crate::apps::is_locked(pid);
    (ident, locked)
}

fn rule_of(rules: &[InputRule], app: &AppIdent) -> Option<logic::RuleMode> {
    logic::rule_for(rules, app, |r| (r.app.as_str(), r.mode.as_str()))
}

/// What the settings want for `app` now (None: leave it alone).
fn wanted(e: &Engine, app: &AppIdent) -> Option<bool> {
    let rule = rule_of(&e.cfg.rules, app);
    let remembered = app.key().and_then(|k| e.remembered.get(&k).copied());
    logic::decide(rule, e.cfg.remember, remembered)
}

/// Keeps `native` for `app` if remembering applies to it.
fn remember(e: &mut Engine, app: &AppIdent, native: bool) {
    if !logic::records(rule_of(&e.cfg.rules, app), e.cfg.remember) {
        return;
    }
    let Some(key) = app.key() else { return };
    if e.remembered.len() >= MAX_REMEMBERED && !e.remembered.contains_key(&key) {
        e.remembered.clear();
    }
    e.remembered.insert(key, native);
}

/// Shows `s` (tells the bar if it changed).
fn publish(s: Shown) {
    let bar = {
        let mut e = engine();
        if e.shown == s {
            return;
        }
        e.shown = s;
        e.bar
    };
    post_bar(bar);
}

unsafe fn run(job: Job) {
    // Cleared first: what comes in while this runs queues it again.
    flag(job).store(false, Ordering::SeqCst);
    match job {
        Job::Foreground => {
            let (next, leaving) = {
                let mut e = engine();
                (e.next_fg.take(), std::mem::take(&mut e.leaving))
            };
            for p in leaving {
                leave(p);
            }
            if let Some((gen, hwnd)) = next {
                arrive(gen, HWND(hwnd as *mut _));
            }
        }
        Job::Poll => observe_now(),
        Job::Toggle => {
            let req = engine().toggle_req.take();
            if let Some((gen, hwnd)) = req {
                toggle_now(gen, HWND(hwnd as *mut _));
            }
        }
    }
}

/// The app left: its mode is kept (asked once more if its window is still
/// there; else the last one seen).
unsafe fn leave(p: Fg) {
    let Some(app) = p.ident.clone() else { return };
    if p.pending || p.locked || !p.resolved {
        return;
    }
    let h = HWND(p.hwnd as *mut _);
    let native = if IsWindow(Some(h)).as_bool() { query(h, false).0.native().or(p.last) } else { p.last };
    if let Some(n) = native {
        remember(&mut engine(), &app, n);
    }
}

/// Window `hwnd` came to the front (generation `gen`): identified, shown,
/// and put back in the mode the settings want.
unsafe fn arrive(gen: u64, hwnd: HWND) {
    let (ident, locked) = identify(hwnd);
    let (s0, t0) = query(hwnd, locked);
    let plan = {
        let mut e = engine();
        if e.gen != gen {
            return;
        }
        e.fg.ident = ident.clone();
        e.fg.locked = locked;
        e.fg.resolved = true;
        e.fg.last = s0.native();
        // Only an IME that answered can be set; never a locked window.
        let want = ident.as_ref().and_then(|app| wanted(&e, app)).filter(|_| !locked && s0.native().is_some() && t0.is_some());
        e.fg.pending = want.is_some();
        want.map(|w| (w, e.cfg_gen))
    };
    publish(s0.clone());
    let Some((want, cfg_gen)) = plan else { return };
    std::thread::sleep(RESTORE_DELAY);

    let finish = |restored: Option<Restored>, seen: Option<bool>| {
        let mut e = engine();
        if e.gen == gen {
            e.fg.pending = false;
            e.fg.restored = restored;
            if let Some(n) = restored.map(|r| r.native).or(seen) {
                e.fg.last = Some(n);
            }
        }
    };
    // Still this app, under settings that still want this?
    let ok = || {
        let e = engine();
        e.gen == gen && (e.cfg_gen == cfg_gen || ident.as_ref().and_then(|app| wanted(&e, app)) == Some(want))
    };
    if !ok() || GetAncestor(GetForegroundWindow(), GA_ROOT) != hwnd || fullscreen_busy() {
        finish(None, None);
        return;
    }
    let (s1, t1) = query(hwnd, false);
    let Some(t1) = t1 else {
        finish(None, None);
        return;
    };
    let layout_changed = t0.is_some_and(|t| t.hkl != t1.hkl);
    match logic::before_restore(s0.native(), s1.native(), layout_changed, want) {
        Before::Apply => {
            if set_native(&t1, &s1, want, &ok) {
                finish(Some(Restored { at_ms: now_ms(), native: want }), None);
            } else {
                finish(None, s1.native());
            }
            publish(query(hwnd, false).0);
        }
        Before::UserChanged => {
            // The user's choice while we waited: kept, not overwritten.
            finish(None, s1.native());
            if let (Some(n), Some(app)) = (s1.native(), ident.as_ref()) {
                let mut e = engine();
                if e.gen == gen {
                    remember(&mut e, app, n);
                }
            }
            publish(s1);
        }
        Before::AlreadyThere | Before::Unknown => {
            finish(None, s1.native());
            publish(s1);
        }
    }
}

/// A poll: what the window in front types with now; a change the user
/// made is kept (and one right after a restore ends our part in it). An
/// app not identified yet (its foreground job was skipped while idle) is
/// identified here.
unsafe fn observe_now() {
    let (h, gen, resolved) = {
        let e = engine();
        (e.fg.hwnd, e.gen, e.fg.resolved)
    };
    // Before the first foreground change: the window in front now.
    let h = if h == 0 { GetAncestor(GetForegroundWindow(), GA_ROOT) } else { HWND(h as *mut _) };
    if !resolved && engine().fg.hwnd == h.0 as isize {
        let (ident, locked) = identify(h);
        let mut e = engine();
        if e.gen == gen && e.fg.hwnd == h.0 as isize {
            e.fg.ident = ident;
            e.fg.locked = locked;
            e.fg.resolved = true;
        }
    }
    let locked = {
        let e = engine();
        e.fg.locked || (e.fg.hwnd == h.0 as isize && !e.fg.resolved)
    };
    let (s, _) = query(h, locked);
    if let Some(n) = s.native() {
        let mut e = engine();
        if e.gen == gen && e.fg.hwnd == h.0 as isize && e.fg.resolved && !e.fg.pending {
            let app = e.fg.ident.clone();
            let recording = app.as_ref().is_some_and(|a| logic::records(rule_of(&e.cfg.rules, a), e.cfg.remember));
            let o = logic::observe(recording, e.fg.restored, now_ms(), n);
            if o.user_override && !e.fg.overridden {
                e.fg.overridden = true;
                e.fg.restored = None;
                crate::log!("input: switched back right after a restore; kept as the user's choice");
            }
            e.fg.last = Some(n);
            if let (Some(_), Some(app)) = (o.store, app) {
                remember(&mut e, &app, n);
            }
        }
    }
    publish(s);
}

/// The indicator's click for window `hwnd` (generation `gen`): the other
/// mode, made only if that window is still in front, identified and not
/// locked, and the target read is still the target when it is sent. That
/// is the app's mode now.
unsafe fn toggle_now(gen: u64, hwnd: HWND) {
    let ok = || {
        let e = engine();
        e.gen == gen && e.fg.hwnd == hwnd.0 as isize && e.fg.resolved && !e.fg.locked
    };
    if !ok() || GetAncestor(GetForegroundWindow(), GA_ROOT) != hwnd {
        return;
    }
    let (s, t) = query(hwnd, false);
    let (Some(n), Some(t)) = (s.native(), t) else { return };
    if set_native(&t, &s, !n, &ok) {
        let mut e = engine();
        if e.gen == gen {
            e.fg.pending = false;
            e.fg.restored = None;
            e.fg.last = Some(!n);
            if let Some(app) = e.fg.ident.clone() {
                remember(&mut e, &app, !n);
            }
        }
    }
    publish(query(hwnd, false).0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_engine() -> Engine {
        Engine {
            cfg: InputCfg::default(),
            cfg_gen: 0,
            bar: 0x1234,
            visible: false,
            remembered: HashMap::new(),
            fg: Fg::default(),
            gen: 0,
            next_fg: None,
            leaving: Vec::new(),
            toggle_req: None,
            menu_target: None,
            shown: Shown::default(),
            caps: false,
            layouts: Vec::new(),
        }
    }

    #[test]
    fn a_bar_alone_is_no_work() {
        // A top bar exists but the indicator is not on show, memory and
        // rules are off: nothing runs.
        let mut e = idle_engine();
        assert!(!active(&e));
        e.visible = true;
        assert!(active(&e));
        e.visible = false;
        e.cfg.remember = true;
        assert!(active(&e));
        e.cfg.remember = false;
        e.cfg.rules.push(InputRule { app: "name:code.exe".into(), name: "Code".into(), mode: "alpha".into() });
        assert!(active(&e));
    }

    #[test]
    fn settings_decide_again_before_an_apply() {
        let mut e = idle_engine();
        let app = AppIdent::new(None, Some("k.exe"));
        e.cfg.remember = true;
        remember(&mut e, &app, true);
        assert_eq!(wanted(&e, &app), Some(true));
        // Memory switched off (apply_config clears it): nothing wanted.
        e.cfg.remember = false;
        assert_eq!(wanted(&e, &app), None);
        // A rule removed or set to Don't touch: nothing wanted either.
        e.cfg.rules.push(InputRule { app: "exe:k.exe".into(), name: "K".into(), mode: "alpha".into() });
        assert_eq!(wanted(&e, &app), Some(false));
        e.cfg.rules[0].mode = "ignore".into();
        assert_eq!(wanted(&e, &app), None);
    }

    #[test]
    fn each_job_kind_has_its_own_flag() {
        assert!(!std::ptr::eq(flag(Job::Foreground), flag(Job::Poll)));
        assert!(!std::ptr::eq(flag(Job::Poll), flag(Job::Toggle)));
        assert!(!std::ptr::eq(flag(Job::Foreground), flag(Job::Toggle)));
    }
}
