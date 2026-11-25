//! A scheduled shutdown or sleep from the system menu ("Schedule shutdown
//! ▸ 30 min", "Sleep after ▸ 30 min"): one at a time (Kind), the newest
//! replacing any other. NopBar keeps the time itself, in memory only
//! (NopBar quitting or restarting drops it: stop), and when it comes runs
//! Windows' own `shutdown.exe /s /t 0` from the system directory, or puts
//! the PC to sleep (SetSuspendState, as the Sleep row does). Not
//! `/s /t <seconds>`: with a timeout Windows adds /f and closes apps with
//! unsaved work without a word; with none, Windows' own screen stops for
//! them. Nothing is shown before either (no dialog, no toast).
//!
//! The wait is measured on GetTickCount64 (milliseconds since boot, sleep
//! included, never moved by clock changes), not the wall clock: setting
//! the clock neither runs it early nor drops nor stretches it. The wall
//! clock only names the time in the menu.
//!
//! A time that comes while the PC sleeps is dropped quietly, never run on
//! waking: a suspend broadcast marks the schedule, and the resume one
//! drops it if its time went by; the timer, should it wake first, waits
//! for that answer (`slept`). Without any broadcast, a time first seen
//! more than LATE_MS after it is dropped too (awake, the timer looks every
//! NAP_MS). The menu row is then the plain one again.
//!
//! Cancelling ours only stops the timer. A shutdown another app scheduled
//! with Windows (shutdown.exe answering ours with "one is scheduled
//! already", 1190) shows as "Cancel scheduled shutdown" without a time;
//! that row runs `shutdown.exe /a`, and only its success (or "none in
//! progress", 1116) clears the row.
//!
//! Every schedule, cancel and stop starts a new generation: a shutdown.exe
//! run from an older one never overwrites a newer state. The state machine
//! (Sched), the timer (run_timer, its clock, naps and action injected) and
//! the command lines are pure and tested; only run_exe runs shutdown.exe
//! and only sleep_now sleeps, and tests never call either.

use std::path::PathBuf;
use std::sync::Mutex;

/// The presets offered, in minutes.
pub const PRESETS: [u32; 4] = [15, 30, 60, 120];

/// ERROR_SHUTDOWN_IS_SCHEDULED: shutdown.exe's exit code when a shutdown
/// is scheduled already.
pub const ALREADY_SCHEDULED: i32 = 1190;
/// ERROR_NO_SHUTDOWN_IN_PROGRESS: `/a` with nothing to call off.
pub const NONE_IN_PROGRESS: i32 = 1116;

/// The longest the timer sleeps between looks at the clock (ms).
pub const NAP_MS: u64 = 5_000;
/// Seen this long (ms) past its time, the shutdown is not run: the PC was
/// asleep then (awake, the timer looks every NAP_MS).
pub const LATE_MS: u64 = 60_000;

/// WM_POWERBROADCAST's events this cares about.
const PBT_APMSUSPEND: usize = 0x4;
const PBT_APMRESUMESUSPEND: usize = 0x7;
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;

/// What a schedule does at its time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `shutdown.exe /s /t 0` (fire_args).
    Shutdown,
    /// SetSuspendState, as the Sleep row (sleep_now).
    Sleep,
}

/// A shutdown or sleep scheduled, as the menu shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pending {
    #[default]
    None,
    /// NopBar's own, due at this wall-clock time (UTC seconds, for its
    /// label only).
    At(Kind, i64),
    /// Windows has one another app scheduled; when is not known.
    Unknown,
}

/// Pure: shutdown.exe's arguments to shut down now. No timeout, so no
/// implied /f: apps with unsaved work get Windows' own screen.
pub fn fire_args() -> Vec<String> {
    vec!["/s".into(), "/t".into(), "0".into()]
}

/// Pure: shutdown.exe's arguments to call another app's scheduled
/// shutdown off.
pub fn cancel_args() -> Vec<String> {
    vec!["/a".into()]
}

