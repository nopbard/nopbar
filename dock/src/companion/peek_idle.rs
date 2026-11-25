//! Quiet edge life evaluated from injected wall-clock time, never frame count.
use super::Edge;
use std::f32::consts::{PI, TAU};

#[derive(Default)]
pub(super) struct Life {
    epoch: f64,
    unit: f64,
}

#[derive(Default, Debug, PartialEq)]
pub(super) struct Sample {
    /// Physical pixels, converted to DIPs by the caller.
    pub breath: f32,
    /// Fraction of the visible width (positive is farther into the screen).
    pub depth: f32,
    pub hat: f32,
    pub busy: bool,
}

impl Life {
    pub fn new(now: f64, unit: f32) -> Self {
        Self {
            epoch: now,
            unit: unit.clamp(0., 1.) as f64,
        }
    }
    pub fn sample(&self, edge: Edge, now: f64, motion: bool, expression: f32) -> Sample {
        if !motion {
            return Sample::default();
        }
        let elapsed = (now - self.epoch).max(0.);
        let breath = 0.75 * (elapsed as f32 * TAU / 3.5).sin();
        let period = if edge == Edge::Bottom {
            8. + 7. * self.unit
        } else {
            6. + 6. * self.unit
        };
        let phase = (elapsed % period) as f32;
        let started = elapsed >= period;
        if edge == Edge::Bottom {
            let active = started && phase < 2.6 && expression > 0.;
            let direction = if ((elapsed / period) as u64).is_multiple_of(3) {
                -1.
            } else {
                1.
            };
            let hat = if active {
                direction * 10. * (PI * phase / 2.6).sin().powi(2) * expression
            } else {
                0.
            };
            Sample {
                breath,
                hat,
                busy: active,
                ..Default::default()
            }
        } else {
            // Breathing and hat droop remain; no unsolicited side extension.
            let wind_start = period * 0.55;
            let wind = ((elapsed - wind_start).max(0.) % (period * 1.5)) as f32 / 0.8;
            let windy = elapsed >= wind_start && wind < 1.;
            let hat = (if windy {
                3. * (TAU * wind).sin() * (PI * wind).sin().powi(2)
            } else {
                0.
            }) * expression;
            Sample {
                breath,
                depth: 0.,
                hat,
                busy: expression > 0. && windy,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn droop_onsets_are_eight_to_fifteen_seconds_and_every_third_reverses() {
        for unit in [0., 0.3, 1.] {
            let life = Life::new(0., unit);
            let mut onsets = Vec::new();
            let mut before = false;
            for i in 0..7000 {
                let t = i as f64 / 100.;
                let sample = life.sample(Edge::Bottom, t, true, 1.);
                if sample.busy && !before {
                    onsets.push(t);
                }
                assert!(sample.hat.abs() <= 10.001);
                before = sample.busy;
            }
            assert!(onsets.len() >= 4);
            for span in onsets.windows(2) {
                assert!((7.99..=15.01).contains(&(span[1] - span[0])));
            }
            for (i, t) in onsets.iter().enumerate() {
                let peak = life.sample(Edge::Bottom, t + 1.3, true, 1.);
                assert_eq!(peak.hat.signum(), if i % 3 == 2 { -1. } else { 1. });
                assert!(!life.sample(Edge::Bottom, t + 2.7, true, 1.).busy);
            }
        }
    }
    #[test]
    fn side_extension_breath_and_disabled_motion() {
        for unit in [0., 0.5, 1.] {
            let life = Life::new(0., unit);
            let period = 6. + 6. * unit as f64;
            for edge in [Edge::Left, Edge::Right] {
                assert!(life.sample(edge, period + 0.2, true, 1.).depth.abs() < 0.0001);
                assert_eq!(life.sample(edge, period + 0.41, true, 1.).depth, 0.);
                assert_eq!(life.sample(edge, period + 0.3, true, 1.).depth, 0.);
                for i in 0..3000 {
                    let t = i as f64 / 100.;
                    assert_eq!(life.sample(edge, t, false, 1.), Sample::default());
                    assert!(life.sample(edge, t, true, 1.).breath.abs() <= 0.751);
                }
            }
            assert!((life.sample(Edge::Bottom, 0.875, true, 1.).breath - 0.75).abs() < 0.0001);
            assert!(life.sample(Edge::Bottom, 3.5, true, 1.).breath.abs() < 0.0001);
        }
    }
}
