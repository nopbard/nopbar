//! UI-visible metadata only. Vault reads and writes stay on worker threads.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use windows::Win32::Foundation::HWND;
#[derive(Clone, Default, PartialEq)]
pub struct Snapshot {
    pub key: Option<String>,
    pub label: String,
    pub accounts: Vec<String>,
}
static CACHE: Mutex<Option<Snapshot>> = Mutex::new(None);
static STARTED: AtomicBool = AtomicBool::new(false);
static OBSERVERS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
pub const CHANGED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 182;
#[cfg(test)]
thread_local! { static TEST_SNAPSHOT: std::cell::RefCell<Option<Snapshot>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
pub struct SnapshotGuard(Option<Snapshot>);
#[cfg(test)]
impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        TEST_SNAPSHOT.with(|v| *v.borrow_mut() = self.0.take());
    }
}
#[cfg(test)]
pub fn test_snapshot(s: Snapshot) -> SnapshotGuard {
    SnapshotGuard(TEST_SNAPSHOT.with(|v| v.borrow_mut().replace(s)))
}
pub fn subscribe(h: HWND) {
    let mut observers = OBSERVERS.lock().unwrap_or_else(|e| e.into_inner());
    if !observers.contains(&(h.0 as isize)) {
        observers.push(h.0 as isize);
    }
    drop(observers);
    ensure();
}
pub fn ensure() {
    if !STARTED.swap(true, Ordering::SeqCst) {
        #[cfg(not(test))]
        refresh();
    }
}
pub fn snapshot() -> Snapshot {
    #[cfg(test)]
    if let Some(s) = TEST_SNAPSHOT.with(|v| v.borrow().clone()) {
        return s;
    }
    ensure();
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}
fn publish(result: super::credentials::Result<Snapshot>) {
    let Ok(next) = result else {
        return;
    }; // A transient vault error is not sign-out.
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.as_ref() == Some(&next) {
        return;
    }
    *cache = Some(next);
    drop(cache);
    let mut observers = OBSERVERS.lock().unwrap_or_else(|e| e.into_inner());
    observers.retain(|h| unsafe {
        windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(HWND(*h as *mut _)),
            CHANGED,
            Default::default(),
            Default::default(),
        )
        .is_ok()
    });
}
pub fn refresh() {
    std::thread::spawn(|| {
        let result = (|| {
            let _lock = super::credentials::Lock::take()?;
            let a = super::auth::load()?;
            let r = a.registrations.get(a.active);
            let next = Snapshot {
                key: r
                    .filter(|r| r.tokens.is_some())
                    .map(|r| format!("{}:{}", r.client_id, r.subject)),
                label: r
                    .map(|r| {
                        format!(
                            "{} · {}{}",
                            r.name,
                            r.client_id,
                            if r.tokens.is_some() {
                                ""
                            } else {
                                crate::i18n::t("chat.disconnected_suffix")
                            }
                        )
                    })
                    .unwrap_or_else(|| crate::i18n::t("control.not_connected").into()),
                accounts: a
                    .registrations
                    .iter()
                    .map(|r| format!("{} · {}", r.name, r.client_id))
                    .collect(),
            };
            // Publish before releasing the vault lock so reloads cannot reorder.
            publish(Ok(next));
            Ok(())
        })();
        let _ = result as super::credentials::Result<()>;
    });
}
pub fn changed() {
    refresh();
    std::thread::spawn(|| super::hint::notify_kind("CompanionAuthChanged"));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_reads_do_not_sign_out_and_cached_reads_do_not_load_the_vault() {
        // Test the cache policy in isolation; no test opens Credential Manager.
        let before = snapshot();
        publish(Err("temporary read error"));
        assert!(snapshot() == before);
        let source = include_str!("session.rs");
        let body = source
            .split("pub fn snapshot()")
            .nth(1)
            .unwrap()
            .split("fn publish")
            .next()
            .unwrap();
        assert!(!body.contains("auth::load"));
    }
}
