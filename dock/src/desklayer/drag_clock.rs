//! One stop event and one queued scene message per active drag.
use super::drag_metrics::{self, dcomp_proc, RawProc};
use std::{
    sync::{atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering}, mpsc, Arc, OnceLock},
    thread::{self, JoinHandle},
    time::Duration,
};
use windows::{
    core::{s, BOOL, HRESULT},
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, STATUS_GRAPHICS_PRESENT_OCCLUDED, WAIT_OBJECT_0, WPARAM},
        Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput},
        System::Threading::{CreateEventW, SetEvent, WaitForSingleObject},
        UI::WindowsAndMessaging::PostMessageW,
    },
};

type Wait = unsafe extern "system" fn(u32, *const HANDLE, u32) -> u32;
type Boost = unsafe extern "system" fn(BOOL) -> HRESULT;

static DISPLAY_GENERATION: AtomicUsize = AtomicUsize::new(0);

pub(super) fn displays_changed() {
    DISPLAY_GENERATION.fetch_add(1, Ordering::AcqRel);
}

#[derive(Default)]
struct Pending(AtomicBool);

impl Pending {
    fn queue(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }

    fn receive(&self) {
        self.0.store(false, Ordering::Release);
    }
}

struct StopEvent(isize);

impl StopEvent {
    fn handle(&self) -> HANDLE {
        HANDLE(self.0 as *mut _)
    }

    fn wait(&self, ms: u32) -> bool {
        unsafe { WaitForSingleObject(self.handle(), ms) == WAIT_OBJECT_0 }
    }
}

impl Drop for StopEvent {
    fn drop(&mut self) {
        unsafe { let _ = CloseHandle(self.handle()); }
    }
}

struct Shared {
    stop: StopEvent,
    pending: Pending,
    monitor: AtomicIsize,
}

pub(super) struct Clock {
    shared: Arc<Shared>,
    thread: JoinHandle<()>,
    unboost: Option<Boost>,
}

impl Clock {
    pub fn start(reply: HWND, tick: u32, stopped: u32, monitor: Option<isize>, displays: Vec<(isize, Option<u32>)>) -> Option<Self> {
        let stop = unsafe { CreateEventW(None, true, false, None) }.ok()?;
        let shared = Arc::new(Shared {
            stop: StopEvent(stop.0 as isize),
            pending: Pending::default(),
            monitor: AtomicIsize::new(monitor.unwrap_or(0)),
        });
        let worker = shared.clone();
        let reply = reply.0 as usize;
        let thread = thread::Builder::new().name("drag-clock".into()).spawn(move || {
            let hwnd = HWND(reply as *mut _);
            run(&worker, hwnd, tick, &displays);
            if !worker.stop.wait(0) {
                unsafe { let _ = PostMessageW(Some(hwnd), stopped, WPARAM(0), LPARAM(0)); }
            }
        }).ok()?;
        static BOOST: OnceLock<Option<Boost>> = OnceLock::new();
        let boost = *BOOST.get_or_init(|| unsafe {
            Some(std::mem::transmute::<RawProc, Boost>(dcomp_proc(s!("DCompositionBoostCompositorClock"))?))
        });
        let unboost = boost.filter(|f| unsafe { f(BOOL(1)).is_ok() });
        Some(Self { shared, thread, unboost })
    }

    pub fn monitor(&self, monitor: Option<isize>) {
        self.shared.monitor.store(monitor.unwrap_or(0), Ordering::Relaxed);
    }

    pub fn received(&self) {
        self.shared.pending.receive();
    }

    pub fn finished(&self) -> bool {
        self.thread.is_finished()
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.shared.stop.handle());
            if let Some(boost) = self.unboost {
                let _ = boost(BOOL(0));
            }
        }
    }
}

fn outputs() -> Vec<(isize, IDXGIOutput)> {
    let mut outputs = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return outputs };
        for index in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(index) else { break };
            for index in 0.. {
                let Ok(output) = adapter.EnumOutputs(index) else { break };
                if let Ok(desc) = output.GetDesc() {
                    if desc.AttachedToDesktop.as_bool() {
                        outputs.push((desc.Monitor.0 as isize, output));
                    }
                }
            }
        }
    }
    outputs
}

struct VBlank {
    monitor: isize,
    done: mpsc::SyncSender<bool>,
}

