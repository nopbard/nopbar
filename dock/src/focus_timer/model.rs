//! The focus timer's state machine, free of windows, clocks and threads:
//! every call takes "now" (Unix milliseconds, wall clock) and works from
//! absolute deadlines, so a PC that slept through a deadline catches up in
//! one step (the waitable timer fires on resume; `advance` settles every
//! phase that ended meanwhile). The tests drive it with plain numbers.

/// A focus length and the break that follows it (milliseconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub focus_ms: i64,
    pub break_ms: i64,
}

impl Plan {
    pub const fn minutes(focus: u32, brk: u32) -> Plan {
        Plan { focus_ms: focus as i64 * 60_000, break_ms: brk as i64 * 60_000 }
    }
}

/// The presets offered: 25 / 5 and 50 / 10 (the third is the user's own,
/// from Settings).
pub const SHORT: Plan = Plan::minutes(25, 5);
pub const LONG: Plan = Plan::minutes(50, 10);

/// What "+5 min" adds.
pub const EXTEND_MS: i64 = 5 * 60_000;
/// Longest a phase may grow to with "+5 min" (a cap against a stuck key).
pub const MAX_PHASE_MS: i64 = 6 * 3_600_000;
/// How long the "Break over" notice stays on the island.
pub const NOTICE_MS: i64 = 8_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Focus,
    Break,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    /// Ends at this wall-clock time (Unix ms).
    Running { deadline: i64 },
    /// Stopped with this much left.
    Paused { left: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Session {
    pub phase: Phase,
    pub plan: Plan,
    /// This phase's whole length (with any "+5 min"), for the ring.
    pub total: i64,
    pub run: Run,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Start(Plan),
    Pause,
    Resume,
    /// Focus: on to the break (not counted). Break: done.
    Skip,
    Stop,
    /// "+5 min" to the phase running (or paused).
    Extend,
}

/// What happened that the outside world hears about (a chime, a count, Do
/// not disturb on or off).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A focus phase ran to its end, at this deadline (Unix ms). Counted
    /// only when it ended while the PC was awake (`counts`).
    FocusDone(i64),
    /// A break ran to its end (or was skipped): the timer is over.
    BreakDone,
    /// The user started a focus phase.
    FocusStarted,
    /// A focus phase ended early (skipped or stopped).
    FocusLeft,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timer {
    pub session: Option<Session>,
    /// "Break over" shows on the island until then (Unix ms).
    pub notice_until: Option<i64>,
}

impl Timer {
    /// Time left in the phase (0 when over or idle).
    pub fn left(&self, now: i64) -> i64 {
        match self.session.map(|s| s.run) {
            Some(Run::Running { deadline }) => (deadline - now).max(0),
            Some(Run::Paused { left }) => left.max(0),
            None => 0,
        }
    }

    pub fn running(&self) -> bool {
        matches!(self.session.map(|s| s.run), Some(Run::Running { .. }))
    }

    pub fn idle(&self) -> bool {
        self.session.is_none() && self.notice_until.is_none()
    }

    /// Applies a user's command at `now`, after settling what ended before
    /// it. Returns everything that happened, in order.
    pub fn apply(&mut self, e: Event, now: i64) -> Vec<Outcome> {
        let mut out = self.advance(now);
        match (e, self.session) {
            (Event::Start(plan), cur) => {
                if cur.is_some_and(|s| s.phase == Phase::Focus) {
                    // A new start replaces the running focus (not counted).
                    out.push(Outcome::FocusLeft);
                }
                let plan = Plan { focus_ms: plan.focus_ms.clamp(60_000, MAX_PHASE_MS), break_ms: plan.break_ms.clamp(0, MAX_PHASE_MS) };
                self.session = Some(Session { phase: Phase::Focus, plan, total: plan.focus_ms, run: Run::Running { deadline: now + plan.focus_ms } });
                self.notice_until = None;
                out.push(Outcome::FocusStarted);
            }
            (Event::Pause, Some(mut s)) => {
                if let Run::Running { deadline } = s.run {
                    s.run = Run::Paused { left: (deadline - now).max(0) };
                    self.session = Some(s);
                }
            }
            (Event::Resume, Some(mut s)) => {
                if let Run::Paused { left } = s.run {
                    s.run = Run::Running { deadline: now + left };
                    self.session = Some(s);
                }
            }
            (Event::Extend, Some(mut s)) => {
                let room = (MAX_PHASE_MS - s.total).max(0);
                let add = EXTEND_MS.min(room);
                s.total += add;
                s.run = match s.run {
                    Run::Running { deadline } => Run::Running { deadline: deadline + add },
                    Run::Paused { left } => Run::Paused { left: left + add },
                };
                self.session = Some(s);
            }
            (Event::Skip, Some(s)) => match s.phase {
                Phase::Focus => {
                    out.push(Outcome::FocusLeft);
                    self.begin_break(s.plan, now, &mut out);
                }
                Phase::Break => {
                    self.session = None;
                    self.notice_until = Some(now + NOTICE_MS);
                    out.push(Outcome::BreakDone);
                }
            },
            (Event::Stop, Some(s)) => {
                if s.phase == Phase::Focus {
                    out.push(Outcome::FocusLeft);
                }
                self.session = None;
                self.notice_until = None;
            }
            (Event::Stop, None) => self.notice_until = None,
            (Event::Pause | Event::Resume | Event::Extend | Event::Skip, None) => {}
        }
        out
    }

    /// Settles every phase whose deadline passed by `now` (one after a
    /// short wait, several after a long sleep: the break counts from the
    /// focus deadline, not from when the PC woke) and drops a notice that
    /// is over.
    pub fn advance(&mut self, now: i64) -> Vec<Outcome> {
        let mut out = Vec::new();
        // At most two phases can end (focus, then its break).
        for _ in 0..2 {
            let Some(s) = self.session else { break };
            let Run::Running { deadline } = s.run else { break };
            if now < deadline {
                break;
            }
            match s.phase {
                Phase::Focus => {
                    out.push(Outcome::FocusDone(deadline));
                    self.begin_break(s.plan, deadline, &mut out);
                }
                Phase::Break => {
                    self.session = None;
                    self.notice_until = Some(deadline + NOTICE_MS);
                    out.push(Outcome::BreakDone);
                }
            }
        }
        if self.notice_until.is_some_and(|u| now >= u) {
            self.notice_until = None;
        }
        out
    }

    /// A break from `at` (no break planned: the timer ends there).
    fn begin_break(&mut self, plan: Plan, at: i64, out: &mut Vec<Outcome>) {
        if plan.break_ms <= 0 {
            self.session = None;
            self.notice_until = Some(at + NOTICE_MS);
            out.push(Outcome::BreakDone);
            return;
        }
        self.session = Some(Session { phase: Phase::Break, plan, total: plan.break_ms, run: Run::Running { deadline: at + plan.break_ms } });
    }

    /// When to look again (Unix ms): the running phase's deadline, else the
    /// notice's end. None: nothing to wait for.
    pub fn next_wake(&self) -> Option<i64> {
        match self.session.map(|s| s.run) {
            Some(Run::Running { deadline }) => Some(deadline),
            Some(Run::Paused { .. }) => None,
            None => self.notice_until,
        }
    }
}

/// Sessions finished today: kept per local day ("yyyymmdd" as a number).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Count {
    pub day: u32,
    pub n: u32,
}

