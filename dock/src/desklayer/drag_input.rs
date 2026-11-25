//! Pointer sample history in QPC milliseconds and screen pixels.
use std::collections::VecDeque;
use windows::Win32::Foundation::POINT;

const HISTORY: usize = 128;
const RESAMPLE_DELAY: f64 = 5.;
const MAX_PREDICTION: f64 = 8.;
const VELOCITY_WINDOW: f64 = 50.;

#[derive(Clone, Copy)]
pub(super) struct Sample {
    pub at: f64,
    pub point: (f32, f32),
}

pub(super) struct Samples(VecDeque<Sample>);

impl Default for Samples {
    fn default() -> Self {
        Self(VecDeque::with_capacity(HISTORY))
    }
}

impl Samples {
    pub fn push(&mut self, at: f64, point: POINT) {
        let sample = Sample { at, point: (point.x as f32, point.y as f32) };
        if let Some(last) = self.0.back_mut().filter(|last| at <= last.at) {
            if at == last.at {
                *last = sample;
            }
            return;
        }
        if self.0.len() == HISTORY {
            self.0.pop_front();
        }
        self.0.push_back(sample);
    }

    pub fn latest(&self) -> Option<Sample> {
        self.0.back().copied()
    }

    /// Average screen px/s over the last 50 ms. Time spent holding the
    /// cursor still before release counts as part of that window.
    pub fn velocity(&self, released: f64) -> (f32, f32) {
        let start = released - VELOCITY_WINDOW;
        let Some(end) = self.latest().filter(|end| end.at > start) else { return (0., 0.) };
        let index = self.0.partition_point(|s| s.at < start);
        let first = self.0[index];
        let before = if index > 0 && first.at - self.0[index - 1].at <= 20. {
            blend(self.0[index - 1], first, start)
        } else {
            first
        };
        if end.at - before.at < 2. {
            return (0., 0.);
        }
        let scale = (1000. / (released - before.at)) as f32;
        ((end.point.0 - before.point.0) * scale, (end.point.1 - before.point.1) * scale)
    }

    pub fn resample(&self, now: f64) -> Option<Sample> {
        let t = now - RESAMPLE_DELAY;
        let first = *self.0.front()?;
        let last = *self.0.back()?;
        if t <= first.at {
            return Some(first);
        }
        if t <= last.at {
            let next = self.0.partition_point(|s| s.at < t);
            return Some(blend(self.0[next - 1], self.0[next], t));
        }
        let Some(previous) = self.0.len().checked_sub(2).and_then(|i| self.0.get(i)).copied() else {
            return Some(last);
        };
        let gap = last.at - previous.at;
        if !(2. ..=20.).contains(&gap) || t - last.at > gap.min(MAX_PREDICTION) {
            return Some(last);
        }
        Some(blend(previous, last, t.min(last.at + MAX_PREDICTION.min(gap / 2.))))
    }
}

fn blend(a: Sample, b: Sample, t: f64) -> Sample {
    let amount = ((t - a.at) / (b.at - a.at)) as f32;
    Sample {
        at: t,
        point: (
            a.point.0 + (b.point.0 - a.point.0) * amount,
            a.point.1 + (b.point.1 - a.point.1) * amount,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_follows_the_point_five_milliseconds_behind_the_tick() {
        let mut s = Samples::default();
        s.push(10., POINT { x: -10, y: 0 });
        assert_eq!(s.resample(60.).unwrap().point, (-10., 0.));
        s.push(20., POINT { x: 10, y: -5 });
        s.push(30., POINT { x: 30, y: -10 });
        assert_eq!(s.resample(10.).unwrap().point, (-10., 0.));
        assert_eq!(s.resample(20.).unwrap().point, (0., -2.5));
        assert_eq!(s.resample(30.).unwrap().point, (20., -7.5));
    }

    #[test]
    fn extrapolation_clamps_to_half_the_interval_or_eight_milliseconds() {
        for (gap, extra) in [(1.9, 0.), (2., 1.), (8., 4.), (20., 8.), (20.1, 0.)] {
            let mut s = Samples::default();
            s.push(0., POINT { x: 0, y: 0 });
            s.push(gap, POINT { x: 100, y: -100 });
            let sample = s.resample(gap + 5. + gap.min(MAX_PREDICTION)).unwrap();
            assert_eq!(sample.at, gap + extra);
            assert!((sample.point.0 - (100. + (100. * extra / gap) as f32)).abs() < 0.01);
        }
    }

    #[test]
    fn stopped_pointer_returns_to_the_last_observed_position() {
        let mut s = Samples::default();
        s.push(0., POINT { x: 0, y: 0 });
        s.push(8., POINT { x: 16, y: 0 });
        assert_eq!(s.resample(17.).unwrap().point, (24., 0.));
        assert_eq!(s.resample(22.).unwrap().point, (16., 0.));
        s.push(17., POINT { x: 16, y: 0 });
        assert_eq!(s.resample(17.).unwrap().point, (16., 0.));
        assert_eq!(s.resample(400.).unwrap().point, (16., 0.));
    }

    #[test]
    fn release_velocity_uses_recent_motion_and_decays_while_waiting() {
        let mut s = Samples::default();
        s.push(0., POINT { x: -400, y: 0 });
        for t in (20..=80).step_by(10) {
            s.push(f64::from(t), POINT { x: t + 100, y: -2 * t });
        }
        assert_eq!(s.velocity(83.), (940., -1880.));
        assert_eq!(s.velocity(100.), (600., -1200.));
        assert_eq!(s.velocity(131.), (0., 0.));
    }

    #[test]
    fn one_recent_sample_cannot_inherit_an_old_flick() {
        let mut s = Samples::default();
        s.push(10., POINT { x: -100, y: 0 });
        s.push(100., POINT { x: 200, y: 0 });
        assert_eq!(s.velocity(102.), (0., 0.));
    }
}
