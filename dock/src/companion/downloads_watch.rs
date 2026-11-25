//! One nonrecursive Downloads registration. No whole-shell file subscription.
use std::{cell::RefCell, path::Path};
use windows::{
    core::{BOOL, HSTRING},
    Win32::{
        Foundation::HWND,
        UI::{
            Shell::{Common::ITEMIDLIST, *},
            WindowsAndMessaging::WM_APP,
        },
    },
};
pub const WM_CHANGED: u32 = WM_APP + 3819;
struct Watch {
    id: u32,
    pidl: *mut ITEMIDLIST,
}
impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            if self.id != 0 {
                let _ = SHChangeNotifyDeregister(self.id);
            }
            windows::Win32::System::Com::CoTaskMemFree(Some(self.pidl.cast()));
        }
    }
}
thread_local! {static WATCH:RefCell<Option<Watch>>=const{RefCell::new(None)};}
pub fn configure(root: Option<&Path>) {
    configure_for(
        root,
        HWND(super::super::OWNER.load(std::sync::atomic::Ordering::SeqCst) as *mut _),
    );
}
fn configure_for(root: Option<&Path>, owner: HWND) {
    WATCH.with(|w| {
        w.borrow_mut().take();
        let Some(root) = root else {
            return;
        };
        unsafe {
            let mut pidl = std::ptr::null_mut();
            if SHParseDisplayName(&HSTRING::from(root.as_os_str()), None, &mut pidl, 0, None)
                .is_err()
            {
                return;
            }
            let mut watch = Watch { id: 0, pidl };
            let entry = SHChangeNotifyEntry {
                pidl,
                fRecursive: BOOL(0),
            };
            // InterruptLevel delivers filesystem renames even without Explorer open.
            // RENAMEITEM delivers old PIDL[0] and new PIDL[1] (Microsoft Shell docs).
            watch.id = SHChangeNotifyRegister(
                owner,
                SHCNRF_InterruptLevel | SHCNRF_ShellLevel | SHCNRF_NewDelivery,
                (SHCNE_CREATE.0 | SHCNE_RENAMEITEM.0 | SHCNE_UPDATEITEM.0) as i32,
                WM_CHANGED,
                1,
                &entry,
            );
            if watch.id != 0 {
                *w.borrow_mut() = Some(watch);
            } else {
                crate::log!("download: cannot watch folder");
            }
        }
    });
}
pub unsafe fn contains(pidl: *const ITEMIDLIST) -> bool {
    !pidl.is_null()
        && WATCH.with(|w| {
            w.borrow()
                .as_ref()
                .is_some_and(|w| ILIsParent(w.pidl, pidl, true).as_bool())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{Foundation::*, UI::WindowsAndMessaging::*};
    thread_local! {static EVENTS:RefCell<Vec<(i32,String,String)>>=const{RefCell::new(Vec::new())};}
    unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if m != WM_CHANGED {
            return DefWindowProcW(h, m, w, l);
        }
        let mut pidls = std::ptr::null_mut();
        let mut event = 0;
        let lock = SHChangeNotification_Lock(
            HANDLE(w.0 as *mut _),
            l.0 as u32,
            Some(&mut pidls),
            Some(&mut event),
        );
        if !lock.is_invalid() {
            let path = |p| {
                let mut text = [0u16; 1024];
                if SHGetPathFromIDListEx(p, &mut text, GPFIDL_DEFAULT).as_bool() {
                    String::from_utf16_lossy(&text[..text.iter().position(|c| *c == 0).unwrap()])
                } else {
                    String::new()
                }
            };
            if !pidls.is_null() {
                let old = path(*pidls);
                let new = if event & SHCNE_RENAMEITEM.0 as i32 != 0 {
                    path(*pidls.add(1))
                } else {
                    String::new()
                };
                EVENTS.with(|e| e.borrow_mut().push((event, old, new)));
            }
            let _ = SHChangeNotification_Unlock(lock);
        }
        LRESULT(0)
    }
    unsafe fn pump(h: HWND, flag: i32, milliseconds: u64) -> bool {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(milliseconds);
        loop {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, Some(h), WM_CHANGED, WM_CHANGED, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            if EVENTS.with(|e| e.borrow().iter().any(|(ev, _, _)| ev & flag != 0)) {
                return true;
            }
            if std::time::Instant::now() >= until {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    #[test]
    #[ignore = "temporary-only filesystem Shell event probe; hidden test window, no desktop capture"]
    fn review_5_native_temporary_download_rename() {
        let root =
            std::env::temp_dir().join(format!("nopbar-shell-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let old = root.join("fixture.crdownload");
        let new = root.join("fixture.pdf");
        let (created, renamed);
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
            let window = crate::companion::window::Window::with_proc(
                1,
                1,
                true,
                windows::core::w!("NopBarShellFixture"),
                Some(proc),
            )
            .unwrap();
            configure_for(Some(&root), window.hwnd);
            assert!(WATCH.with(|w| w.borrow().is_some()));
            // The shell creates its filesystem listener asynchronously.
            // Pump only this hidden fixture window while registration settles.
            pump(window.hwnd, 0, 1000);
            std::fs::write(&old, b"temporary fixture").unwrap();
            created = pump(window.hwnd, SHCNE_CREATE.0 as i32, 5000);
            std::fs::rename(&old, &new).unwrap();
            renamed = pump(window.hwnd, SHCNE_RENAMEITEM.0 as i32, 5000);
            let mut pidl = std::ptr::null_mut();
            SHParseDisplayName(&HSTRING::from(new.as_os_str()), None, &mut pidl, 0, None).unwrap();
            assert!(contains(pidl));
            configure_for(None, window.hwnd);
            assert!(!contains(pidl));
            windows::Win32::System::Com::CoTaskMemFree(Some(pidl.cast()));
        }
        std::fs::remove_file(&new).unwrap();
        std::fs::remove_dir(&root).unwrap();
        assert!(created, "filesystem CREATE did not arrive");
        assert!(renamed, "filesystem RENAMEITEM did not arrive");
        EVENTS.with(|e| {
            assert!(e
                .borrow()
                .iter()
                .any(|(ev, a, b)| ev & SHCNE_RENAMEITEM.0 as i32 != 0
                    && a.ends_with("fixture.crdownload")
                    && b.ends_with("fixture.pdf")))
        });
    }
}
