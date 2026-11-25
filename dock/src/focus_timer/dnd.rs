//! Coalesced, short COM worker. Clock owns the Windows Focus UI; our
//! fallback owns only the DND change it made for the running phase.
use std::sync::{atomic::{AtomicBool, AtomicI64, Ordering}, Mutex, Condvar};
use super::{lease::{self, Lease}, quiet_hours::{Settings, OFF}};
pub const NONE: i64 = i64::MIN;
static HAVE: AtomicI64 = AtomicI64::new(NONE);
static OURS: AtomicBool = AtomicBool::new(false);
static UNSUPPORTED: AtomicBool = AtomicBool::new(false);
static CLOSED: AtomicBool = AtomicBool::new(false);
static LEASE: Mutex<Option<Lease>> = Mutex::new(None);
static RECOVERED: AtomicBool = AtomicBool::new(false);
static RELINQUISHED: AtomicBool = AtomicBool::new(false);
static WAKE_AT: AtomicI64 = AtomicI64::new(NONE);
static WATCH: std::sync::OnceLock<Option<crate::service::Id>> = std::sync::OnceLock::new();
static WORK: Mutex<(bool, u64)> = Mutex::new((false, 0));
static FINISHED: Condvar = Condvar::new();

fn wish() -> i64 {
    let deadline = super::dnd_wish();
    if CLOSED.load(Ordering::SeqCst) || deadline <= super::now_ms() { NONE } else { deadline }
}
pub fn ours() -> bool { OURS.load(Ordering::SeqCst) }
pub fn unsupported() -> bool { UNSUPPORTED.load(Ordering::SeqCst) }

pub fn kick() {
    // Unit tests never change Windows settings or start helpers.
    if cfg!(test) { return; }
    let mut work = WORK.lock().unwrap_or_else(|e| e.into_inner());
    work.1 = work.1.wrapping_add(1);
    if work.0 || (RECOVERED.load(Ordering::SeqCst) && wish() == HAVE.load(Ordering::SeqCst) && !(wish() == NONE && ours())) { return; }
    work.0 = true;
    if std::thread::Builder::new().name("focus-dnd".into()).spawn(run).is_err() { work.0 = false; FINISHED.notify_all(); }
}

fn run() {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    loop {
        let revision = WORK.lock().unwrap_or_else(|e| e.into_inner()).1;
        sync();
        let mut work = WORK.lock().unwrap_or_else(|e| e.into_inner());
        if work.1 == revision {
            work.0 = false;
            FINISHED.notify_all();
            break;
        }
    }
    if initialized { unsafe { CoUninitialize(); } }
}

fn file() -> std::path::PathBuf { crate::config::app_dir().join("focus_dnd_lease.json") }

fn manual_recovery_record(path: &std::path::Path) -> std::io::Result<Option<Lease>> {
    match Lease::load(path) {
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            crate::log!("focus timer: explicit DND selection superseded an invalid recovery record ({e})");
            Ok(None)
        }
        result => result,
    }
}

