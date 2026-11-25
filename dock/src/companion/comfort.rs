//! Continuous secondary motion, independent of art-frame timing.
use super::{model::State, motion::Pose};
use std::f32::consts::TAU;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Deform {
    pub sx: f32,
    pub sy: f32,
    pub skew: f32,
    pub tilt: f32,
    pub bob: f32,
}
impl Default for Deform {
    fn default() -> Self {
        Self {
            sx: 1.,
            sy: 1.,
            skew: 0.,
            tilt: 0.,
            bob: 0.,
        }
    }
}
impl Deform {
    fn mix(self, to: Self, p: f32) -> Self {
        let f = |a: f32, b: f32| a + (b - a) * p;
        Self {
            sx: f(self.sx, to.sx),
            sy: f(self.sy, to.sy),
            skew: f(self.skew, to.skew),
            tilt: f(self.tilt, to.tilt),
            bob: f(self.bob, to.bob),
        }
    }
}
pub fn deformation(state: State, t: f32, dir: f32, scale: f32, dpi: f32, enabled: bool) -> Deform {
    if !enabled {
        return Deform::default();
    }
    let mut d = Deform::default();
    let (period, amp): (f32, f32) = match state {
        State::Idle | State::Sit | State::Wait => (2.8, 0.012),
        State::Sleep => (4.2, 0.02),
        _ => (0., 0.),
    };
    if period > 0. {
        let wave = (t * TAU / period).sin();
        d.sy += wave * amp.max(0.5 / (300. * scale * dpi).max(1.));
        d.sx -= wave * (amp * 2. / 3.).max(0.5 / (200. * scale * dpi).max(1.));
    }
    if matches!(state, State::Idle | State::Wait | State::Sit) {
        d.skew = 1.4 * (t * TAU / 3.3 + 1.1).sin();
    }
    if state == State::Walk {
        d.tilt = -0.8 * dir;
        d.bob = -1.5 * (t * TAU / 0.4).sin().abs();
    }
    d
}
#[derive(Default)]
pub struct Layer {
    key: String,
    at: f64,
    from: Deform,
    last: Deform,
}
impl Layer {
    pub fn apply(
        &mut self,
        p: &mut Pose,
        state: State,
        now: f64,
        scale: f32,
        dpi: f32,
        enabled: bool,
    ) -> f32 {
        if !enabled {
            *self = Self::default();
            p.sx = 1.;
            p.sy = 1.;
            p.jy = 0.;
            p.tilt = 0.;
            p.skew = 0.;
            p.blend = None;
            p.continuous = false;
            return 0.;
        }
        let clip = p
            .frame
            .rsplit_once('_')
            .map_or(p.frame.as_str(), |(s, _)| s);
        let state = match clip {
            "look" | "pet" | "stretch" | "yawn" => State::Idle,
            "read" => State::Sit,
            "carry" => State::Walk,
            _ => state,
        };
        if self.key != clip {
            self.key = clip.into();
            self.from = self.last;
            self.at = now;
        }
        let target = deformation(
            state,
            now as f32,
            if p.flip { -1. } else { 1. },
            scale,
            dpi,
            true,
        );
        let u = ((now - self.at) / super::timing::CLIP_EASE as f64).clamp(0., 1.) as f32;
        let u = u * u * (3. - 2. * u);
        let d = self.from.mix(target, u);
        self.last = d;
        p.sx *= d.sx;
        p.sy *= d.sy;
        p.skew = d.skew;
        p.tilt += d.tilt;
        // Vertical walk motion belongs to the window, not the body bitmap.
        p.continuous = matches!(
            state,
            State::Idle | State::Sit | State::Walk | State::Wait | State::Sleep
        );
        d.bob
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn analytic_breathing_bob_and_disabled_identity() {
        let d = deformation(State::Idle, 0.7, 1., 0.56, 1., true);
        assert!((d.sy - 1.012).abs() < 0.00001);
        assert!((d.sx - 0.992).abs() < 0.00001);
        assert!((deformation(State::Sleep, 1.05, 1., 0.56, 1., true).sy - 1.02).abs() < 0.00001);
        for s in [State::Idle, State::Sleep, State::Walk] {
            assert_eq!(deformation(s, 1., 1., 0.28, 2., false), Deform::default());
        }
        assert_eq!(deformation(State::Walk, 0.1, 1., 0.56, 1., true).bob, -1.5);
        assert_eq!(deformation(State::Walk, 0., -1., 0.56, 1., true).tilt, 0.8);
    }
    #[test]
    fn clip_transition_keeps_previous_transform_then_eases() {
        let mut l = Layer::default();
        let mut p = super::super::motion::Motion::new(0.).pose();
        l.apply(&mut p, State::Idle, 0., 0.56, 1., true);
        let mut p = super::super::motion::Motion::new(0.).pose();
        l.apply(&mut p, State::Idle, 0.7, 0.56, 1., true);
        let old = l.last;
        p.frame = "wait_0".into();
        p.sx = 1.;
        p.sy = 1.;
        l.apply(&mut p, State::Wait, 0.7, 0.56, 1., true);
        assert_eq!(l.last, old);
        p.sx = 1.;
        p.sy = 1.;
        l.apply(&mut p, State::Wait, 0.82, 0.56, 1., true);
        assert!(
            (l.last.sy - deformation(State::Wait, 0.82, 1., 0.56, 1., true).sy).abs() < 0.00001
        );
        l.apply(&mut p, State::Wait, 1., 0.56, 1., false);
        assert_eq!((p.sx, p.sy, p.skew, p.tilt, p.jy), (1., 1., 0., 0., 0.));
    }
}

#[cfg(test)] mod polish_tests {
 use super::*;
 #[test] fn walking_bob_moves_the_window_without_invalidating_the_body_stamp() {
  for frame in ["walk_0","carry_0"] {for dpi in [1.,1.5,2.] {
   let mut layer=Layer::default();let mut stamp=None;let mut positions=vec![];
   for i in 0..60 {let mut p=super::super::Motion::new(0.).pose();p.frame=frame.into();
    let bob=layer.apply(&mut p,State::Walk,i as f64/60.,0.34,dpi,true);
    assert_eq!(p.jy,0.);if i>10 {
     let next=super::super::render::Stamp::new(&p,0.34,dpi,false,true);
     if let Some(ref previous)=stamp {assert_eq!(previous,&next);}stamp=Some(next);positions.push(bob);
    }
   }
   assert!(positions.windows(2).filter(|v|(v[1]-v[0]).abs()>0.01).count()>30);
   let mut p=super::super::Motion::new(0.).pose();p.frame=frame.into();
   assert_eq!(layer.apply(&mut p,State::Walk,2.,0.34,dpi,false),0.);assert_eq!(p.jy,0.);
  }}
 }
}
