//! `s1-tray fake <secs>`: a stand-in tray app in its own process. It adds a
//! version-4 icon, changes its tooltip every 10 s, asks the shell where the
//! icon is (proves explorer really has it after forwarding), logs every
//! callback it receives, and re-adds on TaskbarCreated.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::log;

const UID: u32 = 42;
const WM_TRAY: u32 = WM_APP + 5;
const TIMER: usize = 1;

static mut TICKS: u32 = 0;
static mut TASKBAR_CREATED: u32 = 0;
static mut END_TICKS: u32 = 0;

fn data(h: HWND, tip: &str) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: h,
        uID: UID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() },
        ..Default::default()
    };
    for (i, u) in tip.encode_utf16().take(127).enumerate() {
        nid.szTip[i] = u;
    }
    nid
}

unsafe fn add(h: HWND) {
    let mut nid = data(h, "S1 fake 0");
    let ok_add = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
    nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    let ok_ver = Shell_NotifyIconW(NIM_SETVERSION, &nid).as_bool();
    log(&format!("FAKE add={ok_add} setversion={ok_ver}"));
    where_is(h);
}

unsafe fn where_is(h: HWND) {
    let id = NOTIFYICONIDENTIFIER {
        cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: h,
        uID: UID,
        ..Default::default()
    };
    match Shell_NotifyIconGetRect(&id) {
        Ok(r) => log(&format!("FAKE rect=({},{},{},{})", r.left, r.top, r.right, r.bottom)),
        Err(e) => log(&format!("FAKE rect error {e:?}")),
    }
    let _: RECT;
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let event = (lp.0 & 0xFFFF) as u32;
            let uid = ((lp.0 >> 16) & 0xFFFF) as u32;
            let x = (wp.0 & 0xFFFF) as u16 as i16;
            let y = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
            log(&format!("FAKE callback event={event:#x} uid={uid} at ({x},{y})"));
            LRESULT(0)
        }
        WM_TIMER => {
            TICKS += 1;
            let ticks = TICKS;
            let nid = data(h, &format!("S1 fake {ticks}"));
            let ok = Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool();
            log(&format!("FAKE modify #{ticks} ok={ok}"));
            where_is(h);
            if TICKS >= END_TICKS {
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
                log("FAKE deleted, exiting");
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        m if m == TASKBAR_CREATED && m != 0 => {
            log("FAKE TaskbarCreated -> re-add");
            add(h);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

pub fn run(secs: u32) -> windows::core::Result<()> {
    unsafe {
        END_TICKS = (secs / 10).max(1);
        TASKBAR_CREATED = RegisterWindowMessageW(w!("TaskbarCreated"));
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("S1FakeTrayApp"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let h = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("S1FakeTrayApp"),
            PCWSTR::null(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        add(h);
        SetTimer(Some(h), TIMER, 10_000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