#[cfg(test)]
#[test]
fn explicit_dnd_selection_can_supersede_corrupt_journals() {
    let dir = std::env::temp_dir().join(format!("nopbar-manual-dnd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("lease.json");
    std::fs::write(&path, b"{").unwrap();
    assert_eq!(Lease::load(&path).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(manual_recovery_record(&path).unwrap(), None);
    lease::clear(&path).unwrap();
    let valid = Lease::new(1, 10);
    valid.save(&path).unwrap();
    assert_eq!(manual_recovery_record(&path).unwrap(), Some(valid));
    lease::clear(&path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

/// Serialize an explicit user choice with acquisition, recovery and restoration.
pub(crate) fn set_manually(on: bool) -> Result<(), String> {
    if cfg!(test) { return Err("DND writes are disabled in tests".into()); }
    let mut owned = LEASE.lock().unwrap_or_else(|e| e.into_inner());
    let settings = Settings::new().map_err(|e| e.to_string())?;
    let path = file();
    let stale = !RECOVERED.load(Ordering::SeqCst);
    // A manual selection supersedes even a lease left by an earlier process.
    // Retire its journal before an On write, so recovery cannot undo the choice.
    let retire_before = on && (owned.is_some() || stale);
    if retire_before {
        let old = if stale { manual_recovery_record(&path).map_err(|e| e.to_string())? } else { owned.clone() };
        lease::clear(&path).map_err(|e| e.to_string())?;
        if let Err(error) = settings.set(super::quiet_hours::ON) {
            if let Some(record) = old {
                if let Err(e) = record.save(&path) {
                    crate::log!("focus timer: DND override could not restore the recovery record ({e})");
                }
            }
            return Err(error.to_string());
        }
        *owned = None;
        OURS.store(false, Ordering::SeqCst);
        RECOVERED.store(true, Ordering::SeqCst);
    } else {
        settings.set(if on { super::quiet_hours::ON } else { OFF }).map_err(|e| e.to_string())?;
    }
    RELINQUISHED.store(wish() != NONE, Ordering::SeqCst);
    if !on && (owned.is_some() || stale) {
        match lease::clear(&path) {
            Ok(()) => {
                *owned = None;
                OURS.store(false, Ordering::SeqCst);
                RECOVERED.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                crate::log!("focus timer: manual DND override left a recovery record ({e})");
                HAVE.store(NONE, Ordering::SeqCst);
            }
        }
    }
    drop(owned);
    kick();
    crate::notification_state::changed();
    crate::topbar::island::refresh();
    Ok(())
}

fn sync() {
    let mut want = wish();
    let mut owned = LEASE.lock().unwrap_or_else(|e| e.into_inner());
    let result = (|| -> Result<(), String> {
        let path = file();
        if !RECOVERED.load(Ordering::SeqCst) {
            // A timer is not resumed after a process restart: recover even if its
            // recorded deadline is in the future. Keep the file on any failure.
            if let Some(old) = Lease::load(&path).map_err(|e| e.to_string())? {
                let settings = Settings::new().map_err(|e| e.to_string())?;
                let current = settings.get().map_err(|e| e.to_string())?;
                old.restore_and_clear(&path, &current, |s| settings.set(s).map_err(|e| e.to_string()))?;
            }
            RECOVERED.store(true, Ordering::SeqCst);
        }
        if want == HAVE.load(Ordering::SeqCst) && !(want == NONE && owned.is_some()) { return Ok(()); }
        let settings = Settings::new().map_err(|e| e.to_string())?;
        let current = settings.get().map_err(|e| e.to_string())?;
        // A slow COM activation may have crossed the phase deadline.
        want = wish();
        if want == NONE { RELINQUISHED.store(false, Ordering::SeqCst); }
        if let Some(record) = owned.as_mut() {
            // A changed profile revokes ownership. Do not turn DND back on in
            // the same phase after the user has disabled/changed it.
            if want == NONE || record.expired(super::now_ms()) || current != record.profile {
                if want != NONE && current != record.profile { RELINQUISHED.store(true, Ordering::SeqCst); }
                record.restore_and_clear(&path, &current, |s| settings.set(s).map_err(|e| e.to_string()))?;
                *owned = None;
            } else if record.deadline_ms != want {
                let mut updated = record.clone(); updated.deadline_ms = want;
                updated.save(&path).map_err(|e| e.to_string())?;
                *record = updated;
            }
        } else if want != NONE && current == OFF && !RELINQUISHED.load(Ordering::SeqCst) {
            lease::acquire(&mut owned, &path, super::now_ms(), want, |s| settings.set(s).map_err(|e| e.to_string()))?;
        }
        Ok(())
    })();
    OURS.store(owned.is_some(), Ordering::SeqCst);
    let retry = result.is_err();
    match result {
        Ok(()) => { HAVE.store(want, Ordering::SeqCst); UNSUPPORTED.store(false, Ordering::SeqCst); }
        Err(e) => {
            UNSUPPORTED.store(true, Ordering::SeqCst);
            crate::log!("focus timer: QuietHours unavailable ({e}); recovery record retained");
        }
    }
    drop(owned);
    // An independent deadline also repairs DND if the timer UI/watch failed.
    // Failed COM/disk recovery is retried slowly with the journal intact.
    WAKE_AT.store(if retry { super::now_ms() + 30_000 } else { want }, Ordering::SeqCst);
    if let Some(id) = WATCH.get_or_init(|| crate::service::add(Box::new(Deadline))) {
        crate::service::poke(*id);
    }
    crate::notification_state::changed();
    crate::topbar::island::refresh();
}

struct Deadline;
impl crate::service::Watch for Deadline {
    fn name(&self) -> &'static str { "focus-dnd-recovery" }
    fn handles(&self) -> Vec<windows::Win32::Foundation::HANDLE> { Vec::new() }
    fn wake(&mut self, _: crate::service::Wake) -> crate::service::Next {
        let at = WAKE_AT.load(Ordering::SeqCst);
        if at == NONE { return crate::service::Next::Wait; }
        let left = at - super::now_ms();
        if left > 0 { return crate::service::Next::Sleep(std::time::Duration::from_millis(left as u64)); }
        WAKE_AT.store(NONE, Ordering::SeqCst);
        kick();
        crate::service::Next::Wait
    }
}

pub fn resume_after_cancel() {
    CLOSED.store(false, Ordering::SeqCst);
    HAVE.store(NONE, Ordering::SeqCst);
    kick();
}

pub fn shutdown(wait: std::time::Duration) {
    if cfg!(test) { return; }
    CLOSED.store(true, Ordering::SeqCst);
    kick();
    let work = WORK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = FINISHED.wait_timeout_while(work, wait, |w| w.0);
    if ours() { crate::log!("focus timer: DND restoration incomplete at exit"); }
}
