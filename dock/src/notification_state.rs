//! UI callers read atomics only. At most one external COM query is in flight;
//! a timed-out query cannot publish stale data or create a worker storm.
use std::{sync::{atomic::{AtomicBool, Ordering}, mpsc, OnceLock}, time::{Duration, Instant}};
use windows::UI::{Notifications::{ToastNotificationManager, ToastNotificationManagerForUser}, Shell::FocusSessionManager};
use windows::Win32::{System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED}, UI::Shell::{SHQueryUserNotificationState, QUNS_ACCEPTS_NOTIFICATIONS}};
use crate::focus_timer::quiet_hours::Settings;
use crate::service::{self, Next, Wake, Watch};
const INTERVAL: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(2);
static QUIET: AtomicBool = AtomicBool::new(false);
static WATCH: OnceLock<Option<service::Id>> = OnceLock::new();
static DIRTY: AtomicBool = AtomicBool::new(false);

pub fn quiet() -> bool {
    QUIET.load(Ordering::Acquire)
}
pub fn start() {
    if cfg!(test) { return; }
    WATCH.get_or_init(|| service::add(Box::new(QueryWatch { worker: None, pending: None, next: Instant::now() })));
}
pub fn changed() {
    DIRTY.store(true, Ordering::Release);
    if let Some(Some(id)) = WATCH.get() { service::poke(*id); }
}
#[derive(Clone, Copy, Default)]
struct Sample { shell: bool, focus: bool, dnd: bool, our_profile: bool }
impl Sample {
    fn quiet(self) -> bool { self.shell || self.focus || self.dnd && !self.our_profile }
}
fn dnd_policy(mode: Option<windows::UI::Notifications::ToastNotificationMode>, profile: Option<&str>, owned: bool) -> (bool, bool) {
    use windows::UI::Notifications::ToastNotificationMode;
    use crate::focus_timer::quiet_hours::{OFF, ON};
    let dnd = mode.map_or_else(|| profile.is_some_and(|p| p != OFF), |m| m != ToastNotificationMode::Unrestricted);
    // An automatic/user AlarmsOnly mode can override the selected profile.
    let ours = owned && profile == Some(ON) && mode.is_none_or(|m| m == ToastNotificationMode::PriorityOnly);
    (dnd, ours)
}
struct Pending { result: mpsc::Receiver<Sample>, at: Instant, expired: bool }
impl Pending {
    fn take(&mut self, now: Instant) -> (bool, Option<Sample>) {
        self.expired |= now.saturating_duration_since(self.at) >= TIMEOUT;
        match self.result.try_recv() {
            Ok(sample) => (true, (!self.expired).then_some(sample)),
            Err(mpsc::TryRecvError::Disconnected) => (true, None),
            Err(mpsc::TryRecvError::Empty) => (false, None),
        }
    }
}
type Reply = mpsc::Sender<Sample>;
struct QueryWatch {
    worker: Option<mpsc::SyncSender<Reply>>,
    pending: Option<Pending>,
    next: Instant,
}

fn query_worker(wake: impl Fn() + Send + 'static) -> std::io::Result<(mpsc::SyncSender<Reply>, std::thread::JoinHandle<()>)> {
    let (send, requests) = mpsc::sync_channel::<Reply>(1);
    let worker = std::thread::Builder::new().name("notification-query".into()).spawn(move || {
        let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
        {
            // Keep the apartment alive between polls. WinRT activation
            // factories are cached across calls, including after a thread exits.
            let mut sources = initialized.then(Sources::new);
            for reply in requests {
                let mut sample = sources.as_mut().map(Sources::sample).unwrap_or_default();
                sample.shell = unsafe { SHQueryUserNotificationState() }.is_ok_and(|s| s != QUNS_ACCEPTS_NOTIFICATIONS);
                let _ = reply.send(sample);
                wake();
            }
        }
        if initialized {
            unsafe { RoUninitialize() };
        }
    })?;
    Ok((send, worker))
}

impl Watch for QueryWatch {
    fn name(&self) -> &'static str { "notification-state" }
    fn handles(&self) -> Vec<windows::Win32::Foundation::HANDLE> { Vec::new() }
    fn wake(&mut self, _: Wake) -> Next {
        let now = Instant::now();
        if let Some(p) = &mut self.pending {
            let (finished, sample) = p.take(now);
            if let Some(sample) = sample {
                let quiet = sample.quiet();
                if QUIET.swap(quiet, Ordering::AcqRel) != quiet { crate::companion::refresh(); }
            }
            if finished { self.pending = None; }
        }
        if self.pending.is_none() && (DIRTY.swap(false, Ordering::AcqRel) || now >= self.next) {
            if self.worker.is_none() {
                match query_worker(|| {
                    if let Some(Some(id)) = WATCH.get() { service::poke(*id); }
                }) {
                    Ok((send, _)) => self.worker = Some(send),
                    Err(e) => crate::log!("notification-state: could not start query worker ({e})"),
                }
            }
            let (tx, rx) = mpsc::channel();
            if self.worker.as_ref().is_some_and(|worker| worker.try_send(tx).is_ok()) {
                self.pending = Some(Pending { result: rx, at: now, expired: false });
            } else {
                self.worker = None;
            }
            self.next = now + INTERVAL;
        }
        // Deadline wakes only for a live request, then the normal fallback.
        Next::Sleep(self.pending.as_ref().filter(|p| !p.expired)
            .map_or(INTERVAL, |p| TIMEOUT.saturating_sub(now.saturating_duration_since(p.at))))
    }
}

