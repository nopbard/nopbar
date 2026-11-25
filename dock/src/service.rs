//! The service thread: one thread for every long wait the process has.
//!
//! Many things only sleep until a kernel object is signalled (config.json
//! changed, explorer exited, the GPU was removed, a registry key changed, a
//! settings window connected) or until a time comes (the next update check,
//! a delayed clean-up). Each used to have a thread of its own. Here they are
//! `Watch`es on one thread that sleeps in WaitForMultipleObjects on all
//! their handles at once, with the nearest deadline as the timeout. Zero
//! CPU while nothing happens.
//!
//! A watch's `wake` runs on the service thread and must return quickly:
//! anything slow (a network check, a COM call into another process) goes to
//! a thread of its own (see `crate::idle`). The thread starts with the first
//! watch and ends when the last one is done.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED_0, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject, INFINITE};

/// Why a watch is woken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// Just added (on the service thread).
    Start,
    /// Handle `i` of the ones `handles()` gave was signalled.
    Signaled(usize),
    /// The time asked for came.
    Timer,
    /// Someone called `poke` for it.
    Poked,
}

/// What a watch wants next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Wait on its handles, no deadline.
    Wait,
    /// Do not wait on its handles until this long passed (then Wake::Timer):
    /// settling after a burst of changes.
    Sleep(Duration),
    /// Finished: the watch is dropped (its Drop closes its handles).
    Done,
}

pub trait Watch: Send {
    /// For the log.
    fn name(&self) -> &'static str;
    /// The handles to wait on now (asked again after every wake).
    fn handles(&self) -> Vec<HANDLE>;
    fn wake(&mut self, why: Wake) -> Next;
}

/// A watch added to the service (for `poke`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id(u64);

/// Handles one wait can take, less the service's own wake event.
const MAX_HANDLES: usize = 63;

struct Shared {
    adds: Vec<(u64, Box<dyn Watch>)>,
    pokes: Vec<u64>,
    running: bool,
    /// Auto-reset event that wakes the thread for adds and pokes (0 = none yet).
    event: isize,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared { adds: Vec::new(), pokes: Vec::new(), running: false, event: 0 });
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn shared() -> MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Adds `w`; its `wake(Wake::Start)` runs on the service thread soon. None
/// if the service could not be started (the watch is dropped).
pub fn add(w: Box<dyn Watch>) -> Option<Id> {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let mut s = shared();
    if s.event == 0 {
        match unsafe { CreateEventW(None, false, false, None) } {
            Ok(h) => s.event = h.0 as isize,
            Err(e) => {
                crate::log!("service: no wake event ({e}); {} not watched", w.name());
                return None;
            }
        }
    }
    s.adds.push((id, w));
    if s.running {
        unsafe {
            let _ = SetEvent(HANDLE(s.event as *mut _));
        }
        return Some(Id(id));
    }
    // Started with the lock held: nobody else can add (and be told yes)
    // before the start is known to have worked; the new thread waits for
    // the lock. On failure the only pending add is this call's own.
    let event = s.event;
    if let Err(e) = std::thread::Builder::new().name("service".into()).spawn(move || run(event)) {
        let gone = s.adds.iter().position(|(i, _)| *i == id).map(|i| s.adds.remove(i));
        drop(s);
        crate::log!("service: cannot start the thread ({e})");
        drop(gone);
        return None;
    }
    s.running = true;
    Some(Id(id))
}

/// Wakes watch `id` with Wake::Poked (nothing if it is done).
pub fn poke(id: Id) {
    let mut s = shared();
    if !s.pokes.contains(&id.0) {
        s.pokes.push(id.0);
    }
    if s.event != 0 {
        unsafe {
            let _ = SetEvent(HANDLE(s.event as *mut _));
        }
    }
}

/// Runs `f` on the service thread once `after` has passed. `f` must be
/// quick (it holds up every other watch while it runs).
pub fn after(name: &'static str, after: Duration, f: impl FnOnce() + Send + 'static) -> Option<Id> {
    struct Once<F: FnOnce() + Send> {
        name: &'static str,
        after: Duration,
        f: Option<F>,
    }
    impl<F: FnOnce() + Send> Watch for Once<F> {
        fn name(&self) -> &'static str {
            self.name
        }
        fn handles(&self) -> Vec<HANDLE> {
            Vec::new()
        }
        fn wake(&mut self, why: Wake) -> Next {
            match why {
                Wake::Start => Next::Sleep(self.after),
                _ => {
                    if let Some(f) = self.f.take() {
                        f();
                    }
                    Next::Done
                }
            }
        }
    }
    add(Box::new(Once { name, after, f: Some(f) }))
}

