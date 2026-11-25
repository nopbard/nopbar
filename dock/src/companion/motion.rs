//! Port of the approved 64-frame preview. Art supplies the action;
//! code adds only the preview's weak entry spring only.
use super::{clips::catalog, model::State};
use std::f32::consts::PI;
#[derive(Clone, Copy, Debug)]
struct Spring {
    x: f32,
    v: f32,
    k: f32,
    z: f32,
}
impl Spring {
    fn new(x: f32, k: f32, z: f32) -> Self {
        Self { x, v: 0.0, k, z }
    }
    fn step(&mut self, dt: f32, target: f32) {
        self.v += (self.k * (target - self.x) - 2.0 * self.k.sqrt() * self.z * self.v) * dt;
        self.x += self.v * dt;
    }
}
#[derive(Clone, Debug)]
pub struct Pose {
    pub clip: String,
    pub clip_time: f32,
    pub frame: String,
    pub x: f32,
    pub jy: f32,
    pub sx: f32,
    pub sy: f32,
    pub tilt: f32,
    pub flip: bool,
    pub pivot: Option<(f32,f32)>,
    pub skew:f32,
    pub hat_tilt: f32,
    pub continuous:bool,
    pub blend:Option<(String,f32)>,
}
pub struct Motion {
    pub state: State,
    pub t: f32,
    pub x: f32,
    pub dir: f32,
    clip: String,
    clip_t: f32,
    turn: bool,
    sx: Spring,
    sy: Spring,
}
impl Motion {
    pub fn new(x: f32) -> Self {
        Self {
            state: State::Idle,
            t: 0.0,
            x,
            dir: 1.0,
            clip: "idle".into(),
            clip_t: 0.0,
            turn: false,
            sx: Spring::new(1.0, 240.0, 0.36),
            sy: Spring::new(1.0, 240.0, 0.36),
        }
    }
    pub fn set_state(&mut self, s: State) {
        if self.state == s {
            return;
        }
        self.state = s;
        self.t = 0.0;
        self.turn = false;
        self.sy.v -= 1.6;
        self.sx.v += 1.0;
        self.play(match s {
            State::Idle => "idle",
            State::Sit => "sit",
            State::Walk => "walk",
            State::Notify => "notify",
            State::Wait => "wait",
            State::Done => "done",
            State::Error => "error",
            State::Sleep => "sleep",
            State::Peek => "peek",
        });
    }
    pub fn restart(&mut self) {
        self.clip_t = 0.0;
        self.t = 0.0;
    }
    pub fn yield_at(&mut self,x:f32) {
        self.x=x;
        self.set_state(State::Idle);
    }
    fn play(&mut self, name: &str) {
        self.clip = name.into();
        self.clip_t = 0.0;
    }
    pub fn step(&mut self, dt: f32, left: f32, right: f32) -> Pose {
        let dt = dt.clamp(0.0, 0.25);
        self.t += dt;
        if self.state == State::Walk && !self.turn {
            self.x += self.dir * 52.0 * dt;
            if self.x >= right || self.x <= left {
                self.x = self.x.clamp(left, right.max(left));
                self.turn = true;
                self.play(if self.dir > 0.0 {
                    "turn_left"
                } else {
                    "turn_right"
                });
            }
        }
        self.clip_t += dt;
        let clip = catalog().clip(&self.clip);
        if !clip.repeat && self.clip_t + 0.000001 >= clip.duration() {
            if self.turn {
                self.dir = -self.dir;
                self.turn = false;
                self.play("walk");
            } else if let Some(next) = clip.next.as_deref() {
                self.play(next);
            }
        }
        let mut remain = dt;
        while remain > 0.000001 {
            let d = remain.min(1.0 / 120.0);
            self.sx.step(d, 1.0);
            self.sy.step(d, 1.0);
            remain -= d;
        }
        self.pose()
    }
    pub fn pose(&self) -> Pose {
        Pose {
            clip: self.clip.clone(),
            clip_time: self.clip_t,
            frame: catalog().clip(&self.clip).frame(self.clip_t).into(),
            x: self.x,
            jy: 0.0,
            sx: self.sx.x,
            sy: self.sy.x,
            tilt: 0.0,
            flip: self.clip == "walk" && self.dir < 0.0,
            pivot: None,
            skew:0.,hat_tilt:0.,continuous:false,
            blend:catalog().clip(&self.clip).transition(self.clip_t).map(|(name,a)|(name.into(),a)),
        }
    }
}
/// Frames and timing of hide/show are data; travel duration can outlast a
/// non-looping air clip, holding its final frame as the preview does.
#[derive(Debug)]
pub struct Journey {
    pub clip: &'static str,
    pub clip_time: f32,
    pub frame: String,
    pub progress: Option<f32>,
    pub finished: bool,
}
pub fn journey(out: bool, elapsed: f32) -> Journey {
    let c = catalog();
    let (clip, t, progress, finished) = if out {
        let air = c.clip("show_air").travel_ms.unwrap() / 1000.0;
        if elapsed < air {
            ("show_air", elapsed, Some(elapsed / air), false)
        } else {
            (
                "land",
                elapsed - air,
                None,
                elapsed >= air + c.clip("land").duration(),
            )
        }
    } else {
        let launch = c.clip("hide_launch").duration();
        let air = c.clip("hide_air").travel_ms.unwrap() / 1000.0;
        if elapsed < launch {
            ("hide_launch", elapsed, None, false)
        } else {
            (
                "hide_air",
                elapsed - launch,
                Some(((elapsed - launch) / air).min(1.0)),
                elapsed >= launch + air,
            )
        }
    };
    Journey {
        clip,
        clip_time: t,
        frame: c.clip(clip).frame(t).into(),
        progress,
        finished,
    }
}
pub fn travel(
    p: f32,
    out: bool,
    from: (f32, f32),
    slot: (f32, f32),
    scale: f32,
    slot_scale: f32,
) -> (f32, f32, f32) {
    let p = p.clamp(0.0, 1.0);
    let e = if p < 0.5 {
        2.0 * p * p
    } else {
        1.0 - (-2.0 * p + 2.0).powi(2) / 2.0
    };
    let q = if out { 1.0 - e } else { e };
    (
        from.0 + (slot.0 - from.0) * q,
        from.1 + (slot.1 - from.1) * q - 120.0 * (PI * q).sin(),
        scale + (slot_scale - scale) * q,
    )
}
/// Spring travel keeps exact endpoints and permits a small horizontal arrival
/// overshoot. The floor is a hard constraint, including during the rebound.
pub fn spring_travel(p:f32,out:bool,from:(f32,f32),slot:(f32,f32),scale:f32,slot_scale:f32)->(f32,f32,f32) {
    let p=p.clamp(0.,1.);
    if p==0. || p==1. {return travel(p,out,from,slot,scale,slot_scale);}
    let response=|u:f32|1.-(-8.*u).exp()*((13.86*u).cos()+8./13.86*(13.86*u).sin());
    let e=response(p)/response(1.);
    let q=if out {1.-e}else{e};
    let arc=q.clamp(0.,1.);
    (from.0+(slot.0-from.0)*q,
        (from.1+(slot.1-from.1)*q-120.*(PI*arc).sin()).min(from.1),
        scale+(slot_scale-scale)*q.clamp(0.,1.))
}
#[cfg(test)]
mod tests {
    #[test]
    fn yielding_ends_a_walk_at_its_current_position() {
        use super::*;
        let mut motion=Motion::new(300.);
        motion.set_state(State::Walk);
        let position=motion.step(0.1,100.,900.).x;
        assert!(position>300.);
        motion.yield_at(position);
        assert_eq!(motion.state,State::Idle);
        for _ in 0..30 {assert_eq!(motion.step(0.034,100.,900.).x,position);}
    }
    use super::*;
    #[test]
    fn walking_uses_both_turn_sequences_without_mirroring() {
        let mut m = Motion::new(-55.0);
        m.set_state(State::Walk);
        let mut left = false;
        let mut right = false;
        for _ in 0..1800 {
            let p = m.step(1.0 / 120.0, -200.0, -50.0);
            assert!(p.x >= -200.0 && p.x <= -50.0);
            if p.frame.starts_with("turn_") {
                assert!(!p.flip);
                left |= m.clip == "turn_left";
                right |= m.clip == "turn_right";
            }
            if p.frame.starts_with("walk_") {
                assert_eq!(p.flip, m.dir < 0.0);
            }
        }
        assert!(left && right);
    }
    #[test]
    fn low_fps_keeps_squash_subtle() {
        let mut m = Motion::new(0.0);
        m.set_state(State::Sleep);
        for _ in 0..200 {
            let p = m.step(0.2, -200.0, 200.0);
            assert!(p.sy.is_finite() && (p.sy - 1.0).abs() < 0.1);
        }
    }
    #[test]
    fn hide_prepares_then_flies_show_lands_before_peek() {
        assert_eq!(journey(false, 0.0).frame, "jump_0");
        assert_eq!(journey(false, 0.079).frame, "jump_0");
        assert!(journey(false, 0.079).progress.is_none());
        assert_eq!(journey(false, 0.08).frame, "jump_2");
        assert!(journey(false, 0.91).finished);
        assert_eq!(journey(true, 0.0).frame, "jump_3");
        assert_eq!(journey(true, 0.61).frame, "jump_5");
        assert_eq!(journey(true, 0.81).frame, "jump_7");
        assert!(journey(true, 0.91).finished);
    }
    #[test]
    fn travel_endpoints() {
        let a = travel(0.0, true, (400.0, 900.0), (800.0, 28.0), 0.56, 0.06);
        let b = travel(1.0, true, (400.0, 900.0), (800.0, 28.0), 0.56, 0.06);
        assert!((a.0 - 800.0).abs() < 0.01 && (a.1 - 28.0).abs() < 0.01);
        assert_eq!(b, (400.0, 900.0, 0.56));
        for out in [false,true] {
            for p in [0.,1.] {assert_eq!(spring_travel(p,out,(400.,900.),(800.,28.),0.56,0.06),travel(p,out,(400.,900.),(800.,28.),0.56,0.06));}
            for i in 0..120 {let (_,y,s)=spring_travel(i as f32/120.,out,(400.,900.),(800.,28.),0.56,0.06);assert!(y<=900.);assert!((0.059..=0.561).contains(&s));}
        }
    }
}