struct ToastMode {
    manager: ToastNotificationManagerForUser,
    token: Option<i64>,
}

impl Drop for ToastMode {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            let _ = self.manager.RemoveNotificationModeChanged(token);
        }
    }
}

struct Sources {
    supports_focus: bool,
    focus: Option<FocusSessionManager>,
    settings: Option<Settings>,
    toast: Option<ToastMode>,
}

impl Sources {
    fn new() -> Self {
        Self { supports_focus: FocusSessionManager::IsSupported().unwrap_or(false), focus: None, settings: None, toast: None }
    }

    fn sample(&mut self) -> Sample {
        if self.supports_focus && self.focus.is_none() {
            self.focus = FocusSessionManager::GetDefault().ok();
        }
        let focus = self.focus.as_ref().and_then(|manager| manager.IsFocusActive().ok());
        if focus.is_none() { self.focus = None; }

        // An owned flag alone is insufficient: the user may have chosen AlarmsOnly.
        if self.settings.is_none() { self.settings = Settings::new().ok(); }
        let profile = self.settings.as_ref().and_then(|settings| settings.get().ok());
        if profile.is_none() { self.settings = None; }

        if self.toast.is_none() {
            self.toast = ToastNotificationManager::GetDefault().ok().map(|manager| {
                let token = manager.NotificationModeChanged(&windows::Foundation::TypedEventHandler::new(|_, _| {
                    changed();
                    Ok(())
                })).ok();
                ToastMode { manager, token }
            });
        }
        // The periodic poll also covers Windows versions without this event.
        let mode = self.toast.as_ref().and_then(|toast| toast.manager.NotificationMode().ok());
        if mode.is_none() { self.toast = None; }
        let (dnd, our_profile) = dnd_policy(mode, profile.as_deref(), crate::focus_timer::owns_dnd());
        Sample { shell: false, focus: focus.unwrap_or(false), dnd, our_profile }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "samples Windows notification status in the isolated test harness"]
    fn notification_query_handle_probe() {
        let (worker, thread) = query_worker(|| {}).unwrap();
        let run = |count| {
            for _ in 0..count {
                let (tx, rx) = mpsc::channel();
                worker.send(tx).unwrap();
                rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        };
        let sample = || {
            let u = crate::res::usage();
            (u.handles, u.threads)
        };
        run(8);
        let before = sample();
        run(32);
        let middle = sample();
        run(32);
        let after = sample();
        drop(worker);
        thread.join().unwrap();
        println!("query handles and threads: {before:?}, {middle:?}, {after:?}");
        assert!(after.0 <= before.0 + 4, "handle count grew after warmup");
        assert!(after.1 <= before.1 + 1, "thread count grew after warmup");
    }
    #[test]
    fn alarms_only_override_is_not_mistaken_for_our_priority_profile() {
        use windows::UI::Notifications::ToastNotificationMode as Mode;
        use crate::focus_timer::quiet_hours::{ON, OFF};
        assert_eq!(dnd_policy(Some(Mode::PriorityOnly), Some(ON), true), (true, true));
        assert_eq!(dnd_policy(Some(Mode::AlarmsOnly), Some(ON), true), (true, false));
        assert_eq!(dnd_policy(Some(Mode::PriorityOnly), Some(ON), false), (true, false));
        assert_eq!(dnd_policy(None, Some(ON), true), (true, true));
        assert_eq!(dnd_policy(None, Some(OFF), true), (false, false));
    }
    #[test]
    fn own_dnd_is_excluded_but_user_focus_presentation_and_profile_change_are_not() {
        assert!(!Sample { dnd: true, our_profile: true, ..Default::default() }.quiet());
        assert!(Sample { dnd: true, ..Default::default() }.quiet());
        assert!(Sample { shell: true, dnd: true, our_profile: true, ..Default::default() }.quiet());
        assert!(Sample { focus: true, dnd: true, our_profile: true, ..Default::default() }.quiet());
    }
    #[test]
    fn hung_query_never_blocks_and_late_result_is_discarded() {
        let (tx, rx) = mpsc::channel(); let at = Instant::now();
        let mut pending = Pending { result: rx, at, expired: false };
        assert_eq!(pending.take(at).0, false);
        assert_eq!(pending.take(at + TIMEOUT).0, false);
        assert!(pending.expired);
        tx.send(Sample { dnd: true, ..Default::default() }).unwrap();
        let (done, result) = pending.take(at + TIMEOUT);
        assert!(done); assert!(result.is_none());
    }
    #[test]
    fn query_before_deadline_publishes_and_ui_read_is_independent() {
        let (tx, rx) = mpsc::channel(); let at = Instant::now();
        let mut pending = Pending { result: rx, at, expired: false };
        tx.send(Sample { focus: true, ..Default::default() }).unwrap();
        assert!(pending.take(at).1.unwrap().quiet());
        for _ in 0..1000 { assert!(!quiet()); }
    }
}