impl Count {
    /// Today's number (another day's count is yesterday's: 0 today).
    pub fn today(&self, day: u32) -> u32 {
        if self.day == day { self.n } else { 0 }
    }

    /// One more finished on `day` (a new day starts from 1).
    pub fn add(&mut self, day: u32) {
        if self.day == day {
            self.n = self.n.saturating_add(1);
        } else {
            *self = Count { day, n: 1 };
        }
    }
}

/// How late an end may be noticed and still count as seen: the waitable
/// timer fires on time while the PC is awake; an end found later than this
/// happened while it slept (or NopBar was not running it).
pub const LATE_MS: i64 = 90_000;

/// Does a focus phase that ended at `deadline`, noticed at `now`, count
/// for today? Only when it ended while the PC was awake (noticed within
/// LATE_MS) and on today's local day (`day_of` gives the day of a time).
/// A session that ended while the PC slept is not counted, and it plays
/// no chime when the PC wakes.
pub fn counts(deadline: i64, now: i64, day_of: impl Fn(i64) -> u32) -> bool {
    now - deadline <= LATE_MS && day_of(deadline) == day_of(now)
}

/// Milliseconds until the shown seconds change (the next whole second of
/// `left`, never 0).
pub fn ms_to_next_second(left: i64) -> u32 {
    let r = left.max(0) % 1000;
    (if r == 0 { 1000 } else { r }) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn focus_left(t: &Timer, now: i64) -> (Option<Phase>, i64, bool) {
        (t.session.map(|s| s.phase), t.left(now), t.running())
    }

    #[test]
    fn start_pause_resume_skip_extend() {
        let mut t = Timer::default();
        assert!(t.idle());
        assert_eq!(t.apply(Event::Start(SHORT), 0), [Outcome::FocusStarted]);
        assert_eq!(focus_left(&t, 0), (Some(Phase::Focus), 25 * MIN, true));
        assert_eq!(t.next_wake(), Some(25 * MIN));
        // Paused at 10 min: 15 left, and it stays 15 however long it waits.
        t.apply(Event::Pause, 10 * MIN);
        assert_eq!(focus_left(&t, 10 * MIN), (Some(Phase::Focus), 15 * MIN, false));
        assert_eq!(t.left(500 * MIN), 15 * MIN);
        assert_eq!(t.next_wake(), None, "paused: no deadline");
        assert!(t.apply(Event::Pause, 11 * MIN).is_empty(), "pausing twice is nothing");
        // Resumed at 60 min: ends 15 min later.
        t.apply(Event::Resume, 60 * MIN);
        assert_eq!(t.next_wake(), Some(75 * MIN));
        // +5 min: the deadline and the ring's whole both grow.
        t.apply(Event::Extend, 61 * MIN);
        assert_eq!(t.next_wake(), Some(80 * MIN));
        assert_eq!(t.session.unwrap().total, 30 * MIN);
        // +5 while paused adds to what is left.
        t.apply(Event::Pause, 70 * MIN);
        t.apply(Event::Extend, 70 * MIN);
        assert_eq!(t.left(70 * MIN), 15 * MIN);
        t.apply(Event::Resume, 70 * MIN);
        // Skipping focus goes to the break, not counted.
        assert_eq!(t.apply(Event::Skip, 71 * MIN), [Outcome::FocusLeft]);
        assert_eq!(focus_left(&t, 71 * MIN), (Some(Phase::Break), 5 * MIN, true));
        assert_eq!(t.session.unwrap().total, 5 * MIN, "the break's own length");
        // Skipping the break ends the timer with the notice.
        assert_eq!(t.apply(Event::Skip, 72 * MIN), [Outcome::BreakDone]);
        assert!(t.session.is_none());
        assert_eq!(t.next_wake(), Some(72 * MIN + NOTICE_MS));
        assert!(t.advance(72 * MIN + NOTICE_MS).is_empty());
        assert!(t.idle(), "the notice goes");
    }

    #[test]
    fn deadlines_end_phases_in_order() {
        let mut t = Timer::default();
        t.apply(Event::Start(LONG), 1_000);
        assert!(t.advance(1_000 + 50 * MIN - 1).is_empty(), "not yet");
        assert_eq!(t.advance(1_000 + 50 * MIN), [Outcome::FocusDone(1_000 + 50 * MIN)]);
        assert_eq!(focus_left(&t, 1_000 + 50 * MIN), (Some(Phase::Break), 10 * MIN, true));
        assert_eq!(t.advance(1_000 + 60 * MIN), [Outcome::BreakDone]);
        assert!(t.session.is_none() && t.notice_until.is_some());
    }

    #[test]
    fn a_sleep_jump_settles_from_the_deadlines() {
        // Asleep for an hour in the middle of a 25 / 5: focus ended at 25,
        // the break at 30 (counted from the focus deadline, not the wake).
        let mut t = Timer::default();
        t.apply(Event::Start(SHORT), 0);
        assert_eq!(t.advance(10 * MIN), []);
        assert_eq!(t.advance(70 * MIN), [Outcome::FocusDone(25 * MIN), Outcome::BreakDone], "the real deadline, not the wake");
        assert_eq!(t.notice_until, None, "the notice ended long ago");
        assert!(t.idle());
        // Woke during the break: 2 min into it, 3 left.
        let mut t = Timer::default();
        t.apply(Event::Start(SHORT), 0);
        assert_eq!(t.advance(27 * MIN), [Outcome::FocusDone(25 * MIN)]);
        assert_eq!(t.left(27 * MIN), 3 * MIN);
        // A command after a sleep settles first.
        let mut t = Timer::default();
        t.apply(Event::Start(SHORT), 0);
        assert_eq!(t.apply(Event::Pause, 26 * MIN), [Outcome::FocusDone(25 * MIN)]);
        assert_eq!(focus_left(&t, 26 * MIN), (Some(Phase::Break), 4 * MIN, false), "the break is paused");
        // A paused timer does not move across a sleep.
        let mut t = Timer::default();
        t.apply(Event::Start(SHORT), 0);
        t.apply(Event::Pause, MIN);
        assert!(t.advance(1_000 * MIN).is_empty());
        assert_eq!(t.left(1_000 * MIN), 24 * MIN);
    }

    #[test]
    fn stop_restart_and_limits() {
        let mut t = Timer::default();
        assert!(t.apply(Event::Pause, 0).is_empty() && t.apply(Event::Skip, 0).is_empty() && t.apply(Event::Extend, 0).is_empty(), "idle: nothing");
        t.apply(Event::Start(SHORT), 0);
        assert_eq!(t.apply(Event::Start(LONG), MIN), [Outcome::FocusLeft, Outcome::FocusStarted]);
        assert_eq!(t.left(MIN), 50 * MIN);
        assert_eq!(t.apply(Event::Stop, 2 * MIN), [Outcome::FocusLeft]);
        assert!(t.idle(), "stop leaves no notice");
        // A break with no length ends the timer at the focus deadline.
        t.apply(Event::Start(Plan::minutes(25, 0)), 0);
        assert_eq!(t.advance(25 * MIN), [Outcome::FocusDone(25 * MIN), Outcome::BreakDone]);
        // "+5" stops growing at the cap.
        t.apply(Event::Start(Plan { focus_ms: MAX_PHASE_MS, break_ms: 0 }), 0);
        t.apply(Event::Extend, 0);
        assert_eq!(t.left(0), MAX_PHASE_MS);
        // A focus shorter than a minute is a minute.
        t.apply(Event::Start(Plan { focus_ms: 5, break_ms: 0 }), 0);
        assert_eq!(t.left(0), MIN);
    }

    #[test]
    fn only_ends_seen_awake_count() {
        // Days of 1000 ms for the test.
        let day = |t: i64| (t / 1_000_000) as u32;
        assert!(counts(500_000, 500_000, day), "on time");
        assert!(counts(500_000, 500_000 + LATE_MS, day), "a little late (a busy moment)");
        assert!(!counts(500_000, 500_000 + LATE_MS + 1, day), "found on wake: it ended while asleep");
        assert!(!counts(999_990, 1_000_010, day), "ended yesterday, noticed after midnight");
    }

    #[test]
    fn sessions_count_per_day() {
        let mut c = Count::default();
        assert_eq!(c.today(20261102), 0);
        c.add(20261102);
        c.add(20261102);
        assert_eq!(c.today(20261102), 2);
        // Past midnight: yesterday's count is not today's.
        assert_eq!(c.today(20261103), 0);
        c.add(20261103);
        assert_eq!(c, Count { day: 20261103, n: 1 });
        // The clock went back a day (or the file is from another PC): that
        // day starts over too.
        c.add(20261101);
        assert_eq!(c.today(20261101), 1);
        assert_eq!(c.today(20261103), 0);
    }

    #[test]
    fn ticks_follow_the_seconds() {
        assert_eq!(ms_to_next_second(18_420), 420);
        assert_eq!(ms_to_next_second(18_000), 1000);
        assert_eq!(ms_to_next_second(-5), 1000);
    }
}
