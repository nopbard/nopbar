//! Secondary motion in artwork units. The clip/input models remain authoritative.
use super::motion::Pose;

#[derive(Clone, Copy, Debug, Default)]
struct Spring {
    x: f32,
    v: f32,
}
impl Spring {
    fn step(&mut self, dt: f32, target: f32, frequency: f32) {
        let mut remaining = dt.clamp(0., 0.25);
        while remaining > 0.000001 {
            let h = remaining.min(1. / 240.);
            // Damping ratio 0.5: one visible overshoot, then a small return.
            self.v += (frequency * frequency * (target - self.x) - frequency * self.v) * h;
            self.x += self.v * h;
            remaining -= h;
        }
    }
    fn resting(&self, target: f32, epsilon: f32) -> bool {
        (self.x - target).abs() < epsilon && self.v.abs() < epsilon * 10.
    }
    fn settle(&mut self, target: f32, epsilon: f32) {
        if self.resting(target, epsilon) {
            self.x = target;
            self.v = 0.;
        }
    }
}

/// A separate clock is needed for stand/read_doze/land: those clips reuse
/// another action's frame names. Callers supply the actual selected clip.
#[derive(Clone, Copy)]
pub struct Sample<'a> {
    pub clip: &'a str,
    pub time: f32,
    pub direction: f32,
}

#[derive(Default)]
pub struct Layer {
    hat: Spring,
    shape: Spring,
    lean: Spring,
    horizontal: Spring,
    last: Option<(f32, f32, f32)>,
    velocity: (f32, f32),
    key: String,
    elapsed: f32,
    active: bool,
}
impl Layer {
    pub fn moving(&self) -> bool {
        self.active
    }

    /// Returns a visual x only. The model's destination, drag pivot and floor
    /// are never changed by this follower. Pointer-held and airborne x are exact.
    pub fn apply(
        &mut self,
        p: &mut Pose,
        placement: (f32, f32, f32),
        dt: f32,
        sample: Sample<'_>,
        enabled: bool,
    ) -> f32 {
        let (x, y, scale) = placement;
        if !enabled {
            *self = Self::default();
            return x;
        }
        let dt = dt.clamp(0., 0.25);
        let clip = sample.clip;
        if self.key != clip {
            self.key = clip.into();
            self.elapsed = 0.;
        } else {
            self.elapsed += dt;
        }
        let time = if sample.time.is_finite() {
            sample.time
        } else {
            self.elapsed
        };
        let profile = profile(clip, time, sample.direction);
        if self.last.is_none() {
            self.horizontal.x = x;
        }
        let follow = !matches!(clip, "grab" | "fall" | "show_air" | "hide_air");
        if follow {
            // An 80ms preparation compresses opposite the departure direction.
            let target = if matches!(clip, "walk" | "carry") && time < 0.08 {
                self.horizontal.x
            } else {
                x
            };
            self.horizontal.step(dt, target, 30.);
            self.horizontal.settle(target, 0.02);
        } else {
            self.horizontal = Spring { x, v: 0. };
        }
        let draw_x = self.horizontal.x;
        self.shape.step(dt, profile.squash, 38.);
        self.shape.settle(profile.squash, 0.0003);
        self.lean.step(dt, profile.lean, 28.);
        self.lean.settle(profile.lean, 0.02);
        let limit = if profile.major { 0.12 } else { 0.04 };
        // Ordinary old breathing survives. Event profiles replace old entry
        // impulses rather than multiplying independent non-conserving scales.
        let base = if profile.major { 1. } else { p.sy };
        p.sy = (base + self.shape.x).clamp(1. / (1. + limit), 1. + limit);
        p.sx = 1. / p.sy;
        p.skew += self.lean.x;
        p.jy += profile.lift;
        // Breathing alone must not keep the fast timer awake. Feed event
        // deformation, not the legacy idle breathing/skew, into acceleration.
        let center_y = y + (p.jy - 150. * self.shape.x) * scale;
        let center_x = draw_x - self.lean.x * 2. * scale;
        if let Some((px, py, tilt)) = self.last {
            if dt > 0.00001 {
                let vx = (center_x - px) / dt;
                let vy = (center_y - py) / dt;
                // Acceleration impulses: positive x => positive shear (tip
                // left), downward landing => forward nod. Large teleports are
                // bounded rather than injecting unbounded energy.
                let dvx = ((vx - self.velocity.0) / scale.max(0.28)).clamp(-700., 700.);
                let dvy = ((vy - self.velocity.1) / scale.max(0.28)).clamp(-700., 700.);
                self.hat.v += (dvx * 1.2 - dvy * 3.2 - (p.tilt - tilt) * 4.).clamp(-300., 300.);
                self.velocity = (vx, vy);
            }
        }
        self.last = Some((center_x, center_y, p.tilt));
        self.hat.step(dt, profile.hat, 26.);
        self.hat.x = self.hat.x.clamp(-14., 14.);
        if self.hat.x.abs() >= 14. && self.hat.v * self.hat.x > 0. {
            self.hat.v = 0.;
        }
        self.hat.settle(profile.hat, 0.08);
        p.hat_tilt = (p.hat_tilt + self.hat.x).clamp(-14., 14.);
        // Quarter-degree bins avoid raster invalidations from invisible tails.
        p.hat_tilt = (p.hat_tilt * 4.).round() / 4.;
        p.continuous = true;
        self.active = !self.hat.resting(profile.hat, 0.08)
            || !self.shape.resting(profile.squash, 0.0003)
            || !self.lean.resting(profile.lean, 0.02)
            || follow && !self.horizontal.resting(x, 0.02)
            || profile.changing;
        draw_x
    }
}

