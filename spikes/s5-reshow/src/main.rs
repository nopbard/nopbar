//! S5 spike: which actions make explorer show the (hidden) taskbar again?
//!
//! Watch-only: polls every Shell_TrayWnd / Shell_SecondaryTrayWnd every 25 ms
//! and logs any change of visibility or on-screen rect. Lines typed on stdin
//! are written into the log as markers ("# win-key"), so each change can be
//! matched to the action right before it. Polling is fine for a spike; the
//! product uses WinEvent hooks (spec §5.1).
//! Log: stdout and %TEMP%\s5-reshow.log

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetSystemMetrics, GetWindowRect, IsWindowVisible, SM_CXSCREEN, SM_CYSCREEN,
};

fn log(line: &str) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let line = format!("[{}.{:03}] {}", t.as_secs() % 100_000, t.subsec_millis(), line);
    println!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("s5-reshow.log")) {
        let _ = writeln!(f, "{line}");
    }
}

#[derive(PartialEq, Clone, Copy)]
struct State {
    visible: bool,
    rect: (i32, i32, i32, i32),
    on_screen_px: i32,
}

fn trays() -> Vec<(String, HWND)> {
    let mut out = Vec::new();
    unsafe {
        for class in [w!("Shell_TrayWnd"), w!("Shell_SecondaryTrayWnd")] {
            let mut after: Option<HWND> = None;
            while let Ok(h) = FindWindowExW(None, after, class, PCWSTR::null()) {
                out.push((String::from_utf16_lossy(class.as_wide()), h));
                after = Some(h);
            }
        }
    }
    out
}

fn state(h: HWND, sw: i32, sh: i32) -> State {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    // How many pixel rows of the bar are inside the primary screen.
    let rows = (r.bottom.min(sh) - r.top.max(0)).max(0) * ((r.right.min(sw) - r.left.max(0)) > 0) as i32;
    State { visible: unsafe { IsWindowVisible(h).as_bool() }, rect: (r.left, r.top, r.right, r.bottom), on_screen_px: rows }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let mut last: HashMap<isize, State> = HashMap::new();
    log(&format!("watching taskbars, screen {sw}x{sh}"));
    loop {
        while let Ok(m) = rx.try_recv() {
            if m.trim() == "q" {
                log("exit");
                return;
            }
            log(&format!("# {m}"));
        }
        for (class, h) in trays() {
            let s = state(h, sw, sh);
            let key = h.0 as isize;
            if last.get(&key) != Some(&s) {
                log(&format!(
                    "{class} {:#x} visible={} rect={:?} rows_on_screen={}",
                    key, s.visible, s.rect, s.on_screen_px
                ));
                last.insert(key, s);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
