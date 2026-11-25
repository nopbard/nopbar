//! Worker threads that exist only while there is work.
//!
//! An `IdleQueue` owns a job queue and at most one thread serving it. The
//! thread is started by the first job and ends by itself after `idle` with
//! nothing to do; the next job starts a fresh one. An idle dock therefore
//! keeps no thread (no stack, no COM apartment, no device) for work that
//! comes now and then: icon loads, window-list scans, wallpaper snapshots.
//!
//! The thread body looks like a channel loop:
//!
//! ```ignore
//! let q = IdleQueue::new("icons", IDLE, 0, |jobs| {
//!     init_com();
//!     while let Some(job) = jobs.next() { work(job) }
//!     uninit_com();
//! });
//! q.send(job);
//! ```
//!
//! The queue stays owned through worker teardown. A job arriving during
//! teardown is served on that thread after cleanup. A job queued after the
//! worker gives up the queue starts a new one.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

type Body<T> = Box<dyn Fn(&mut Jobs<T>) + Send + Sync>;

struct State<T> {
    items: VecDeque<T>,
    /// A thread is serving the queue (or is being started).
    running: bool,
    /// Threads started so far (tests and the debug log).
    started: u64,
}

struct Inner<T> {
    state: Mutex<State<T>>,
    cv: Condvar,
    name: &'static str,
    idle: Duration,
    /// Most jobs waiting at once (0 = no limit).
    cap: usize,
    body: Body<T>,
    /// Tests: thread starts fail.
    #[cfg(test)]
    fail_spawn: std::sync::atomic::AtomicBool,
}

impl<T> Inner<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// See the module docs.
pub struct IdleQueue<T: Send + 'static> {
    inner: Arc<Inner<T>>,
}

/// The serving thread's view of the queue.
pub struct Jobs<T> {
    inner: Arc<Inner<T>>,
    /// next() said "idle, end": the body is about to clean up.
    ended: bool,
}

impl<T: Send + 'static> IdleQueue<T> {
    /// `body` runs on each new thread; it should loop on `jobs.next()` and
    /// return when it gives None.
    pub fn new(name: &'static str, idle: Duration, cap: usize, body: impl Fn(&mut Jobs<T>) + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State { items: VecDeque::new(), running: false, started: 0 }),
                cv: Condvar::new(),
                name,
                idle,
                cap,
                body: Box::new(body),
                #[cfg(test)]
                fail_spawn: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// Queues `job`, starting the thread if none is serving. Err gives the
    /// job back: the queue is full, or no thread could be started.
    pub fn send(&self, job: T) -> Result<(), T> {
        let mut s = self.inner.lock();
        if self.inner.cap > 0 && s.items.len() >= self.inner.cap {
            return Err(job);
        }
        s.items.push_back(job);
        if s.running {
            drop(s);
            self.inner.cv.notify_one();
            return Ok(());
        }
        // The thread is started with the lock still held: no other sender
        // can see `running` before the start is known to have worked (the
        // new thread just waits for the lock), and on failure the job at
        // the back is certainly this call's own.
        let inner = self.inner.clone();
        let spawned = self.spawn(move || loop {
            let mut jobs = Jobs { inner: inner.clone(), ended: false };
            (inner.body)(&mut jobs);
            let mut state = inner.lock();
            if !jobs.ended || state.items.is_empty() {
                state.running = false;
                return;
            }
        });
        match spawned {
            Ok(()) => {
                s.running = true;
                s.started += 1;
                Ok(())
            }
            Err(e) => {
                let job = s.items.pop_back();
                drop(s);
                crate::log!("{}: cannot start the worker thread ({e})", self.inner.name);
                job.map_or(Ok(()), Err)
            }
        }
    }

    fn spawn(&self, f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
        #[cfg(test)]
        if self.inner.fail_spawn.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::other("test: no thread"));
        }
        std::thread::Builder::new().name(self.inner.name.into()).spawn(f).map(|_| ())
    }

    /// A thread is serving the queue now.
    #[allow(dead_code)]
    pub fn running(&self) -> bool {
        self.inner.lock().running
    }

    /// Threads started so far.
    #[allow(dead_code)]
    pub fn started(&self) -> u64 {
        self.inner.lock().started
    }
}

impl<T> Jobs<T> {
    /// The next job, waiting for one; None once nothing came for the idle
    /// time (the thread should then return).
    pub fn next(&mut self) -> Option<T> {
        if self.ended {
            return None;
        }
        let mut s = self.inner.lock();
        loop {
            if let Some(j) = s.items.pop_front() {
                return Some(j);
            }
            let (g, r) = self.inner.cv.wait_timeout(s, self.inner.idle).unwrap_or_else(|e| e.into_inner());
            s = g;
            if r.timed_out() && s.items.is_empty() {
                self.ended = true;
                return None;
            }
        }
    }

