//! Surviving a GPU device loss (driver reset / TDR, device removed, hung or
//! reset; stability audit A01).
//!
//! The dock, the top bars and every panel draw through one `Gfx`. When its
//! D3D device is lost (the device-removed event fires, or a draw fails with a
//! device-lost HRESULT) `Gfx` posts WM_GPU_LOST here. We then:
//! 1. close the transient panels (menus, previews, stacks, search, Launchpad,
//!    Control Center): they are reopened by the user, drawn fresh;
//! 2. make a new rendering device (bounded backoff, hardware first, WARP
//!    later; see gfx::RecoveryPlan). The D3D11 device is made on a helper
//!    thread (gfx::make_device_async): creating one can block for seconds
//!    inside a driver that is being reset or replaced, and the UI thread
//!    must keep running meanwhile. Its result comes back as WM_GPU_DEVICE;
//!    only then is the D2D device made and swapped in, here (the D2D
//!    factory is single-threaded). One device is made at a time; a result
//!    for an attempt that was given up on (deadline, a newer loss) is
//!    dropped;
//! 3. redraw everything long-lived: dock icons, masks, labels, badges,
//!    speakers, the mica backdrop, every top bar's cells and tray icons.
//!
//! Too many losses in a short time, or no device after every attempt: the
//! dock quits with GPU_GIVE_UP_EXIT, a crash code for the guardian, which
//! gives the taskbar back and starts a fresh dock (within its restart budget).

use super::*;
use crate::gfx::RecoveryPlan;
use windows::core::Error;
use windows::Win32::Foundation::E_FAIL;

/// Gfx -> dock: the device is (probably) lost. Coalesced by the state below.
pub(super) const WM_GPU_LOST: u32 = WM_APP + 77;
/// Next recovery attempt (backoff).
pub(super) const GPU_RETRY_TIMER: usize = 42;
/// The dock or a top bar was busy (a modal loop): redraw shortly.
pub(super) const GPU_REDRAW_TIMER: usize = 43;
/// Recovered onto the software device: try the hardware one again later.
pub(super) const GPU_UPGRADE_TIMER: usize = 44;
/// The helper did not bring a device back in time.
pub(super) const GPU_DEADLINE_TIMER: usize = 45;
/// Helper -> dock: a device was made (or not); wParam = its ticket.
pub(super) const WM_GPU_DEVICE: u32 = WM_APP + 81;
const UPGRADE_AFTER_MS: u32 = 60_000;
/// How long one device creation may take before the attempt counts as
/// failed (the helper may still be stuck in the driver: the next attempt
/// then finds it busy and fails at once, so the backoff runs out and the
/// dock gives up instead of collecting stuck threads).
const DEVICE_DEADLINE_MS: u64 = 10_000;
/// No hardware upgrade this soon after a display change: driver installs
/// and resets come as a storm of display changes, and a new device made
/// in the middle of one is the likeliest to be lost again.
const DISPLAY_SETTLE_MS: u64 = 20_000;
/// Hardware upgrade tries per software fallback.
const UPGRADE_TRIES: u32 = 3;
const REDRAW_RETRY_MS: u32 = 50;
/// Exit code when the GPU cannot be brought back: an NTSTATUS-style error,
/// so the guardian treats it as a crash (restore + restart, budgeted).
pub const GPU_GIVE_UP_EXIT: u32 = 0xC0DE_0A01;

/// What a device being made is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Job {
    /// Replacing a lost device.
    Recover,
    /// From WARP back to the hardware device.
    Upgrade,
}

/// What a device result is answered with (Flight::arrive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// The result of the creation in flight.
    Answer(Job),
    /// A recovery of this same loss that was counted as failed (its
    /// deadline passed, or none could be set); still without a device and
    /// no newer creation under way, a good one is taken after all.
    LateRecover,
    /// Nobody waits for it: dropped. Always so for an upgrade abandoned
    /// for a new loss (made before that loss).
    Drop,
}

/// The one device creation in flight, and which device losses they belong
/// to (pure, tested).
#[derive(Debug, Default)]
struct Flight {
    /// (ticket, job, started at ms, loss generation).
    now: Option<(u64, Job, u64, u64)>,
    /// A recovery given up on (deadline): (ticket, loss generation).
    timed_out: Option<(u64, u64)>,
    /// Bumped by every new device loss.
    gen: u64,
}

