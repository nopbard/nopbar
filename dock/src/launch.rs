//! Opening apps, files and tasks for the dock, Launchpad, search, stacks
//! and jump lists, and one shared notice when that fails: what could not
//! be opened, why (in words), and what to do next (try again, show where
//! it is, choose an app, unpin a broken pin).
//!
//! Windows' own error boxes are turned off (SEE_MASK_FLAG_NO_UI) so there
//! is exactly one message. The notice is our popup menu (keyboard and
//! screen reader ready), shown from a message posted to a small window of
//! this thread: callers may be inside their own borrows, or closing a
//! panel, when the failure is known.

use std::cell::RefCell;
use std::collections::VecDeque;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::i18n::{t, tf};
use crate::popup::Align;
pub mod environment;

/// Who asked to open it (what the notice may offer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A pinned dock icon: (its name, its pin path).
    DockPin(String, String),
    /// Any other dock icon, a dock menu row.
    Dock,
    Launchpad,
    Search,
    Stack,
    JumpList,
    TopBar,
    /// The settings window (a Windows settings page it links to).
    Settings,
}

/// Something to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub file: String,
    /// "open", "runas", ...
    pub verb: String,
    pub args: String,
    pub dir: String,
    /// What the user knows it as (an app's name, a file's name).
    pub name: String,
    pub source: Source,
    /// File activation must target this package, including after Retry.
    pub(super) package_id: Option<String>,
}

impl Target {
    pub fn new(file: &str, name: &str, source: Source) -> Self {
        // A browser's web app without a shortcut (its AppsFolder path):
        // its shortcut found since, else its browser with its arguments,
        // as the program and its parameters (webapps.rs).
        let (file, args) = match crate::webapps::launch_for(file) {
            Some(l) => (l.exe, l.args),
            None => (file.to_string(), String::new()),
        };
        Target { file, verb: "open".into(), args, dir: String::new(), name: name.into(), source, package_id: None }
    }
    /// Target::new for a pinned dock item of app `aumid`: a web app's
    /// shortcut gone since the dock last read it (renamed, deleted while
    /// the app was closed) opens the app instead (webapps::pin_launch).
    /// A failure is still told for the pin (`source`, with Unpin).
    pub fn for_pin(file: &str, name: &str, source: Source, aumid: Option<&str>) -> Self {
        match aumid.and_then(|a| crate::webapps::pin_launch(file, a)) {
            Some(l) => {
                crate::log!("launch: {file:?} is gone; its web app opens as {:?}", l.exe);
                Target { file: l.exe, verb: "open".into(), args: l.args, dir: String::new(), name: name.into(), source, package_id: None }
            }
            None => Target::new(file, name, source),
        }
    }
    pub fn packaged_file(appid: String, file: String) -> Self {
        Self { file, package_id: Some(appid), verb: "open".into(), args: String::new(),
            dir: String::new(), name: String::new(), source: Source::JumpList }
    }
    pub fn verb(mut self, v: &str) -> Self {
        self.verb = v.into();
        self
    }
    pub fn args(mut self, a: &str, dir: &str) -> Self {
        self.args = a.into();
        self.dir = dir.into();
        self
    }
}

/// Why it did not open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The file, shortcut target or folder is not there (moved, deleted,
    /// uninstalled).
    Missing,
    /// Not allowed (permissions, policy, blocked).
    Denied,
    /// No app is set to open this kind of file.
    NoApp,
    /// Not a program this PC can run (wrong architecture, damaged).
    NotRunnable,
    /// On a drive or network place that is not reachable now.
    Offline,
    /// Anything else: Windows' error code.
    Other(u32),
}

