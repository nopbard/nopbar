//! One companion clock. No global timer-resolution changes and no input hooks.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    core::{Result, PCWSTR},
    Win32::{
        Foundation::*,
        Graphics::Dwm::{DwmFlush, DwmGetCompositionTimingInfo, DWM_TIMING_INFO},
        System::Threading::*,
        UI::WindowsAndMessaging::PostMessageW,
    },
};

// Schedule generations and individual delivery tokens never alias old messages.
static GENERATION: AtomicU64 = AtomicU64::new(1);
fn new_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::Relaxed)
}
pub fn period(ms: u32) -> Duration {
    Duration::from_nanos(match ms {
        17 => 1_000_000_000 / 60,
        34 => 1_000_000_000 / 30,
        67 => 1_000_000_000 / 15,
        134 => 2_000_000_000 / 15,
        _ => ms.max(1) as u64 * 1_000_000,
    })
}
fn compatible_refresh(hz: f64) -> bool {
    hz.is_finite()
        && [60., 120., 180., 240.]
            .iter()
            .any(|&rate| (hz - rate).abs() <= 1.)
}
fn composition_hz() -> Option<f64> {
    let mut info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe {
        DwmGetCompositionTimingInfo(HWND::default(), &mut info).ok()?;
    }
    let rate = info.rateRefresh;
    (rate.uiDenominator != 0).then(|| rate.uiNumerator as f64 / rate.uiDenominator as f64)
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Schedule {
    period: Duration,
    vsync: bool,
}
struct Pending {
    token: u64,
    generation: u64,
    at: Instant,
    claimed: bool,
}
struct State {
    schedule: Option<Schedule>,
    generation: u64,
    quit: bool,
    compatible: bool,
    awake: bool,
    pending: Option<Pending>,
}
impl State {
    fn effective(&self) -> Option<Schedule> {
        self.schedule.map(|s| Schedule {
            vsync: s.vsync && self.compatible && self.awake,
            ..s
        })
    }
    fn invalidate(&mut self) {
        self.generation = new_generation();
        self.pending = None;
    }
}
// Kernel handles stay owned by the worker/shared Arc even after a bounded detach.
struct Owned(usize);
impl Owned {
    fn handle(&self) -> HANDLE {
        HANDLE(self.0 as _)
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle());
        }
    }
}
struct Shared {
    state: Mutex<State>,
    changed: Owned,
    failed: AtomicBool,
    #[cfg(test)]
    wakes: AtomicU64,
    #[cfg(test)]
    vsync_ticks: AtomicU64,
    #[cfg(test)]
    parked: AtomicBool,
}
pub struct Pacer {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    target: Arc<AtomicUsize>,
}
pub struct Ticket {
    shared: Arc<Shared>,
    token: u64,
    pub active: bool,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if self.active {
            let mut state = self.shared.state.lock().unwrap();
            if state
                .pending
                .as_ref()
                .is_some_and(|p| p.token == self.token)
            {
                state.pending = None;
            }
        }
    }
}
fn post(target: &AtomicUsize, message: u32, token: u64) -> bool {
    let hwnd = target.load(Ordering::Acquire);
    if hwnd == 0 {
        return false;
    }
    unsafe {
        PostMessageW(
            Some(HWND(hwnd as _)),
            message,
            WPARAM(token as usize),
            LPARAM(0),
        )
        .is_ok()
    }
}
// Runs on normal errors and unwinding. Publish failed before notifying the UI,
// since its arm() can run before JoinHandle::is_finished becomes true.
struct WorkerExit<F: FnOnce()> {
    shared: Arc<Shared>,
    notify: Option<F>,
}
impl<F: FnOnce()> Drop for WorkerExit<F> {
    fn drop(&mut self) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.quit {
            self.shared.failed.store(true, Ordering::Release);
            if let Some(notify) = self.notify.take() {
                notify();
            }
        }
    }
}
impl Pacer {
    pub fn new(hwnd: HWND) -> Result<Self> {
        let target = Arc::new(AtomicUsize::new(hwnd.0 as usize));
        let ticks = target.clone();
        let errors = target.clone();
        Self::with_run(
            move |shared, timer| {
                worker(
                    shared,
                    timer,
                    move |token| post(&ticks, super::WM_APP_TICK, token),
                    || unsafe { DwmFlush().is_ok() },
                )
            },
            move || {
                post(&errors, super::WM_REFRESH, 0);
            },
            target,
            composition_hz().is_some_and(compatible_refresh),
            !crate::dock::session_idle(),
        )
    }
    #[cfg(test)]
    fn with_sink(sink: impl FnMut(u64) -> bool + Send + 'static) -> Result<Self> {
        Self::with_run(
            move |shared, timer| worker(shared, timer, sink, || unsafe { DwmFlush().is_ok() }),
            || {},
            Arc::new(AtomicUsize::new(0)),
            composition_hz().is_some_and(compatible_refresh),
            true,
        )
    }
    fn with_run(
        run: impl FnOnce(Arc<Shared>, Owned) + Send + 'static,
        notify: impl FnOnce() + Send + 'static,
        target: Arc<AtomicUsize>,
        compatible: bool,
        awake: bool,
    ) -> Result<Self> {
        unsafe {
            let changed = Owned(CreateEventW(None, true, false, PCWSTR::null())?.0 as usize);
            let timer = Owned(
                CreateWaitableTimerExW(
                    None,
                    PCWSTR::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS.0,
                )?
                .0 as usize,
            );
            let shared = Arc::new(Shared {
                state: Mutex::new(State {
                    schedule: None,
                    generation: new_generation(),
                    quit: false,
                    compatible,
                    awake,
                    pending: None,
                }),
                changed,
                failed: AtomicBool::new(false),
                #[cfg(test)]
                wakes: AtomicU64::new(0),
                #[cfg(test)]
                vsync_ticks: AtomicU64::new(0),
                #[cfg(test)]
                parked: AtomicBool::new(false),
            });
            let background = shared.clone();
            let worker = thread::Builder::new()
                .name("companion-pacer".into())
                .spawn(move || {
                    let _exit = WorkerExit {
                        shared: background.clone(),
                        notify: Some(notify),
                    };
                    run(background, timer);
                })
                .map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))?;
            Ok(Self {
                shared,
                worker: Some(worker),
                target,
            })
        }
    }
    pub fn arm(&self, ms: Option<u32>, vsync: bool) {
        self.schedule(ms.map(|ms| Schedule {
            period: period(ms),
            vsync,
        }));
    }
    pub fn healthy(&self) -> bool {
        !self.shared.failed.load(Ordering::Acquire)
            && self.worker.as_ref().is_some_and(|w| !w.is_finished())
    }
    fn schedule(&self, schedule: Option<Schedule>) {
        let mut state = self.shared.state.lock().unwrap();
        if state.schedule == schedule {
            return;
        }
        state.schedule = schedule;
        state.invalidate();
        unsafe {
            let _ = SetEvent(self.shared.changed.handle());
        }
    }
    pub fn refresh_display(&self) {
        self.environment(Some(composition_hz().is_some_and(compatible_refresh)), None);
    }
    pub fn set_awake(&self, awake: bool) {
        self.environment(None, Some(awake));
    }
    fn environment(&self, compatible: Option<bool>, awake: Option<bool>) {
        let mut state = self.shared.state.lock().unwrap();
        if compatible.is_none() && awake == Some(state.awake) {
            return;
        }
        if let Some(value) = compatible {
            state.compatible = value;
        }
        if let Some(value) = awake {
            state.awake = value;
        }
        state.invalidate();
        unsafe {
            let _ = SetEvent(self.shared.changed.handle());
        }
    }
    pub fn ticket(&self, token: u64) -> Ticket {
        let mut state = self.shared.state.lock().unwrap();
        let active = !state.quit
            && state.schedule.is_some()
            && state.pending.as_ref().is_some_and(|p| {
                p.token == token && p.generation == state.generation && !p.claimed
            });
        if active {
            state.pending.as_mut().unwrap().claimed = true;
        }
        Ticket {
            shared: self.shared.clone(),
            token,
            active,
        }
    }
}
impl Drop for Pacer {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.quit = true;
            state.invalidate();
            self.target.store(0, Ordering::Release);
            unsafe {
                let _ = SetEvent(self.shared.changed.handle());
            }
        }
        if let Some(worker) = self.worker.take() {
            use std::os::windows::io::AsRawHandle;
            // Wait on the actual thread, not a sleep loop whose 1ms delays can
            // round up to system ticks and overrun the shutdown budget.
            let finished = unsafe { WaitForSingleObject(HANDLE(worker.as_raw_handle()), 200) };
            if finished == WAIT_OBJECT_0 {
                let _ = worker.join();
            } else {
                crate::log!("companion: pacer shutdown exceeded 200ms; detached with invalidated target and tickets");
            }
        }
    }
}
fn next_deadline(deadline: Instant, now: Instant, period: Duration) -> Instant {
    let next = deadline + period;
    if next <= now {
        now + period
    } else {
        next
    }
}
#[derive(Default)]
struct VsyncCadence {
    last: Option<Instant>,
    misses: u8,
}
impl VsyncCadence {
    fn observe(&mut self, now: Instant, period: Duration) -> bool {
        if self.last.is_some_and(|last| {
            now.saturating_duration_since(last).abs_diff(period) > Duration::from_millis(4)
        }) {
            self.misses = self.misses.saturating_add(1);
        } else {
            self.misses = 0;
        }
        self.last = Some(now);
        self.misses < 3
    }
}
#[derive(Default)]
struct FlushStatus {
    fast: u8,
}
impl FlushStatus {
    fn healthy(&mut self, ok: bool, elapsed: Duration) -> bool {
        if !ok {
            return false;
        }
        self.fast = if elapsed < Duration::from_millis(1) {
            self.fast.saturating_add(1)
        } else {
            0
        };
        self.fast < 2
    }
}
fn worker(
    shared: Arc<Shared>,
    timer: Owned,
    mut sink: impl FnMut(u64) -> bool,
    mut flush: impl FnMut() -> bool,
) {
    let mut generation = 0;
    let mut deadline = Instant::now();
    let mut vsync_failed = false;
    let mut cadence = VsyncCadence::default();
    let mut flush_status = FlushStatus::default();
    loop {
        let (schedule, current) = {
            let state = shared.state.lock().unwrap();
            if state.quit {
                return;
            }
            unsafe {
                let _ = ResetEvent(shared.changed.handle());
            }
            (state.effective(), state.generation)
        };
        let Some(schedule) = schedule else {
            unsafe {
                let _ = CancelWaitableTimer(timer.handle());
            }
            #[cfg(test)]
            shared.parked.store(true, Ordering::Release);
            let result = unsafe { WaitForSingleObject(shared.changed.handle(), INFINITE) };
            #[cfg(test)]
            shared.parked.store(false, Ordering::Release);
            if result != WAIT_OBJECT_0 {
                return;
            }
            continue;
        };
        if generation != current {
            generation = current;
            deadline = Instant::now() + schedule.period;
            vsync_failed = false;
            cadence = VsyncCadence::default();
            flush_status = FlushStatus::default();
        }
        if schedule.vsync && !vsync_failed {
            let start = Instant::now();
            let ok = flush();
            if !flush_status.healthy(ok, start.elapsed()) {
                vsync_failed = true;
                continue;
            }
            if Instant::now() + schedule.period / 4 < deadline {
                continue;
            }
        } else {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let due = -(remaining
                .as_nanos()
                .div_ceil(100)
                .max(1)
                .min(i64::MAX as u128) as i64);
            unsafe {
                if let Err(e) = SetWaitableTimerEx(timer.handle(), &due, 0, None, None, None, 0) {
                    crate::log!("companion: pacer wait failed: {e}");
                    return;
                }
                let result = WaitForMultipleObjects(
                    &[shared.changed.handle(), timer.handle()],
                    false,
                    INFINITE,
                );
                if result == WAIT_OBJECT_0 {
                    continue;
                }
                if result != WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
                    return;
                }
            }
        }
        let mut state = shared.state.lock().unwrap();
        if state.quit {
            return;
        }
        if state.generation != generation || state.schedule.is_none() {
            continue;
        }
        #[cfg(test)]
        shared.wakes.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        if schedule.vsync && !vsync_failed {
            shared.vsync_ticks.fetch_add(1, Ordering::Relaxed);
        }
        let now = Instant::now();
        if schedule.vsync && !vsync_failed && !cadence.observe(now, schedule.period) {
            vsync_failed = true;
            deadline = now;
        }
        // A timed-out delivery gets a fresh token. Queued old messages and an
        // old in-progress Ticket cannot consume or clear the replacement.
        if state
            .pending
            .as_ref()
            .is_none_or(|p| now.saturating_duration_since(p.at) > Duration::from_millis(250))
        {
            let token = new_generation();
            state.pending = Some(Pending {
                token,
                generation,
                at: now,
                claimed: false,
            });
            if !sink(token) {
                state.pending = None;
            }
        }
        deadline = next_deadline(deadline, Instant::now(), schedule.period);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use windows::Win32::{
        System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        UI::WindowsAndMessaging::*,
    };
    fn qpc() -> i64 {
        let mut value = 0;
        unsafe {
            QueryPerformanceCounter(&mut value).unwrap();
        }
        value
    }
    #[test]
    fn only_supported_refresh_multiples_enable_vsync_and_environment_rearms() {
        for hz in [59., 59.94, 60., 61., 119.88, 120., 180., 240., 241.] {
            assert!(compatible_refresh(hz), "{hz}");
        }
        for hz in [
            0.,
            30.,
            58.9,
            61.1,
            75.,
            90.,
            144.,
            165.,
            360.,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert!(!compatible_refresh(hz), "{hz}");
        }
        let p = Pacer::with_sink(|_| true).unwrap();
        p.environment(Some(true), Some(true));
        p.arm(Some(17), true);
        assert!(p.shared.state.lock().unwrap().effective().unwrap().vsync);
        let old = p.shared.state.lock().unwrap().generation;
        p.environment(Some(false), None);
        assert_ne!(old, p.shared.state.lock().unwrap().generation);
        assert!(!p.shared.state.lock().unwrap().effective().unwrap().vsync);
        p.environment(Some(true), Some(false));
        assert!(!p.shared.state.lock().unwrap().effective().unwrap().vsync);
        p.set_awake(true);
        assert!(p.shared.state.lock().unwrap().effective().unwrap().vsync);
    }
    #[test]
    fn one_fast_flush_is_allowed_but_two_consecutive_fast_returns_fail() {
        let mut f = FlushStatus::default();
        let fast = Duration::from_micros(100);
        let normal = Duration::from_millis(8);
        assert!(f.healthy(true, fast));
        assert!(f.healthy(true, normal));
        assert!(f.healthy(true, fast));
        assert!(!f.healthy(true, fast));
        assert!(f.healthy(true, normal));
        assert!(!f.healthy(false, normal));
    }
    #[test]
    fn blocked_flush_detaches_within_budget_and_cannot_post_after_drop() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (posted_tx, posted_rx) = mpsc::channel();
        let (exited_tx, exited_rx) = mpsc::channel();
        let target = Arc::new(AtomicUsize::new(42));
        let p = Pacer::with_run(
            move |shared, timer| {
                worker(
                    shared,
                    timer,
                    move |token| posted_tx.send(token).is_ok(),
                    move || {
                        let _ = entered_tx.send(());
                        let _ = release_rx.recv();
                        true
                    },
                );
                let _ = exited_tx.send(());
            },
            || panic!("normal shutdown must not notify failure"),
            target.clone(),
            true,
            true,
        )
        .unwrap();
        p.arm(Some(17), true);
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let at = Instant::now();
        drop(p);
        let elapsed = at.elapsed();
        assert!(elapsed < Duration::from_millis(300), "{elapsed:?}");
        assert_eq!(target.load(Ordering::Acquire), 0);
        release_tx.send(()).unwrap();
        exited_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(posted_rx.try_recv().is_err());
    }
    #[test]
    fn worker_error_notifies_once_and_is_unhealthy_before_ui_rearms() {
        let (tx, rx) = mpsc::channel();
        let p = Pacer::with_run(
            |_, _| {},
            move || {
                let _ = tx.send(());
            },
            Arc::new(AtomicUsize::new(0)),
            false,
            true,
        )
        .unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!p.healthy());
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
    }
    #[test]
    fn duplicate_delivery_cannot_acknowledge_an_in_progress_ticket() {
        let (tx, rx) = mpsc::channel();
        let p = Pacer::with_sink(move |token| tx.send(token).is_ok()).unwrap();
        p.arm(Some(17), false);
        let token = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let ticket = p.ticket(token);
        assert!(ticket.active);
        assert!(!p.ticket(token).active);
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(ticket);
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok());
    }
    fn frequency() -> f64 {
        let mut value = 0;
        unsafe {
            QueryPerformanceFrequency(&mut value).unwrap();
        }
        value as f64
    }
    fn stats(times: &[i64]) -> (f64, f64) {
        let hz = frequency();
        let mut values: Vec<_> = times
            .windows(2)
            .map(|p| (p[1] - p[0]) as f64 * 1000. / hz)
            .collect();
        values.sort_by(f64::total_cmp);
        (
            values.iter().sum::<f64>() / values.len() as f64,
            values[(values.len() - 1) * 95 / 100],
        )
    }
    #[test]
    fn api_periods_preserve_exact_fps_and_deadlines_skip_missed_ticks() {
        assert!((period(17).as_secs_f64() - 1. / 60.).abs() < 1e-8);
        assert!((period(34).as_secs_f64() - 1. / 30.).abs() < 1e-8);
        let now = Instant::now();
        assert_eq!(
            next_deadline(now, now + Duration::from_secs(1), period(17)),
            now + Duration::from_secs(1) + period(17)
        );
    }
    #[test]
    fn unstable_vsync_falls_back_after_three_cadence_misses() {
        let mut cadence = VsyncCadence::default();
        let mut at = Instant::now();
        let period = period(17);
        assert!(cadence.observe(at, period));
        for _ in 0..60 {
            at += period;
            assert!(cadence.observe(at, period));
        }
        for i in 0..3 {
            at += period + Duration::from_millis(6);
            assert_eq!(cadence.observe(at, period), i < 2);
        }
    }
    #[test]
    fn normal_vsync_breaks_the_miss_streak() {
        let mut cadence = VsyncCadence::default();
        let mut at = Instant::now();
        let period = period(17);
        assert!(cadence.observe(at, period));
        for _ in 0..5 {
            at += period + Duration::from_millis(6);
            assert!(cadence.observe(at, period));
            at += period;
            assert!(cadence.observe(at, period));
        }
    }
    #[test]
    fn watchdog_replaces_a_lost_tick_and_old_ticket_cannot_clear_replacement() {
        let (tx, rx) = mpsc::channel();
        let p = Pacer::with_sink(move |token| tx.send(token).is_ok()).unwrap();
        p.arm(Some(17), false);
        let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let stale = p.ticket(first);
        assert!(stale.active);
        let replacement = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("watchdog must retry after 250ms");
        assert_ne!(replacement, first);
        drop(stale);
        assert!(p.ticket(replacement).active);
    }
    #[test]
    fn coalesces_until_ui_finishes_and_rejects_stale_ticks() {
        let (tx, rx) = mpsc::channel();
        let p = Pacer::with_sink(move |generation| tx.send(generation).is_ok()).unwrap();
        p.arm(Some(17), false);
        let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let ticket = p.ticket(first);
        assert!(ticket.active);
        assert!(rx.recv_timeout(Duration::from_millis(80)).is_err());
        drop(ticket);
        let second = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        p.arm(None, false);
        assert!(!p.ticket(second).active);
        p.arm(Some(34), false);
        let third = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_ne!(second, third);
        assert!(p.ticket(third).active);
    }
    #[test]
    fn stopped_worker_has_zero_periodic_wakes_and_shutdown_is_prompt() {
        let (tx, rx) = mpsc::channel();
        let p = Pacer::with_sink(move |generation| tx.send(generation).is_ok()).unwrap();
        p.arm(Some(17), false);
        let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(p.ticket(first).active);
        p.arm(None, false);
        let limit = Instant::now() + Duration::from_secs(1);
        while !p.shared.parked.load(Ordering::Acquire) {
            assert!(Instant::now() < limit);
            thread::yield_now();
        }
        let wakes = p.shared.wakes.load(Ordering::Relaxed);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(p.shared.wakes.load(Ordering::Relaxed), wakes);
        let at = Instant::now();
        drop(p);
        assert!(at.elapsed() < Duration::from_millis(100));
    }
    #[test]
    fn failed_posts_retry_and_recreated_workers_reject_old_generations() {
        let (tx, rx) = mpsc::channel();
        let mut attempts = 0;
        let p = Pacer::with_sink(move |generation| {
            attempts += 1;
            if attempts == 1 {
                false
            } else {
                tx.send(generation).is_ok()
            }
        })
        .unwrap();
        p.arm(Some(17), false);
        let old = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(p);
        let (tx, rx) = mpsc::channel();
        let p = Pacer::with_sink(move |generation| tx.send(generation).is_ok()).unwrap();
        p.arm(Some(17), false);
        let current = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!p.ticket(old).active);
        assert!(p.ticket(current).active);
        assert_ne!(old, current);
    }
    #[test]
    #[ignore = "QPC scheduler study, isolated own thread queue; no HWND/input/capture"]
    fn pacing_before_after_measurements() {
        let mut report = String::from(
            "state,request_ms,before_mean_ms,before_p95_ms,after_mean_ms,after_p95_ms\n",
        );
        for (name, ms, vsync) in [
            ("Idle", 50, false),
            ("Walk", 17, false),
            ("Drag", 17, true),
            ("Held swing", 17, true),
            ("Breathing", 34, false),
        ] {
            let mut times = Vec::new();
            unsafe {
                let timer = SetTimer(None, 0, ms, None);
                assert_ne!(timer, 0);
                let mut msg = MSG::default();
                for _ in 0..131 {
                    assert!(GetMessageW(&mut msg, None, WM_TIMER, WM_TIMER).0 > 0);
                    times.push(qpc());
                }
                KillTimer(None, timer).unwrap();
            }
            let before = stats(&times[10..]);
            // A private UI-like thread message queue receives PostThreadMessage
            // from the production worker. This never targets the running app.
            let thread = unsafe { GetCurrentThreadId() };
            let (tx, rx) = mpsc::channel();
            let p = Pacer::with_sink(move |generation| {
                let ok = unsafe {
                    PostThreadMessageW(
                        thread,
                        super::super::WM_APP_TICK,
                        WPARAM(generation as usize),
                        LPARAM(0),
                    )
                    .is_ok()
                };
                let _ = tx.send(ok);
                ok
            })
            .unwrap();
            p.arm(Some(ms), vsync);
            times.clear();
            for _ in 0..131 {
                let mut msg = MSG::default();
                unsafe {
                    assert!(
                        GetMessageW(
                            &mut msg,
                            None,
                            super::super::WM_APP_TICK,
                            super::super::WM_APP_TICK
                        )
                        .0 > 0
                    );
                }
                assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
                let ticket = p.ticket(msg.wParam.0 as u64);
                assert!(ticket.active);
                times.push(qpc());
                drop(ticket);
            }
            let vsync_ticks = p.shared.vsync_ticks.load(Ordering::Relaxed);
            let wakes = p.shared.wakes.load(Ordering::Relaxed);
            eprintln!(
                "{name}: DwmFlush ticks={vsync_ticks}, timer ticks={}",
                wakes - vsync_ticks
            );
            p.arm(None, false);
            drop(p);
            // Stop cannot retract a message already posted to this private queue.
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(
                    &mut msg,
                    None,
                    super::super::WM_APP_TICK,
                    super::super::WM_APP_TICK,
                    PM_REMOVE,
                )
                .as_bool()
                {}
            }
            let after = stats(&times[10..]);
            let row = format!(
                "{name},{:.3},{:.3},{:.3},{:.3},{:.3}\n",
                period(ms).as_secs_f64() * 1000.,
                before.0,
                before.1,
                after.0,
                after.1
            );
            print!("{row}");
            report.push_str(&row);
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../target/companion-pacing.csv");
            std::fs::write(path, &report).unwrap();
            let target = period(ms).as_secs_f64() * 1000.;
            assert!(
                (after.0 - target).abs() <= 1.5,
                "{name}: mean {} vs {target}",
                after.0
            );
            assert!(
                (after.1 - target).abs() <= 4.,
                "{name}: p95 {} vs {target}",
                after.1
            );
        }
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/companion-pacing.csv");
        std::fs::write(path, report).unwrap();
    }
    #[test]
    #[ignore = "wall-clock accuracy; run alone without concurrent builds"]
    fn requested_periods_meet_mean_and_p95_tolerances() {
        for ms in [17, 34, 50] {
            let (tx, rx) = mpsc::channel();
            let p =
                Pacer::with_sink(move |generation| tx.send((generation, qpc())).is_ok()).unwrap();
            p.arm(Some(ms), false);
            let mut times = Vec::new();
            for _ in 0..131 {
                let (generation, at) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
                times.push(at);
                drop(p.ticket(generation));
            }
            p.arm(None, false);
            let (mean, p95) = stats(&times[10..]);
            let target = period(ms).as_secs_f64() * 1000.;
            eprintln!("requested {target:.3} ms: mean={mean:.3}, p95={p95:.3}");
            assert!((mean - target).abs() <= 1.5, "mean={mean}, target={target}");
            assert!((p95 - target).abs() <= 4., "p95={p95}, target={target}");
        }
    }
}

