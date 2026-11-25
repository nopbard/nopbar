//! The successful sign-in process notifies the running dock once. Startup and
//! failed sign-ins never show the hint or make a network request.
use std::sync::atomic::{AtomicBool, Ordering};
static PENDING: AtomicBool = AtomicBool::new(false);
static SHOW: AtomicBool = AtomicBool::new(false);
pub fn waiting_to_show() -> bool {SHOW.load(Ordering::SeqCst)}
pub fn take_awaken() -> bool {PENDING.swap(false,Ordering::SeqCst)}
#[cfg(test)] pub fn queue_awaken_for_test() {SHOW.store(false,Ordering::SeqCst);PENDING.store(true,Ordering::SeqCst);}
pub fn show_ready() {SHOW.store(true,Ordering::SeqCst);super::mcp::wake();}
pub fn notify() {
    notify_kind("CompanionConnected");
}
pub fn notify_kind(kind: &str) {
    use std::{io::Write, os::windows::fs::OpenOptionsExt};
    let name = format!("{}.agents", crate::worker::ipc::pipe_name());
    if let Ok(mut pipe) = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(0x00100000)
        .open(name)
    {
        if kind == "CompanionHistory" {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                let mut pid=0;
                if windows::Win32::System::Pipes::GetNamedPipeServerProcessId(windows::Win32::Foundation::HANDLE(pipe.as_raw_handle()),&mut pid).is_ok() {
                    let _=windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
                }
            }
        }
        let _ = writeln!(pipe, "{}", serde_json::json!({"type":kind}));
    }
}
pub fn receive() {
    PENDING.store(true, Ordering::SeqCst);
    super::session::refresh();
    super::mcp::wake();
}
pub fn poll() {
    if !super::connected() || !SHOW.load(Ordering::SeqCst) {
        return;
    }
    super::super::APP.with(|a| {
        let a = a.borrow();
        let Some(a) = a.as_ref() else {
            return;
        };
        let Some(g) = a.ground else {
            return;
        };
        if !a.shown || g.covered || super::super::living::away() {
            return;
        }
        let Some(body) = (unsafe { a.sprite.as_ref().and_then(|s| s.window.body_bounds()) }) else {
            return;
        };
        SHOW.store(false,Ordering::SeqCst);
        crate::topbar::companion_tip(
            &a.gfx,
            g.dpi,
            crate::i18n::t("companion.chat_hint"),
            windows::Win32::Foundation::POINT {
                x: (body.left + body.right) / 2,
                y: body.top - (8. * g.dpi) as i32,
            },
            g.monitor,
        );
        let _ = crate::motion::after(2000, Box::new(crate::topbar::hide_tip));
    });
}
