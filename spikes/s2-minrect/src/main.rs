//! S2 spike: does the OS minimize animation go to the rect we return for
//! HSHELL_GETMINRECT while explorer's (hidden) taskbar also listens?
//!
//!   s2-minrect hook <left> <top> <size>   answer GETMINRECT with that rect (physical px)
//!   s2-minrect target                     a plain 900x600 red window to minimize
//!   s2-minrect cycle <n>                  minimize/restore the target n times
//! Log: stdout and %TEMP%\s2-minrect.log

use std::io::Write;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

const HSHELL_GETMINRECT: usize = 5;

#[repr(C)]
struct ShellHookInfo {
    hwnd: HWND,
    rc: RECT,
}

static mut SHELLHOOK: u32 = 0;
static mut TARGET_RC: RECT = RECT { left: 0, top: 0, right: 0, bottom: 0 };
/// Write the rect as 16-bit RECTS (left, top, right, bottom) instead of RECT.
static mut SHORTS: bool = false;

fn log(line: &str) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let line = format!("[{}.{:03}] {}", t.as_secs() % 100_000, t.subsec_millis(), line);
    println!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("s2-minrect.log")) {
        let _ = writeln!(f, "{line}");
    }
}

fn title(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe extern "system" fn hook_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == SHELLHOOK && msg != 0 {
        if wp.0 == HSHELL_GETMINRECT {
            let info = &mut *(lp.0 as *mut ShellHookInfo);
            let old = info.rc;
            let raw = std::slice::from_raw_parts((lp.0 as *const u8).add(8), 16);
            let s16: Vec<i16> = raw.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            log(&format!("  raw as i16: {:?}", s16));
            if SHORTS {
                let p = (lp.0 as *mut u8).add(8) as *mut i16;
                let r = TARGET_RC;
                *p = r.left as i16;
                *p.add(1) = r.top as i16;
                *p.add(2) = r.right as i16;
                *p.add(3) = r.bottom as i16;
            } else {
                info.rc = TARGET_RC;
            }
            log(&format!(
                "GETMINRECT for {:#x} {:?}: ({},{},{},{}) -> ({},{},{},{})",
                info.hwnd.0 as isize,
                title(info.hwnd),
                old.left,
                old.top,
                old.right,
                old.bottom,
                info.rc.left,
                info.rc.top,
                info.rc.right,
                info.rc.bottom
            ));
            return LRESULT(1);
        }
        return LRESULT(0);
    }
    DefWindowProcW(h, msg, wp, lp)
}

unsafe fn make_window(class: PCWSTR, proc_: WNDPROC, title: PCWSTR, style: WINDOW_STYLE, brush: bool) -> windows::core::Result<HWND> {
    let hinst = GetModuleHandleW(None)?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: proc_,
        hInstance: hinst.into(),
        lpszClassName: class,
        hbrBackground: if brush { CreateSolidBrush(COLORREF(0x0000_20E0)) } else { Default::default() },
        ..Default::default()
    };
    RegisterClassExW(&wc);
    CreateWindowExW(WINDOW_EX_STYLE(0), class, title, style, 200, 200, 900, 600, None, None, Some(hinst.into()), None)
}

fn pump() {
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn plain_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        PostQuitMessage(0);
    }
    DefWindowProcW(h, msg, wp, lp)
}

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let num = |i: usize, d: i32| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        match args.get(1).map(String::as_str) {
            Some("hook") => {
                let (l, t, s) = (num(2, 100), num(3, 100), num(4, 64));
                TARGET_RC = RECT { left: l, top: t, right: l + s, bottom: t + s };
                SHORTS = args.iter().any(|a| a == "--shorts");
                SHELLHOOK = RegisterWindowMessageW(w!("SHELLHOOK"));
                // Shell hook needs a real (non message-only) top-level window.
                let h = make_window(w!("S2MinRectHook"), Some(hook_proc), PCWSTR::null(), WS_POPUP, false)?;
                let ok = RegisterShellHookWindow(h).as_bool();
                log(&format!("hook window {:#x}, RegisterShellHookWindow={ok}, target rect ({l},{t}) size {s} shorts={}", h.0 as isize, { SHORTS }));
                pump();
            }
            Some("target") => {
                let h = make_window(w!("S2Target"), Some(plain_proc), w!("S2 target"), WS_OVERLAPPEDWINDOW, true)?;
                let _ = ShowWindow(h, SW_SHOWNORMAL);
                log(&format!("target window {:#x}", h.0 as isize));
                pump();
            }
            Some("cycle") => {
                let n = num(2, 3);
                let h = FindWindowW(w!("S2Target"), PCWSTR::null())?;
                for i in 0..n {
                    let _ = ShowWindow(h, SW_MINIMIZE);
                    log(&format!("cycle {i}: minimized"));
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                    let _ = ShowWindow(h, SW_RESTORE);
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                }
            }
            _ => println!("usage: s2-minrect hook <l> <t> <size> | target | cycle <n>"),
        }
    }
    Ok(())
}
