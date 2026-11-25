//! Injected pointer/time model. World coordinates are physical screen pixels.
use super::{clips, motion::Pose};
use serde::{Deserialize, Serialize};
use std::f32::consts::{PI, TAU};
pub const MAX_HELD_TILT: f32 = 25.;
fn wrap_angle(theta:f32)->f32 {
    let angle=(theta+PI).rem_euclid(TAU)-PI;
    if angle<=-PI {PI} else {angle}
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Mouse,
    Touch,
    Pen,
}
impl Kind {
    pub fn threshold(self) -> f32 {
        if self == Self::Mouse {
            4.
        } else {
            8.
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Rest,
    Pressed,
    Held,
    Fall,
    Land,
    Peek,
}
#[derive(Clone, Copy, Debug)]
pub enum Input {
    Wheel {steps:i32},
    Down { id: u32, kind: Kind, point: Point },
    Move { id: u32, point: Point },
    Up { id: u32, point: Point },
    Cancel { id: u32 },
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Release {
    None,
    Tap,
    Hide,
    Fall,
}
#[derive(Default)]
pub struct Events {
    pub landed: bool,
    pub finished: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Home {
    pub monitor: u64,
    pub ratio: f32,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub edge: Option<super::peek::Edge>,
}
impl Home {
    pub fn release_edge(&mut self) {
        match self.edge {Some(super::peek::Edge::Left)=>self.ratio=0.,Some(super::peek::Edge::Right)=>self.ratio=1.,_=>{}}
        self.edge=None;
    }
    pub fn at(monitor: u64, x: f32, left: f32, right: f32) -> Self {
        Self {
            monitor,
            edge: None,
            ratio: if right > left {
                ((x - left) / (right - left)).clamp(0., 1.)
            } else {
                0.5
            },
        }
    }
    pub fn x(self, left: f32, right: f32) -> f32 {
        left + (right - left).max(0.)
            * if self.ratio.is_finite() {
                self.ratio.clamp(0., 1.)
            } else {
                0.5
            }
    }
}
pub fn monitor_key(key: &str) -> u64 {
    key.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}
pub struct Grab {
    pub phase: Phase,
    pub point: Point,
    pub pivot: Point,
    pub theta: f32,
    pub omega: f32,
    pub stretch: f32,
    pub speed: f32,
    id: u32,
    kind: Kind,
    origin: Point,
    last: Point,
    velocity: f32,
    acceleration: f32,
    scale: f32,
    time: f32,
    vy: f32,
    floor: f32,
    fall_height: f32,
    straight: f32,
}
impl Default for Grab {
    fn default() -> Self {
        Self {
            phase: Phase::Rest,
            point: Point::default(),
            pivot: Point::new(0.5, 0.),
            theta: 0.,
            omega: 0.,
            stretch: 1.,
            speed: 0.,
            id: 0,
            kind: Kind::Mouse,
            origin: Point::default(),
            last: Point::default(),
            velocity: 0.,
            acceleration: 0.,
            scale: 0.56,
            time: 0.,
            vy: 0.,
            floor: 0.,
            fall_height: 0.,
            straight: 0.,
        }
    }
}
impl Grab {
    pub fn abort(&mut self) { *self=Self::default(); }
    pub fn protect_from_cover(&self)->bool {
        matches!(self.phase,Phase::Pressed|Phase::Held|Phase::Fall|Phase::Land)
    }
    pub fn rescale(&mut self, scale: f32) {
        self.scale = scale;
    }
    pub fn accepts_release(&self,id:u32)->bool {self.id==id&&matches!(self.phase,Phase::Pressed|Phase::Held)}
    pub fn busy(&self) -> bool {
        self.phase != Phase::Rest
    }
    pub fn fast(&self) -> bool {
        matches!(self.phase, Phase::Held | Phase::Fall | Phase::Land)
    }
    pub fn down(&mut self, id: u32, kind: Kind, point: Point, pivot: Point, scale: f32) -> bool {
        if self.busy() {
            return false;
        }
        *self = Self {
            id,
            kind,
            point,
            origin: point,
            last: point,
            pivot,
            scale,
            phase: Phase::Pressed,
            ..Self::default()
        };
        true
    }
    pub fn moved(&mut self, id: u32, point: Point) {
        if self.id != id || !matches!(self.phase, Phase::Pressed | Phase::Held) {
            return;
        }
        self.point = point;
        if self.phase == Phase::Pressed
            && (point.x - self.origin.x).hypot(point.y - self.origin.y) > self.kind.threshold()
        {
            self.phase = Phase::Held;
            self.theta = 0.04;
            self.last = point;
        }
    }
    pub fn up(
        &mut self,
        id: u32,
        point: Point,
        slot: bool,
        floor: f32,
        left: f32,
        right: f32,
    ) -> Release {
        if self.id != id || !matches!(self.phase, Phase::Pressed | Phase::Held) {
            return Release::None;
        }
        self.moved(id, point);
        if self.phase == Phase::Pressed {
            self.phase = Phase::Rest;
            return Release::Tap;
        }
        if slot {
            self.phase = Phase::Rest;
            return Release::Hide;
        }
        let p = self.pose();
        let f = &clips::catalog().frames[&p.frame];
        let dx = (f.ax - self.pivot.x * f.w as f32) * self.scale;
        let dy = (f.ground - self.pivot.y * f.h as f32) * self.scale * self.stretch;
        self.point = Point::new(
            (point.x + dx * self.theta.cos() - dy * self.theta.sin()).clamp(left, right.max(left)),
            point.y + dx * self.theta.sin() + dy * self.theta.cos(),
        );
        self.phase = Phase::Fall;
        self.time = 0.;
        self.vy = 0.;
        self.floor = floor;
        self.fall_height = (floor - self.point.y).max(0.);
        self.theta = wrap_angle(self.theta);
        self.straight = 0.;
        // Start the upright spring without outward momentum across the +/-pi seam.
        self.omega = 0.;
        Release::Fall
    }
    pub fn cancel(&mut self, id: u32, floor: f32, left: f32, right: f32) -> Release {
        if self.id != id {
            return Release::None;
        }
        if self.phase == Phase::Pressed {
            self.phase = Phase::Rest;
            return Release::None;
        }
        self.up(id, self.point, false, floor, left, right)
    }
    pub fn step(&mut self, dt: f32) -> Events {
        let dt = dt.clamp(0., 0.1);
        let mut e = Events::default();
        self.time += dt;
        match self.phase {
            Phase::Held => {
                let f = &clips::catalog().frames["jump_3"];
                let dx = (f.ax - self.pivot.x * f.w as f32) * self.scale;
                let dy = (f.ground - 0.35 * f.h as f32 - self.pivot.y * f.h as f32) * self.scale;
                let distance = dx.hypot(dy);
                let h = f.h as f32 * self.scale;
                // The pointer remains the geometric pivot, but upright is the
                // spring's equilibrium even when that pivot is a foot.
                let length = 0.60 * h;
                let stiffness = (TAU / 0.9).powi(2) + (7.5f32 / 2.).powi(2);
                let raw = ((self.point.x - self.last.x) / dt.max(0.001)).clamp(-4000., 4000.);
                self.speed =
                    (self.point.x - self.last.x).hypot(self.point.y - self.last.y) / dt.max(0.001);
                let accel = ((raw - self.velocity) / dt.max(0.001)).clamp(-20000., 20000.);
                self.acceleration += (accel - self.acceleration) * (1. - (-dt / 0.08).exp());
                self.velocity = raw;
                self.last = self.point;
                let coupling = (distance / (0.60 * h)).clamp(0.12, 1.);
                let mut remain = dt;
                while remain > 0.000001 {
                    let d = remain.min(1. / 240.);
                    self.omega += (-stiffness * self.theta
                        - 7.5 * self.omega
                        - ((self.acceleration + 4. * self.velocity) / length) * coupling)
                        * d;
                    self.omega = self.omega.clamp(-18., 18.);
                    let limit = MAX_HELD_TILT.to_radians();
                    self.theta = (self.theta + self.omega * d).clamp(-limit, limit);
                    if self.theta.abs() >= limit && self.theta * self.omega > 0. {
                        self.omega = 0.;
                    }
                    remain -= d;
                }
                self.stretch = 1. + (self.speed / 10000.).min(0.06);
            }
            Phase::Fall => {
                let mut remain = dt;
                while remain > 0.000001 {
                    let d = remain.min(1. / 240.);
                    // Slightly overdamped: no overshoot beyond the half-turn route.
                    self.omega += ((self.straight - self.theta) * 80. - 18. * self.omega) * d;
                    self.theta = wrap_angle(self.theta + self.omega * d);
                    self.vy += 3200. * self.scale * d;
                    self.point.y += self.vy * d;
                    remain -= d;
                }
                self.stretch += (1. - self.stretch) * (dt * 12.).min(1.);
                if self.point.y >= self.floor {
                    self.point.y = self.floor;
                    self.phase = Phase::Land;
                    self.time = 0.;
                    self.theta = 0.;
                    self.stretch = 1.;
                    e.landed = true;
                }
            }
            Phase::Land => {
                if self.time >= clips::catalog().clip("land").duration() {
                    self.phase = Phase::Peek;
                    self.time = 0.;
                }
            }
            Phase::Peek => {
                if self.time >= clips::catalog().clip("peek").duration() {
                    self.phase = Phase::Rest;
                    e.finished = true;
                }
            }
            _ => {}
        }
        e
    }
    pub fn pose(&self) -> Pose {
        let frame = match self.phase {
            Phase::Held if self.speed > 600. => {
                format!("jump_{}", 2 + (self.time * 6.) as usize % 3)
            }
            Phase::Land => clips::catalog().clip("land").frame(self.time).into(),
            Phase::Peek => clips::catalog().clip("peek").frame(self.time).into(),
            _ => "jump_3".into(),
        };
        let squash = if self.phase == Phase::Land {
            (self.fall_height / (700. * self.scale)).min(0.08)
                * (PI * self.time / 0.3).sin().max(0.)
        } else {
            0.
        };
        Pose {
            clip: match self.phase {Phase::Held=>"grab",Phase::Fall=>"fall",Phase::Land=>"land",Phase::Peek=>"peek",_=>"idle"}.into(),
            clip_time: self.time,
            frame,
            skew:0.,hat_tilt:0.,continuous:true,blend:None,
            x: self.point.x,
            jy: 0.,
            sx: 1. + squash,
            sy: if matches!(self.phase, Phase::Held | Phase::Fall) {
                self.stretch
            } else {
                1. - squash
            },
            tilt: self.theta.to_degrees(),
            flip: false,
            pivot: if self.phase == Phase::Held {
                Some((self.pivot.x, self.pivot.y))
            } else {
                None
            },
        }
    }
    pub fn radius(&self) -> f32 {
        let p = self.pose();
        ["jump_2", "jump_3", "jump_4"]
            .into_iter()
            .map(|name| {
                let f = &clips::catalog().frames[name];
                let (px, py) = if let Some((x, y)) = p.pivot {
                    (x * f.w as f32, y * f.h as f32)
                } else {
                    (f.ax, f.ground)
                };
                [
                    (0., 0.),
                    (f.w as f32, 0.),
                    (0., f.h as f32),
                    (f.w as f32, f.h as f32),
                ]
                .into_iter()
                .map(|(x, y)| (x - px).hypot((y - py) * 1.06))
                .fold(0., f32::max)
                    * self.scale
            })
            .fold(0., f32::max)
    }
}
pub fn interval(base: Option<u32>, fast: bool, battery: bool) -> Option<u32> {
    if fast {
        Some(if battery { 34 } else { 17 })
    } else {
        base
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_touch_and_pen_presses_remain_taps_until_drag_threshold() {
        for kind in [Kind::Touch, Kind::Pen] {
            let mut g = Grab::default();
            let at = Point::new(100., 100.);
            assert!(g.down(1, kind, at, Point::new(0.5, 0.5), 0.56));
            for _ in 0..8 {g.step(0.1);}
            assert_eq!(g.up(1, Point::new(108., 100.), false, 800., 0., 1000.), Release::Tap);
            assert!(g.down(1, kind, at, Point::new(0.5, 0.5), 0.56));
            for _ in 0..8 {g.step(0.1);}
            assert_eq!(g.up(1, Point::new(108.1, 100.), false, 800., 0., 1000.), Release::Fall);
        }
    }
    #[test]
    fn review_angle_is_bounded_and_release_takes_the_short_way() {
        for turns in [-100., 100.] {
            let mut g = held(0.98);
            g.theta = turns * TAU + 2.8;
            g.step(1. / 120.);
            assert!(g.theta > -PI && g.theta <= PI, "{}", g.theta);
            g.up(1, g.point, false, 2000., -2000., 2000.);
            assert!((g.straight - g.theta).abs() <= PI);
            let before = g.theta.abs();
            for _ in 0..30 { g.step(1. / 120.); }
            assert!(g.theta.abs() < before);
        }
        for theta in [-PI+0.001,PI-0.001,-0.01,0.01] {
            let mut g=held(0.98);g.theta=theta;g.omega=theta.signum()*18.;
            g.up(1,g.point,false,10000.,-2000.,2000.);
            let mut previous=g.theta.abs();
            for _ in 0..120 {
                g.step(1./120.);
                assert!(g.theta.abs()<=previous+0.00001,"release {theta}: {previous} -> {}",g.theta);
                previous=g.theta.abs();
            }
        }
    }
    fn held(y: f32) -> Grab {
        let f = &clips::catalog().frames["jump_3"];
        let mut g = Grab::default();
        g.down(
            1,
            Kind::Mouse,
            Point::new(0., 0.),
            Point::new(f.ax / f.w as f32, y),
            0.56,
        );
        g.moved(1, Point::new(5., 0.));
        g
    }
    #[test]
    fn thresholds_and_second_pointer() {
        for kind in [Kind::Mouse, Kind::Touch, Kind::Pen] {
            let mut g = Grab::default();
            g.down(1, kind, Point::default(), Point::new(0.5, 0.), 0.56);
            assert!(!g.down(2, kind, Point::default(), Point::default(), 0.56));
            g.moved(2, Point::new(100., 0.));
            g.moved(1, Point::new(kind.threshold(), 0.));
            assert_eq!(g.phase, Phase::Pressed);
            assert_eq!(g.up(1, g.point, false, 100., -100., 100.), Release::Tap);
            g.down(1, kind, Point::default(), Point::default(), 0.56);
            g.moved(1, Point::new(kind.threshold() + 0.01, 0.));
            assert_eq!(g.phase, Phase::Held);
        }
    }
    #[test]
    fn all_pivots_stay_upright_under_drag_and_settle() {
        for y in [0.03, 0.5, 0.98] {
            for dt in [1. / 120., 1. / 30., 0.1] {
                let mut g = held(y);
                for i in 0..180 {
                    g.moved(1, Point::new((i as f32 * 0.4).sin() * 500., 50.));
                    g.step(dt);
                    assert!(g.theta.to_degrees().abs() <= 25.001, "pivot {y}: {}", g.theta.to_degrees());
                    let p = g.pose();
                    let f = &clips::catalog().frames[&p.frame];
                    let m = super::super::render::body_matrix(&p, g.point.x, g.point.y, 0.56);
                    let (u,v) = (g.pivot.x * f.w as f32, g.pivot.y * f.h as f32);
                    assert!((u*m.M11+v*m.M21+m.M31-g.point.x).abs() < 0.001);
                    assert!((u*m.M12+v*m.M22+m.M32-g.point.y).abs() < 0.001);
                }
                for _ in 0..600 {g.step(1. / 120.);}
                assert!(g.theta.abs() < 0.01 && g.omega.abs() < 0.01);
            }
        }
    }
    #[test]
    fn off_center_pivots_settle_upright() {
        for x in [0.15, 0.85] {
            for y in [0.15, 0.9] {
                let mut g = held(y);
                g.pivot.x = x;
                for _ in 0..600 {
                    g.step(1. / 120.);
                }
                assert!(g.theta.abs() < 0.001, "pivot {x},{y}: {}", g.theta);
            }
        }
    }
    #[test]
    fn drop_slot_and_landing_sequence() {
        let mut g = held(0.05);
        assert_eq!(g.up(1, g.point, true, 100., -100., 100.), Release::Hide);
        let mut g = held(0.05);
        assert_eq!(g.up(1, g.point, false, 600., -100., 100.), Release::Fall);
        let mut phases = Vec::new();
        let mut frames = Vec::new();
        for _ in 0..1000 {
            g.step(1. / 120.);
            if phases.last() != Some(&g.phase) {
                phases.push(g.phase);
            }
            let p = g.pose();
            if frames.last() != Some(&p.frame) {
                frames.push(p.frame);
            }
            if g.phase == Phase::Rest {
                break;
            }
        }
        assert_eq!(phases, [Phase::Fall, Phase::Land, Phase::Peek, Phase::Rest]);
        for n in ["jump_5", "jump_6", "jump_7", "peek_1", "peek_2"] {
            assert!(frames.iter().any(|f| f == n));
        }
        assert_eq!(g.point.y, 600.);
    }
    #[test]
    fn torso_swings_less_and_stretch_is_capped() {
        let mut tip = held(0.03);
        let mut body = held(0.60);
        let mut tip_max = 0f32;
        let mut body_max = 0f32;
        for i in 0..180 {
            let x = 5. + if i < 12 { i as f32 * 3. } else { 36. };
            tip.moved(1, Point::new(x, 0.));
            body.moved(1, Point::new(x, 0.));
            tip.step(1. / 120.);
            body.step(1. / 120.);
            tip_max = tip_max.max(tip.theta.abs());
            body_max = body_max.max(body.theta.abs());
            assert!(tip.stretch <= 1.06);
        }
        assert!(body_max < tip_max, "{body_max} vs {tip_max}");
    }
    #[test]
    fn pendulum_period_and_free_fall_are_time_driven() {
        let mut g = held(0.03);
        g.theta = 0.35;
        g.step(1. / 120.);
        g.step(1. / 120.);
        let mut crossings = Vec::new();
        for i in 2..180 {
            let before = g.theta;
            g.step(1. / 120.);
            if before.signum() != g.theta.signum() {
                crossings.push(i as f32 / 120.);
            }
        }
        assert!(crossings.len() >= 2);
        assert!(
            (0.8..1.0).contains(&(2. * (crossings[1] - crossings[0]))),
            "{crossings:?}"
        );
        g.speed = 1000.;
        let before = g.pose();
        let f = &clips::catalog().frames[&before.frame];
        g.up(1, g.point, false, 1000., -2000., 2000.);
        let from = g.point.y;
        assert!(g.point.y.is_finite() && f.h > 0);
        g.step(0.1);
        assert!(g.point.y > from && g.point.y < 1000.);
        assert!(g.radius().is_finite());
    }
    #[test]
    fn cancelled_press_never_taps_and_held_cancel_falls() {
        let mut g = Grab::default();
        g.down(1, Kind::Touch, Point::default(), Point::default(), 0.56);
        assert_eq!(g.cancel(1, 500., -100., 100.), Release::None);
        let mut g = held(0.03);
        assert_eq!(g.cancel(1, 500., -100., 100.), Release::Fall);
    }
    #[test]
    fn position_roundtrip_and_frame_cap() {
        let home = Home::at(monitor_key("display-one"), -1500., -1920., 0.);
        let s = serde_json::to_string(&home).unwrap();
        let saved: Home = serde_json::from_str(&s).unwrap();
        assert!((saved.x(-2560., 0.) + 2000.).abs() < 0.01);
        assert_eq!(interval(Some(100), true, false), Some(17));
        assert_eq!(interval(Some(100), false, false), Some(100));
    }
}