impl Flight {
    fn busy(&self) -> bool {
        self.now.is_some()
    }
    fn start(&mut self, ticket: u64, job: Job, now_ms: u64) {
        self.now = Some((ticket, job, now_ms, self.gen));
    }
    /// A new device loss: what was made before it is not taken. An upgrade
    /// in flight is abandoned (true: one was); a timed-out recovery of an
    /// earlier loss is forgotten.
    fn new_loss(&mut self) -> bool {
        self.gen += 1;
        self.timed_out = None;
        if matches!(self.now, Some((_, Job::Upgrade, ..))) {
            self.now = None;
            return true;
        }
        false
    }
    /// A result for `ticket` arrived; `recovering`: still without a device.
    fn arrive(&mut self, ticket: u64, recovering: bool) -> Verdict {
        match self.now {
            Some((t, job, ..)) if t == ticket => {
                self.now = None;
                return Verdict::Answer(job);
            }
            Some(_) => return Verdict::Drop,
            None => {}
        }
        if recovering && self.timed_out == Some((ticket, self.gen)) {
            self.timed_out = None;
            return Verdict::LateRecover;
        }
        Verdict::Drop
    }
    /// The creation in flight is counted as failed now (its deadline, or
    /// no deadline could be set): its job. A recovery's late result may
    /// still be taken (LateRecover); an upgrade's is dropped.
    fn give_up(&mut self) -> Option<Job> {
        let (ticket, job, _, gen) = self.now.take()?;
        if job == Job::Recover {
            self.timed_out = Some((ticket, gen));
        }
        Some(job)
    }
    /// The deadline timer fired: Ok(job) given up on, Err(ms) still to
    /// wait (a timer from an earlier flight), Err(0) nothing in flight.
    fn expire(&mut self, now_ms: u64) -> std::result::Result<Job, u64> {
        match self.now {
            Some((_, _, at, _)) if now_ms.saturating_sub(at) >= DEVICE_DEADLINE_MS => self.give_up().ok_or(0),
            Some((_, _, at, _)) => Err((at + DEVICE_DEADLINE_MS).saturating_sub(now_ms).max(1)),
            None => Err(0),
        }
    }
    /// The deadline timer was (re)armed or not: without one nothing would
    /// ever end a creation that hangs, so it counts as failed at once.
    fn armed(&mut self, ok: bool) -> Option<Job> {
        if ok {
            None
        } else {
            self.give_up()
        }
    }
}

/// How long a hardware upgrade waits for the displays to settle (ms);
/// None: go ahead.
fn upgrade_wait(now_ms: u64, last_display_change: Option<u64>) -> Option<u64> {
    let since = now_ms.saturating_sub(last_display_change?);
    (since < DISPLAY_SETTLE_MS).then(|| DISPLAY_SETTLE_MS - since)
}

thread_local! {
    static GFX: RefCell<Option<Rc<Gfx>>> = const { RefCell::new(None) };
    static PLAN: RefCell<RecoveryPlan> = RefCell::new(RecoveryPlan::default());
    static FLIGHT: RefCell<Flight> = RefCell::new(Flight::default());
    /// The last WM_DISPLAYCHANGE / WM_DPICHANGED (ms since start).
    static DISPLAY_AT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}
/// A recovery is under way (an attempt is scheduled).
static RECOVERING: AtomicBool = AtomicBool::new(false);
/// What still has to be redrawn after the last successful recovery.
static DOCK_DIRTY: AtomicBool = AtomicBool::new(false);
static BARS_DIRTY: AtomicBool = AtomicBool::new(false);
static UPGRADES_LEFT: AtomicU32 = AtomicU32::new(0);
/// Set when the dock gave up on the GPU: main() exits with it.
static EXIT_CODE: AtomicU32 = AtomicU32::new(0);

/// The exit code the dock asked for (0 = normal).
pub fn requested_exit_code() -> u32 {
    EXIT_CODE.load(Ordering::SeqCst)
}

/// Starts watching for device loss on the dock window.
pub(super) fn init(gfx: &Rc<Gfx>, hwnd: HWND) {
    GFX.with(|g| *g.borrow_mut() = Some(gfx.clone()));
    gfx.watch_device_loss(hwnd, WM_GPU_LOST);
    // One-off panels on this thread draw with it (no second device).
    crate::gfx::share_on_this_thread(gfx);
}