struct Entry {
    id: u64,
    watch: Box<dyn Watch>,
    deadline: Option<Instant>,
    /// Not waiting on its handles until the deadline.
    sleeping: bool,
}

/// Applies a watch's answer. False: drop it.
fn apply(e: &mut Entry, next: Next, now: Instant) -> bool {
    match next {
        Next::Wait => {
            e.deadline = None;
            e.sleeping = false;
        }
        Next::Sleep(d) => {
            e.deadline = Some(now + d);
            e.sleeping = true;
        }
        Next::Done => return false,
    }
    true
}

/// Milliseconds to wait for the nearest deadline (INFINITE if none).
pub fn timeout_ms(deadlines: impl Iterator<Item = Option<Instant>>, now: Instant) -> u32 {
    match deadlines.flatten().min() {
        None => INFINITE,
        Some(d) => {
            let ms = d.saturating_duration_since(now).as_millis();
            // Round up: waking a hair early would only wait again.
            let ms = if d > now && ms == 0 { 1 } else { ms };
            ms.min((INFINITE - 1) as u128) as u32
        }
    }
}

/// Which (entry, handle-of-entry) a flat wait index points at, given each
/// entry's handle count in wait order.
pub fn locate(counts: &[usize], mut idx: usize) -> Option<(usize, usize)> {
    for (e, &n) in counts.iter().enumerate() {
        if idx < n {
            return Some((e, idx));
        }
        idx -= n;
    }
    None
}