/// Pure: a Win32 error from ShellExecuteEx; None = not a failure to tell
/// (the user said no to the administrator prompt, or there was no error).
pub fn classify(code: u32) -> Option<Reason> {
    Some(match code {
        0 => return None,
        // ERROR_CANCELLED: the UAC prompt was declined.
        1223 => return None,
        2 | 3 | 15 | 123 | 161 | 1392 => Reason::Missing,
        5 | 32 | 1260 | 4551 | 225 => Reason::Denied,
        // ERROR_NO_ASSOCIATION. (31 is ERROR_GEN_FAILURE here, a Win32
        // code, not ShellExecute's SE_ERR_NOASSOC: it stays Other.)
        1155 => Reason::NoApp,
        11 | 191 | 193 | 216 | 1150 | 1151 => Reason::NotRunnable,
        51 | 53 | 59 | 64 | 67 | 1222 | 1231 | 1311 => Reason::Offline,
        c => Reason::Other(c),
    })
}

impl Reason {
    /// The sentence that explains it.
    pub fn text(self) -> String {
        match self {
            Reason::Missing => t("launch.reason.missing").into(),
            Reason::Denied => t("launch.reason.denied").into(),
            Reason::NoApp => t("launch.reason.no_app").into(),
            Reason::NotRunnable => t("launch.reason.not_runnable").into(),
            Reason::Offline => t("launch.reason.offline").into(),
            Reason::Other(c) => tf("launch.reason.other", &[("code", &c.to_string())]),
        }
    }
}

/// What the notice offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Retry,
    /// Show this file (selected) or folder in File Explorer.
    Reveal(String),
    OpenFolder(String),
    /// "Open with…" for a file with no app.
    ChooseApp,
    /// Take the broken pin off the dock: (name, path).
    Unpin(String, String),
    /// Open a Windows page or program (Startup apps, Task Manager): (its
    /// row's text, what to open).
    Open(String, String),
}

/// Something on disk to show for `file`: the file itself if it is there,
/// else the nearest folder above it that is (never a drive root alone
/// when nothing below it exists... a root is still a place to look).
/// `exists` tells whether a path is there. Shell names and URLs: None.
pub fn location(file: &str, exists: &dyn Fn(&str) -> bool) -> Option<Action> {
    let f = file.trim().trim_matches('"');
    let fs_path = f.len() > 2 && (f.as_bytes()[1] == b':' || f.starts_with("\\\\"));
    if !fs_path {
        return None;
    }
    if exists(f) {
        return Some(Action::Reveal(f.to_string()));
    }
    let mut p = std::path::Path::new(f);
    while let Some(parent) = p.parent() {
        let s = parent.to_string_lossy();
        if s.is_empty() {
            break;
        }
        if exists(&s) {
            return Some(Action::OpenFolder(s.into_owned()));
        }
        p = parent;
    }
    None
}

/// Pure: the rows the notice offers, in order.
pub fn actions(reason: Reason, target: &Target, exists: &dyn Fn(&str) -> bool) -> Vec<Action> {
    let mut v = vec![Action::Retry];
    if reason == Reason::NoApp {
        v.push(Action::ChooseApp);
    }
    // A file with arguments (a jump list task) is the program, not a
    // document: its folder is still where to look.
    if let Some(a) = location(&target.file, exists) {
        v.push(a);
    }
    // A pin that cannot open: Unpin. A web app's pin with nothing to start
    // it by (no shortcut, no command kept: its AppsFolder path, which the
    // shell may not know) is one whatever Windows says.
    let web_path = crate::webapps::apps_folder_aumid(&target.file).is_some_and(crate::webapps::is_web_app);
    if let Source::DockPin(name, path) = &target.source {
        if reason == Reason::Missing || (web_path && reason != Reason::Denied) {
            v.push(Action::Unpin(name.clone(), path.clone()));
        }
    }
    v
}

unsafe fn shell_execute(verb: &str, file: &str, args: &str, dir: &str) -> Result<(), u32> {
    environment::launch(verb, file, args, dir)
}

/// Invoked before normal startup; never initializes NopBar or reads its data.
pub fn helper(args: &[String]) -> i32 {
    if args.len() != 6 { return 87; }
    use std::io::Read;
    let mut gate = [0];
    if std::io::stdin().read_exact(&mut gate).is_err() || gate != *b"1" { return 87; }
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = shell_execute_here(&args[2], &args[3], &args[4], &args[5]);
        if com { CoUninitialize(); }
        result.err().unwrap_or(0) as i32
    }
}

