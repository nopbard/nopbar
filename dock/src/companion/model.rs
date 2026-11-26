//! Pure scheduling; hidden companions need no animation timer.
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub visible: bool,
    #[serde(skip_serializing)]
    pub roam: bool,
    pub ignore_mouse: bool,
    pub snooze_until: Option<u64>,
    pub situations: bool,
    #[serde(skip_serializing)]
    pub life: bool,
    pub morning_briefing: bool,
    pub day_wrap: bool,
    pub download_notifications: bool,
    pub routine: super::routine::Routine,
    pub activity_budget: super::purpose::Budget,
    pub last_greet: Option<u32>,
    #[serde(skip_serializing)]
    pub sleep: bool,
    pub boil: bool,
    pub animations: bool,
    #[serde(flatten,with="super::size::fields")]
    pub size: Size,
    pub home: Option<super::grab::Home>,
}
impl Config {
    pub fn quiet_base(mut self) -> Self {
        self.roam = false;
        self.sleep = false;
        self
    }
    /// All manual visibility controls cancel a persisted break, even when the
    /// requested visibility already matches (e.g. a settings preview).
    pub fn set_visible(&mut self,visible:bool){self.visible=visible;self.snooze_until=None;}
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            visible: false,
            roam: true,
            ignore_mouse: false,
            snooze_until: None,
            situations: true,
            life: true,
            morning_briefing: true,
            day_wrap: true,
            download_notifications: true,
            routine: Default::default(),
            activity_budget: Default::default(),
            last_greet: None,
            sleep: true,
            boil: false,
            animations: true,
            size: Size::default(),
            home: None,
        }
    }
}
pub use super::size::Size;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Sit,
    Walk,
    Notify,
    Wait,
    Done,
    Error,
    Sleep,
    Peek,
}

#[derive(Clone, Debug, Default)]
pub struct Signal {
    pub waits: Vec<(String, Instant)>,
    /// Kept in the agent snapshot; the visual scheduler deliberately ignores work.
    #[cfg_attr(not(test), allow(dead_code))]
    pub working: bool,
    pub result: Option<(Instant, bool)>,
}
pub struct Model {
    pub state: State,
    pub signal: Signal,
    pub since: Instant,
    idle_since: Instant,
    notify_until: Option<Instant>,
    peek_until: Option<Instant>,
    replay_result: Option<(Instant, bool, Instant)>,
}
impl Model {
    pub fn new(now: Instant) -> Self {
        Self {
            state: State::Idle,
            signal: Signal::default(),
            since: now,
            idle_since: now,
            notify_until: None,
            peek_until: None,
            replay_result: None,
        }
    }
    /// Returns true only for a newly waiting session (not every refresh).
    pub fn observe(&mut self, next: Signal, now: Instant) -> bool {
        if !next.waits.is_empty()
            || next.result != self.signal.result && next.result.is_some()
        {
            self.replay_result = None;
        }
        let new_wait = next.waits.iter().any(|w| !self.signal.waits.contains(w));
        if new_wait {
            self.peek_until = None;
            self.notify_until = Some(
                now + Duration::from_secs_f32(
                    crate::companion::clips::catalog().clip("notify").duration(),
                ),
            );
        }
        if !next.waits.is_empty()
            || next.result != self.signal.result
            || !self.signal.waits.is_empty()
        {
            self.idle_since = now;
        }
        self.signal = next;
        new_wait
    }
    /// Keep the event identity intact so the next live poll cannot expire or
    /// replay the same completion again. Only unseen results get a new deadline.
    pub fn resume_after_grab(&mut self, next: Signal, now: Instant) -> bool {
        let replay = next.result.filter(|r| Some(*r) != self.signal.result);
        let waiting = self.observe(next, now);
        if self.signal.waits.is_empty() {
            self.replay_result = replay.map(|(at, failed)| (at, failed, now + Duration::from_secs(4)));
        }
        waiting
    }
    pub fn settled(&mut self,now:Instant){self.idle_since=now;self.peek_until=None;}
    pub fn landed(&mut self, now: Instant) {
        self.idle_since = now;
        self.peek_until = Some(
            now + Duration::from_secs_f32(
                crate::companion::clips::catalog().clip("peek").duration(),
            ),
        );
    }
    #[cfg(test)]
    pub fn update(&mut self, now: Instant, cfg: Config) -> State {
        self.update_with_sleep(now,cfg,600.0)
    }
    pub fn update_with_sleep(&mut self,now:Instant,cfg:Config,sleep_after:f32)->State {
        let elapsed = now.saturating_duration_since(self.idle_since).as_secs_f32();
        let state = if self.peek_until.is_some_and(|t| now < t) {
            State::Peek
        } else if !self.signal.waits.is_empty() {
            if self.notify_until.is_some_and(|t| now < t) {
                State::Notify
            } else {
                State::Wait
            }
        } else if let Some((_, failed, _)) = self.replay_result.filter(|(_, _, until)| now < *until) {
            if failed { State::Error } else { State::Done }
        } else if let Some((at, failed)) = self
            .signal
            .result
            .filter(|(at, _)| now.saturating_duration_since(*at) < Duration::from_secs(4))
        {
            let _ = at;
            if failed {
                State::Error
            } else {
                State::Done
            }
        } else if cfg.sleep
            && elapsed >= sleep_after + crate::companion::clips::catalog().clip("sit").duration()
        {
            State::Sleep
        } else if cfg.sleep && elapsed >= sleep_after {
            State::Sit
        } else if cfg.roam && elapsed >= 60.0 {
            State::Walk
        } else {
            State::Idle
        };
        if state != self.state {
            self.state = state;
            self.since = now;
        }
        state
    }
}

