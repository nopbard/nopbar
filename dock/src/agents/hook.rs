//! `nopbar.exe --agent <claude|codex> [payload]`: what the agents' hooks
//! run (connect.rs puts it there). Claude Code hands its hook a JSON
//! object on stdin; Codex's notify adds one as the last argument.
//!
//! It finds the session's processes (one snapshot: the agent itself, the
//! first ancestor that is not a shell; the host, the first ancestor with a
//! window of its own, never Explorer), sends one line to the running
//! dock's pipe and exits. It never waits for the dock, prints nothing (a
//! UserPromptSubmit hook's output would reach the model) and always exits
//! 0: no dock running, a busy pipe or a bad payload all end quietly. The
//! whole run stays well under 100 ms (stdin is read for READ_MS at most).

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindow, GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW};

use super::Agent;

/// The longest stdin is waited on (Claude writes it at once and closes).
const READ_MS: u64 = 60;
/// The most of stdin read.
const MAX_IN: u64 = 1 << 20;
/// Ancestors looked at.
const DEPTH: usize = 16;

/// Shells and console hosts between an agent and its app (never the
/// session's own process, never the window to bring forward).
const SHELLS: &[&str] = &["cmd.exe", "bash.exe", "sh.exe", "zsh.exe", "fish.exe", "pwsh.exe", "powershell.exe", "conhost.exe", "openconsole.exe", "wsl.exe", "wslhost.exe", "nu.exe"];

/// The hook's run; the process's exit code (always 0).
pub fn run(args: &[String]) -> i32 {
    if ignore_chat(std::env::var_os("NOPBAR_CHAT").as_deref()) {return 0;}
    let Some(agent) = args.get(2).and_then(|a| Agent::parse(a)) else { return 0 };
    let raw = match agent {
        Agent::Claude => read_stdin(),
        Agent::Codex => args.get(3..).and_then(|a| a.last()).cloned().unwrap_or_default(),
    };
    let payload: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    let procs = Procs::snapshot();
    let me = std::process::id();
    let chain = procs.ancestors(me);
    let windows = top_windows();
    let found = find(&chain, &windows);
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let line = line(agent, &payload, &found, &cwd, now_filetime());
    send(&line);
    0
}

/// Run bare by a Claude Code hook (one too old for the hook's args):
/// Claude sets CLAUDE_PROJECT_DIR for its hooks and pipes their stdin. A
/// person starting NopBar has neither.
pub fn bare_hook() -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE};
    let h = windows::Win32::Foundation::HANDLE(std::io::stdin().as_raw_handle());
    std::env::var_os("CLAUDE_PROJECT_DIR").is_some() && !h.is_invalid() && !h.0.is_null() && unsafe { GetFileType(h) } == FILE_TYPE_PIPE
}

/// Stdin, as much as arrives within READ_MS.
fn read_stdin() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("stdin".into()).spawn(move || {
        let mut s = String::new();
        let _ = std::io::stdin().take(MAX_IN).read_to_string(&mut s);
        let _ = tx.send(s);
    });
    rx.recv_timeout(Duration::from_millis(READ_MS)).unwrap_or_default()
}

/// What the process tree told: the agent's process and the host (its
/// pid and window).
#[derive(Debug, Default, PartialEq)]
pub struct Found {
    pub agent_pid: u32,
    pub host: Option<(u32, isize)>,
}

/// Pure: in `chain` (the hook's ancestors, nearest first: (pid, exe
/// name)), the agent (the first that is no shell) and the host (the first
/// with a top-level window in `windows`, pid -> window, Explorer never).
pub fn find(chain: &[(u32, String)], windows: &HashMap<u32, isize>) -> Found {
    let is_shell = |n: &str| SHELLS.iter().any(|s| n.eq_ignore_ascii_case(s));
    let agent_pid = chain.iter().find(|(_, n)| !is_shell(n)).map(|(p, _)| *p).unwrap_or(0);
    let host = chain
        .iter()
        .take_while(|(_, n)| !n.eq_ignore_ascii_case("explorer.exe"))
        .find_map(|(p, n)| if is_shell(n) && !n.eq_ignore_ascii_case("conhost.exe") { None } else { windows.get(p).map(|h| (*p, *h)) });
    Found { agent_pid, host }
}

