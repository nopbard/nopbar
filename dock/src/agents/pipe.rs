//! The hooks' own pipe: `\\.\pipe\NopBar.<session>.agents`, inbound only,
//! the user's own processes only (the settings pipe's descriptor), no
//! answers. Apart from the settings pipe so a burst of hooks (parallel
//! tool calls in several sessions) never takes a settings window's slot.
//!
//! LISTENERS threads each serve one instance: wait for a hook, read what
//! it wrote until it closes (READ_MS at most: one that connects and stays
//! silent is let go), hand each line to the board, and wait again. A hook
//! that finds every instance busy waits a moment and gives up (hook.rs).

use std::time::Duration;

use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{ReadFile, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_INBOUND};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject, INFINITE};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

const LISTENERS: usize = 2;
/// A hook's whole line must arrive within this long.
const READ_MS: u32 = 1000;
/// The most read from one hook.
const MAX_IN: usize = 64 * 1024;

/// The pipe's name for this Windows session.
pub fn name() -> String {
    format!("{}.agents", crate::worker::ipc::pipe_name())
}

/// Starts the listeners (once, at the dock's start).
pub fn listen() {
    let Some(sd) = (unsafe { crate::worker::ipc::owner_only_sd() }) else {
        // Fail closed: no pipe rather than one others can open.
        crate::log!("agents: no security descriptor; the hooks' pipe is off");
        return;
    };
    let sd = sd.0 as isize;
    for i in 0..LISTENERS {
        let _ = std::thread::Builder::new().name(format!("agents-pipe-{i}")).spawn(move || unsafe { serve(sd) });
    }
}

unsafe fn serve(sd: isize) {
    let name = HSTRING::from(name());
    let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd as *mut _, bInheritHandle: false.into() };
    let Ok(ev) = CreateEventW(None, true, false, None) else { return };
    let mut failures = 0;
    loop {
        let pipe = CreateNamedPipeW(&name, PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES, 0, MAX_IN as u32, 0, Some(&sa));
        if pipe == INVALID_HANDLE_VALUE {
            failures += 1;
            if failures >= 5 {
                crate::log!("agents: the hooks' pipe could not be made ({:?})", GetLastError());
                let _ = CloseHandle(ev);
                return;
            }
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        failures = 0;
        let _ = ResetEvent(ev);
        let mut ov = OVERLAPPED { hEvent: ev, ..Default::default() };
        let connected = match ConnectNamedPipe(pipe, Some(&mut ov)) {
            Ok(()) => true,
            Err(_) => match GetLastError() {
                ERROR_PIPE_CONNECTED => true,
                ERROR_IO_PENDING => {
                    let mut n = 0u32;
                    WaitForSingleObject(ev, INFINITE) == WAIT_OBJECT_0 && GetOverlappedResult(pipe, &ov, &mut n, false).is_ok()
                }
                _ => false,
            },
        };
        if connected {
            let data = read_all(pipe, ev);
            for line in String::from_utf8_lossy(&data).lines() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                    if v.get("type").and_then(|t| t.as_str()) == Some("CompanionAuthChanged") {
                        crate::companion::chat::session::refresh();
                    }
                    if v.get("type").and_then(|t| t.as_str()) == Some("CompanionHistory") {
                        crate::companion::chat::request_history();
                    }
                    if v.get("type").and_then(|t| t.as_str()) == Some("CompanionConnected") {
                        crate::companion::chat::hint::receive();
                    }
                    if v.get("type").and_then(|t| t.as_str()) == Some("CompanionCliChanged") {
                        crate::companion::chat::cli::refresh(Default::default());
                    }
                    if v.get("type").and_then(|t| t.as_str()) == Some("CompanionOutput") {
                        crate::companion::chat::mcp::ingest(&v);
                    }
                    if v.get("type").and_then(|t| t.as_str()) == Some("Agent") {
                        super::ingest(&v);
                    }
                }
            }
        }
        let _ = DisconnectNamedPipe(pipe);
        let _ = CloseHandle(pipe);
    }
}

/// What the connected hook writes until it closes, within READ_MS.
unsafe fn read_all(pipe: HANDLE, ev: HANDLE) -> Vec<u8> {
    let started = std::time::Instant::now();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while out.len() < MAX_IN {
        let left = (READ_MS as u128).saturating_sub(started.elapsed().as_millis()) as u32;
        if left == 0 {
            break;
        }
        let _ = ResetEvent(ev);
        let mut ov = OVERLAPPED { hEvent: ev, ..Default::default() };
        let mut n = 0u32;
        let r = ReadFile(pipe, Some(&mut buf), None, Some(&mut ov));
        if r.is_err() && GetLastError() != ERROR_IO_PENDING {
            break; // closed (all read) or broken
        }
        if WaitForSingleObject(ev, left) != WAIT_OBJECT_0 {
            let _ = CancelIoEx(pipe, Some(&ov));
            let _ = GetOverlappedResult(pipe, &ov, &mut n, true);
            break;
        }
        if GetOverlappedResult(pipe, &ov, &mut n, false).is_err() || n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    out
}