#[cfg(test)] mod assistant_measurement {
    use super::*;
    #[test]
    #[ignore = "isolated timer measurement, no HWND or screen access"]
    fn static_idle_pacer_measurement() {
        fn cpu()->u64{use windows::Win32::{Foundation::FILETIME,System::Threading::{GetCurrentProcess,GetProcessTimes}};unsafe{let(mut c,mut e,mut k,mut u)=(FILETIME::default(),FILETIME::default(),FILETIME::default(),FILETIME::default());GetProcessTimes(GetCurrentProcess(),&mut c,&mut e,&mut k,&mut u).unwrap();((k.dwHighDateTime as u64)<<32|k.dwLowDateTime as u64)+((u.dwHighDateTime as u64)<<32|u.dwLowDateTime as u64)}}
        for ms in [50,250] {
            let (tx,rx)=std::sync::mpsc::channel();
            let p=Pacer::with_sink(move |token|tx.send(token).is_ok()).unwrap();
            let before_cpu=cpu();let at=Instant::now();p.arm(Some(ms),false);
            let mut ticks=0;
            while at.elapsed()<Duration::from_secs(3) {
                if let Ok(token)=rx.recv_timeout(Duration::from_millis(300)) {
                    drop(p.ticket(token));ticks+=1;
                }
            }
            eprintln!("static idle: period={ms}ms ticks={ticks} elapsed={:.3}s process_cpu={:.3}ms",at.elapsed().as_secs_f64(),(cpu()-before_cpu) as f64/10000.);
            assert!(ticks <= 3100/ms+2);
            p.arm(None,false);
        }
    }
}