/// A shortcut's Start in, otherwise the target executable's folder. Shell
/// targets and document associations keep the shell's own directory rules.
unsafe fn working_directory(file: &str, dir: &str) -> String {
    if !dir.is_empty() { return dir.into(); }
    use windows::core::Interface;
    use windows::Win32::{System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ}, UI::Shell::{IShellLinkW, ShellLink}};
    let mut exe = file.to_string();
    if file.to_ascii_lowercase().ends_with(".lnk") {
        if let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) {
            if let Ok(pf) = link.cast::<IPersistFile>() {
                if pf.Load(&HSTRING::from(file), STGM_READ).is_ok() {
                    let mut buf = [0u16; 32768];
                    if link.GetWorkingDirectory(&mut buf).is_ok() && buf[0] != 0 {
                        return String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]);
                    }
                    if link.GetPath(&mut buf, std::ptr::null_mut(), 0).is_ok() {
                        exe = String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]);
                    }
                }
            }
        }
    }
    if exe.to_ascii_lowercase().ends_with(".exe") {
        if !std::path::Path::new(&exe).is_absolute() {
            let mut path = [0u16; 32768];
            let n = windows::Win32::Storage::FileSystem::SearchPathW(None, &HSTRING::from(&exe), None, Some(&mut path), None);
            if n > 0 && (n as usize) < path.len() { exe = String::from_utf16_lossy(&path[..n as usize]); }
        }
        std::path::Path::new(&exe).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    } else { String::new() }
}

unsafe fn shell_execute_here(verb: &str, file: &str, args: &str, dir: &str) -> Result<(), u32> {
    let dir = working_directory(file, dir);
    let (v, f) = (HSTRING::from(verb), HSTRING::from(file));
    let (a, d) = (HSTRING::from(args), HSTRING::from(&dir));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI | windows::Win32::UI::Shell::SEE_MASK_NOASYNC | windows::Win32::UI::Shell::SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(v.as_ptr()),
        lpFile: PCWSTR(f.as_ptr()),
        lpParameters: if args.is_empty() { PCWSTR::null() } else { PCWSTR(a.as_ptr()) },
        lpDirectory: if dir.is_empty() { PCWSTR::null() } else { PCWSTR(d.as_ptr()) },
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    match ShellExecuteExW(&mut info) {
        Ok(()) => {
            if !info.hProcess.is_invalid() {
                use windows::Win32::System::Threading::GetProcessId;
                let pid = GetProcessId(info.hProcess);
                if pid != 0 { let _ = AllowSetForegroundWindow(pid); }
                let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
            }
            Ok(())
        },
        Err(e) => {
            let last = GetLastError().0;
            // The HRESULT carries the Win32 code when GetLastError lost it.
            let code = if last != 0 { last } else { (e.code().0 as u32) & 0xFFFF };
            Err(code)
        }
    }
}

/// Opens `t` now on a helper thread, so a slow shell or network target
/// cannot hold up the caller (Launchpad animating away). Call on the UI
/// thread: a failure is told by it, like open()'s. With no helper thread
/// to be had it is not opened here either: the notice says so.
pub fn open_soon(t: Target) { let _ = open_async(t, None); }
fn open_async(t: Target, at: Option<(POINT, Align)>) -> bool {
    prepare();
    soon(
        t,
        spawn_shell_job,
        move |job| {
            if let Err(code) = open_here(&job) {
                if let Some(reason) = classify(code) { later(notice(job, reason, at)); }
            }
        },
        |t, reason| report(t, reason, at),
    )
}

fn spawn_shell_job(job: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new().name("launch".into()).spawn(move || with_shell_com(job)).map(drop)
}