/// What the timer of a generation does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// Cancelled, replaced, dropped or stopped: the timer ends.
    Stop,
    /// Look again in this many ms.
    Wait(u64),
    /// The time has come (begin_fire decides, under the lock).
    Fire,
    /// The time went by while the PC slept: dropped, not run.
    Drop,
}

/// What cancel asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelPlan {
    /// Nothing to run (ours is off already, or nothing to cancel).
    Done,
    /// Run `shutdown.exe /a` for generation `gen`, then end_cancel.
    Abort(u64),
}

/// The scheduled shutdown's whole state (one, behind STATE). Times are
/// GetTickCount64 milliseconds unless named wall.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sched {
    pub pending: Pending,
    /// Bumped by every schedule, cancel, drop and stop.
    pub gen: u64,
    /// Ours: when it is due.
    pub due_ms: u64,
    /// A suspend came while ours was pending (the resume decides).
    pub slept: bool,
    /// NopBar is quitting: nothing is scheduled or run any more.
    pub stopped: bool,
    /// A `/a` is on its way (a second is not sent meanwhile).
    pub cancelling: bool,
}

impl Sched {
    /// Ours, `kind` `minutes` from `now_ms` (`wall`: the wall clock now,
    /// UTC seconds, for the label), in place of any other: its generation
    /// (None once stopped).
    pub fn schedule(&mut self, kind: Kind, minutes: u32, now_ms: u64, wall: i64) -> Option<u64> {
        if self.stopped {
            return None;
        }
        self.gen += 1;
        self.pending = Pending::At(kind, wall + i64::from(minutes) * 60);
        self.due_ms = now_ms + u64::from(minutes) * 60_000;
        self.slept = false;
        Some(self.gen)
    }

    fn ours(&self, gen: u64) -> bool {
        !self.stopped && gen == self.gen && matches!(self.pending, Pending::At(..))
    }

    /// The timer of generation `gen` at `now_ms`.
    pub fn tick(&self, gen: u64, now_ms: u64) -> Tick {
        if !self.ours(gen) {
            Tick::Stop
        } else if now_ms > self.due_ms + LATE_MS {
            Tick::Drop
        } else if now_ms >= self.due_ms {
            // Slept meanwhile: the resume broadcast decides (it drops it);
            // never run on waking.
            if self.slept { Tick::Wait(1_000) } else { Tick::Fire }
        } else {
            Tick::Wait((self.due_ms - now_ms).clamp(1, NAP_MS))
        }
    }

    /// The timer of `gen` is about to run its action: which, if it still
    /// may (decided under the lock, against cancel and stop). The menu
    /// shows nothing pending meanwhile.
    pub fn begin_fire(&mut self, gen: u64, now_ms: u64) -> Option<Kind> {
        let Pending::At(kind, _) = self.pending else { return None };
        if self.tick(gen, now_ms) != Tick::Fire {
            return None;
        }
        self.pending = Pending::None;
        Some(kind)
    }

    /// The `kind` action of `gen` answered `code`: after a shutdown,
    /// Windows' other one shows (1190), unless something newer happened
    /// meanwhile.
    pub fn end_fire(&mut self, gen: u64, kind: Kind, code: Option<i32>) {
        if gen != self.gen || self.stopped || self.pending != Pending::None {
            return;
        }
        if kind == Kind::Shutdown && code == Some(ALREADY_SCHEDULED) {
            self.pending = Pending::Unknown;
        } else if code != Some(0) {
            crate::log!("shutdown: scheduled {kind:?} failed ({code:?})");
        }
    }

    /// The timer of `gen` found its time slept through: dropped.
    pub fn drop_missed(&mut self, gen: u64) {
        if self.ours(gen) {
            self.pending = Pending::None;
            self.gen += 1;
        }
    }

    /// The PC is going to sleep.
    pub fn suspend(&mut self) {
        if matches!(self.pending, Pending::At(..)) {
            self.slept = true;
        }
    }

    /// The PC woke at `now_ms`: ours is dropped if its time came while it
    /// slept (or came at all: it cannot have run since), else kept.
    pub fn resume(&mut self, now_ms: u64) {
        if !matches!(self.pending, Pending::At(..)) {
            return;
        }
        if now_ms >= self.due_ms {
            crate::log!("shutdown: the scheduled time went by in sleep; dropped");
            self.pending = Pending::None;
            self.gen += 1;
        }
        self.slept = false;
    }