/// A display change arrived (not debounced): hardware upgrades wait for
/// the displays to settle.
pub(super) fn display_changed() {
    DISPLAY_AT.with(|d| d.set(Some(since_start() as u64)));
}

fn gfx() -> Option<Rc<Gfx>> {
    GFX.with(|g| g.borrow().clone())
}

/// Displays changed (often part of a driver reset): ask the device whether
/// it is still there; a lost one posts WM_GPU_LOST.
pub(super) fn check() {
    if let Some(g) = gfx() {
        g.check_device();
    }
}

/// WM_GPU_LOST: the removal event fired, or a draw failed on a lost device.
pub(super) unsafe fn on_lost(hwnd: HWND) {
    let Some(gfx) = gfx() else { return };
    if !gfx.check_device() {
        // Stale notice (already recovered) or a false alarm.
        return;
    }
    if RECOVERING.swap(true, Ordering::SeqCst) {
        return; // an attempt is already scheduled
    }
    let _ = KillTimer(Some(hwnd), GPU_UPGRADE_TIMER);
    if FLIGHT.with(|f| f.borrow_mut().new_loss()) {
        let _ = KillTimer(Some(hwnd), GPU_DEADLINE_TIMER);
    }
    let ok = PLAN.with(|p| p.borrow_mut().lost(since_start() as u64));
    if !ok {
        crate::log!("gpu: device lost too often lately; giving up");
        give_up(hwnd);
        return;
    }
    close_panels();
    attempt(hwnd);
}

/// Timers of this module (true if `id` was one).
pub(super) unsafe fn on_timer(hwnd: HWND, id: usize) -> bool {
    match id {
        GPU_RETRY_TIMER => {
            let _ = KillTimer(Some(hwnd), GPU_RETRY_TIMER);
            attempt(hwnd);
        }
        GPU_REDRAW_TIMER => {
            let _ = KillTimer(Some(hwnd), GPU_REDRAW_TIMER);
            redraw(hwnd);
        }
        GPU_UPGRADE_TIMER => {
            let _ = KillTimer(Some(hwnd), GPU_UPGRADE_TIMER);
            upgrade(hwnd);
        }
        GPU_DEADLINE_TIMER => {
            let _ = KillTimer(Some(hwnd), GPU_DEADLINE_TIMER);
            match FLIGHT.with(|f| f.borrow_mut().expire(since_start() as u64)) {
                Ok(job) => flight_failed(hwnd, job, "it took too long"),
                Err(0) => {}
                Err(ms) => arm_deadline(hwnd, ms),
            }
        }
        _ => return false,
    }
    true
}

/// One recovery attempt: a device is made on a helper thread (answered by
/// WM_GPU_DEVICE, or the deadline). A helper still busy counts as failed.
unsafe fn attempt(hwnd: HWND) {
    if FLIGHT.with(|f| f.borrow().busy()) {
        return; // its result or its deadline goes on from here
    }
    let allow_warp = PLAN.with(|p| p.borrow().allow_warp());
    match crate::gfx::make_device_async(allow_warp, hwnd, WM_GPU_DEVICE) {
        Some(ticket) => start_flight(hwnd, ticket, Job::Recover),
        None => attempt_failed(hwnd, "the last one is still being made".into()),
    }
}

fn start_flight(hwnd: HWND, ticket: u64, job: Job) {
    FLIGHT.with(|f| f.borrow_mut().start(ticket, job, since_start() as u64));
    unsafe { arm_deadline(hwnd, DEVICE_DEADLINE_MS) };
}

/// Sets the deadline of the creation in flight; if it cannot be set, the
/// creation counts as failed now (nothing else would ever end one that
/// hangs: RECOVERING would stay set and later losses go unheard).
unsafe fn arm_deadline(hwnd: HWND, ms: u64) {
    let ok = SetTimer(Some(hwnd), GPU_DEADLINE_TIMER, ms.clamp(1, u32::MAX as u64) as u32, None) != 0;
    if let Some(job) = FLIGHT.with(|f| f.borrow_mut().armed(ok)) {
        flight_failed(hwnd, job, "no deadline could be set");
    }
}

/// The creation in flight was given up on.
unsafe fn flight_failed(hwnd: HWND, job: Job, why: &str) {
    match job {
        Job::Recover => attempt_failed(hwnd, why.into()),
        Job::Upgrade => {
            crate::log!("gpu: hardware device not made ({why})");
            upgrade_failed(hwnd);
        }
    }
}