/// Ceil division keeps actual frame rates at or below the requested cap.
pub fn interval(
    state: State,
    visible: bool,
    suppressed: bool,
    travel: bool,
    battery: bool,
) -> Option<u32> {
    if suppressed || !visible && !travel {
        return None;
    }
    let fps = if travel {
        60
    } else {
        match state {
            State::Sleep => 15,
            State::Idle | State::Walk => 20, // continuous-layer CPU fallback, measured by harmony
            State::Sit | State::Wait => 30,
            _ => 20,
        }
    };
    Some((if battery {2000u32}else{1000u32}).div_ceil(fps))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn working_signal_leaves_idle_clock_and_life_alone() {
        let t = Instant::now();
        let mut m = Model::new(t);
        for sec in [0, 30, 60] {
            let now = t + Duration::from_secs(sec);
            m.observe(Signal { working: true, ..Default::default() }, now);
            assert_eq!(m.update(now, Config::default()), if sec < 60 { State::Idle } else { State::Walk });
        }
    }
    #[test]
    fn deferred_completion_survives_live_polls_without_changing_event_identity() {
        let t = Instant::now();
        let mut m = Model::new(t);
        let s = Signal { result: Some((t, false)), ..Default::default() };
        let landed = t + Duration::from_secs(30);
        m.resume_after_grab(s.clone(), landed);
        for seconds in [0, 1, 3, 4] {
            let now = landed + Duration::from_secs(seconds);
            m.observe(s.clone(), now);
            assert_eq!(m.update(now, Config::default()), if seconds < 4 { State::Done } else { State::Idle });
        }
        m.resume_after_grab(s, landed + Duration::from_secs(5));
        assert_eq!(m.update(landed + Duration::from_secs(5), Config::default()), State::Idle);
    }
    #[test]
    fn review_wait_interrupts_peek_and_caps_are_lower() {
        let t = Instant::now();
        let mut m = Model::new(t);
        m.landed(t);
        m.observe(
            Signal {
                waits: vec![("new".into(), t)],
                ..Default::default()
            },
            t,
        );
        assert_eq!(m.update(t, Config::default()), State::Notify);
        for s in [
            State::Notify,
            State::Wait,
            State::Done,
            State::Error,
        ] {
            assert_eq!(interval(s, true, false, false, false), Some(if matches!(s,State::Wait){34}else{50}));
        }
        assert_eq!(interval(State::Walk, true, false, false, false), Some(50));
        assert_eq!(interval(State::Walk, true, false, false, true), Some(100));
        assert_eq!(interval(State::Walk, true, false, true, false), Some(17));
    }
    #[test]
    fn priority_and_one_notification_per_wait() {
        let t = Instant::now();
        let mut m = Model::new(t);
        let c = Config::default();
        let s = Signal {
            waits: vec![("a".into(), t)],
            working: true,
            result: Some((t, true)),
        };
        assert!(m.observe(s.clone(), t));
        assert_eq!(m.update(t, c), State::Notify);
        assert!(!m.observe(s.clone(), t + Duration::from_secs(1)));
        assert_eq!(m.update(t + Duration::from_secs(1), c), State::Wait);
        let mut s2 = s.clone();
        s2.waits.push(("b".into(), t));
        assert!(m.observe(s2, t + Duration::from_secs(2)));
        assert_eq!(m.update(t + Duration::from_secs(2), c), State::Notify);
        m.observe(
            Signal {
                working: true,
                result: Some((t, true)),
                ..Default::default()
            },
            t,
        );
        assert_eq!(m.update(t, c), State::Error);
        m.observe(
            Signal {
                result: Some((t, true)),
                ..Default::default()
            },
            t,
        );
        assert_eq!(m.update(t, c), State::Error);
        assert_eq!(m.update(t + Duration::from_secs(4), c), State::Idle);
    }
    #[test]
    fn idle_deadlines_and_switches() {
        let t = Instant::now();
        let mut m = Model::new(t);
        let mut c = Config::default();
        for (sec, expected) in [
            (59, State::Idle),
            (60, State::Walk),
            (600, State::Sit),
            (602, State::Sleep),
        ] {
            assert_eq!(m.update(t + Duration::from_secs(sec), c), expected);
        }
        c.sleep = false;
        assert_eq!(m.update(t + Duration::from_secs(700), c), State::Walk);
        c.roam = false;
        assert_eq!(m.update(t + Duration::from_secs(700), c), State::Idle);
        m.landed(t);
        assert_eq!(m.update(t, c), State::Peek);
        assert_eq!(
            m.update(
                t + Duration::from_secs_f32(
                    crate::companion::clips::catalog().clip("peek").duration()
                ),
                c
            ),
            State::Idle
        );
    }
    #[test]
    fn timers_stop_and_battery_halves_fps() {
        assert_eq!(interval(State::Idle, false, false, false, false), None);
        assert_eq!(interval(State::Idle, true, true, false, false), None);
        assert_eq!(interval(State::Idle, true, false, false, false), Some(50));
        assert_eq!(interval(State::Idle, true, false, false, true), Some(100));
        assert_eq!(interval(State::Sleep, true, false, false, true), Some(134));
        assert_eq!(interval(State::Sleep, true, false, true, false), Some(17));
    }
    #[test]
    fn config_defaults_and_roundtrip() {
        let old: crate::config::Config = serde_json::from_str("{}").unwrap();
        assert_eq!(old.companion, Config::default());
        assert!(old.companion.enabled);
        assert!(old.companion.life);
        assert_eq!(old.companion.last_greet,None);
        assert!(!old.companion.boil);
        let previous: Config = serde_json::from_str(r#"{"visible":true,"boil":true}"#).unwrap();
        assert!(previous.enabled && previous.visible && previous.boil);
        let mut c = old;
        c.companion.visible = true;
        c.companion.size = Size::new(0.70);
        c.companion.last_greet = Some(20261113);
        let again: crate::config::Config =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(again.companion, c.companion);
    }

    #[test]
    fn legacy_activity_fields_are_read_only() {
        let old: Config = serde_json::from_str(r#"{"roam":false,"life":false,"sleep":false}"#).unwrap();
        assert!(!old.roam && !old.life && !old.sleep);
        let written = serde_json::to_value(old).unwrap();
        for field in ["roam", "life", "sleep"] {
            assert!(written.get(field).is_none(), "{field}");
        }
    }
}

#[cfg(test)] mod assistant_quiet_tests {
    use super::*;
    #[test] fn legacy_preferences_cannot_restart_autonomous_motion() {
        let now=Instant::now();
        for life in [false,true] { for roam in [false,true] { for sleep in [false,true] {
            let cfg=Config {life,roam,sleep,..Default::default()}.quiet_base();
            let mut model=Model::new(now);
            for seconds in [0,60,600,7200,86400] {
                assert_eq!(model.update(now+Duration::from_secs(seconds),cfg),State::Idle);
            }
        }}}
    }
}