    /// The row "Cancel scheduled shutdown" was picked.
    pub fn cancel(&mut self) -> CancelPlan {
        if self.stopped {
            return CancelPlan::Done;
        }
        match self.pending {
            Pending::At(..) => {
                self.pending = Pending::None;
                self.gen += 1;
                CancelPlan::Done
            }
            Pending::Unknown if !self.cancelling => {
                self.cancelling = true;
                CancelPlan::Abort(self.gen)
            }
            _ => CancelPlan::Done,
        }
    }

    /// `/a` of `gen` answered `code`: the row goes only when Windows has
    /// nothing scheduled any more (0, or 1116); on any other answer it
    /// stays, to try again.
    pub fn end_cancel(&mut self, gen: u64, code: Option<i32>) {
        self.cancelling = false;
        if gen != self.gen || self.pending != Pending::Unknown {
            return;
        }
        match code {
            Some(0) | Some(NONE_IN_PROGRESS) => self.pending = Pending::None,
            other => crate::log!("shutdown: shutdown.exe /a failed ({other:?}); still scheduled"),
        }
    }

    /// NopBar is quitting: ours is dropped and nothing runs from now on.
    pub fn stop(&mut self) {
        self.stopped = true;
        self.gen += 1;
        if matches!(self.pending, Pending::At(..)) {
            self.pending = Pending::None;
        }
    }

    /// The session goes on after all (a logoff was cancelled): shutdowns
    /// can be scheduled again; what stop dropped stays dropped.
    pub fn reopen(&mut self) {
        self.stopped = false;
    }

    /// What the menu shows at `now_ms`: ours past its time is over.
    pub fn shown(&self, now_ms: u64) -> Pending {
        match self.pending {
            Pending::At(..) if self.stopped || now_ms >= self.due_ms => Pending::None,
            p => p,
        }
    }
}

/// Pure: a WM_POWERBROADCAST `wparam` that says the PC woke up.
pub fn is_resume(wparam: usize) -> bool {
    matches!(wparam, PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC)
}

/// Pure: shutdown.exe in the system directory `dir`, only if Windows gave
/// an absolute one (None: nothing is run; never a search by name).
pub fn exe_in(dir: Option<PathBuf>) -> Option<PathBuf> {
    dir.filter(|d| d.is_absolute()).map(|d| d.join("shutdown.exe"))
}