/// WM_GPU_DEVICE: the helper made a device (or failed) for `ticket`.
pub(super) unsafe fn on_device(hwnd: HWND, ticket: u64) {
    // Always taken, so nothing waits in the slot (a stale one is dropped).
    let made = crate::gfx::take_made(ticket);
    let job = match FLIGHT.with(|f| f.borrow_mut().arrive(ticket, RECOVERING.load(Ordering::SeqCst))) {
        Verdict::Answer(job) => job,
        Verdict::LateRecover => {
            // This loss's recovery, counted as failed: a good one is used.
            if let (Some(Ok(m)), Some(gfx)) = (made, gfx()) {
                if let Ok(warp) = gfx.install(m) {
                    crate::log!("gpu: a device made late is used");
                    let _ = KillTimer(Some(hwnd), GPU_RETRY_TIMER);
                    recovered(hwnd, warp);
                }
            }
            return;
        }
        Verdict::Drop => {
            if made.is_some() {
                crate::log!("gpu: a device result nobody waits for was dropped");
            }
            return;
        }
    };
    let _ = KillTimer(Some(hwnd), GPU_DEADLINE_TIMER);
    let Some(gfx) = gfx() else { return };
    let made = made.unwrap_or_else(|| Err(Error::from(E_FAIL)));
    match job {
        Job::Recover => match made.and_then(|m| gfx.install(m)) {
            Ok(warp) => recovered(hwnd, warp),
            Err(e) => attempt_failed(hwnd, e.to_string()),
        },
        Job::Upgrade => {
            // A loss meanwhile dropped the upgrade (on_lost): not here.
            let hw = made.and_then(|m| if m.warp() { Err(Error::from(E_FAIL)) } else { gfx.install(m) });
            match hw {
                Ok(_) => {
                    crate::log!("gpu: back on the hardware device; redrawing");
                    dirty_and_redraw(hwnd);
                }
                Err(_) => upgrade_failed(hwnd),
            }
        }
    }
}

/// A new device is in: the upgrade is scheduled (if on WARP), all redrawn.
unsafe fn recovered(hwnd: HWND, warp: bool) {
    PLAN.with(|p| p.borrow_mut().succeeded());
    RECOVERING.store(false, Ordering::SeqCst);
    crate::log!("gpu: new rendering device{}; redrawing", if warp { " (software)" } else { "" });
    // (Software by choice, NOPBAR_RENDER: no upgrade.)
    if warp && !crate::gfx::prefer_software() {
        UPGRADES_LEFT.store(UPGRADE_TRIES, Ordering::SeqCst);
        SetTimer(Some(hwnd), GPU_UPGRADE_TIMER, UPGRADE_AFTER_MS, None);
    }
    dirty_and_redraw(hwnd);
}

unsafe fn dirty_and_redraw(hwnd: HWND) {
    crate::companion::refresh();
    DOCK_DIRTY.store(true, Ordering::SeqCst);
    BARS_DIRTY.store(true, Ordering::SeqCst);
    // Anything opened while there was no device (drawing failed, e.g. a
    // blank Launchpad) closes too; reopening draws it whole.
    close_panels();
    redraw(hwnd);
}

/// The attempt brought no device: the next one is scheduled, or we give up.
unsafe fn attempt_failed(hwnd: HWND, e: String) {
    let next = PLAN.with(|p| {
        let mut p = p.borrow_mut();
        p.failed();
        p.next_delay()
    });
    match next {
        Some(ms) => {
            crate::log!("gpu: no new device yet ({e}); next try in {ms} ms");
            if SetTimer(Some(hwnd), GPU_RETRY_TIMER, ms.max(1), None) == 0 {
                crate::log!("gpu: cannot schedule the next try; giving up");
                give_up(hwnd);
            }
        }
        None => {
            crate::log!("gpu: no device after every try ({e}); giving up");
            give_up(hwnd);
        }
    }
}

