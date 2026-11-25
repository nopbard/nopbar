//! The window's install, on a worker thread: the inner installer runs
//! silently with the updater's switches, its log is read for progress,
//! and the exit code decides.

use std::path::Path;
use std::time::Duration;

use crate::payload;
use crate::progress;
use crate::strings::Failure;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// What the installer's log says so far (0.0..0.97, never backwards).
    Progress(f32),
    Finished(Result<(), Failure>),
}

/// Where a failed run's log is kept (the temp folder goes): next to the
/// temp folders, for a bug report.
pub const FAILED_LOG: &str = "NopBar-setup-failed.log";

/// Installs from `bytes` through a folder in `root`; `notify` hears the
/// steps; `after_success` runs before the last event (it makes sure NopBar
/// runs).
pub fn install(bytes: &[u8], version: &str, root: &Path, mut notify: impl FnMut(Event), after_success: impl FnOnce()) {
    if bytes.is_empty() {
        notify(Event::Finished(Err(Failure::NoPayload)));
        return;
    }
    payload::sweep(root);
    let Ok(x) = payload::extract(bytes, root, &payload::inner_name(version)) else {
        notify(Event::Finished(Err(Failure::Extract)));
        return;
    };
    let log = x.dir.join("setup.log");
    let Ok(mut child) = payload::silent_command(&x.exe, &log).spawn() else {
        notify(Event::Finished(Err(Failure::Start)));
        return;
    };
    let mut last = -1.0;
    let code = loop {
        let done = child.try_wait();
        let f = progress::fraction(&progress::read_log(&log));
        if f > last {
            notify(Event::Progress(f));
            last = f;
        }
        match done {
            Ok(Some(s)) => break s.code().unwrap_or(1),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break child.wait().ok().and_then(|s| s.code()).unwrap_or(1),
        }
    };
    if code != 0 {
        let _ = std::fs::copy(&log, root.join(FAILED_LOG));
    }
    drop(x);
    if code == 0 {
        after_success();
        notify(Event::Finished(Ok(())));
    } else {
        notify(Event::Finished(Err(Failure::Code(code))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::tests::{cmd_exe, Scratch};

    #[test]
    fn no_payload_says_so() {
        let s = Scratch::new("install-none");
        let mut ev = Vec::new();
        install(b"", "9.9.9", &s.0, |e| ev.push(e), || panic!("nothing was installed"));
        assert_eq!(ev, [Event::Finished(Err(Failure::NoPayload))]);
    }

    #[test]
    fn exit_codes_decide_and_nothing_is_left_behind() {
        // cmd.exe given the installer's switches: it ignores them and,
        // with no input, exits 0, like a good install. (after_success is
        // a stub: tests never start NopBar.)
        let s = Scratch::new("install-ok");
        let mut ev = Vec::new();
        let mut started = false;
        install(&cmd_exe(), "9.9.9", &s.0, |e| ev.push(e), || started = true);
        assert!(started);
        assert_eq!(ev.last(), Some(&Event::Finished(Ok(()))));
        let fractions: Vec<f32> = ev.iter().filter_map(|e| if let Event::Progress(f) = e { Some(*f) } else { None }).collect();
        assert!(fractions.windows(2).all(|w| w[0] < w[1]), "{fractions:?}");
        assert!(s.entries().is_empty(), "{:?}", s.entries());
    }
}