/// The system directory as Windows reports it (GetSystemDirectoryW), not
/// from the environment.
pub fn system_dir() -> Option<PathBuf> {
    let mut buf = [0u16; 520];
    let n = unsafe { windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buf)) } as usize;
    (n > 0 && n < buf.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

static STATE: Mutex<Sched> = Mutex::new(Sched { pending: Pending::None, gen: 0, due_ms: 0, slept: false, stopped: false, cancelling: false });

fn now_ms() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

fn lock<R>(state: &Mutex<Sched>, f: impl FnOnce(&mut Sched) -> R) -> Option<R> {
    state.lock().ok().map(|mut s| f(&mut s))
}

fn with<R>(f: impl FnOnce(&mut Sched) -> R) -> Option<R> {
    lock(&STATE, f)
}

/// The timer of generation `gen` on `state`, start to end: looks at
/// `now` (the tick count), naps (`nap`, ms) until the time, then runs
/// `fire` (the action, its exit code) once, unless dropped, cancelled,
/// replaced or stopped first.
fn run_timer(state: &Mutex<Sched>, gen: u64, now: &dyn Fn() -> u64, nap: &mut dyn FnMut(u64), fire: &mut dyn FnMut(Kind) -> Option<i32>) {
    loop {
        match lock(state, |s| s.tick(gen, now())).unwrap_or(Tick::Stop) {
            Tick::Stop => return,
            Tick::Wait(ms) => nap(ms),
            Tick::Drop => {
                crate::log!("shutdown: the scheduled time went by in sleep; dropped");
                lock(state, |s| s.drop_missed(gen));
                return;
            }
            Tick::Fire => {
                let Some(kind) = lock(state, |s| s.begin_fire(gen, now())).flatten() else { continue };
                let code = fire(kind);
                lock(state, |s| s.end_fire(gen, kind, code));
                return;
            }
        }
    }
}

/// The action of a schedule, now.
fn fire(kind: Kind) -> Option<i32> {
    match kind {
        Kind::Shutdown => run_exe(&fire_args()),
        Kind::Sleep => sleep_now(),
    }
}

/// Puts the PC to sleep, as the Sleep row: Some(0) once it is awake again,
/// else Windows' error.
pub fn sleep_now() -> Option<i32> {
    if unsafe { windows::Win32::System::Power::SetSuspendState(false, false, false) } {
        Some(0)
    } else {
        Some(unsafe { windows::Win32::Foundation::GetLastError() }.0 as i32)
    }
}

/// What the system menu shows now.
pub fn current() -> Pending {
    with(|s| s.shown(now_ms())).unwrap_or_default()
}

/// A power broadcast came to the bar: a suspend marks ours, a resume drops
/// it if its time came meanwhile (its timer then stops by itself).
pub fn on_power(wparam: usize) {
    if wparam == PBT_APMSUSPEND {
        with(|s| s.suspend());
    } else if is_resume(wparam) {
        with(|s| s.resume(now_ms()));
    }
}

/// Shuts the PC down or puts it to sleep (`kind`) in `minutes`, in place
/// of any other schedule: NopBar's timer, on a helper thread.
pub fn schedule(kind: Kind, minutes: u32) {
    let Some(gen) = with(|s| s.schedule(kind, minutes, now_ms(), crate::agenda::unix_now() as i64)).flatten() else { return };
    let spawned = std::thread::Builder::new().name("shutdown-timer".into()).spawn(move || {
        run_timer(&STATE, gen, &now_ms, &mut |ms| {if remaining().is_some_and(|(g,left)|g==gen&&left<=60_000){crate::companion::cue(crate::companion::Cue::Shutdown(gen));}std::thread::sleep(std::time::Duration::from_millis(ms));}, &mut fire);
    });
    if spawned.is_err() {
        // No timer: nothing may look scheduled.
        crate::log!("shutdown: no timer thread; not scheduled");
        with(|s| s.cancel());
    }
}

/// Calls the scheduled shutdown off: ours at once; another app's with
/// shutdown.exe /a on a helper thread.
pub fn cancel() {
    crate::companion::refresh();
    let Some(CancelPlan::Abort(gen)) = with(|s| s.cancel()) else { return };
    let spawned = std::thread::Builder::new().name("shutdown-cancel".into()).spawn(move || {
        let code = run_exe(&cancel_args());
        with(|s| s.end_cancel(gen, code));
    });
    if spawned.is_err() {
        with(|s| s.end_cancel(gen, None));
    }
}

/// NopBar is quitting: ours is dropped before anything else, and no timer
/// runs its action from here on (it decides under the same lock).
pub fn stop() {
    with(|s| s.stop());
}

/// A cancelled logoff: schedules can be made again (Sched::reopen).
pub fn reopen() {
    with(|s| s.reopen());
}

/// Windows' shutdown.exe from the system directory, its window hidden;
/// None when that directory is not known (never searched for by name).
fn run_exe(args: &[String]) -> Option<i32> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = exe_in(system_dir())?;
    std::process::Command::new(exe).args(args).creation_flags(CREATE_NO_WINDOW).status().ok()?.code()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const MIN: u64 = 60_000;
    const KINDS: [Kind; 2] = [Kind::Shutdown, Kind::Sleep];

    /// Ours (`kind`), 30 minutes from 0 ms (wall 1000 s), as a timer would
    /// see it.
    fn scheduled_as(kind: Kind) -> (Sched, u64) {
        let mut s = Sched::default();
        let gen = s.schedule(kind, 30, 0, 1_000).unwrap();
        (s, gen)
    }

    fn scheduled() -> (Sched, u64) {
        scheduled_as(Kind::Shutdown)
    }

    #[test]
    fn the_shutdown_runs_with_no_timeout_so_no_forced_close() {
        // No "/t N" with N > 0 (Windows would add /f) and no "/f".
        assert_eq!(fire_args(), ["/s", "/t", "0"]);
        assert!(!fire_args().iter().any(|a| a.eq_ignore_ascii_case("/f")));
        assert_eq!(cancel_args(), ["/a"]);
        assert_eq!(PRESETS, [15, 30, 60, 120]);
    }

    #[test]
    fn shutdown_exe_comes_only_from_the_system_directory() {
        assert_eq!(exe_in(Some(PathBuf::from(r"C:\Windows\System32"))), Some(PathBuf::from(r"C:\Windows\System32\shutdown.exe")));
        // Not known, or not absolute: nothing is run (no search by name).
        assert_eq!(exe_in(None), None);
        assert_eq!(exe_in(Some(PathBuf::from("System32"))), None);
        assert_eq!(exe_in(Some(PathBuf::new())), None);
        // Windows' answer, not %SystemRoot% (only read here, never run).
        let dir = system_dir().expect("the system directory");
        assert!(dir.is_absolute(), "{dir:?}");
        assert!(dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("system32")), "{dir:?}");
        let src = include_str!("shutdown.rs");
        let code = src.find("#[cfg(test)]").map_or(src, |i| &src[..i]);
        assert!(!code.contains("var_os(\"SystemRoot\")") && !code.contains("unwrap_or_else(|| \"shutdown.exe\""));
    }

    #[test]
    fn the_wait_is_measured_on_the_tick_count_not_the_wall_clock() {
        for kind in KINDS {
            let (s, gen) = scheduled_as(kind);
            // The label is the wall clock's; the wait the tick count's.
            assert_eq!(s.pending, Pending::At(kind, 1_000 + 1_800));
            assert_eq!(s.due_ms, 30 * MIN);
            assert_eq!(s.tick(gen, 0), Tick::Wait(NAP_MS));
            assert_eq!(s.tick(gen, 30 * MIN - 2), Tick::Wait(2));
            assert_eq!(s.tick(gen, 30 * MIN), Tick::Fire);
            // Seen a little late (a busy PC): still run.
            assert_eq!(s.tick(gen, 30 * MIN + LATE_MS), Tick::Fire);
            // The clock set an hour on or back changes nothing: tick never
            // reads it (the same ms, the same step).
            assert_eq!(s.tick(gen, 10 * MIN), Tick::Wait(NAP_MS));
            // The menu shows it until it is due.
            assert_eq!(s.shown(30 * MIN - 1), Pending::At(kind, 2_800));
            assert_eq!(s.shown(30 * MIN), Pending::None);
        }
    }

    #[test]
    fn a_time_slept_through_is_never_run_on_waking() {
        for kind in KINDS {
            // Asleep a minute before the time, awake 1 s, 30 s, 60 s or an
            // hour after it: dropped, the timer first or the resume first.
            for late in [1_000, 30_000, 60_000, 3_600_000] {
                let woke = 30 * MIN + late;
                // Resume first.
                let (mut s, gen) = scheduled_as(kind);
                s.suspend();
                s.resume(woke);
                assert_eq!(s.pending, Pending::None, "{kind:?} {late}");
                assert_eq!(s.tick(gen, woke), Tick::Stop, "{kind:?} {late}");
                // Timer first: it waits for the resume, never fires.
                let (mut s, gen) = scheduled_as(kind);
                s.suspend();
                assert!(matches!(s.tick(gen, woke), Tick::Wait(_) | Tick::Drop), "{kind:?} {late}");
                assert_eq!(s.begin_fire(gen, woke), None, "{kind:?} {late}");
                s.resume(woke);
                assert_eq!(s.tick(gen, woke), Tick::Stop, "{kind:?} {late}");
                assert_eq!(s.shown(woke), Pending::None);
            }
            // Woken before the time: kept, and run at its time.
            let (mut s, gen) = scheduled_as(kind);
            s.suspend();
            s.resume(20 * MIN);
            assert_eq!(s.pending, Pending::At(kind, 2_800));
            assert_eq!(s.tick(gen, 30 * MIN), Tick::Fire);
            // No broadcast at all: more than a minute late is dropped.
            let (mut s, gen) = scheduled_as(kind);
            assert_eq!(s.tick(gen, 30 * MIN + LATE_MS + 1), Tick::Drop);
            s.drop_missed(gen);
            assert_eq!((s.pending, s.tick(gen, 31 * MIN + LATE_MS)), (Pending::None, Tick::Stop));
        }
        // Another app's is Windows' to keep.
        let mut s = Sched { pending: Pending::Unknown, ..Default::default() };
        s.suspend();
        s.resume(u64::MAX / 2);
        assert_eq!(s.pending, Pending::Unknown);
        // Which broadcasts are a wake-up.
        assert!(is_resume(0x7) && is_resume(0x12));
        assert!(!is_resume(PBT_APMSUSPEND) && !is_resume(0xA) && !is_resume(0x8013));
    }

    #[test]
    fn an_older_run_never_overwrites_a_newer_schedule() {
        // A fires; while its action answers, B is scheduled (either kind);
        // A's answer (1190 or a failure) then leaves B alone.
        for (ka, kb) in [(Kind::Shutdown, Kind::Shutdown), (Kind::Shutdown, Kind::Sleep), (Kind::Sleep, Kind::Shutdown), (Kind::Sleep, Kind::Sleep)] {
            for code in [Some(ALREADY_SCHEDULED), Some(5), None, Some(0)] {
                let (mut s, a) = scheduled_as(ka);
                assert_eq!(s.begin_fire(a, 30 * MIN), Some(ka));
                assert_eq!(s.shown(30 * MIN), Pending::None);
                let b = s.schedule(kb, 15, 30 * MIN, 5_000).unwrap();
                s.end_fire(a, ka, code);
                assert_eq!(s.pending, Pending::At(kb, 5_000 + 900), "{ka:?} {kb:?} {code:?}");
                assert_eq!(s.tick(b, 31 * MIN), Tick::Wait(NAP_MS), "B's timer goes on");
                assert_eq!(s.tick(a, 31 * MIN), Tick::Stop, "A's ends");
            }
        }
        // Nothing newer: after a shutdown, Windows' other one shows; after
        // a sleep, 1190 means nothing of the kind (it is not shutdown.exe's).
        let (mut s, a) = scheduled();
        assert_eq!(s.begin_fire(a, 30 * MIN), Some(Kind::Shutdown));
        s.end_fire(a, Kind::Shutdown, Some(ALREADY_SCHEDULED));
        assert_eq!(s.pending, Pending::Unknown);
        let (mut s, a) = scheduled_as(Kind::Sleep);
        assert_eq!(s.begin_fire(a, 30 * MIN), Some(Kind::Sleep));
        s.end_fire(a, Kind::Sleep, Some(ALREADY_SCHEDULED));
        assert_eq!(s.pending, Pending::None);
        // A fire runs once.
        for kind in KINDS {
            let (mut s, a) = scheduled_as(kind);
            assert_eq!(s.begin_fire(a, 30 * MIN), Some(kind));
            assert_eq!(s.begin_fire(a, 30 * MIN), None);
        }
    }

    #[test]
    fn one_schedule_at_a_time_the_newest_wins() {
        // A sleep scheduled over a shutdown (or the other way) replaces
        // it: the old timer ends, only the new one runs, at its own time.
        for (old, new) in [(Kind::Shutdown, Kind::Sleep), (Kind::Sleep, Kind::Shutdown)] {
            let (mut s, a) = scheduled_as(old);
            let b = s.schedule(new, 15, 5 * MIN, 1_300).unwrap();
            assert_eq!(s.pending, Pending::At(new, 1_300 + 900));
            assert_eq!(s.tick(a, 30 * MIN), Tick::Stop, "{old:?}'s timer ends");
            assert_eq!(s.begin_fire(a, 20 * MIN), None);
            assert_eq!(s.tick(b, 20 * MIN), Tick::Fire);
            assert_eq!(s.begin_fire(b, 20 * MIN), Some(new));
        }
    }

    #[test]
    fn cancelling_ours_only_stops_the_timer() {
        for kind in KINDS {
            let (mut s, gen) = scheduled_as(kind);
            assert_eq!(s.cancel(), CancelPlan::Done);
            assert_eq!((s.pending, s.tick(gen, 30 * MIN)), (Pending::None, Tick::Stop));
            assert_eq!(s.begin_fire(gen, 30 * MIN), None, "never runs after a cancel");
        }
    }

    #[test]
    fn another_apps_shutdown_goes_only_when_windows_says_so() {
        let unknown = || Sched { pending: Pending::Unknown, ..Default::default() };
        // Success and "none in progress": the row goes.
        for code in [0, NONE_IN_PROGRESS] {
            let mut s = unknown();
            let CancelPlan::Abort(g) = s.cancel() else { panic!() };
            s.end_cancel(g, Some(code));
            assert_eq!(s.pending, Pending::None, "{code}");
        }
        // Any other answer, or not run: it stays, to try again.
        for code in [Some(5), Some(1), Some(ALREADY_SCHEDULED), None] {
            let mut s = unknown();
            let CancelPlan::Abort(g) = s.cancel() else { panic!() };
            s.end_cancel(g, code);
            assert_eq!(s.pending, Pending::Unknown, "{code:?}");
            assert!(matches!(s.cancel(), CancelPlan::Abort(_)), "it can be tried again");
        }
        // A second request while one is on its way sends nothing.
        let mut s = unknown();
        let CancelPlan::Abort(g) = s.cancel() else { panic!() };
        assert_eq!(s.cancel(), CancelPlan::Done);
        // Done, then a new schedule: a late answer leaves it alone.
        s.end_cancel(g, Some(0));
        let b = s.schedule(Kind::Sleep, 15, 0, 0).unwrap();
        s.end_cancel(g, Some(0));
        assert_eq!(s.tick(b, 0), Tick::Wait(NAP_MS));
        // The row runs `/a` through the injected runner only.
        let log = RefCell::new(Vec::<Vec<String>>::new());
        let run = |a: &[String]| {
            log.borrow_mut().push(a.to_vec());
            Some(0)
        };
        let mut s = unknown();
        if let CancelPlan::Abort(g) = s.cancel() {
            let code = run(&cancel_args());
            s.end_cancel(g, code);
        }
        assert_eq!(*log.borrow(), [cancel_args()]);
        assert_eq!(s.pending, Pending::None);
    }

    #[test]
    fn quitting_nopbar_stops_the_timer_before_it_can_fire() {
        for kind in KINDS {
            // Due as NopBar quits: stop wins (begin_fire decides under the
            // same lock), and nothing is scheduled afterwards.
            let (mut s, gen) = scheduled_as(kind);
            s.stop();
            assert_eq!(s.begin_fire(gen, 30 * MIN), None);
            assert_eq!(s.tick(gen, 30 * MIN), Tick::Stop);
            assert_eq!(s.shown(0), Pending::None);
            assert_eq!(s.schedule(kind, 15, 0, 0), None);
            // Stopped while its action was answering: the answer is dropped.
            let (mut s, gen) = scheduled_as(kind);
            assert_eq!(s.begin_fire(gen, 30 * MIN), Some(kind));
            s.stop();
            s.end_fire(gen, kind, Some(ALREADY_SCHEDULED));
            assert_eq!(s.pending, Pending::None);
            assert_eq!(s.cancel(), CancelPlan::Done);
            // A cancelled logoff: scheduling works again, the old one stays gone.
            let (mut s, a) = scheduled_as(kind);
            s.stop();
            s.reopen();
            assert_eq!(s.tick(a, 30 * MIN), Tick::Stop);
            let b = s.schedule(kind, 15, 0, 0).expect("schedules again");
            assert_eq!(s.tick(b, 15 * MIN), Tick::Fire);
        }
    }

    /// The timer on a fake clock that naps forward by itself (nothing
    /// sleeps); `fire` only records what it was asked to run.
    fn run_fake(state: &Mutex<Sched>, gen: u64, start: u64, between: &dyn Fn(&Mutex<Sched>, u64)) -> (Vec<Kind>, u64) {
        let clock = Cell::new(start);
        let fired = RefCell::new(Vec::new());
        run_timer(
            state,
            gen,
            &|| clock.get(),
            &mut |ms| {
                clock.set(clock.get() + ms);
                between(state, clock.get());
            },
            &mut |k| {
                fired.borrow_mut().push(k);
                Some(0)
            },
        );
        (fired.into_inner(), clock.get())
    }

    #[test]
    fn the_timer_runs_its_own_action_once_at_its_time() {
        for kind in KINDS {
            let (s, gen) = scheduled_as(kind);
            let state = Mutex::new(s);
            let (fired, at) = run_fake(&state, gen, 0, &|_, _| {});
            assert_eq!((fired, at), (vec![kind], 30 * MIN), "{kind:?}");
            assert_eq!(state.lock().unwrap().pending, Pending::None);
        }
        // Cancelled on the way: it ends without running anything.
        let (s, gen) = scheduled_as(Kind::Sleep);
        let state = Mutex::new(s);
        let (fired, _) = run_fake(&state, gen, 0, &|st, now| {
            if now >= 10 * MIN {
                st.lock().unwrap().cancel();
            }
        });
        assert!(fired.is_empty());
        // Slept through (a suspend, then a resume after its time): dropped.
        let (s, gen) = scheduled_as(Kind::Sleep);
        let state = Mutex::new(s);
        let (fired, _) = run_fake(&state, gen, 0, &|st, now| {
            let mut s = st.lock().unwrap();
            if now == 25 * MIN {
                s.suspend();
            } else if now >= 30 * MIN {
                s.resume(now + 3_600_000);
            }
        });
        assert!(fired.is_empty());
        assert_eq!(state.lock().unwrap().pending, Pending::None);
        // Replaced by a shutdown: the sleep timer ends without running.
        let (s, gen) = scheduled_as(Kind::Sleep);
        let state = Mutex::new(s);
        let (fired, _) = run_fake(&state, gen, 0, &|st, now| {
            if now == 5 * MIN {
                st.lock().unwrap().schedule(Kind::Shutdown, 60, now, 0);
            }
        });
        assert!(fired.is_empty());
        assert!(matches!(state.lock().unwrap().pending, Pending::At(Kind::Shutdown, _)));
        // First seen long after its time, with no broadcast: dropped.
        let (s, gen) = scheduled_as(Kind::Sleep);
        let state = Mutex::new(s);
        let (fired, _) = run_fake(&state, gen, 30 * MIN + LATE_MS + 1, &|_, _| {});
        assert!(fired.is_empty());
        assert_eq!(state.lock().unwrap().pending, Pending::None);
    }
}