/// Running on WARP: the hardware device is preferred once it is back.
unsafe fn upgrade(hwnd: HWND) {
    let Some(gfx) = gfx() else { return };
    if RECOVERING.load(Ordering::SeqCst) || !gfx.on_warp() || gfx.is_lost() || FLIGHT.with(|f| f.borrow().busy()) {
        return;
    }
    // Displays still changing (a driver install or reset): later, without
    // using up a try.
    if let Some(ms) = upgrade_wait(since_start() as u64, DISPLAY_AT.with(|d| d.get())) {
        SetTimer(Some(hwnd), GPU_UPGRADE_TIMER, ms.min(u32::MAX as u64) as u32, None);
        return;
    }
    match crate::gfx::make_device_async(false, hwnd, WM_GPU_DEVICE) {
        Some(ticket) => start_flight(hwnd, ticket, Job::Upgrade),
        None => upgrade_failed(hwnd),
    }
}

/// No hardware device this time: again later, a few times.
unsafe fn upgrade_failed(hwnd: HWND) {
    if UPGRADES_LEFT.fetch_sub(1, Ordering::SeqCst) > 1 {
        SetTimer(Some(hwnd), GPU_UPGRADE_TIMER, UPGRADE_AFTER_MS, None);
    } else {
        crate::log!("gpu: staying on the software device");
    }
}

/// Everything transient drawn on the old device closes; reopening draws it new.
fn close_panels() {
    crate::popup::cancel();
    stack::cancel();
    preview::hide();
    crate::search::cancel();
    crate::launchpad::cancel();
    crate::topbar::control::cancel();
    crate::topbar::stats::cancel();
}

/// Redraws what is still dirty; parts that are busy retry shortly.
unsafe fn redraw(hwnd: HWND) {
    // (Views are built again: not while one is borrowed, see views::reconcile.)
    if DOCK_DIRTY.load(Ordering::SeqCst) && !super::views::any_view_busy() && super::views::with_primary(|d| d.redraw_after_gpu_loss()).is_some() {
        DOCK_DIRTY.store(false, Ordering::SeqCst);
        // The other displays' docks are built again on the new device.
        super::views::destroy_all();
        super::views::post_reconcile();
    }
    if BARS_DIRTY.load(Ordering::SeqCst) && crate::topbar::redraw_all() {
        BARS_DIRTY.store(false, Ordering::SeqCst);
        // The widgets on this thread draw on the same device.
        crate::pieces::redraw_all();
    }
    if DOCK_DIRTY.load(Ordering::SeqCst) || BARS_DIRTY.load(Ordering::SeqCst) {
        SetTimer(Some(hwnd), GPU_REDRAW_TIMER, REDRAW_RETRY_MS, None);
    }
}

/// No way back on this process: quit with a crash code so the guardian
/// restores the taskbar and starts a fresh dock (budgeted).
unsafe fn give_up(hwnd: HWND) {
    EXIT_CODE.store(GPU_GIVE_UP_EXIT, Ordering::SeqCst);
    let _ = PostMessageW(Some(hwnd), WM_QUIT_REQUEST, WPARAM(0), LPARAM(0));
}