/// open_soon's steps: `open` runs only inside what `spawn` started; if
/// that fails, `fail` is told why (never a shell call on this thread).
fn soon(
    t: Target,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    open: impl FnOnce(Target) + Send + 'static,
    fail: impl FnOnce(Target, Reason),
) -> bool {
    let job = t.clone();
    if let Err(e) = spawn(Box::new(move || open(job))) {
        // ERROR_NOT_ENOUGH_MEMORY when the OS gave no code.
        let code = e.raw_os_error().map_or(8, |c| c as u32);
        crate::log!("launch: no thread to open {} ({e})", t.file);
        fail(t, classify(code).unwrap_or(Reason::Other(code)));
        false
    } else { true }
}

/// On a helper thread (call prepare() on the UI thread first): COM set up
/// for ShellExecute around `f`.
pub fn with_shell_com<R>(f: impl FnOnce() -> R) -> R {
    let com = unsafe { CoInitializeEx(None, COINIT(COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0)).is_ok() };
    let r = f();
    if com {
        unsafe { CoUninitialize() };
    }
    r
}

/// Opens `t` on this (helper) thread, inside with_shell_com. Err(code) for
/// a failure worth telling (classify); a declined admin prompt is Ok. The
/// caller decides what follows (another candidate, report_later).
pub fn open_here(t: &Target) -> Result<(), u32> {
    let result = unsafe {
        match &t.package_id {
            Some(id) => crate::jumplist::open_packaged_file(id, &t.file)
                .map_err(|e| e.code().0 as u32 & 0xffff),
            None => shell_execute(&t.verb, &t.file, &t.args, &t.dir),
        }
    };
    match result {
        Ok(()) => Ok(()),
        Err(code) => {
            crate::log!("launch: could not open {} ({} {}): error {code}", t.file, t.verb, t.args);
            if classify(code).is_some() {
                Err(code)
            } else {
                Ok(())
            }
        }
    }
}

/// From any thread: the shared notice for `t` failing with `code` (shown
/// by the thread that called prepare()).
pub fn report_later(t: Target, code: u32) {
    if let Some(reason) = classify(code) {
        later(notice(t, reason, None));
    }
}

/// Queues a launch on a worker; true means accepted, errors are posted to UI.
pub fn open(t: &Target) -> bool { open_async(t.clone(), None) }
/// Asynchronous launch with failure anchored next to the original dock icon.
pub fn open_at(t: &Target, at: POINT, align: Align) -> bool { open_async(t.clone(), Some((at, align))) }
/// Shows `path` selected in Explorer without blocking the UI.
pub fn reveal(path: &str) {
    open_soon(Target::new("explorer.exe", "Explorer", Source::Dock).args(&format!("/select,\"{path}\""), ""));
}

// ---- the notice ------------------------------------------------------------------

/// One notice: two lines that say what and why, then what can be done.
struct Notice {
    title: String,
    detail: String,
    actions: Vec<Action>,
    /// What Action::Retry opens again.
    retry: Option<Target>,
    at: Option<(POINT, Align)>,
}