/// The active local schedule; no operating-system action is performed.
pub fn remaining()->Option<(u64,u64)>{with(|s|if s.ours(s.gen)&&!s.cancelling&&!s.slept&&!s.stopped {Some((s.gen,s.due_ms.saturating_sub(now_ms())))}else{None}).flatten()}
/// Atomic generation guard: a stale warning never cancels a newer schedule.
/// Local cancel uses the same Sched::cancel path as the system menu.
fn cancel_matching(s:&mut Sched,gen:u64)->bool {if s.ours(gen)&&!s.cancelling {s.cancel();true}else{false}}
pub fn cancel_generation(gen:u64)->bool {let cancelled=with(|s|cancel_matching(s,gen)).unwrap_or(false);if cancelled{crate::companion::refresh();}cancelled}

#[cfg(test)]mod companion_tests {use super::*;
#[test]fn warning_click_cancels_only_its_local_generation(){let mut s=Sched::default();let first=s.schedule(Kind::Shutdown,1,0,0).unwrap();let next=s.schedule(Kind::Sleep,2,10,0).unwrap();assert!(!cancel_matching(&mut s,first));assert!(s.ours(next));assert!(cancel_matching(&mut s,next));assert_eq!(s.pending,Pending::None);assert!(!cancel_matching(&mut s,next));}
}
