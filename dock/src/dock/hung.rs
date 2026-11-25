//! "Not responding" apps: a dimmed icon with an amber "!" badge, a
//! "— Not responding" label and a "Force quit" row in the menu.
//!
//! Cheap by design: `IsHungAppWindow` (no message is sent to the app) on
//! each running app's windows, after every window-list refresh and on a
//! 2 s timer that only runs while some dock app has windows (and not while
//! the session is locked or asleep or the display is off). A window
//! counts as hung after `IsHungAppWindow` has said so for `DEBOUNCE`
//! (Windows itself only says so after ~5 s without the app reading its
//! messages), and stops counting the first time it answers again.

use super::*;
use std::time::Duration;

/// Repeating check while apps run.
pub(super) const HUNG_TIMER: usize = 15;
const HUNG_CHECK_MS: u32 = 2000;
/// How long a window must stay hung before the dock shows it.
pub(super) const DEBOUNCE: Duration = Duration::from_secs(3);
/// Icon opacity while its app does not respond.
pub(super) const HUNG_DIM: f32 = 0.55;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Change {
    /// The window has been hung for DEBOUNCE.
    Start(isize),
    /// It responds again (or is gone).
    End(isize),
}

/// Debounce state, by window handle. Pure: the caller passes the clock
/// and what `IsHungAppWindow` said.
#[derive(Default)]
pub(super) struct HangWatch {
    /// Hung windows and when they were first seen hung.
    since: HashMap<isize, Instant>,
    /// Hung for DEBOUNCE: shown.
    flagged: HashSet<isize>,
    /// A window seen hung (shown or not yet) responds again or is gone;
    /// cleared by `take_recovered`.
    recovered: bool,
}

impl HangWatch {
    /// `hung_now`: the windows that are hung right now. Every other window
    /// is responding (or gone), so its state is cleared at once.
    pub fn update(&mut self, now: Instant, hung_now: &[isize]) -> Vec<Change> {
        let mut out = Vec::new();
        let before = self.since.len();
        self.since.retain(|h, _| hung_now.contains(h));
        if self.since.len() < before {
            self.recovered = true;
        }
        let mut ended: Vec<isize> = self.flagged.iter().copied().filter(|h| !hung_now.contains(h)).collect();
        ended.sort_unstable();
        for h in ended {
            self.flagged.remove(&h);
            out.push(Change::End(h));
        }
        for &h in hung_now {
            let t = *self.since.entry(h).or_insert(now);
            if now.saturating_duration_since(t) >= DEBOUNCE && self.flagged.insert(h) {
                out.push(Change::Start(h));
            }
        }
        out
    }

    /// Some window stopped hanging since the last call, even one that was
    /// never shown as hung (its ghost may have stood in front meanwhile).
    pub fn take_recovered(&mut self) -> bool {
        std::mem::take(&mut self.recovered)
    }

    pub fn is_flagged(&self, h: isize) -> bool {
        self.flagged.contains(&h)
    }

    /// Nothing runs any more: forget everything (ends are reported).
    pub fn clear(&mut self) -> Vec<Change> {
        self.update(Instant::now(), &[])
    }

    pub fn is_empty(&self) -> bool {
        self.since.is_empty() && self.flagged.is_empty()
    }
}

/// Processes the dock never ends, whatever they own: the idle / System
/// processes, our own, anything in session 0 (services) and the shell and
/// session processes Windows needs to keep running.
pub(super) fn protected(pid: u32, exe: Option<&str>, session: Option<u32>, own: u32) -> bool {
    const NAMES: [&str; 16] = [
        "explorer.exe",
        "csrss.exe",
        "wininit.exe",
        "winlogon.exe",
        "smss.exe",
        "services.exe",
        "lsass.exe",
        "svchost.exe",
        "dwm.exe",
        "fontdrvhost.exe",
        "sihost.exe",
        "ctfmon.exe",
        "shellexperiencehost.exe",
        "startmenuexperiencehost.exe",
        "searchhost.exe",
        "logonui.exe",
    ];
    if pid <= 4 || pid == own || session == Some(0) {
        return true;
    }
    let Some(exe) = exe else { return false };
    let name = exe.rsplit(['\\', '/']).next().unwrap_or(exe).to_lowercase();
    NAMES.contains(&name.as_str()) || name == "nopbar.exe"
}

/// Full path of the exe of the process behind handle `h`.
unsafe fn exe_of(h: windows::Win32::Foundation::HANDLE) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok().then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

