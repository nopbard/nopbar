//! Every child starts suspended, enters a kill-on-close job, then runs.
//! Closing a bubble, cancelling a worker or exiting NopBar kills descendants.
use std::{
    io::Read,
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Child, Command, Stdio},
    sync::mpsc,
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{JobObjects::*, Threading::CREATE_SUSPENDED},
};
pub const NO_WINDOW: u32 = 0x08000000;
/// Login can launch the user's browser. Own only the terminal, never its tree.
pub struct LoginProcess(Child);
impl LoginProcess {
    pub const FLAGS: u32 = 0x00000010; // CREATE_NEW_CONSOLE, no Job Object.
    pub fn spawn(command: &mut Command) -> Result<Self, &'static str> {
        Self::with_flags(command, Self::FLAGS)
    }
    fn with_flags(command: &mut Command, flags: u32) -> Result<Self, &'static str> {
        command
            .creation_flags(flags)
            .spawn()
            .map(Self)
            .map_err(|_| "chat.error.spawn")
    }
    pub fn child(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for LoginProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtResumeProcess(process: HANDLE) -> i32;
}
pub struct Process {
    child: Child,
    job: HANDLE,
}
impl Process {
    pub fn spawn(command: &mut Command, visible: bool) -> Result<Self, &'static str> {
        unsafe {
            let job = CreateJobObjectW(None, None).map_err(|_| "chat.error.spawn")?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as _,
                std::mem::size_of_val(&info) as u32,
            )
            .is_err()
            {
                let _ = CloseHandle(job);
                return Err("chat.error.spawn");
            }
            command
                .creation_flags(CREATE_SUSPENDED.0 | if visible { 0x00000010 } else { NO_WINDOW });
            let mut child = match command.spawn() {
                Ok(c) => c,
                Err(_) => {
                    let _ = CloseHandle(job);
                    return Err("chat.error.spawn");
                }
            };
            let handle = HANDLE(child.as_raw_handle());
            if AssignProcessToJobObject(job, handle).is_err() || NtResumeProcess(handle) < 0 {
                let _ = child.kill();
                let _ = child.wait();
                let _ = CloseHandle(job);
                return Err("chat.error.spawn");
            }
            Ok(Self { child, job })
        }
    }
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }
    pub fn command(path: &std::path::Path) -> Command {
        let mut c = Command::new(path);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("NOPBAR_CHAT", "1");
        c
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateJobObject(self.job, 1);
            let _ = CloseHandle(self.job);
        }
        let _ = self.child.wait();
    }
}
pub fn reader(mut source: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    // Bounded backpressure: hostile/noisy children cannot grow memory without limit.
    let (tx, rx) = mpsc::sync_channel(32);
    std::thread::spawn(move || {
        let mut buf = [0; 4096];
        loop {
            match source.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}
pub fn output(
    path: &std::path::Path,
    args: &[&str],
    cwd: &std::path::Path,
) -> Result<(bool, String), &'static str> {
    output_cancel(path, args, cwd, &mut || false)
}
pub fn output_cancel(
    path: &std::path::Path,
    args: &[&str],
    cwd: &std::path::Path,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(bool, String), &'static str> {
    capture(path, args, cwd, cancelled, false)
}
/// Human-readable diagnostics may legitimately be written to either pipe.
pub fn diagnostic_cancel(
    path: &std::path::Path,
    args: &[&str],
    cwd: &std::path::Path,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(bool, String), &'static str> {
    capture(path, args, cwd, cancelled, true)
}
fn capture(
    path: &std::path::Path,
    args: &[&str],
    cwd: &std::path::Path,
    cancelled: &mut impl FnMut() -> bool,
    merge_stderr: bool,
) -> Result<(bool, String), &'static str> {
    if cancelled() {
        return Err("chat.install.cancelled");
    }
    let mut c = Process::command(path);
    c.args(args).current_dir(cwd).stdin(Stdio::null());
    let mut p = Process::spawn(&mut c, false)?;
    let rx = reader(p.child().stdout.take().unwrap());
    let err = reader(p.child().stderr.take().unwrap());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut streams = [Vec::new(), Vec::new()];
    let mut total = 0;
    let mut exited = None;
    let mut closed = [false; 2];
    loop {
        if cancelled() {
            return Err("chat.install.cancelled");
        }
        for (i, source) in [&rx, &err].into_iter().enumerate() {
            // Drain both pipes concurrently, including bytes delivered after exit.
            // Limit a batch so cancellation/deadline cannot be starved by output.
            for _ in 0..32 {
                match source.try_recv() {
                    Ok(b) => {
                        total += b.len();
                        if total > 1024 * 1024 {
                            return Err("chat.error.protocol");
                        }
                        if i == 0 || merge_stderr {
                            streams[i].extend(b);
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        closed[i] = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        if exited.is_none() {
            exited = p.child().try_wait().map_err(|_| "chat.error.spawn")?;
        }
        if let Some(status) = exited.filter(|_| closed.iter().all(|v| *v)) {
            let mut text = String::from_utf8_lossy(&streams[0]).into_owned();
            if merge_stderr && !streams[1].is_empty() {
                text.push('\n');
                text.push_str(&String::from_utf8_lossy(&streams[1]));
            }
            return Ok((status.success(), text));
        }
        if std::time::Instant::now() >= deadline {
            return Err("chat.error.response_timeout");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn login_terminal_does_not_kill_browser_descendant() {
        use windows::Win32::{
            Foundation::WAIT_TIMEOUT,
            System::Threading::{
                OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
                PROCESS_TERMINATE,
            },
        };
        assert_eq!(LoginProcess::FLAGS, 0x10);
        let exe = super::super::fixture::executable();
        let mut command = Process::command(&exe);
        command.arg("tree").stdin(Stdio::null());
        let mut terminal = LoginProcess::with_flags(&mut command, NO_WINDOW).unwrap();
        let rx = reader(terminal.child().stdout.take().unwrap());
        let bytes = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let pid = String::from_utf8(bytes)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        unsafe {
            let browser = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid).unwrap();
            drop(terminal);
            let state = WaitForSingleObject(browser, 100);
            // Always clean up the fake browser, even if the assertion fails.
            let _ = TerminateProcess(browser, 0);
            let _ = WaitForSingleObject(browser, 5000);
            CloseHandle(browser).unwrap();
            assert_eq!(state, WAIT_TIMEOUT);
        }
    }
    #[test]
    fn cancellation_kills_entire_tree_before_guard_returns() {
        use windows::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };
        let exe = super::super::fixture::executable();
        let mut c = Process::command(&exe);
        c.arg("tree").stdin(Stdio::null());
        let mut p = Process::spawn(&mut c, false).unwrap();
        let rx = reader(p.child().stdout.take().unwrap());
        let bytes = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let pid = String::from_utf8(bytes)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        unsafe {
            let descendant = OpenProcess(PROCESS_SYNCHRONIZE, false, pid).unwrap();
            drop(p);
            assert_eq!(
                WaitForSingleObject(descendant, 5000),
                windows::Win32::Foundation::WAIT_OBJECT_0
            );
            CloseHandle(descendant).unwrap();
        }
    }
    #[test]
    fn fixture_install_progress_and_environment() {
        let exe = super::super::fixture::executable();
        let cwd = exe.parent().unwrap();
        let (ok, out) = output(&exe, &["installer"], cwd).unwrap();
        assert!(ok);
        assert!(out.contains("download") && out.contains("installed"));
        let (ok, out) = output(&exe, &[], cwd).unwrap();
        assert!(ok);
        assert!(out.contains("fixture reply"));
    }
}
