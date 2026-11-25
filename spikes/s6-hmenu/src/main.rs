//! S6 spike: can we read another process's HMENU and drive it from our own
//! popup?
//!
//!   s6-hmenu target                     test window whose menu changes on every WM_INITMENUPOPUP
//!   s6-hmenu read <class>               dump the menu tree, then again after WM_INITMENUPOPUP
//!   s6-hmenu popup <class> <sub> [run]  show submenu <sub> of that HMENU from OUR window,
//!                                       auto-pick the first enabled item (Down, Enter) and,
//!                                       with `run`, post its WM_COMMAND to the owner
//! Log: stdout and %TEMP%\s6-hmenu.log

use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;

const ID_NEW: u32 = 101;
const ID_EXIT: u32 = 102;
const ID_UNDO: u32 = 201;
const ID_TOGGLE: u32 = 202;
const ID_RECENT: u32 = 300;

static INITS: AtomicU32 = AtomicU32::new(0);

fn log(line: &str) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let line = format!("[{}.{:03}] {}", t.as_secs() % 100_000, t.subsec_millis(), line);
    println!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("s6-hmenu.log")) {
        let _ = writeln!(f, "{line}");
    }
}

// ---------- test target ----------

unsafe fn build_menu() -> windows::core::Result<HMENU> {
    let bar = CreateMenu()?;
    let file = CreatePopupMenu()?;
    AppendMenuW(file, MF_STRING, ID_NEW as usize, w!("&New\tCtrl+N"))?;
    AppendMenuW(file, MF_SEPARATOR, 0, PCWSTR::null())?;
    AppendMenuW(file, MF_STRING, ID_EXIT as usize, w!("E&xit"))?;
    let edit = CreatePopupMenu()?;
    AppendMenuW(edit, MF_STRING | MF_GRAYED, ID_UNDO as usize, w!("&Undo\tCtrl+Z"))?;
    AppendMenuW(edit, MF_STRING, ID_TOGGLE as usize, w!("&Toggle"))?;
    AppendMenuW(bar, MF_POPUP, file.0 as usize, w!("&File"))?;
    AppendMenuW(bar, MF_POPUP, edit.0 as usize, w!("&Edit"))?;
    Ok(bar)
}