fn run(event: isize) {
    let wake_ev = HANDLE(event as *mut _);
    let mut entries: Vec<Entry> = Vec::new();
    let mut overflow_logged = false;
    loop {
        let (adds, pokes) = {
            let mut s = shared();
            let adds = std::mem::take(&mut s.adds);
            let pokes = std::mem::take(&mut s.pokes);
            if adds.is_empty() && pokes.is_empty() && entries.is_empty() {
                // Nothing left to watch: end (a later add starts a new one).
                s.running = false;
                return;
            }
            (adds, pokes)
        };
        for (id, mut watch) in adds {
            let next = watch.wake(Wake::Start);
            let mut e = Entry { id, watch, deadline: None, sleeping: false };
            if apply(&mut e, next, Instant::now()) {
                entries.push(e);
            }
        }
        for id in pokes {
            if let Some(i) = entries.iter().position(|e| e.id == id) {
                let next = entries[i].watch.wake(Wake::Poked);
                if !apply(&mut entries[i], next, Instant::now()) {
                    entries.remove(i);
                }
            }
        }
        if entries.is_empty() {
            continue;
        }

        // The wait: our event first, then every awake watch's handles.
        let mut handles = vec![wake_ev];
        let mut counts = Vec::with_capacity(entries.len());
        for e in &entries {
            let hs = if e.sleeping { Vec::new() } else { e.watch.handles() };
            if handles.len() + hs.len() > MAX_HANDLES + 1 {
                if !overflow_logged {
                    overflow_logged = true;
                    crate::log!("service: too many handles; {} is not waited on", e.watch.name());
                }
                counts.push(0);
                continue;
            }
            counts.push(hs.len());
            handles.extend(hs);
        }
        let now = Instant::now();
        let ms = timeout_ms(entries.iter().map(|e| e.deadline), now);
        let r: WAIT_EVENT = unsafe { WaitForMultipleObjects(&handles, false, ms) };
        let now = Instant::now();

        if r == WAIT_TIMEOUT {
            // Fall through to the timers below.
        } else if r == WAIT_FAILED {
            // A watch handed a bad handle: find it and drop that watch
            // rather than spinning on the failure.
            let mut flat = 1;
            let mut bad = Vec::new();
            for (i, &n) in counts.iter().enumerate() {
                for h in &handles[flat..flat + n] {
                    if unsafe { WaitForSingleObject(*h, 0) } == WAIT_FAILED {
                        bad.push(i);
                        break;
                    }
                }
                flat += n;
            }
            if bad.is_empty() {
                crate::log!("service: wait failed; trying again shortly");
                std::thread::sleep(Duration::from_millis(100));
            }
            for i in bad.into_iter().rev() {
                crate::log!("service: {} gave a handle that cannot be waited on; dropped", entries[i].watch.name());
                entries.remove(i);
            }
            continue;
        } else {
            let raw = if r.0 >= WAIT_ABANDONED_0.0 { r.0 - WAIT_ABANDONED_0.0 } else { r.0 - WAIT_OBJECT_0.0 };
            let idx = raw as usize;
            if idx > 0 {
                if let Some((e, h)) = locate(&counts, idx - 1) {
                    let next = entries[e].watch.wake(Wake::Signaled(h));
                    if !apply(&mut entries[e], next, now) {
                        entries.remove(e);
                    }
                }
            }
        }

        // Deadlines that have come (also checked after a signal: a busy
        // handle must not hold a due timer up for long).
        let mut i = 0;
        while i < entries.len() {
            if entries[i].deadline.is_some_and(|d| d <= now) {
                let next = entries[i].watch.wake(Wake::Timer);
                if !apply(&mut entries[i], next, Instant::now()) {
                    entries.remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Sender};
    use windows::Win32::Foundation::CloseHandle;

    #[test]
    fn timeout_is_the_nearest_deadline() {
        let now = Instant::now();
        assert_eq!(timeout_ms([None, None].into_iter(), now), INFINITE);
        let ms = timeout_ms([None, Some(now + Duration::from_millis(500)), Some(now + Duration::from_millis(200))].into_iter(), now);
        assert_eq!(ms, 200);
        assert_eq!(timeout_ms([Some(now)].into_iter(), now), 0, "due now: do not wait");
        assert_eq!(timeout_ms([Some(now + Duration::from_micros(300))].into_iter(), now), 1, "sub-ms rounds up");
    }

    #[test]
    fn flat_index_maps_to_its_watch() {
        let counts = [2, 0, 1, 3];
        assert_eq!(locate(&counts, 0), Some((0, 0)));
        assert_eq!(locate(&counts, 1), Some((0, 1)));
        assert_eq!(locate(&counts, 2), Some((2, 0)));
        assert_eq!(locate(&counts, 3), Some((3, 0)));
        assert_eq!(locate(&counts, 5), Some((3, 2)));
        assert_eq!(locate(&counts, 6), None);
    }

    struct EventWatch {
        ev: isize,
        tx: Sender<Wake>,
        fires: u32,
    }
    impl Watch for EventWatch {
        fn name(&self) -> &'static str {
            "test-event"
        }
        fn handles(&self) -> Vec<HANDLE> {
            vec![HANDLE(self.ev as *mut _)]
        }
        fn wake(&mut self, why: Wake) -> Next {
            let _ = self.tx.send(why);
            if why == Wake::Signaled(0) {
                self.fires += 1;
                if self.fires == 2 {
                    return Next::Done;
                }
            }
            Next::Wait
        }
    }
    impl Drop for EventWatch {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(HANDLE(self.ev as *mut _));
            }
        }
    }

    #[test]
    fn signals_pokes_and_timers_reach_their_watch() {
        let (tx, rx) = channel();
        let ev = unsafe { CreateEventW(None, false, false, None) }.unwrap();
        let raw = ev.0 as isize;
        let id = add(Box::new(EventWatch { ev: raw, tx, fires: 0 })).unwrap();
        let next = || rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next(), Wake::Start);
        unsafe { SetEvent(HANDLE(raw as *mut _)).unwrap() };
        assert_eq!(next(), Wake::Signaled(0));
        poke(id);
        assert_eq!(next(), Wake::Poked);

        let (ttx, trx) = channel();
        let t0 = Instant::now();
        after("test-after", Duration::from_millis(50), move || {
            let _ = ttx.send(());
        })
        .unwrap();
        trx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(t0.elapsed() >= Duration::from_millis(45), "fired early: {:?}", t0.elapsed());

        // Second signal: the watch says Done and is dropped (event closed).
        unsafe { SetEvent(HANDLE(raw as *mut _)).unwrap() };
        assert_eq!(next(), Wake::Signaled(0));
        // Pokes to a finished watch go nowhere.
        poke(id);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }
}