// A driver can block in WaitForVBlank indefinitely. Keep that call on
// one shared thread with no request backlog and no drag-owned resources.
fn vblank(monitor: isize, budget: u32) -> bool {
    static WORKER: OnceLock<Option<mpsc::SyncSender<VBlank>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<VBlank>(0);
        thread::Builder::new().name("drag-vblank".into()).spawn(move || {
            let mut current = None;
            let mut displays = Vec::new();
            while let Ok(job) = rx.recv() {
                let generation = DISPLAY_GENERATION.load(Ordering::Acquire);
                if current != Some(generation) {
                    displays = outputs();
                    current = Some(generation);
                }
                let ready = displays.iter().find(|(id, _)| *id == job.monitor)
                    .is_some_and(|(_, output)| unsafe { output.WaitForVBlank().is_ok() });
                let _ = job.done.try_send(ready);
            }
        }).ok().map(|_| tx)
    });
    if let Some(worker) = worker {
        let (done, result) = mpsc::sync_channel(1);
        if worker.try_send(VBlank { monitor, done }).is_ok() {
            return result.recv_timeout(Duration::from_millis(u64::from(budget))).unwrap_or(false);
        }
    }
    false
}

fn wait_for_frame(compositor: Option<impl FnOnce() -> bool>, dxgi: impl FnOnce() -> bool) -> bool {
    compositor.is_some_and(|wait| wait()) || dxgi()
}

fn remaining_period(period: u32, elapsed: f64) -> u32 {
    (f64::from(period) - elapsed).ceil().max(0.) as u32
}

fn run(shared: &Shared, reply: HWND, message: u32, displays: &[(isize, Option<u32>)]) {
    static WAIT: OnceLock<Option<Wait>> = OnceLock::new();
    let wait = *WAIT.get_or_init(|| unsafe {
        Some(std::mem::transmute::<RawProc, Wait>(dcomp_proc(s!("DCompositionWaitForCompositorClock"))?))
    });
    let stop = shared.stop.handle();
    while !shared.stop.wait(0) {
        let monitor = shared.monitor.load(Ordering::Relaxed);
        let hz = displays.iter().find_map(|(id, hz)| (*id == monitor).then_some(*hz).flatten()).unwrap_or(60);
        let period = 1000u32.div_ceil(hz);
        let start = drag_metrics::now();
        let ready = wait_for_frame(
            wait.map(|wait| move || {
                let result = unsafe { wait(1, &stop, period.min(40)) };
                // The conceptual and reference documents use different success codes.
                result <= WAIT_OBJECT_0.0 + 1 || result == STATUS_GRAPHICS_PRESENT_OCCLUDED.0 as u32
            }),
            || {
                let budget = remaining_period(period, drag_metrics::now() - start);
                budget > 0 && !shared.stop.wait(0) && vblank(monitor, budget)
            },
        );
        let elapsed = drag_metrics::now() - start;
        let delay = if !ready || elapsed < 1. { remaining_period(period, elapsed) } else { 0 };
        if shared.stop.wait(delay) {
            break;
        }
        if shared.pending.queue() && unsafe {
            PostMessageW(Some(reply), message, WPARAM(0), LPARAM(0))
        }.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn only_one_tick_can_be_queued_until_the_scene_consumes_it() {
        let pending = Pending::default();
        assert!(pending.queue());
        assert!(!pending.queue());
        pending.receive();
        assert!(pending.queue());
    }

    #[test]
    fn windows_ten_without_compositor_clock_uses_dxgi_or_the_remaining_period() {
        let missing = None::<fn() -> bool>;
        assert!(wait_for_frame(missing, || true));
        assert!(!wait_for_frame(missing, || false));
        assert_eq!(remaining_period(17, 2.5), 15);
        assert_eq!(remaining_period(17, 17.), 0);
    }

    #[test]
    fn compositor_clock_precedes_dxgi_and_can_fall_back() {
        assert!(wait_for_frame(Some(|| true), || panic!("unexpected fallback")));
        let calls = RefCell::new(Vec::new());
        assert!(wait_for_frame(
            Some(|| { calls.borrow_mut().push("compositor"); false }),
            || { calls.borrow_mut().push("dxgi"); true },
        ));
        assert_eq!(*calls.borrow(), ["compositor", "dxgi"]);
    }
}