impl Dock {
    /// The rendering device was replaced: every surface lost its pixels.
    /// Drop every cached drawing and draw it all again (like a theme change,
    /// plus the icon caches).
    pub(super) fn redraw_after_gpu_loss(&mut self) {
        self.shelf.borrow_mut().clear();
        // Replies on their way still land (size-checked); new ones are asked.
        self.icon_wait.clear();
        self.rest_fails.clear();
        self.labels.clear();
        self.shadow.forget();
        // The device may be the software one now (or the hardware one
        // again): Glass counts WARP as blur off (theme::glass_blur), so the
        // look is resolved again from the raw theme. The panels closed
        // already (close_panels); the other docks are built from this one.
        let before = std::mem::replace(&mut self.theme, self.theme_raw.resolve(&self.sys).windows_shell());
        let look_changed = before != self.theme;
        if look_changed {
            crate::log!("gpu: the look follows the device (finish {:?})", self.theme.finish);
        }
        self.apply_theme();
        self.apply_material();
        self.rebuild_items();
        self.reset_speakers();
        let c = self.gfx.compositor.clone();
        let _ = slide_anim(&c, &self.slide, SLIDE_MS);
        let _ = slide_anim(&c, self.shadow.slide(), SLIDE_MS);
        self.style_label();
        self.report_active(unsafe { GetForegroundWindow() });
        self.layout();
        if look_changed && self.primary {
            // The top bars and their panels follow (redraw_all next).
            crate::topbar::apply_config(&self.gfx, &self.cfg, &self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_device_in_flight_and_stale_results_are_dropped() {
        let mut f = Flight::default();
        assert!(!f.busy());
        f.start(7, Job::Recover, 1_000);
        assert!(f.busy());
        assert_eq!(f.arrive(6, true), Verdict::Drop, "an older ticket answers nobody");
        assert!(f.busy());
        assert_eq!(f.arrive(7, true), Verdict::Answer(Job::Recover));
        assert!(!f.busy());
        assert_eq!(f.arrive(7, true), Verdict::Drop, "the same result twice counts once");
    }

    #[test]
    fn a_device_that_takes_too_long_is_given_up_on() {
        let mut f = Flight::default();
        assert_eq!(f.expire(5_000), Err(0), "nothing in flight");
        f.start(1, Job::Recover, 1_000);
        // A timer left from an earlier flight: wait the rest.
        assert_eq!(f.expire(4_000), Err(DEVICE_DEADLINE_MS - 3_000));
        assert_eq!(f.expire(1_000 + DEVICE_DEADLINE_MS), Ok(Job::Recover));
        assert!(!f.busy());
        // Its good result, still without a device: taken after all, once.
        assert_eq!(f.arrive(1, true), Verdict::LateRecover);
        assert_eq!(f.arrive(1, true), Verdict::Drop);
        // Recovered meanwhile: dropped.
        f.start(2, Job::Recover, 0);
        assert_eq!(f.expire(DEVICE_DEADLINE_MS), Ok(Job::Recover));
        assert_eq!(f.arrive(2, false), Verdict::Drop);
    }

    #[test]
    fn an_upgrade_abandoned_for_a_new_loss_is_never_installed() {
        // Upgrade A in flight; the device is lost; the recovery's first
        // try finds the helper busy (no flight, a retry scheduled); then
        // A's good result arrives.
        let mut f = Flight::default();
        f.start(3, Job::Upgrade, 0);
        assert!(f.new_loss(), "the upgrade is abandoned");
        assert!(!f.busy());
        assert_eq!(f.arrive(3, true), Verdict::Drop, "made before the loss: never a recovery");
        // A recovery in flight is not abandoned by a loss notice.
        f.start(4, Job::Recover, 0);
        assert!(!f.new_loss());
        assert_eq!(f.arrive(4, true), Verdict::Answer(Job::Recover));
        // A timed-out upgrade is not rescued either.
        f.start(5, Job::Upgrade, 0);
        assert_eq!(f.expire(DEVICE_DEADLINE_MS), Ok(Job::Upgrade));
        assert_eq!(f.arrive(5, true), Verdict::Drop);
    }

    #[test]
    fn a_timed_out_recovery_of_an_earlier_loss_is_not_taken() {
        let mut f = Flight::default();
        f.start(1, Job::Recover, 0);
        assert_eq!(f.expire(DEVICE_DEADLINE_MS), Ok(Job::Recover));
        f.new_loss();
        assert_eq!(f.arrive(1, true), Verdict::Drop);
    }

    #[test]
    fn no_deadline_means_the_creation_failed_at_once() {
        let mut f = Flight::default();
        f.start(9, Job::Recover, 0);
        assert_eq!(f.armed(true), None, "armed: it waits");
        assert!(f.busy());
        // SetTimer failed: counted as failed now, not waited on forever.
        assert_eq!(f.armed(false), Some(Job::Recover));
        assert!(!f.busy(), "the next try or the give-up can run");
        assert_eq!(f.armed(false), None, "nothing in flight any more");
        // The same for an upgrade (and its late result is dropped).
        f.start(10, Job::Upgrade, 0);
        assert_eq!(f.armed(false), Some(Job::Upgrade));
        assert_eq!(f.arrive(10, false), Verdict::Drop);
    }

    #[test]
    fn upgrades_wait_for_the_displays_to_settle() {
        assert_eq!(upgrade_wait(100_000, None), None);
        assert_eq!(upgrade_wait(100_000, Some(95_000)), Some(DISPLAY_SETTLE_MS - 5_000));
        assert_eq!(upgrade_wait(100_000, Some(100_000 - DISPLAY_SETTLE_MS)), None);
        // A clock that went backwards is not a storm forever.
        assert_eq!(upgrade_wait(0, Some(10)), Some(DISPLAY_SETTLE_MS));
    }

    #[test]
    fn giving_up_reads_as_a_crash_to_the_guardian() {
        // The guardian restarts only crash codes; quit / taskkill codes not.
        assert!(crate::guardian::is_crash(GPU_GIVE_UP_EXIT));
    }
}