pub fn interval(base: Option<u32>, moving: bool, battery: bool) -> Option<u32> {
    base.map(|ms| {
        if moving {
            ms.min(if battery { 34 } else { 17 })
        } else {
            ms
        }
    })
}

/// Replace window bob with ground-pivot deformation, including the old bob's
/// crossfade tail after walking has already changed into idle or thinking.
pub fn ground_bob(legacy: f32, enabled: bool) -> f32 {
    if enabled {
        0.
    } else {
        legacy
    }
}

#[derive(Default)]
struct Profile {
    squash: f32,
    lean: f32,
    lift: f32,
    hat: f32,
    major: bool,
    changing: bool,
}

fn pulse(t: f32, start: f32, length: f32) -> f32 {
    let u = (t - start) / length;
    if (0. ..1.).contains(&u) {
        (u * std::f32::consts::PI).sin()
    } else {
        0.
    }
}

fn profile(clip: &str, t: f32, direction: f32) -> Profile {
    let mut p = Profile::default();
    match clip {
        "walk" | "carry" => {
            p.squash = -0.027 * (t * std::f32::consts::TAU / 0.4).cos().max(0.);
            p.lean = if t < 0.08 {
                -2.5 * direction * pulse(t, 0., 0.08)
            } else {
                0.
            };
            p.changing = true;
        }
        "turn_left" | "turn_right" => {
            p.major = true;
            p.squash = 0.111 * pulse(t, 0.12, 0.28);
            p.lean = direction * 2. * pulse(t, 0.1, 0.3);
            p.changing = t < 0.5;
        }
        "grab" => {
            p.major = true;
            p.squash = 0.08 * pulse(t, 0., 0.35);
            p.changing = t < 0.35;
        }
        "fall" => {
            p.squash = 0.035;
            p.changing = false;
        }
        "land" => {
            p.major = true;
            p.squash = -0.12 * pulse(t, 0., 0.16) + 0.055 * pulse(t, 0.16, 0.18);
            p.hat = -9. * pulse(t, 0.02, 0.18);
            p.changing = t < 0.34;
        }
        "done" | "error" => {
            p.major = true;
            p.squash = -0.10 * pulse(t, 0., 0.10) + 0.09 * pulse(t, 0.10, 0.22)
                - 0.12 * pulse(t, 0.32, 0.14);
            p.lift = -if clip == "done" { 24. } else { 14. } * pulse(t, 0.10, 0.22);
            p.changing = t < 0.46;
        }
        "notify" => {
            p.major = true;
            p.squash = 0.06 * pulse(t, 0., 0.15) - 0.035 * pulse(t, 0.15, 0.15);
            p.changing = t < 0.3;
        }
        "poke" => {
            p.major = true;
            p.squash = -0.08 * pulse(t, 0., 0.16);
            p.lean = -direction * 5. * pulse(t, 0., 0.18);
            p.changing = t < 0.18;
        }
        "pet" => {
            p.squash = -0.025;
            p.lean = -direction * 2.5;
            p.changing = false;
        }
        "pet_end" => {
            p.squash = 0.025 * pulse(t, 0., 0.15);
            p.changing = t < 0.15;
        }
        "sit" => {
            p.squash = -0.04 * pulse(t, 0.2, 0.25);
            p.changing = t < 0.45;
        }
        "stand" => {
            p.squash = 0.04 * pulse(t, 0., 0.4);
            p.changing = t < 0.4;
        }
        "stretch" | "yawn" => {
            p.squash = 0.04 * pulse(t, 0.05, 0.8);
            p.changing = t < 0.85;
        }
        "read_doze" => {
            p.hat = if t < 1.2 {
                -7. * (t / 1.2).clamp(0., 1.)
            } else {
                5. * pulse(t, 1.2, 0.22)
            };
            p.changing = t < 1.42;
        }
        "hide_launch" => {
            p.major = true;
            p.squash = -0.10 * pulse(t, 0., 0.08);
            p.changing = t < 0.08;
        }
        "hide_air" | "show_air" => {
            p.major = true;
            p.squash = 0.06 * pulse(t, 0., 0.4);
            p.changing = t < 0.4;
        }
        _ => {}
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secondary_hat_remains_visible_in_walk_turn_and_stretch() {
        for clip in ["walk", "turn_left", "stretch"] {
            let mut l = Layer::default();
            let mut peak = 0f32;
            for i in 0..90 {
                let t = i as f32 / 30.;
                let mut p = Motion::new(0.).pose();
                let x = if clip == "walk" { t.min(1.) * 52. } else { 0. };
                l.apply(
                    &mut p,
                    (x, 300., 0.56),
                    1. / 30.,
                    Sample {
                        clip: if t < 1. { clip } else { "idle" },
                        time: t,
                        direction: 1.,
                    },
                    true,
                );
                peak = peak.max(p.hat_tilt.abs());
                if t >= 2. {
                    assert!(p.hat_tilt.abs() <= 0.5);
                }
            }
            assert!(
                peak >= 1.,
                "{clip}: secondary motion disappeared ({peak} degrees)"
            );
        }
    }
    use crate::companion::{anchors::Anchors, clips, motion::Motion, render};
    #[test]
    fn acceleration_lags_and_stops_within_one_second() {
        let mut l = Layer::default();
        let mut maximum = 0f32;
        for i in 0..240 {
            let mut p = Motion::new(0.).pose();
            let x = if i < 30 { i as f32 * 2. } else { 60. };
            l.apply(
                &mut p,
                (x, 300., 0.56),
                1. / 120.,
                Sample {
                    clip: "grab",
                    time: 2.,
                    direction: 1.,
                },
                true,
            );
            maximum = maximum.max(p.hat_tilt.abs());
            if i == 1 {
                assert!(
                    p.hat_tilt > 0.,
                    "rightward acceleration must leave tip left"
                );
            }
            assert!(p.hat_tilt.abs() <= 14.);
            if i >= 150 {
                assert!(p.hat_tilt.abs() <= 0.5);
                assert!(!l.moving());
            }
        }
        assert!(maximum > 1.);
    }
    #[test]
    fn all_clips_conserve_volume_and_keep_the_floor_pivot() {
        for name in clips::catalog().clips.keys() {
            let mut l = Layer::default();
            for i in 0..360 {
                let t = i as f32 / 120.;
                let mut p = Motion::new(0.).pose();
                p.frame = clips::catalog().clip(name).frame(t).into();
                l.apply(
                    &mut p,
                    (100., 300., 0.56),
                    1. / 120.,
                    Sample {
                        clip: name,
                        time: t,
                        direction: 1.,
                    },
                    true,
                );
                assert!((p.sx * p.sy - 1.).abs() < 0.00001, "{name}");
                assert!(
                    (0.88..=1.12001).contains(&p.sx) && (0.88..=1.12001).contains(&p.sy),
                    "{name}: {},{}",
                    p.sx,
                    p.sy
                );
                let f = &clips::catalog().frames[&p.frame];
                let m = render::body_matrix(&p, 100., 300., 0.56);
                let foot = f.ax * m.M12 + f.ground * m.M22 + m.M32;
                assert!((foot - (300. + p.jy * 0.56)).abs() < 0.001, "{name}");
            }
        }
    }
    #[test]
    fn disabled_is_exact_identity_and_idle_returns_to_original_cadence() {
        let mut l = Layer::default();
        let mut p = Motion::new(12.).pose();
        let old = p.clone();
        let x = l.apply(
            &mut p,
            (12., 30., 0.28),
            0.1,
            Sample {
                clip: "land",
                time: 0.1,
                direction: 1.,
            },
            false,
        );
        assert_eq!(x, 12.);
        assert_eq!(
            (p.sx, p.sy, p.hat_tilt, p.jy),
            (old.sx, old.sy, old.hat_tilt, old.jy)
        );
        assert!(!l.moving());
        for battery in [false, true] {
            let base = Some(if battery { 100 } else { 50 });
            assert_eq!(interval(base, false, battery), base);
            assert_eq!(
                interval(base, true, battery),
                Some(if battery { 34 } else { 17 })
            );
            assert_eq!(interval(None, true, battery), None);
        }
    }
    #[test]
    fn walking_has_anticipation_and_a_bounded_stop_overshoot() {
        let mut l = Layer::default();
        let mut peak = 0f32;
        for i in 0..240 {
            let t = i as f32 / 120.;
            let target = 52. * t.min(1.);
            let mut p = Motion::new(0.).pose();
            let x = l.apply(
                &mut p,
                (target, 300., 0.56),
                1. / 120.,
                Sample {
                    clip: if t < 1. { "walk" } else { "idle" },
                    time: t,
                    direction: 1.,
                },
                true,
            );
            if t < 0.08 {
                assert_eq!(x, 0.);
            }
            if t >= 1. {
                peak = peak.max(x);
            }
            if i == 239 {
                assert!((x - 52.).abs() < 0.05);
            }
        }
        assert!(peak > 52. && peak < 56., "{peak}");
    }
    #[test]
    fn hat_moves_effect_anchors_but_body_anchor_is_stable() {
        let mut p = Motion::new(0.).pose();
        p.frame = "think_0".into();
        let a = Anchors::load(&p.frame).posed(&p, 1.);
        p.hat_tilt = 14.;
        let b = Anchors::load(&p.frame).posed(&p, 1.);
        assert_ne!(a.tip, b.tip);
        assert_eq!(a.feet, b.feet);
        assert_eq!(a.nose, b.nose);
        let body = Anchors::body_tip(&p);
        let spinner = Anchors::spinner_foot(&p, 100., 300., 0.56);
        p.hat_tilt = -14.;
        assert_eq!(body, Anchors::body_tip(&p));
        assert_ne!(spinner, Anchors::spinner_foot(&p, 100., 300., 0.56));
        p.hat_tilt = 0.;
        assert_eq!(Anchors::spinner_foot(&p, 100., 300., 0.56), (100., 300.));
    }
    #[test]
    fn idle_breathing_does_not_keep_fast_clock_alive_and_low_fps_is_bounded() {
        let mut l = Layer::default();
        for i in 0..200 {
            let t = i as f32 * 0.1;
            let mut p = Motion::new(0.).pose();
            p.sy = 1. + 0.012 * t.sin();
            p.skew = 1.4 * t.cos();
            l.apply(
                &mut p,
                (100., 300., 0.56),
                0.1,
                Sample {
                    clip: "idle",
                    time: t,
                    direction: 1.,
                },
                true,
            );
            assert!(!l.moving());
            assert_eq!(p.hat_tilt, 0.);
        }
        for i in 0..100 {
            let mut p = Motion::new(0.).pose();
            l.apply(
                &mut p,
                (if i % 2 == 0 { 1000. } else { 0. }, 300., 0.28),
                0.25,
                Sample {
                    clip: "land",
                    time: i as f32 * 0.25,
                    direction: 1.,
                },
                true,
            );
            assert!(p.hat_tilt.is_finite() && p.hat_tilt.abs() <= 14.);
        }
        let mut p = Motion::new(0.).pose();
        l.apply(
            &mut p,
            (50., 300., 0.28),
            0.1,
            Sample {
                clip: "land",
                time: 0.1,
                direction: 1.,
            },
            false,
        );
        assert_eq!((p.sx, p.sy, p.hat_tilt), (1., 1., 0.));
        assert!(!l.moving());
    }
}