/// Now as a FILETIME (100 ns): the line's time (hooks race: a line older
/// than the last heard is dropped).
fn now_filetime() -> u64 {
    let ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime() };
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

/// Pure: the line for the dock's pipe (agents::Msg::parse reads it). The
/// app's exe is not in it: the dock reads it from the app's process.
pub fn line(agent: Agent, payload: &serde_json::Value, found: &Found, cwd_here: &str, t: u64) -> String {
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (event, session, cwd, ntype) = match agent {
        Agent::Claude => (s("hook_event_name"), s("session_id"), s("cwd"), s("notification_type")),
        // Older Codex sends no thread id: one Codex process, one session.
        Agent::Codex => {
            let t = s("thread-id");
            (s("type"), if t.is_empty() { format!("pid{}", found.agent_pid) } else { t }, s("cwd"), String::new())
        }
    };
    let cwd = if cwd.is_empty() { cwd_here.to_string() } else { cwd };
    let (host_pid, hwnd) = found.host.unwrap_or((0, 0));
    serde_json::json!({
        "type": "Agent",
        "agent": agent.id(),
        "event": event,
        "ntype": ntype,
        "session": session,
        "cwd": cwd,
        "agent_pid": found.agent_pid,
        "host_pid": host_pid,
        "hwnd": hwnd as i64,
        "t": t,
    })
    .to_string()
}

/// One line to the running dock's pipe; nothing if none runs or it is
/// busy past a moment.
fn send(line: &str) {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{GetLastError, ERROR_PIPE_BUSY, GENERIC_WRITE};
    use windows::Win32::Storage::FileSystem::{CreateFileW, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING};
    use windows::Win32::System::Pipes::WaitNamedPipeW;
    let name = HSTRING::from(super::pipe::name());
    unsafe {
        let open = || CreateFileW(&name, GENERIC_WRITE.0, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None);
        let h = match open() {
            Ok(h) => h,
            Err(_) if GetLastError() == ERROR_PIPE_BUSY && WaitNamedPipeW(&name, 30).as_bool() => match open() {
                Ok(h) => h,
                Err(_) => return,
            },
            Err(_) => return,
        };
        let mut data = line.as_bytes().to_vec();
        data.push(b'\n');
        let _ = WriteFile(h, Some(&data), None, None);
        let _ = CloseHandle(h);
    }
}

/// Every process now: pid -> (parent, exe name).
struct Procs(HashMap<u32, (u32, String)>);

impl Procs {
    fn snapshot() -> Procs {
        use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
        let mut map = HashMap::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Procs(map) };
            let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                map.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
        }
        Procs(map)
    }

    /// `pid`'s ancestors, nearest first (a parent id reused by a newer
    /// process ends the walk at a loop or DEPTH).
    fn ancestors(&self, pid: u32) -> Vec<(u32, String)> {
        let mut out: Vec<(u32, String)> = Vec::new();
        let mut at = self.0.get(&pid).map(|p| p.0);
        while let Some(p) = at {
            if p == 0 || out.len() >= DEPTH || out.iter().any(|(q, _)| *q == p) {
                break;
            }
            let Some((parent, name)) = self.0.get(&p) else { break };
            out.push((p, name.clone()));
            at = Some(*parent);
        }
        out
    }
}

/// An app's own window: visible, top-level, unowned, no tool window.
fn app_window(h: HWND) -> bool {
    unsafe {
        IsWindowVisible(h).as_bool() && GetWindow(h, GW_OWNER).map_or(true, |o| o.0.is_null()) && (GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0) == 0
    }
}

/// Each process's frontmost app window (pid -> window; Z order: the one
/// used last comes first).
fn top_windows() -> HashMap<u32, isize> {
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> windows::core::BOOL {
        let map = &mut *(lp.0 as *mut HashMap<u32, isize>);
        if app_window(h) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            map.entry(pid).or_insert(h.0 as isize);
        }
        true.into()
    }
    let mut map: HashMap<u32, isize> = HashMap::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut map as *mut _ as isize));
    }
    map
}