/// When the process behind handle `h` started (FILETIME as u64).
unsafe fn start_of(h: windows::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok().then(|| ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
}

/// Full path of process `pid`'s exe (None if it cannot be asked).
pub(super) fn process_exe(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let exe = exe_of(h);
        let _ = CloseHandle(h);
        exe
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EndResult {
    Ended,
    /// TerminateProcess itself failed.
    Failed,
    /// May not open it (elevated, protected).
    Denied,
    /// Not the process the menu offered any more, recovered, or protected.
    Cancelled(&'static str),
}

/// Ends process `pid` only if it is still the one the menu offered.
/// One handle is opened first (terminate + query) and every check runs on
/// it, then the same handle ends it: while we hold it the process object
/// (and its id) cannot be reused, so nothing else can be ended by mistake.
pub(super) unsafe fn end_verified(pid: u32, start: u64, hw: HWND) -> EndResult {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetProcessId, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE};
    let Ok(h) = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return EndResult::Denied };
    let verdict = (|| {
        if GetProcessId(h) != pid {
            return Err("handle names another process");
        }
        if start_of(h) != Some(start) {
            return Err("process id reused");
        }
        let Some(exe) = exe_of(h) else { return Err("exe unknown") };
        // (By id, but the id is pinned to this process while `h` is open.)
        let Some(session) = session_of(pid) else { return Err("session unknown") };
        if protected(pid, Some(&exe), Some(session), std::process::id()) {
            return Err("protected process");
        }
        let mut owner = 0u32;
        GetWindowThreadProcessId(hw, Some(&mut owner));
        if owner != pid {
            return Err("window has another owner");
        }
        if !IsHungAppWindow(hw).as_bool() {
            return Err("responding again");
        }
        Ok(())
    })();
    let out = match verdict {
        Ok(()) => {
            if TerminateProcess(h, 1).is_ok() {
                EndResult::Ended
            } else {
                EndResult::Failed
            }
        }
        Err(why) => EndResult::Cancelled(why),
    };
    let _ = CloseHandle(h);
    out
}

pub(super) fn session_of(pid: u32) -> Option<u32> {
    let mut s = 0u32;
    unsafe { windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(pid, &mut s).is_ok().then_some(s) }
}

/// Whether we may end `pid` (opens and closes a PROCESS_TERMINATE handle;
/// nothing is ended). False for elevated apps when we are not.
pub(super) fn can_terminate(pid: u32) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE};
    unsafe {
        match OpenProcess(PROCESS_TERMINATE, false, pid) {
            Ok(h) => {
                let _ = windows::Win32::Foundation::CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

/// `protected()` for a live process.
pub(super) fn is_protected(pid: u32) -> bool {
    protected(pid, process_exe(pid).as_deref(), session_of(pid), std::process::id())
}

impl Dock {
    /// The window list changed: start or stop the check timer and look now.
    pub(super) fn hang_sync(&mut self) {
        full_quit::warm(self.items.iter().filter(|it| it.kind == Kind::App && !it.stack && full_quit::should_warm(it.pinned, it.windows.len(), it.tray.len()))
            .filter_map(|it| it.id.exe.as_deref().and_then(|exe| full_quit::target(exe, it.id.aumid.as_deref(), it.web.is_some(), it.path.to_lowercase().starts_with("shell:appsfolder")))).collect());
        let running = self.items.iter().any(|it| it.kind == Kind::App && !it.windows.is_empty());
        // Locked, asleep or the display off: no timer (session.rs calls
        // this again once the user is back).
        let idle = session::idle();
        unsafe {
            if running && !idle {
                if !self.hang_timer {
                    SetTimer(Some(self.hwnd), HUNG_TIMER, HUNG_CHECK_MS, None);
                    self.hang_timer = true;
                }
            } else if self.hang_timer {
                let _ = KillTimer(Some(self.hwnd), HUNG_TIMER);
                self.hang_timer = false;
            }
        }
        if running {
            self.check_hangs();
        } else if !self.hang.is_empty() {
            let changes = self.hang.clear();
            self.apply_hangs(&changes);
        }
    }

    /// HUNG_TIMER / window-list refresh: ask Windows about every window.
    pub(super) fn check_hangs(&mut self) {
        let hung: Vec<isize> = self
            .items
            .iter()
            .filter(|it| it.kind == Kind::App)
            .flat_map(|it| it.windows.iter())
            .filter(|&&h| unsafe { IsHungAppWindow(h).as_bool() })
            .map(|h| h.0 as isize)
            .collect();
        let changes = self.hang.update(Instant::now(), &hung);
        self.apply_hangs(&changes);
    }

    /// Logs the changes and brings icons, badges and labels up to date.
    fn apply_hangs(&mut self, changes: &[Change]) {
        let name_of = |d: &Dock, h: isize| {
            d.items.iter().find(|it| it.windows.contains(&HWND(h as *mut _))).map(|it| it.name.clone()).unwrap_or_else(|| "?".into())
        };
        for c in changes {
            match *c {
                Change::Start(h) => crate::log!("dock: {} not responding (window {h:#x})", name_of(self, h)),
                Change::End(h) => crate::log!("dock: {} responding again or closed (window {h:#x})", name_of(self, h)),
            }
        }
        // A hung window's ghost stood in front, and the app's own window
        // takes its place again without a foreground event: look again
        // (a game that went fullscreen while loading is noticed now). Not
        // tied to the badge's debounce: a short hang counts too.
        if self.hang.take_recovered() {
            schedule_fullscreen_check();
        }
        let mut changed = false;
        for it in &mut self.items {
            let hung = it.kind == Kind::App && it.windows.iter().any(|h| self.hang.is_flagged(h.0 as isize));
            if hung != it.hung {
                it.hung = hung;
                changed = true;
            }
        }
        if changed {
            self.refresh_marks();
            self.layout();
            self.layout_label();
        }
    }

    /// The flagged windows of item `i` that are still hung.
    pub(super) fn hung_windows(&self, i: usize) -> Vec<HWND> {
        let Some(it) = self.items.get(i) else { return Vec::new() };
        it.windows.iter().copied().filter(|h| self.hang.is_flagged(h.0 as isize) && unsafe { IsHungAppWindow(*h).as_bool() }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hang_shows_after_debounce_and_clears_at_once() {
        let mut w = HangWatch::default();
        let t0 = Instant::now();
        assert!(w.update(t0, &[1]).is_empty());
        assert!(w.update(t0 + Duration::from_millis(2900), &[1]).is_empty(), "not yet");
        assert_eq!(w.update(t0 + Duration::from_secs(3), &[1]), vec![Change::Start(1)]);
        assert!(w.is_flagged(1));
        assert!(w.update(t0 + Duration::from_secs(5), &[1]).is_empty(), "reported once");
        assert!(!w.take_recovered(), "still hung");
        assert_eq!(w.update(t0 + Duration::from_secs(6), &[]), vec![Change::End(1)]);
        assert!(w.take_recovered());
        assert!(!w.is_flagged(1));
        assert!(w.is_empty());
    }

    #[test]
    fn a_short_hiccup_restarts_the_wait() {
        let mut w = HangWatch::default();
        let t0 = Instant::now();
        w.update(t0, &[7]);
        // Responds for a moment before the debounce ends: nothing shown, no End.
        assert!(w.update(t0 + Duration::from_secs(2), &[]).is_empty());
        // But the recovery is told (the fullscreen check looks again).
        assert!(w.take_recovered());
        assert!(!w.take_recovered(), "told once");
        assert!(w.update(t0 + Duration::from_secs(4), &[7]).is_empty(), "the clock starts over");
        assert_eq!(w.update(t0 + Duration::from_secs(7), &[7]), vec![Change::Start(7)]);
    }

    #[test]
    fn windows_are_tracked_apart() {
        let mut w = HangWatch::default();
        let t0 = Instant::now();
        w.update(t0, &[1]);
        w.update(t0 + Duration::from_secs(2), &[1, 2]);
        assert_eq!(w.update(t0 + Duration::from_secs(3), &[1, 2]), vec![Change::Start(1)]);
        assert_eq!(w.update(t0 + Duration::from_secs(5), &[2]), vec![Change::End(1), Change::Start(2)]);
        assert_eq!(w.clear(), vec![Change::End(2)]);
        assert!(w.is_empty());
    }

    #[test]
    fn protected_processes_are_never_ended() {
        let own = 4242;
        assert!(protected(0, None, None, own));
        assert!(protected(4, Some("System"), Some(0), own));
        assert!(protected(own, Some("C:\\Apps\\NopBar\\nopbar.exe"), Some(1), own));
        assert!(protected(900, Some("C:\\Windows\\explorer.exe"), Some(1), own));
        assert!(protected(900, Some("C:\\Windows\\EXPLORER.EXE"), Some(1), own), "case");
        assert!(protected(900, Some("C:\\Windows\\System32\\dwm.exe"), Some(1), own));
        assert!(protected(900, Some("C:\\Somewhere\\other.exe"), Some(0), own), "session 0 is services");
        assert!(protected(900, Some("D:\\x\\nopbar.exe"), Some(1), own), "another NopBar copy");
        assert!(!protected(900, Some("C:\\Program Files\\App\\app.exe"), Some(1), own));
        assert!(!protected(900, Some("C:\\Program Files\\Explorer++\\Explorer++.exe"), Some(1), own), "name must match exactly");
        assert!(!protected(900, None, Some(1), own), "unknown exe: the open check decides");
    }
}
