//! ShellExecute has no environment argument. A short-lived copy of our exe
//! receives a fresh userenv block and performs only the shell call, before
//! logging/configuration/single-instance initialization. No process-global
//! environment mutation (other threads may be starting their own children).
use std::{ffi::OsString, os::windows::{ffi::OsStringExt, process::CommandExt}};
use windows::Win32::{Foundation::{CloseHandle, HANDLE}, Security::{TOKEN_QUERY, TOKEN_DUPLICATE}, System::Threading::{GetCurrentProcess, OpenProcessToken}};

type Environment = Vec<(OsString, OsString)>;

#[link(name = "userenv")]
unsafe extern "system" {
    fn CreateEnvironmentBlock(block: *mut *mut std::ffi::c_void, token: HANDLE, inherit: i32) -> i32;
    fn DestroyEnvironmentBlock(block: *const std::ffi::c_void) -> i32;
}


fn fresh() -> Result<Environment, u32> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE, &mut token).map_err(|e| e.code().0 as u32 & 0xffff)?;
        let mut block = std::ptr::null_mut();
        let ok = CreateEnvironmentBlock(&mut block, token, 0);
        let error = windows::Win32::Foundation::GetLastError().0;
        let _ = CloseHandle(token);
        if ok == 0 { return Err(error); }
        let mut env = Vec::new();
        let mut p = block as *const u16;
        while *p != 0 {
            let mut n = 0;
            while *p.add(n) != 0 { n += 1; }
            let entry = std::slice::from_raw_parts(p, n);
            if let Some(i) = entry.iter().position(|&c| c == b'=' as u16).filter(|&i| i > 0) {
                env.push((OsString::from_wide(&entry[..i]), OsString::from_wide(&entry[i + 1..])));
            }
            p = p.add(n + 1);
        }
        DestroyEnvironmentBlock(block);
        Ok(env)
    }
}

fn command(exe: &std::path::Path, env: &Environment) -> std::process::Command {
    let mut c = std::process::Command::new(exe);
    c.env_clear().envs(env.iter().cloned()).creation_flags(0x08000000); // CREATE_NO_WINDOW
    c
}

pub fn launch(verb: &str, file: &str, args: &str, dir: &str) -> Result<(), u32> {
    // Read on every launch: also works in settings windows without a broadcast host.
    let env = fresh()?;
    let exe = std::env::current_exe().map_err(|e| e.raw_os_error().unwrap_or(2) as u32)?;
    let child = command(&exe, &env).args(["--native-launch", verb, file, args, dir])
        .stdin(std::process::Stdio::piped()).spawn().map_err(os_error)?;
    let status = finish_child(child, |pid| unsafe {
        // The OS can deny foreground permission; launching still remains valid.
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
    }).map_err(os_error)?;
    if status.success() { Ok(()) } else { Err(status.code().unwrap_or(1) as u32) }
}
fn os_error(e: std::io::Error) -> u32 { e.raw_os_error().unwrap_or(1) as u32 }
fn finish_child(mut child: std::process::Child, grant: impl FnOnce(u32)) -> std::io::Result<std::process::ExitStatus> {
    use std::io::Write;
    grant(child.id());
    // The helper cannot race the parent's foreground grant.
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(b"1\n") {
            drop(stdin);
            let _ = child.wait();
            return Err(error);
        }
    }
    child.wait()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_gate_fixture() {
        let Some(path) = std::env::var_os("NOPBAR_GATE_FIXTURE") else { return; };
        use std::io::Read;
        let mut gate = [0];
        std::io::stdin().read_exact(&mut gate).unwrap();
        assert_eq!(gate, *b"1");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "granted");
    }
    #[test]
    fn helper_waits_for_permission_before_launch_and_parent_waits_for_exit() {
        let marker = std::env::temp_dir().join(format!("nopbar-grant-{}", uuid::Uuid::new_v4()));
        let child = command(&std::env::current_exe().unwrap(), &fresh().unwrap())
            .args(["--exact", "launch::environment::tests::helper_gate_fixture", "--nocapture"])
            .env("NOPBAR_GATE_FIXTURE", &marker)
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).spawn().unwrap();
        let mut granted = false;
        let status = finish_child(child, |pid| {
            assert!(pid > 0); granted = true;
            std::fs::write(&marker, "granted").unwrap();
        }).unwrap();
        std::fs::remove_file(marker).unwrap();
        assert!(granted); assert!(status.success(), "{status:?}");
    }
    #[test]
    fn child_receives_replaced_path_without_mutating_parent() {
        let parent = std::env::var_os("PATH");
        let cmd = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("cmd.exe");
        let mut env = fresh().unwrap();
        env.retain(|(k, _)| !k.to_string_lossy().eq_ignore_ascii_case("PATH"));
        for path in ["C:\\native-test-one", "C:\\native-test-two"] {
            let mut changed = env.clone();
            changed.push(("PATH".into(), path.into()));
            changed.push(("NOPBAR_NATIVE_TEST".into(), "temporary".into()));
            let out = command(&cmd, &changed).args(["/d", "/c"]).raw_arg("echo %PATH%&echo %NOPBAR_NATIVE_TEST%")
                .output().unwrap();
            assert!(out.status.success(), "status={:?}; stdout={:?}; stderr={:?}", out.status, String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            let text = String::from_utf8_lossy(&out.stdout);
            assert_eq!(text.lines().map(str::trim).collect::<Vec<_>>(), [path, "temporary"]);
        }
        assert_eq!(std::env::var_os("PATH"), parent);
    }
}