unsafe extern "system" fn target_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_INITMENUPOPUP => {
            // Mimic real apps: state and items are decided only when the
            // popup is about to open.
            let sub = HMENU(wp.0 as *mut _);
            let n = INITS.fetch_add(1, Ordering::SeqCst) + 1;
            if GetMenuState(sub, ID_UNDO, MF_BYCOMMAND) != u32::MAX {
                let _ = EnableMenuItem(sub, ID_UNDO, MF_BYCOMMAND | if n % 2 == 1 { MF_ENABLED } else { MF_GRAYED });
                let _ = CheckMenuItem(sub, ID_TOGGLE, (MF_BYCOMMAND | if n % 2 == 1 { MF_CHECKED } else { MF_UNCHECKED }).0);
                let label = HSTRING::from(format!("Recent {n}"));
                let _ = AppendMenuW(sub, MF_STRING, (ID_RECENT + n) as usize, &label);
            }
            log(&format!("TARGET WM_INITMENUPOPUP #{n} (index {})", lp.0 & 0xFFFF));
            LRESULT(0)
        }
        WM_COMMAND => {
            log(&format!("TARGET WM_COMMAND id={}", wp.0 & 0xFFFF));
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

// ---------- reading ----------

#[derive(Debug)]
struct Item {
    text: String,
    id: u32,
    state: u32,
    submenu: Option<HMENU>,
    sep: bool,
}

/// Reads one item with the two-step string read (length, then text) into
/// OUR buffer. This is the call that is in question across processes.
unsafe fn item(menu: HMENU, pos: u32) -> Option<Item> {
    let mut mii = MENUITEMINFOW {
        cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_STRING | MIIM_ID | MIIM_STATE | MIIM_SUBMENU | MIIM_FTYPE,
        ..Default::default()
    };
    GetMenuItemInfoW(menu, pos, true, &mut mii).ok()?;
    let mut text = String::new();
    if mii.cch > 0 {
        let mut buf = vec![0u16; mii.cch as usize + 1];
        mii.dwTypeData = PWSTR(buf.as_mut_ptr());
        mii.cch += 1;
        mii.fMask = MIIM_STRING;
        GetMenuItemInfoW(menu, pos, true, &mut mii).ok()?;
        text = String::from_utf16_lossy(&buf[..mii.cch as usize]);
    }
    Some(Item {
        text,
        id: mii.wID,
        state: mii.fState.0,
        submenu: (!mii.hSubMenu.0.is_null()).then_some(mii.hSubMenu),
        sep: mii.fType.0 & MFT_SEPARATOR.0 != 0,
    })
}

fn state_str(s: u32) -> String {
    let mut v = Vec::new();
    if s & MFS_GRAYED.0 != 0 {
        v.push("grayed");
    }
    if s & MFS_CHECKED.0 != 0 {
        v.push("checked");
    }
    v.join("+")
}

unsafe fn dump(menu: HMENU, depth: usize) -> usize {
    let n = GetMenuItemCount(Some(menu));
    let mut count = 0;
    for pos in 0..n.max(0) as u32 {
        match item(menu, pos) {
            Some(it) if it.sep => log(&format!("{}----", "  ".repeat(depth))),
            Some(it) => {
                count += 1;
                log(&format!("{}{:?} id={} {}", "  ".repeat(depth), it.text, it.id, state_str(it.state)));
                // MIIM_SUBMENU can come back empty across processes; GetSubMenu does not.
                let sub = it.submenu.unwrap_or_else(|| GetSubMenu(menu, pos as i32));
                if !sub.0.is_null() {
                    count += dump(sub, depth + 1);
                }
            }
            None => log(&format!("{}<item {pos}: read failed: {:?}>", "  ".repeat(depth), windows::core::Error::from_thread())),
        }
    }
    count
}

unsafe fn find(args: &[String]) -> windows::core::Result<HWND> {
    // "<class>" or "<class>|<exact title>"
    let spec = args.get(2).cloned().unwrap_or_else(|| "S6Target".into());
    match spec.split_once('|') {
        Some((c, t)) => FindWindowW(&HSTRING::from(c), &HSTRING::from(t)),
        None => FindWindowW(&HSTRING::from(spec), PCWSTR::null()),
    }
}

unsafe extern "system" fn host_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(h, msg, wp, lp)
}

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        match args.get(1).map(String::as_str) {
            Some("target") => {
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(target_proc),
                    hInstance: hinst.into(),
                    lpszClassName: w!("S6Target"),
                    ..Default::default()
                };
                RegisterClassExW(&wc);
                let h = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("S6Target"),
                    w!("S6 target"),
                    WS_OVERLAPPEDWINDOW,
                    300,
                    300,
                    600,
                    400,
                    None,
                    Some(build_menu()?),
                    Some(hinst.into()),
                    None,
                )?;
                let _ = ShowWindow(h, SW_SHOWNORMAL);
                log(&format!("target {:#x}", h.0 as isize));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            Some("read") => {
                let h = find(&args)?;
                let bar = GetMenu(h);
                if bar.0.is_null() {
                    log("GetMenu: NULL (no HMENU)");
                    return Ok(());
                }
                log(&format!("--- {:#x} menu tree (as is) ---", h.0 as isize));
                let n1 = dump(bar, 0);
                // Ask the owner to prepare each popup, like opening it would.
                for pos in 0..GetMenuItemCount(Some(bar)).max(0) {
                    let sub = GetSubMenu(bar, pos);
                    if !sub.0.is_null() {
                        let mut r = 0usize;
                        let ok = SendMessageTimeoutW(h, WM_INITMENUPOPUP, WPARAM(sub.0 as usize), LPARAM(pos as isize), SMTO_ABORTIFHUNG, 500, Some(&mut r));
                        log(&format!("sent WM_INITMENUPOPUP to popup {pos}: {}", ok.0 != 0));
                    }
                }
                log("--- after WM_INITMENUPOPUP ---");
                let n2 = dump(bar, 0);
                log(&format!("items: {n1} -> {n2}"));
            }
            Some("popup") => {
                let h = find(&args)?;
                let pos: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
                let run = args.iter().any(|a| a == "run");
                let sub = GetSubMenu(GetMenu(h), pos);
                if sub.0.is_null() {
                    log("no such submenu");
                    return Ok(());
                }
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(host_proc),
                    hInstance: hinst.into(),
                    lpszClassName: w!("S6Host"),
                    ..Default::default()
                };
                RegisterClassExW(&wc);
                let host = CreateWindowExW(WS_EX_TOOLWINDOW, w!("S6Host"), PCWSTR::null(), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None)?;
                let _ = ShowWindow(host, SW_SHOW);
                let _ = SetForegroundWindow(host);
                let mut r = 0usize;
                let _ = SendMessageTimeoutW(h, WM_INITMENUPOPUP, WPARAM(sub.0 as usize), LPARAM(pos as isize), SMTO_ABORTIFHUNG, 500, Some(&mut r));
                // Pick the first enabled item without a human: post Down, Enter
                // into our own queue (the menu's modal loop reads it), and
                // cancel the menu after 3 s no matter what.
                let host_v = host.0 as isize;
                std::thread::spawn(move || {
                    let host = HWND(host_v as *mut _);
                    std::thread::sleep(Duration::from_millis(500));
                    for vk in [VK_DOWN.0 as usize, VK_RETURN.0 as usize] {
                        let _ = PostMessageW(Some(host), WM_KEYDOWN, WPARAM(vk), LPARAM(0));
                        std::thread::sleep(Duration::from_millis(80));
                    }
                    std::thread::sleep(Duration::from_millis(2500));
                    let _ = PostMessageW(Some(host), WM_CANCELMODE, WPARAM(0), LPARAM(0));
                });
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let id = TrackPopupMenuEx(sub, (TPM_RETURNCMD | TPM_NONOTIFY | TPM_LEFTALIGN).0, pt.x, pt.y, host, None);
                log(&format!("TrackPopupMenuEx on foreign HMENU returned id={}", id.0));
                if run && id.0 != 0 {
                    let ok = PostMessageW(Some(h), WM_COMMAND, WPARAM(id.0 as usize), LPARAM(0));
                    log(&format!("posted WM_COMMAND {} to owner: {ok:?}", id.0));
                }
                let _ = DestroyWindow(host);
            }
            _ => println!("usage: s6-hmenu target | read <class> | popup <class> <sub> [run]"),
        }
    }
    Ok(())
}