/// A process's local executable path.
pub(super) fn exe_path(pid: u32) -> String {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = CloseHandle(h);
        if ok {
            String::from_utf16_lossy(&buf[..n as usize])
        } else {
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(v: &[(u32, &str)]) -> Vec<(u32, String)> {
        v.iter().map(|(p, n)| (*p, n.to_string())).collect()
    }

    #[test]
    #[ignore = "opens the hooks' pipe of this Windows session: cargo test -p dock agents_pipe -- --ignored"]
    fn agents_pipe_round_trip() {
        super::super::listen();
        std::thread::sleep(Duration::from_millis(200));
        // A finished session: nothing is kept awake by the test.
        let found = Found { agent_pid: 0, host: None };
        let payload = serde_json::json!({"session_id":"pipe-test","cwd":"C:/w/pipe-test","hook_event_name":"Stop"});
        let started = std::time::Instant::now();
        send(&line(Agent::Claude, &payload, &found, "", now_filetime()));
        let sent = started.elapsed();
        let seen = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            super::super::with(|b| b.sessions.iter().any(|s| s.name == "pipe-test")).unwrap_or(false)
        });
        assert!(seen, "the board heard the hook");
        assert!(sent < Duration::from_millis(50), "sending took {sent:?}");
    }

    #[test]
    fn the_agent_and_its_app_are_found() {
        // Orca: the hook under cmd, under claude, under PowerShell, under
        // Orca's terminal daemon (no window), under Orca.
        let c = chain(&[(5, "cmd.exe"), (4, "claude.exe"), (3, "powershell.exe"), (2, "orca.exe"), (1, "Orca.exe"), (0, "explorer.exe")]);
        let w: HashMap<u32, isize> = [(1, 100), (0, 999)].into();
        assert_eq!(find(&c, &w), Found { agent_pid: 4, host: Some((1, 100)) });
        // Windows Terminal: the shell's parent has the window.
        let c = chain(&[(4, "codex.exe"), (3, "pwsh.exe"), (2, "WindowsTerminal.exe")]);
        let w: HashMap<u32, isize> = [(2, 200)].into();
        assert_eq!(find(&c, &w), Found { agent_pid: 4, host: Some((2, 200)) });
        // A classic console: Explorer is never the host.
        let c = chain(&[(4, "node.exe"), (3, "cmd.exe"), (0, "explorer.exe")]);
        let w: HashMap<u32, isize> = [(0, 999)].into();
        assert_eq!(find(&c, &w), Found { agent_pid: 4, host: None });
    }

    #[test]
    fn the_line_carries_only_what_the_board_reads() {
        let found = Found { agent_pid: 4, host: Some((1, 100)) };
        let claude = serde_json::json!({"session_id":"abc","cwd":"C:\\w\\agent-board","hook_event_name":"Notification","notification_type":"permission_prompt","message":"secret prompt text"});
        let l = line(Agent::Claude, &claude, &found, "C:\\here", 7);
        let v: serde_json::Value = serde_json::from_str(&l).unwrap();
        assert!(!l.contains("secret"), "no prompt text leaves the hook");
        let m = super::super::Msg::parse(&v).unwrap();
        assert_eq!((m.session.as_str(), m.ev, m.host_pid, m.hwnd, m.agent_pid), ("abc", super::super::Ev::To(super::super::St::Wait), 1, 100, 4));
        assert_eq!(super::super::name_of(&m.cwd, m.agent), "agent-board");
        // Codex's notify, with and without a thread id.
        let codex = serde_json::json!({"type":"agent-turn-complete","thread-id":"t1","cwd":"D:\\sample-project","last-assistant-message":"x"});
        let m = super::super::Msg::parse(&serde_json::from_str(&line(Agent::Codex, &codex, &found, "", 0)).unwrap()).unwrap();
        assert_eq!((m.session.as_str(), m.ev), ("t1", super::super::Ev::To(super::super::St::Done)));
        let old = serde_json::json!({"type":"agent-turn-complete"});
        let m = super::super::Msg::parse(&serde_json::from_str(&line(Agent::Codex, &old, &found, "C:\\here\\x", 0)).unwrap()).unwrap();
        assert_eq!((m.session.as_str(), m.cwd.as_str()), ("pid4", "C:\\here\\x"));
    }
}

fn ignore_chat(marker: Option<&std::ffi::OsStr>) -> bool {marker==Some(std::ffi::OsStr::new("1"))}
#[cfg(test)] #[test] fn chat_hooks_are_ignored_before_stdin_or_process_inspection(){assert!(ignore_chat(Some(std::ffi::OsStr::new("1"))));assert!(!ignore_chat(None));}