thread_local! {
    static QUEUE: RefCell<VecDeque<Notice>> = const { RefCell::new(VecDeque::new()) };
    static HOST: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

const WM_NOTICE: u32 = WM_APP + 1;
const RETRY_TIMER: usize = 1;

/// Queues a notice (shown soon, from this thread's message loop).
pub fn report(target: Target, reason: Reason, at: Option<(POINT, Align)>) {
    queue(notice(target, reason, at));
}

/// The notice for `target` failing to open.
fn notice(target: Target, reason: Reason, at: Option<(POINT, Align)>) -> Notice {
    let exists = |p: &str| std::path::Path::new(p).exists();
    let name = if target.name.is_empty() { file_name(&target.file) } else { target.name.clone() };
    Notice { title: tf("launch.title", &[("name", &name)]), detail: reason.text(), actions: actions(reason, &target, &exists), retry: Some(target), at }
}

/// A notice about something other than opening (a setting that did not
/// take, a command that failed): `title`, `detail`, then `actions`.
pub fn tell(title: String, detail: String, actions: Vec<Action>, at: Option<(POINT, Align)>) {
    queue(Notice { title, detail, actions, retry: None, at });
}

/// The UI thread's notice window (prepare()), for tell_later().
static UI_HOST: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// Notices from other threads, taken by the UI thread's window.
static LATER: std::sync::Mutex<Vec<Notice>> = std::sync::Mutex::new(Vec::new());

/// Called on the UI thread before work goes to a helper thread that may
/// tell_later(): its notices are shown by this thread.
pub fn prepare() {
    let h = unsafe { host() };
    UI_HOST.store(h.0 as isize, std::sync::atomic::Ordering::SeqCst);
}

/// tell(), from any thread (shown by the thread that called prepare()).
pub fn tell_later(title: String, detail: String, actions: Vec<Action>, at: Option<(POINT, Align)>) {
    later(Notice { title, detail, actions, retry: None, at });
}

/// A notice from another thread, for the thread that called prepare().
fn later(n: Notice) {
    if let Ok(mut l) = LATER.lock() {
        l.push(n);
    }
    let h = UI_HOST.load(std::sync::atomic::Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_NOTICE, WPARAM(0), LPARAM(0));
        }
    }
}

