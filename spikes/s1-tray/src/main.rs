//! S1 spike: can a normal user-mode process receive Shell_NotifyIcon traffic
//! by owning a window of class `Shell_TrayWnd`, while forwarding everything to
//! explorer so the real taskbar keeps working?
//!
//! Usage: `s1-tray` then type commands on stdin:
//!   f            which HWND does FindWindow("Shell_TrayWnd") return?
//!   l            list cached icons
//!   b            broadcast TaskbarCreated (collect existing icons)
//!   c <i> l|r|d  left / right / double click cached icon i
//!   q            quit
//! Log: stdout and %TEMP%\s1-tray.log

mod fake;
mod proto;

use std::ffi::c_void;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;

use proto::*;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_CMD: u32 = WM_APP + 1;
const NIN_SELECT: u32 = WM_USER;

static OURS: AtomicIsize = AtomicIsize::new(0);
static REAL: AtomicIsize = AtomicIsize::new(0);
static EXPLORER_PID: AtomicU32 = AtomicU32::new(0);
static ICONS: Mutex<Vec<TrayMsg>> = Mutex::new(Vec::new());
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WM_TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static HOOKS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
const TIMER_REASSERT: usize = 7;
static REASSERT_STEP: AtomicU32 = AtomicU32::new(0);
/// Delays (ms) of the re-assert attempts after an explorer restart.
const REASSERT_DELAYS: [u32; 5] = [500, 1500, 3000, 5000, 10000];

fn hwnd(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

pub fn log(line: &str) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let line = format!("[{}.{:03}] {}", t.as_secs() % 100_000, t.subsec_millis(), line);
    println!("{line}");
    let path = std::env::temp_dir().join("s1-tray.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn pid_of(h: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// explorer's own Shell_TrayWnd: the top-level window of that class that is
/// not ours.
fn find_real_tray() -> Option<HWND> {
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut isize);
        if class_of(h) == "Shell_TrayWnd" && pid_of(h) != GetCurrentProcessId() {
            *out = h.0 as isize;
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut found: isize = 0;
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut found as *mut isize as isize));
    }
    (found != 0).then(|| hwnd(found))
}

fn refresh_real() {
    if let Some(h) = find_real_tray() {
        let old = REAL.swap(h.0 as isize, Ordering::SeqCst);
        EXPLORER_PID.store(pid_of(h), Ordering::SeqCst);
        if old != h.0 as isize {
            log(&format!("real explorer tray = {:#x} (pid {})", h.0 as isize, pid_of(h)));
        }
    } else {
        log("WARN: explorer Shell_TrayWnd not found");
    }
}

