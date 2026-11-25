//! After a good install: NopBar runs. The installer's [Run] entry starts
//! it (packaging/inno/nopbar.iss, silent mode too); this only starts the
//! installed copy if, a few seconds later, it is still not running.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

/// The installed nopbar.exe: InstallLocation of the Apps & features entry
/// the installer writes (release::UNINSTALL_KEY), else its default folder.
pub fn installed_exe() -> Option<PathBuf> {
    let loc = reg_string(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\NopBar"), w!("InstallLocation"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(|l| Path::new(&l).join("Programs").join("NopBar")))?;
    Some(loc.join("nopbar.exe"))
}

fn reg_string(key: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).ok().ok()?;
    }
    let n = (size as usize / 2).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string())
}

/// Same file path, as Windows compares them (case, `/`, a trailing `\`).
pub fn same_path(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    n(a) == n(b)
}

/// Some process runs `exe`.
pub fn running(exe: &Path) -> bool {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok && !found {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            if String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case("nopbar.exe") {
                if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID) {
                    let mut buf = [0u16; 1024];
                    let mut n = buf.len() as u32;
                    if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok() {
                        found = same_path(Path::new(&String::from_utf16_lossy(&buf[..n as usize])), exe);
                    }
                    let _ = CloseHandle(h);
                }
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Waits up to `wait` for the installed NopBar to run; starts it if not.
pub fn ensure_running(wait: Duration) -> Result<(), String> {
    let exe = installed_exe().ok_or("no install folder")?;
    let until = Instant::now() + wait;
    loop {
        if running(&exe) {
            return Ok(());
        }
        if Instant::now() >= until {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if !exe.is_file() {
        return Err(format!("{} is not there", exe.display()));
    }
    std::process::Command::new(&exe).current_dir(exe.parent().unwrap_or(Path::new("."))).spawn().map(|_| ()).map_err(|e| format!("{}: {e}", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_compare_like_windows() {
        let a = Path::new(r"C:\Users\u\AppData\Local\Programs\NopBar\nopbar.exe");
        assert!(same_path(a, Path::new(r"c:\users\U\appdata\local\programs\nopbar\NOPBAR.EXE")));
        assert!(same_path(a, Path::new("C:/Users/u/AppData/Local/Programs/NopBar/nopbar.exe")));
        assert!(same_path(Path::new(r"C:\x\"), Path::new(r"C:\x")));
        assert!(!same_path(a, Path::new(r"C:\Users\u\AppData\Local\Programs\NopBar2\nopbar.exe")));
    }

    #[test]
    fn the_install_location_is_a_nopbar_exe() {
        // Reads only (the registry and the process list); starts nothing.
        let exe = installed_exe().expect("LOCALAPPDATA is set");
        assert!(exe.ends_with("nopbar.exe"));
        let _ = running(&exe);
        assert!(!running(Path::new(r"C:\no\such\nopbar.exe")));
    }
}