fn queue(n: Notice) {
    queue_only(n);
    unsafe {
        let h = host();
        if !h.is_invalid() {
            let _ = PostMessageW(Some(h), WM_NOTICE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Into this thread's queue (shown on the next WM_NOTICE).
fn queue_only(n: Notice) {
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        // The same thing failing twice in a row is said once.
        if !q.iter().any(|f| f.title == n.title && f.detail == n.detail) {
            q.push_back(n);
        }
    });
}

/// This thread's message-only window (made on first use).
unsafe fn host() -> HWND {
    let h = HOST.get();
    if h != 0 {
        return HWND(h as *mut _);
    }
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarLaunchNotice"),
        ..Default::default()
    };
    // Fails harmlessly once registered.
    RegisterClassExW(&wc);
    let h = CreateWindowExW(WINDOW_EX_STYLE(0), w!("NopBarLaunchNotice"), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None)
        .unwrap_or_default();
    HOST.set(h.0 as isize);
    h
}

/// Something modal of ours is up (a menu, a panel), or the shared
/// rendering device is lost and not replaced yet: the notice waits.
fn busy() -> bool {
    crate::popup::is_tracking()
        || crate::search::is_open()
        || crate::launchpad::is_open()
        || crate::gfx::shared().is_some_and(|g| g.is_lost())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NOTICE | WM_TIMER => {
            // Notices from helper threads join this thread's queue.
            let later: Vec<Notice> = LATER.lock().map(|mut l| std::mem::take(&mut *l)).unwrap_or_default();
            for n in later {
                queue_only(n);
            }
            if msg == WM_TIMER {
                let _ = KillTimer(Some(hwnd), RETRY_TIMER);
            }
            if busy() {
                SetTimer(Some(hwnd), RETRY_TIMER, 300, None);
                return LRESULT(0);
            }
            if let Some(f) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
                show(f);
                if QUEUE.with(|q| !q.borrow().is_empty()) {
                    let _ = PostMessageW(Some(hwnd), WM_NOTICE, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The label of an action row.
fn action_label(a: &Action) -> String {
    match a {
        Action::Retry => t("launch.action.retry").into(),
        Action::Reveal(_) | Action::OpenFolder(_) => t("launch.action.location").into(),
        Action::ChooseApp => t("launch.action.choose_app").into(),
        Action::Unpin(..) => t("dock.menu.unpin").into(),
        Action::Open(label, _) => label.clone(),
    }
}

/// The notice's rows (design system 11.4): the title, the reason as one
/// paragraph under it (they wrap to the notice's width: long names, long
/// translations; neither can be picked), a separator, then what can be
/// done, a row each (ids from 1, in `actions`' order). No color, nothing
/// shakes.
fn notice_rows(f: &Notice) -> Vec<crate::popup::Item> {
    use crate::popup::Item;
    let mut items = vec![Item::title(f.title.clone()), Item::note(f.detail.clone()), Item::sep()];
    for (k, a) in f.actions.iter().enumerate() {
        items.push(Item::new(k as u32 + 1, action_label(a)));
    }
    items
}

/// The notice: what failed and why, then what can be done. Esc or a click
/// elsewhere closes it (the menu's own close: 120 ms).
fn show(f: Notice) {
    use crate::popup::{Outcome, Request};
    let acts = f.actions.clone();
    let items = notice_rows(&f);
    // The dock's device when this is the dock's thread (lost with it, and
    // closed by its recovery); a device of its own only elsewhere.
    let gfx = match crate::gfx::shared() {
        Some(g) => g,
        None => match crate::gfx::Gfx::new() {
            Ok(g) => std::rc::Rc::new(g),
            Err(_) => {
                crate::log!("launch: no graphics for the notice");
                return;
            }
        },
    };
    let cfg = crate::config::Config::load();
    let sys = crate::syslook::current();
    let theme = cfg.load_theme(&crate::config::app_dir()).resolve(&sys);
    let (at, align) = f.at.unwrap_or_else(|| {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        (p, Align::TopLeft)
    });
    let scale = unsafe {
        let h = windows::Win32::Graphics::Gdi::MonitorFromPoint(at, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST);
        crate::monitors::info(h.0 as isize).map(|m| m.scale()).unwrap_or(1.0)
    };
    // Off a dock icon (menu_anchor, the popup gap in it) it keeps to the
    // room past the anchor; at the pointer it may slide.
    let hangs = f.at.is_some();
    let out = crate::popup::track(Request { gfx: &gfx, theme: &theme, items, at, align, scale, titles: Default::default(), on_slide: None, drag_ids: 0..0, on_space: None, on_pick: None, hangs });
    let Outcome::Picked(id) = out else { return };
    let Some(a) = acts.get((id as usize).wrapping_sub(1)) else { return };
    match a {
        Action::Retry => {
            if let Some(target) = &f.retry {
                let _ = match f.at {
                    Some((p, al)) => open_at(target, p, al),
                    None => open(target),
                };
            }
        }
        Action::Reveal(p) => reveal(p),
        Action::OpenFolder(p) => {
            open_soon(Target::new("explorer.exe", "Explorer", Source::Dock).args(&format!("\"{p}\""), ""));
        }
        Action::ChooseApp => {
            if let Some(target) = &f.retry {
                open_soon(Target::new(&target.file, &target.name, target.source.clone()).verb("openas"));
            }
        }
        Action::Open(_, what) => {
            open_soon(Target::new(what, "", Source::Dock));
        }
        Action::Unpin(name, path) => crate::dock::set_pinned(name, path, false),
    }
}

fn file_name(path: &str) -> String {
    let p = path.trim_end_matches(['\\', '/']);
    p.rsplit(['\\', '/']).next().unwrap_or(p).to_string()
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "launch_harmony.rs"]
mod harmony;

#[cfg(test)]
mod tests {
    #[test]
    fn shortcut_start_folder_wins_and_empty_start_uses_target_parent() {
        use windows::{core::{HSTRING, Interface}, Win32::{System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED}, UI::Shell::{IShellLinkW, ShellLink}}};
        unsafe {
            let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let path = std::env::temp_dir().join(format!("nopbar-native-link-{}.lnk",std::process::id()));
            let link: IShellLinkW = CoCreateInstance(&ShellLink,None,CLSCTX_INPROC_SERVER).unwrap();
            let exe = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("cmd.exe");
            link.SetPath(&HSTRING::from(exe.as_os_str())).unwrap();
            let explicit = std::env::temp_dir();
            let file: IPersistFile = link.cast().unwrap();
            link.SetWorkingDirectory(&HSTRING::from(explicit.as_os_str())).unwrap();
            file.Save(&HSTRING::from(path.as_os_str()),true).unwrap();
            assert_eq!(std::path::PathBuf::from(super::working_directory(&path.to_string_lossy(),"")),explicit);
            link.SetWorkingDirectory(&HSTRING::new()).unwrap();
            file.Save(&HSTRING::from(path.as_os_str()),true).unwrap();
            assert!(super::working_directory(&path.to_string_lossy(),"").eq_ignore_ascii_case(&exe.parent().unwrap().to_string_lossy()));
            std::fs::remove_file(&path).unwrap();
            drop(file); drop(link);
            if initialized { CoUninitialize(); }
        }
    }
    use super::*;
    #[test]
    fn working_folder_uses_explicit_start_or_executable_parent() {
        unsafe {
            assert_eq!(working_directory(r"C:\Apps\Tool\tool.exe", ""), r"C:\Apps\Tool");
            assert_eq!(working_directory(r"C:\Apps\Tool\tool.exe", r"C:\Work"), r"C:\Work");
            assert_eq!(working_directory("ms-settings:", ""), "");
            assert!(std::path::Path::new(&working_directory("cmd.exe", "")).is_absolute());
        }
    }

    #[test]
    fn errors_are_sorted_into_reasons() {
        assert_eq!(classify(0), None);
        assert_eq!(classify(1223), None, "a declined admin prompt is not a failure");
        assert_eq!(classify(2), Some(Reason::Missing));
        assert_eq!(classify(3), Some(Reason::Missing));
        assert_eq!(classify(5), Some(Reason::Denied));
        assert_eq!(classify(1155), Some(Reason::NoApp));
        assert_eq!(classify(31), Some(Reason::Other(31)), "a general failure, not a missing association");
        assert_eq!(classify(193), Some(Reason::NotRunnable));
        assert_eq!(classify(53), Some(Reason::Offline));
        assert_eq!(classify(1231), Some(Reason::Offline));
        assert_eq!(classify(87), Some(Reason::Other(87)));
    }

    #[test]
    fn location_is_the_nearest_thing_there() {
        let there = |p: &str| p == r"C:\Tools" || p == r"C:\Tools\a.exe" || p == r"C:\";
        assert_eq!(location(r"C:\Tools\a.exe", &there), Some(Action::Reveal(r"C:\Tools\a.exe".into())));
        assert_eq!(location(r"C:\Tools\gone\b.exe", &there), Some(Action::OpenFolder(r"C:\Tools".into())));
        assert_eq!(location("\"C:\\Tools\\a.exe\"", &there), Some(Action::Reveal(r"C:\Tools\a.exe".into())));
        assert_eq!(location(r"shell:AppsFolder\Microsoft.App", &there), None);
        assert_eq!(location("https://example.com", &there), None);
        assert_eq!(location(r"D:\x\y.txt", &|_| false), None);
    }

    #[test]
    fn actions_fit_the_failure() {
        let none = |_: &str| false;
        let pin = Target::new(r"C:\gone\a.lnk", "A", Source::DockPin("A".into(), r"C:\gone\a.lnk".into()));
        assert_eq!(actions(Reason::Missing, &pin, &none), vec![Action::Retry, Action::Unpin("A".into(), r"C:\gone\a.lnk".into())]);
        // Only a missing pin is offered for removal.
        assert_eq!(actions(Reason::Denied, &pin, &none), vec![Action::Retry]);
        let doc = Target::new(r"C:\d\x.abc", "x.abc", Source::Search);
        let there = |p: &str| p == r"C:\d\x.abc";
        assert_eq!(actions(Reason::NoApp, &doc, &there), vec![Action::Retry, Action::ChooseApp, Action::Reveal(r"C:\d\x.abc".into())]);
        let app = Target::new(r"shell:AppsFolder\X", "X", Source::Launchpad);
        assert_eq!(actions(Reason::Missing, &app, &none), vec![Action::Retry]);
    }

    #[test]
    fn a_web_app_pin_with_nothing_to_start_it_offers_unpin() {
        let none = |_: &str| false;
        // No shortcut, no command kept: its AppsFolder path as it is, which
        // the shell may not know (whatever error it gives).
        let path = crate::webapps::apps_folder_path("Chrome._crx_nothingknownxyz");
        let pin = Target::for_pin(&path, "App", Source::DockPin("App".into(), path.clone()), Some("Chrome._crx_nothingknownxyz"));
        assert_eq!(pin.file, path);
        let unpin = Action::Unpin("App".into(), path.clone());
        for r in [Reason::Missing, Reason::Other(1168)] {
            assert_eq!(actions(r, &pin, &none), vec![Action::Retry, unpin.clone()], "{r:?}");
        }
        assert_eq!(actions(Reason::NoApp, &pin, &none).last(), Some(&unpin));
        assert_eq!(actions(Reason::Denied, &pin, &none), vec![Action::Retry]);
        // A packaged app's path: as before.
        let pkg = Target::new(r"shell:AppsFolder\Vendor.App_x!App", "X", Source::DockPin("X".into(), r"shell:AppsFolder\Vendor.App_x!App".into()));
        assert_eq!(actions(Reason::Other(1168), &pkg, &none), vec![Action::Retry]);
    }

    /// No helper thread to be had: the target is not opened on the
    /// caller's (UI) thread; the failure notice is asked for instead.
    #[test]
    fn open_soon_without_a_thread_never_opens_here() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let opened = Arc::new(AtomicBool::new(false));
        let o = opened.clone();
        let mut failed = None;
        let t = Target::new(r"C:\Windows\System32\Taskmgr.exe", "Task Manager", Source::TopBar);
        soon(
            t.clone(),
            |_job| Err(std::io::Error::from_raw_os_error(8)),
            move |_| o.store(true, Ordering::SeqCst),
            |t, r| failed = Some((t, r)),
        );
        assert!(!opened.load(Ordering::SeqCst), "the shell is not called on the caller's thread");
        assert_eq!(failed, Some((t.clone(), Reason::Other(8))), "the failure is told");
        // A thread to be had: opened there, nothing told here.
        let (o, mut told) = (opened.clone(), false);
        soon(
            t,
            |job| {
                job();
                Ok(())
            },
            move |_| o.store(true, Ordering::SeqCst),
            |_, _| told = true,
        );
        assert!(opened.load(Ordering::SeqCst));
        assert!(!told);
    }

    #[test]
    fn packaged_jump_file_uses_the_launch_worker_and_sta() {
        use windows::Win32::System::Com::{CoGetApartmentType, APTTYPE_MAINSTA, APTTYPE_STA};
        let target = Target::packaged_file("TestPackage!App".into(), r"C:\test file.txt".into());
        let ui = std::thread::current().id();
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(soon(target.clone(), spawn_shell_job, move |job| {
            let (mut apt, mut qualifier) = Default::default();
            unsafe { CoGetApartmentType(&mut apt, &mut qualifier).unwrap(); }
            tx.send((std::thread::current().id(), apt, job)).unwrap();
        }, |_, _| panic!("launch worker failed")));
        let (worker, apt, job) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_ne!(ui, worker);
        assert!(matches!(apt, APTTYPE_STA | APTTYPE_MAINSTA));
        assert_eq!(job.package_id.as_deref(), Some("TestPackage!App"));
        assert_eq!(job, target);
    }

    #[test]
    fn names_from_paths() {
        assert_eq!(file_name(r"C:\a\b.txt"), "b.txt");
        assert_eq!(file_name(r"C:\a\folder\"), "folder");
        assert_eq!(file_name("x"), "x");
    }

    #[test]
    fn strings_exist() {
        for k in ["launch.title", "launch.reason.missing", "launch.reason.denied", "launch.reason.no_app", "launch.reason.not_runnable", "launch.reason.offline", "launch.reason.other", "launch.action.retry", "launch.action.location", "launch.action.choose_app"] {
            assert_ne!(t(k), k, "{k}");
        }
    }
}