fn assert_top() {
    let ours = hwnd(OURS.load(Ordering::SeqCst));
    unsafe {
        let _ = SetWindowPos(ours, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

fn apply(m: &TrayMsg) {
    let mut icons = ICONS.lock().unwrap();
    let pos = icons.iter().position(|i| i.hwnd == m.hwnd && i.uid == m.uid);
    match (m.message, pos) {
        (NIM_ADD, None) => icons.push(m.clone()),
        (NIM_ADD, Some(i)) => icons[i] = m.clone(),
        (NIM_MODIFY, Some(i)) => {
            let c = &mut icons[i];
            if m.flags & NIF_MESSAGE != 0 {
                c.callback_msg = m.callback_msg;
            }
            if m.flags & NIF_ICON != 0 {
                c.hicon = m.hicon;
            }
            if m.flags & NIF_TIP != 0 {
                c.tip = m.tip.clone();
            }
        }
        (NIM_DELETE, Some(i)) => {
            icons.remove(i);
        }
        (NIM_SETVERSION, Some(i)) => icons[i].version = m.version,
        _ => {}
    }
}

/// (Re)attach the explorer-only WinEvent hooks to the current explorer pid.
unsafe fn hook_explorer() {
    let mut hooks = HOOKS.lock().unwrap();
    for h in hooks.drain(..) {
        let _ = UnhookWinEvent(HWINEVENTHOOK(h as *mut c_void));
    }
    let pid = EXPLORER_PID.load(Ordering::SeqCst);
    for (a, b) in [(EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW), (EVENT_OBJECT_REORDER, EVENT_OBJECT_REORDER)] {
        let h = SetWinEventHook(a, b, None, Some(win_event), pid, 0, WINEVENT_OUTOFCONTEXT);
        hooks.push(h.0 as isize);
    }
    log(&format!("hooked explorer pid {pid}"));
}

fn findwindow_is_ours() -> bool {
    let found = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.map(|h| h.0 as isize).unwrap_or(0);
    found == OURS.load(Ordering::SeqCst)
}

fn nim_name(m: u32) -> &'static str {
    match m {
        NIM_ADD => "ADD",
        NIM_MODIFY => "MODIFY",
        NIM_DELETE => "DELETE",
        NIM_SETFOCUS => "SETFOCUS",
        NIM_SETVERSION => "SETVERSION",
        _ => "?",
    }
}

unsafe fn on_copydata(wp: WPARAM, lp: LPARAM) -> LRESULT {
    let cds = &*(lp.0 as *const COPYDATASTRUCT);
    let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
    if let Some(m) = parse_copydata(cds.dwData, bytes) {
        log(&format!(
            "NIM_{} hwnd={:#x} pid={} uid={} flags={:#x} cb={:#x} hicon={:#x} ver={} tip={:?}",
            nim_name(m.message),
            m.hwnd,
            pid_of(hwnd(m.hwnd as isize)),
            m.uid,
            m.flags,
            m.callback_msg,
            m.hicon,
            m.version,
            m.tip
        ));
        apply(&m);
    } else {
        log(&format!("COPYDATA dwData={} cb={} (forward only)", cds.dwData, cds.cbData));
    }
    // Forward everything (tray, AppBar dwData=0, icon-rect dwData=3) and
    // hand explorer's answer back to the caller.
    let real = hwnd(REAL.load(Ordering::SeqCst));
    let mut result = 0usize;
    let ok = SendMessageTimeoutW(real, WM_COPYDATA, wp, lp, SMTO_ABORTIFHUNG | SMTO_NORMAL, 3000, Some(&mut result));
    if ok.0 == 0 {
        log("WARN: forward to explorer failed/timed out");
    }
    LRESULT(result as isize)
}

unsafe fn click(index: usize, kind: &str) {
    let Some(icon) = ICONS.lock().unwrap().get(index).cloned() else {
        log(&format!("no icon #{index}"));
        return;
    };
    let owner = hwnd(icon.hwnd as isize);
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let send = |ev: u32| {
        let (w, l) = pack_callback(icon.version, icon.uid, ev, pt.x, pt.y);
        let _ = PostMessageW(Some(owner), icon.callback_msg, WPARAM(w), LPARAM(l));
    };
    let _ = AllowSetForegroundWindow(pid_of(owner));
    match kind {
        "r" => {
            let _ = SetForegroundWindow(owner);
            send(WM_RBUTTONDOWN);
            send(WM_RBUTTONUP);
            if icon.version >= 4 {
                send(WM_CONTEXTMENU);
            }
            let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        }
        "d" => {
            send(WM_LBUTTONDOWN);
            send(WM_LBUTTONUP);
            send(WM_LBUTTONDBLCLK);
            send(WM_LBUTTONUP);
        }
        _ => {
            send(WM_LBUTTONDOWN);
            send(WM_LBUTTONUP);
            if icon.version >= 4 {
                send(NIN_SELECT);
            }
        }
    }
    log(&format!("clicked #{index} ({kind}) tip={:?} ver={}", icon.tip, icon.version));
}

unsafe fn run_command(line: &str) {
    let parts: Vec<&str> = line.split_whitespace().collect();
    match parts.as_slice() {
        ["f"] => {
            let found = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).map(|h| h.0 as isize).unwrap_or(0);
            let ours = OURS.load(Ordering::SeqCst);
            log(&format!(
                "FindWindow -> {:#x} ({}) ours={:#x} real={:#x}",
                found,
                if found == ours { "OURS" } else { "explorer" },
                ours,
                REAL.load(Ordering::SeqCst)
            ));
        }
        ["l"] => {
            let icons = ICONS.lock().unwrap();
            log(&format!("{} icons cached", icons.len()));
            for (i, m) in icons.iter().enumerate() {
                log(&format!("  #{i} pid={} uid={} ver={} tip={:?}", pid_of(hwnd(m.hwnd as isize)), m.uid, m.version, m.tip));
            }
        }
        ["b"] => {
            let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
            let _ = PostMessageW(Some(HWND_BROADCAST), msg, WPARAM(0), LPARAM(0));
            log("broadcast TaskbarCreated");
        }
        ["c", i, k] => {
            if let Ok(i) = i.parse() {
                click(i, k);
            }
        }
        ["ct", prefix, k] => {
            let idx = ICONS.lock().unwrap().iter().position(|m| m.tip.starts_with(prefix));
            match idx {
                Some(i) => click(i, k),
                None => log(&format!("no icon with tip starting {prefix:?}")),
            }
        }
        ["q"] => PostQuitMessage(0),
        _ => log("commands: f | l | b | c <i> l|r|d | ct <tip-prefix> l|r|d | q"),
    }
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => on_copydata(wp, lp),
        WM_CMD => {
            let lines: Vec<String> = std::mem::take(&mut *PENDING.lock().unwrap());
            for l in lines {
                run_command(&l);
            }
            LRESULT(0)
        }
        m if m != 0 && m == WM_TASKBAR_CREATED.load(Ordering::SeqCst) => {
            // explorer restarted: new real tray + pid; take Z-order back now
            // and again on a short schedule while explorer finishes init.
            log("TaskbarCreated received (explorer restarted?)");
            refresh_real();
            hook_explorer();
            assert_top();
            REASSERT_STEP.store(0, Ordering::SeqCst);
            SetTimer(Some(h), TIMER_REASSERT, REASSERT_DELAYS[0], None);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_REASSERT => {
            let step = REASSERT_STEP.fetch_add(1, Ordering::SeqCst) as usize;
            let before = findwindow_is_ours();
            refresh_real();
            assert_top();
            log(&format!("reassert #{step}: before={} after={}", before, findwindow_is_ours()));
            match REASSERT_DELAYS.get(step + 1) {
                Some(&d) => {
                    SetTimer(Some(h), TIMER_REASSERT, d, None);
                }
                None => {
                    let _ = KillTimer(Some(h), TIMER_REASSERT);
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

/// explorer (re)created or reordered its taskbar: refresh the real handle and
/// take the top Z-order back.
unsafe extern "system" fn win_event(_: HWINEVENTHOOK, event: u32, h: HWND, id_object: i32, _: i32, _: u32, _: u32) {
    if id_object != OBJID_WINDOW.0 || h.0.is_null() || class_of(h) != "Shell_TrayWnd" {
        return;
    }
    if h.0 as isize == OURS.load(Ordering::SeqCst) {
        return;
    }
    refresh_real();
    assert_top();
    log(&format!("explorer tray event {:#x} on {:#x} -> ours first: {}", event, h.0 as isize, findwindow_is_ours()));
}

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("fake") {
        let secs = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);
        return fake::run(secs);
    }
    unsafe {
        WM_TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::SeqCst);
        refresh_real();
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("Shell_TrayWnd"),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            log(&format!("RegisterClassExW failed: {:?}", windows::core::Error::from_thread()));
            return Ok(());
        }
        let ours = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("Shell_TrayWnd"),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        OURS.store(ours.0 as isize, Ordering::SeqCst);
        log(&format!("spy Shell_TrayWnd created: {:#x}", ours.0 as isize));
        assert_top();

        // Only explorer's events; out of context so nothing is injected.
        hook_explorer();

        run_command("f");

        let target = ours.0 as isize;
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                PENDING.lock().unwrap().push(line);
                let _ = PostMessageW(Some(hwnd(target)), WM_CMD, WPARAM(0), LPARAM(0));
            }
        });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        log("exit");
    }
    Ok(())
}
