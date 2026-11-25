//! A seek's short pause is still the same live track. Pure, monotonic time.
use super::model::Track;
use std::time::{Duration, Instant};
#[derive(Clone, Default)]
pub(super) struct Grace {
    until: Option<Instant>,
}
fn identity(a: &Track, b: &Track) -> bool {
    a.session == b.session
        && a.gen == b.gen
        && a.app == b.app
        && a.title == b.title
        && a.artist == b.artist
}
pub(super) fn same_pill(a: Option<&Track>, b: Option<&Track>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => identity(a, b) && a.playing == b.playing
            && a.can_toggle == b.can_toggle && a.can_prev == b.can_prev && a.can_next == b.can_next,
        _ => false,
    }
}
impl Grace {
    pub fn observe(&mut self, old: Option<&Track>, new: Option<&Track>, now: Instant) {
        match (old, new) {
            (Some(a), Some(b)) if identity(a, b) => {
                if b.playing {
                    self.until = None;
                } else if a.playing {
                    self.until = Some(now + Duration::from_secs(5));
                }
            }
            _ => self.until = None,
        }
    }
    pub fn deadline(&self,now:Instant)->Option<Instant>{self.until.filter(|u|*u>now)}
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.until
            .filter(|u| *u > now)
            .map(|u| u.duration_since(now))
    }
    pub fn keeps(&self, now: Instant) -> bool {
        self.remaining(now).is_some()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_control_changes_are_not_timeline_only() {
        let a = Track::default();
        for b in [Track {can_toggle:true,..a.clone()}, Track {can_prev:true,..a.clone()}, Track {can_next:true,..a.clone()}] {
            assert!(!same_pill(Some(&a),Some(&b)));
        }
    }
    use crate::topbar::island::model::{
        live, step, Activity, Enabled, Event, Inputs, Phase, Power,
    };
    fn track(playing: bool) -> Track {
        Track {
            title: "video".into(),
            session: 1,
            gen: 1,
            playing,
            ..Default::default()
        }
    }
    #[test]
    fn pause_one_second_then_play_never_changes_phase_or_lead() {
        let t = Instant::now();
        let a = track(true);
        let b = track(false);
        let mut g = Grace::default();
        g.observe(Some(&a), Some(&b), t);
        let inp = Inputs {
            track: Some(b.clone()),
            power: Some(Power::Low(19)),
            ..Default::default()
        };
        let all = live(&inp, Enabled::ALL, g.keeps(t + Duration::from_secs(1)));
        assert!(matches!(all[0], Activity::Media(_)));
        assert_eq!(
            step(Phase::Pill, Event::Shown(!all.is_empty())),
            Phase::Pill
        );
        g.observe(Some(&b), Some(&a), t + Duration::from_secs(1));
        assert!(!g.keeps(t + Duration::from_secs(1)));
        assert_eq!(
            step(
                Phase::Pill,
                Event::Shown(
                    !live(
                        &Inputs {
                            track: Some(a),
                            ..Default::default()
                        },
                        Enabled::ALL,
                        false
                    )
                    .is_empty()
                )
            ),
            Phase::Pill
        );
    }
    #[test]
    fn six_seconds_hides_without_another_media_event() {
        let t = Instant::now();
        let a = track(true);
        let b = track(false);
        let mut g = Grace::default();
        g.observe(Some(&a), Some(&b), t);
        assert_eq!(g.remaining(t), Some(Duration::from_secs(5)));
        let all = live(
            &Inputs {
                track: Some(b),
                ..Default::default()
            },
            Enabled::ALL,
            g.keeps(t + Duration::from_secs(6)),
        );
        assert!(all.is_empty());
        assert_eq!(step(Phase::Pill, Event::Shown(false)), Phase::Hidden);
    }
    #[test]
    fn replacement_and_initial_pause_have_no_grace() {
        let t = Instant::now();
        let a = track(true);
        let b = track(false);
        let mut g = Grace::default();
        g.observe(None, Some(&b), t);
        assert!(!g.keeps(t));
        g.observe(Some(&a), Some(&b), t);
        let c = Track {
            gen: 2,
            ..b.clone()
        };
        g.observe(Some(&b), Some(&c), t);
        assert!(!g.keeps(t));
        g.observe(Some(&a), Some(&b), t);
        g.observe(
            Some(&b),
            Some(&Track {
                session: 2,
                ..b.clone()
            }),
            t,
        );
        assert!(!g.keeps(t));
    }
    #[test]
    fn seeking_does_not_change_pill_and_never_extends_deadline() {
        let t = Instant::now();
        let a = track(true);
        let b = track(false);
        let mut g = Grace::default();
        g.observe(Some(&a), Some(&b), t);
        let c = Track {
            seek_refused: 1,
            timeline:Some(super::super::model::Timeline{start_ms:0,end_ms:60_000,pos_ms:35_000,at:t,rate:1.,seekable:true,seek_min_ms:0,seek_max_ms:0}),
            ..b.clone()
        };
        assert!(same_pill(Some(&b), Some(&c)));
        g.observe(Some(&b), Some(&c), t + Duration::from_secs(4));
        assert_eq!(
            g.remaining(t + Duration::from_secs(4)),
            Some(Duration::from_secs(1))
        );
        assert!(!g.keeps(t + Duration::from_secs(5)));
    }
}