    /// A job already waiting, without waiting (for coalescing bursts).
    pub fn try_next(&mut self) -> Option<T> {
        if self.ended {
            return None;
        }
        self.inner.lock().items.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn jobs_arriving_during_teardown_reuse_the_same_worker() {
        let (started, teardown) = channel();
        let (release, wait) = channel();
        let wait = Mutex::new(wait);
        let (done, completed) = channel();
        let first = std::sync::atomic::AtomicBool::new(true);
        let queue = IdleQueue::new("t-teardown", Duration::from_millis(1), 2, move |jobs| {
            while let Some(n) = jobs.next() {
                done.send(n).unwrap();
            }
            if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                started.send(()).unwrap();
                wait.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        queue.send(1).unwrap();
        assert_eq!(completed.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        teardown.recv_timeout(Duration::from_secs(5)).unwrap();
        queue.send(2).unwrap();
        let workers = queue.started();
        release.send(()).unwrap();
        assert_eq!(completed.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
        assert_eq!(workers, 1);
    }

    #[test]
    fn runs_jobs_in_order_then_ends_and_restarts() {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let q = IdleQueue::new("t-order", Duration::from_millis(30), 0, move |jobs| {
            while let Some(n) = jobs.next() {
                let _ = tx.lock().unwrap().send(n);
            }
        });
        for n in 0..5 {
            q.send(n).unwrap();
        }
        let got: Vec<i32> = (0..5).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert_eq!(got, vec![0, 1, 2, 3, 4]);
        // Ends once idle.
        let t0 = std::time::Instant::now();
        while q.running() {
            assert!(t0.elapsed() < Duration::from_secs(5), "worker should end when idle");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(q.started(), 1);
        // A later job starts a new one.
        q.send(9).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 9);
        assert_eq!(q.started(), 2);
    }

    #[test]
    fn no_job_is_lost_around_the_idle_edge() {
        // Idle time about as long as the gap between jobs: many sends land
        // right when the worker decides to end.
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let q = IdleQueue::new("t-edge", Duration::from_millis(1), 0, move |jobs| {
            while let Some(n) = jobs.next() {
                let _ = tx.lock().unwrap().send(n);
            }
        });
        for n in 0..300u32 {
            q.send(n).unwrap();
            if n % 3 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let mut got: Vec<u32> = (0..300).map(|_| rx.recv_timeout(Duration::from_secs(5)).expect("a job was lost")).collect();
        got.sort_unstable();
        assert_eq!(got, (0..300).collect::<Vec<_>>());
    }

    #[test]
    fn full_queue_gives_the_job_back() {
        let (go_tx, go_rx) = channel::<()>();
        let go = Mutex::new(go_rx);
        let q = IdleQueue::new("t-cap", Duration::from_millis(50), 2, move |jobs| {
            while let Some(_n) = jobs.next() {
                // Hold the worker until the test lets it go.
                let _ = go.lock().unwrap().recv_timeout(Duration::from_secs(5));
            }
        });
        q.send(1).unwrap();
        // Wait until the worker took job 1, so 2 and 3 fill the queue.
        let t0 = std::time::Instant::now();
        while !q.inner.lock().items.is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        q.send(2).unwrap();
        q.send(3).unwrap();
        assert_eq!(q.send(4), Err(4));
        for _ in 0..3 {
            let _ = go_tx.send(());
        }
    }

    #[test]
    fn failed_start_gives_back_only_its_own_job() {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let q = IdleQueue::new("t-fail", Duration::from_millis(50), 0, move |jobs| {
            while let Some(n) = jobs.next() {
                let _ = tx.lock().unwrap().send(n);
            }
        });
        q.inner.fail_spawn.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(q.send(1), Err(1), "the failed call gets its job back");
        assert!(!q.running(), "no thread claimed");
        assert!(q.inner.lock().items.is_empty(), "nothing left behind");
        // A second sender racing the failure: it either got the lock first
        // (and also failed, keeping its own job) or after (same): no job
        // is ever accepted without a thread to run it.
        let q = std::sync::Arc::new(q);
        let q2 = q.clone();
        let other = std::thread::spawn(move || q2.send(2));
        let first = q.send(3);
        let second = other.join().unwrap();
        assert_eq!((first, second), (Err(3), Err(2)));
        assert!(q.inner.lock().items.is_empty());
        q.inner.fail_spawn.store(false, std::sync::atomic::Ordering::SeqCst);
        q.send(4).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 4, "only the accepted job runs");
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn early_return_frees_the_slot() {
        let q = IdleQueue::new("t-early", Duration::from_secs(10), 0, |_jobs: &mut Jobs<u8>| {});
        q.send(1).unwrap();
        let t0 = std::time::Instant::now();
        while q.running() {
            assert!(t0.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        // The job stayed queued; the next send starts a thread again.
        q.send(2).unwrap();
        assert_eq!(q.started(), 2);
    }
}
