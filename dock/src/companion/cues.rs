//! One-shot reactions and overlapping quiet reasons. No OS calls here.
use super::{clips, model::State};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cue {
    FocusStart,
    BreakStart,
    BreakEnd,
    Shutdown(u64),
    ShutdownCancelled,
    Battery { level: u8, cycle: u64 },
    Charging(u64),
    Agenda { key: String, at: i64 },
    Removable,
    Update(String),
}
#[derive(Clone, Copy, Debug)]
pub enum Calm {
    Focus,
    Meeting,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Click {
    CancelShutdown(u64),
    Agenda(i64),
}
impl Cue {
    pub fn focus_feedback(&self) -> bool {
        matches!(self, Self::FocusStart | Self::BreakStart | Self::BreakEnd)
    }
    fn kind(&self) -> u8 {
        match self {
            Self::FocusStart => 0,
            Self::BreakStart => 1,
            Self::BreakEnd => 2,
            Self::Shutdown(_) => 3,
            Self::ShutdownCancelled => 4,
            Self::Battery { level: 10, .. } => 10,
            Self::Battery { .. } => 5,
            Self::Charging(_) => 6,
            Self::Agenda { .. } => 7,
            Self::Removable => 8,
            Self::Update(_) => 9,
        }
    }
    fn target(&self) -> Option<String> {
        match self {
            Self::Battery { level, cycle } => Some(format!("{cycle}/{level}")),
            Self::Agenda { key, at } => Some(format!("{key}/{at}")),
            Self::Update(v) => Some(v.clone()),
            _ => None,
        }
    }
    pub fn priority(&self) -> u8 {
        match self {
            Self::Shutdown(_) => 5,
            Self::Agenda { .. } => 3,
            Self::FocusStart | Self::BreakStart | Self::BreakEnd => 2,
            _ => 1,
        }
    }
    pub fn bounces(&self) -> bool {
        matches!(
            self,
            Self::BreakStart | Self::BreakEnd | Self::Shutdown(_) | Self::Agenda { .. }
        )
    }
    fn clip(&self) -> &'static str {
        match self {
            Self::FocusStart | Self::ShutdownCancelled | Self::Update(_) => "peek",
            Self::BreakStart => "done",
            Self::BreakEnd | Self::Shutdown(_) | Self::Agenda { .. } => "notify",
            Self::Battery { .. } => "sit",
            Self::Charging(_) => "idle",
            Self::Removable => "wait",
        }
    }
    pub fn effects(&self) -> &'static [&'static str] {
        match self {
            Self::FocusStart => &["ticks_right"],
            Self::BreakStart => &[],
            Self::BreakEnd | Self::Agenda { .. } => &[],
            Self::Shutdown(_) => &[],
            Self::ShutdownCancelled | Self::Charging(_) | Self::Update(_) => &["sparkle"],
            Self::Battery { .. } => &["drop"],
            Self::Removable => &[],
        }
    }
}
struct Pending {
    cue: Cue,
    at: Instant,
    elapsed: f32,
    emitted: bool,
}
#[derive(Default)]
pub struct Reactions {
    queue: Vec<Pending>,
    last: HashMap<u8, Instant>,
    targets: HashSet<(u8, String)>,
    calm: u8,
    suppressed: bool,
    pub serial: u64,
}
pub struct Reaction {
    pub state: State,
    pub effects: &'static [&'static str],
    pub quiet: bool,
    pub active: bool,
    pub started: bool,
}
impl Reactions {
    pub fn suppress(&mut self, quiet: bool) { self.suppressed = quiet; }
    pub fn calm(&mut self, reason: Calm, on: bool) {
        let bit = match reason {
            Calm::Focus => 1,
            Calm::Meeting => 2,
        };
        if on {
            self.calm |= bit;
        } else {
            self.calm &= !bit;
        }
    }
    pub fn quiet(&self) -> bool {
        self.calm != 0
    }
    pub fn clear(&mut self) {
        self.queue.clear();
    }
    pub fn accept(&mut self, cue: Cue, now: Instant, visible: bool) -> bool {
        let kind = cue.kind();
        if self
            .last
            .get(&kind)
            .is_some_and(|at| now.saturating_duration_since(*at) < Duration::from_secs(600))
        {
            return false;
        }
        if let Some(target) = cue.target() {
            if self.targets.contains(&(kind, target.clone())) {
                return false;
            }
            if self.targets.len() >= 1024 {
                self.targets.clear();
            }
            self.targets.insert((kind, target));
        }
        self.last.insert(kind, now);
        let bounce = !visible && cue.bounces();
        if visible || matches!(cue, Cue::Shutdown(_) | Cue::Agenda { .. }) {
            if self.queue.len() >= 16 {
                self.queue.remove(0);
            }
            self.queue.push(Pending {
                cue,
                at: now,
                elapsed: 0.,
                emitted: false,
            });
            self.serial += 1;
        }
        bounce
    }
    pub fn retain_shutdown(&mut self, generation: Option<u64>) {
        self.queue
            .retain(|p| !matches!(p.cue,Cue::Shutdown(g) if Some(g)!=generation));
    }
    fn best(&self, agent_wait: bool) -> Option<usize> {
        self.queue
            .iter()
            .enumerate()
            .filter(|(_, p)| !agent_wait || p.cue.priority() > 4)
            .filter(|(_, p)| !self.suppressed || p.cue.focus_feedback())
            .max_by_key(|(_, p)| p.cue.priority())
            .map(|(i, _)| i)
    }
    pub fn click(&self, now: Instant, agent_wait: bool) -> Option<Click> {
        let p = &self.queue[self.best(agent_wait)?];
        if now.saturating_duration_since(p.at) > Duration::from_secs(600) {
            return None;
        }
        match p.cue {
            Cue::Shutdown(g) => Some(Click::CancelShutdown(g)),
            Cue::Agenda { at, .. } => Some(Click::Agenda(at)),
            _ => None,
        }
    }
    pub fn clicked(&mut self, click: &Click) {
        self.queue.retain(|p| match (&p.cue, click) {
            (Cue::Shutdown(g), Click::CancelShutdown(c)) => g != c,
            (Cue::Agenda { at, .. }, Click::Agenda(c)) => at != c,
            _ => true,
        });
    }
    pub fn tick(&mut self, now: Instant, dt: f32, base: State, agent_wait: bool) -> Reaction {
        self.queue
            .retain(|p| !p.emitted || now.saturating_duration_since(p.at) < Duration::from_secs(600));
        let quiet = self.quiet();
        loop {
            let Some(i) = self.best(agent_wait) else {
                return Reaction {
                    state: if quiet
                        && matches!(base, State::Idle | State::Walk | State::Sleep | State::Sit)
                    {
                        State::Idle
                    } else {
                        base
                    },
                    effects: &[],
                    quiet,
                    active: false,
                    started: false,
                };
            };
            let p = &mut self.queue[i];
            let duration = clips::catalog().clip(p.cue.clip()).duration();
            if p.elapsed >= duration && !matches!(p.cue, Cue::Shutdown(_)) {
                self.queue.remove(i);
                self.serial += 1;
                continue;
            }
            let state = if matches!(p.cue, Cue::Shutdown(_)) && p.elapsed >= duration {
                State::Wait
            } else {
                match p.cue.clip() {
                    "peek" => State::Peek,
                    "done" => State::Done,
                    "notify" => State::Notify,
                    "sit" => State::Sit,
                    "wait" => State::Wait,
                    _ => State::Idle,
                }
            };
            let started = !p.emitted;
            let effects = if p.emitted { &[] } else { p.cue.effects() };
            p.emitted = true;
            p.elapsed += dt.clamp(0., 0.25);
            // The focus-start acknowledgment finishes before focus calm begins;
            // an active meeting always suppresses decorative particles.
            return Reaction {
                state,
                effects,
                active: true,
                started,
                quiet: quiet && !p.cue.focus_feedback(),
            };
        }
    }
}
/// Take only displayable signals. Deferred signals keep their queue position.
pub fn take_ready(queue: &mut Vec<(Cue, Instant)>, quiet: bool) -> Vec<(Cue, Instant)> {
    let (ready, deferred) = std::mem::take(queue).into_iter()
        .partition(|(cue, _): &(Cue, Instant)| !quiet || cue.focus_feedback());
    *queue = deferred;
    ready
}
pub fn sleep_after(hour: u16, enabled: bool) -> f32 {
    if enabled && hour < 6 {
        200.
    } else {
        600.
    }
}
#[derive(Default)]
pub struct PowerEdges {
    previous: Option<bool>,
    cycle: u64,
    low20: bool,
    low10: bool,
}
impl PowerEdges {
    pub const fn new() -> Self {
        Self {
            previous: None,
            cycle: 0,
            low20: false,
            low10: false,
        }
    }
    pub fn observe(&mut self, pct: Option<u8>, ac: bool) -> Vec<Cue> {
        let mut out = Vec::new();
        if self.previous == Some(false) && ac {
            self.cycle += 1;
            self.low20 = false;
            self.low10 = false;
            out.push(Cue::Charging(self.cycle));
        }
        self.previous = Some(ac);
        if !ac {
            if let Some(p) = pct {
                for (level, seen) in [(20, &mut self.low20), (10, &mut self.low10)] {
                    if p < level && !*seen {
                        *seen = true;
                        out.push(Cue::Battery {
                            level,
                            cycle: self.cycle,
                        });
                    }
                }
            }
        }
        out
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quiet_retains_other_cues_and_all_focus_feedback_remains_visible() {
        let t = Instant::now();
        let mut queue = vec![(Cue::Update("v1".into()), t), (Cue::FocusStart, t),
            (Cue::BreakStart, t), (Cue::BreakEnd, t)];
        let ready = take_ready(&mut queue, true);
        assert_eq!(ready.len(), 3); assert_eq!(queue.len(), 1);
        for (cue, _) in ready {
            let mut r = Reactions::default(); r.calm(Calm::Focus, true);
            r.calm(Calm::Meeting, true); r.suppress(true);
            r.accept(cue, t, true);
            let reaction = r.tick(t, 0.1, State::Idle, false);
            assert!(reaction.active); assert!(!reaction.quiet);
        }
        let mut r = Reactions::default(); r.suppress(true);
        r.accept(Cue::Battery { level: 10, cycle: 1 }, t, true);
        assert!(!r.tick(t, 0.1, State::Idle, false).active);
        r.suppress(false);
        assert!(r.tick(t + Duration::from_secs(1000), 0.1, State::Idle, false).active);
        assert_eq!(take_ready(&mut queue, false).len(), 1);
        assert!(take_ready(&mut queue, false).is_empty());
    }
    #[test]
    fn every_notification_returns_to_quiet_idle_after_completion() {
        let t = Instant::now();
        for cue in [Cue::FocusStart, Cue::BreakStart, Cue::BreakEnd,
            Cue::ShutdownCancelled, Cue::Battery {level:10,cycle:1},
            Cue::Charging(1), Cue::Agenda {key:"fixture".into(),at:1},
            Cue::Removable, Cue::Update("fixture".into())] {
            let mut reactions = Reactions::default();
            reactions.accept(cue.clone(),t,true);
            assert!(reactions.tick(t,0.25,State::Idle,false).active);
            for _ in 0..120 { reactions.tick(t,0.25,State::Idle,false); }
            let result=reactions.tick(t,0.25,State::Idle,false);
            assert!(!result.active,"{cue:?}");
            assert_eq!(result.state,State::Idle,"{cue:?}");
            assert!(result.effects.is_empty());
        }
    }
    #[test]
    fn priorities_spacing_targets_and_hidden() {
        let t = Instant::now();
        let mut m = Reactions::default();
        assert!(!m.accept(Cue::FocusStart, t, false));
        assert!(m.accept(Cue::BreakStart, t, false));
        assert!(!m.accept(Cue::BreakStart, t + Duration::from_secs(1), false));
        m.accept(
            Cue::Agenda {
                key: "a".into(),
                at: 50,
            },
            t,
            true,
        );
        assert_eq!(m.tick(t, 0.1, State::Wait, true).state, State::Wait);
        m.accept(Cue::Shutdown(1), t, true);
        assert_eq!(m.tick(t, 0.1, State::Wait, true).state, State::Notify);
        assert_eq!(m.click(t, true), Some(Click::CancelShutdown(1)));
        m.clicked(&Click::CancelShutdown(1));
        assert_eq!(m.click(t, false), Some(Click::Agenda(50)));
        m.accept(Cue::Update("1".into()), t, true);
        let n = m.queue.len();
        m.accept(Cue::Update("1".into()), t + Duration::from_secs(601), true);
        assert_eq!(m.queue.len(), n);
    }
    #[test]
    fn calm_reasons_overlap_and_resume() {
        let t = Instant::now();
        let mut m = Reactions::default();
        m.calm(Calm::Focus, true);
        m.calm(Calm::Meeting, true);
        m.calm(Calm::Focus, false);
        assert!(m.tick(t, 0.1, State::Walk, false).quiet);
        assert_eq!(m.tick(t, 0.1, State::Sleep, false).state, State::Idle);
        assert_eq!(m.tick(t, 0.1, State::Idle, false).state, State::Idle);
        m.calm(Calm::Meeting, false);
        assert_eq!(m.tick(t, 0.1, State::Walk, false).state, State::Walk);
    }
    #[test]
    fn one_shot_returns_and_warning_waits_until_cancel() {
        let t = Instant::now();
        let mut m = Reactions::default();
        m.accept(Cue::Removable, t, true);
        for _ in 0..8 {
            m.tick(t, 0.2, State::Idle, false);
        }
        assert_eq!(m.tick(t, 0.1, State::Idle, false).state, State::Idle);
        m.accept(Cue::Shutdown(2), t, true);
        for _ in 0..8 {
            m.tick(t, 0.2, State::Idle, false);
        }
        assert_eq!(m.tick(t, 0.1, State::Idle, false).state, State::Wait);
        m.retain_shutdown(None);
        assert_eq!(m.click(t, false), None);
        assert_eq!(sleep_after(0, true), 200.);
        assert_eq!(sleep_after(5, true), 200.);
        assert_eq!(sleep_after(6, true), 600.);
        assert_eq!(sleep_after(2, false), 600.);
    }
    #[test]
    fn battery_edges_once_per_discharge() {
        let mut p = PowerEdges::default();
        assert_eq!(p.observe(Some(19), false).len(), 1);
        assert!(p.observe(Some(18), false).is_empty());
        assert_eq!(p.observe(Some(9), false).len(), 1);
        assert_eq!(p.observe(Some(10), true).len(), 1);
        assert_eq!(p.observe(Some(9), false).len(), 2);
    }
}
