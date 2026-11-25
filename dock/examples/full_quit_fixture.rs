//! Isolated Restart Manager test target. Never used by the product.
//! Hidden top-level window; stdin EOF and a 20s timer both end it normally.
#![windows_subsystem = "windows"]
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

static VETO: AtomicBool = AtomicBool::new(false);
const STOP: u32 = WM_APP + 1;

fn report(text: &str) {
    println!("{text}");
    let _ = std::io::stdout().flush();
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => {
            report("QUERY");
            LRESULT(if VETO.load(Ordering::Relaxed) { 0 } else { 1 })
        }
        WM_ENDSESSION => {
            report(if w.0 != 0 { "END" } else { "CANCEL" });
            if w.0 != 0 {
                let _ = DestroyWindow(h);
            }
            LRESULT(0)
        }
        WM_TIMER | STOP => {
            let _ = DestroyWindow(h);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, w, l),
    }
}

fn main() -> windows::core::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    assert!(matches!(mode.as_str(), "accept" | "veto"), "test mode required");
    VETO.store(mode == "veto", Ordering::Relaxed);
    unsafe {
        let module = GetModuleHandleW(None)?;
        let class = w!("NopBarFullQuitOwnedFixture");
        let wc = WNDCLASSW {
            hInstance: module.into(),
            lpszClassName: class,
            lpfnWndProc: Some(proc),
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&wc), 0);
        let h = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("Full quit test fixture"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            160,
            80,
            None,
            None,
            Some(module.into()),
            None,
        )?;
        assert_ne!(SetTimer(Some(h), 1, 20_000, None), 0);
        let raw = h.0 as isize;
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
            // This is the fixture's own shutdown, not injected input.
            let _ = PostMessageW(Some(HWND(raw as *mut _)), STOP, WPARAM(0), LPARAM(0));
        });
        report("READY");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
