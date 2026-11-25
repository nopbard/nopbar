//! Frame-sampled gestures. Coordinates are DIPs; clocks are injected seconds.
#[cfg(test)]
use super::grab::Point;

#[cfg(test)]
#[derive(Default)]
pub struct Gaze {
    pub frame: u8,
    horizontal: i8,
    up: bool,
    last: Option<Point>,
    moved_at: f64,
    step_at: f64,
}
#[cfg(test)]
impl Gaze {
    pub fn sample(&mut self, now: f64, cursor: Point, head: Point) -> Option<u8> {
        let first = self.last.is_none();
        if self.last != Some(cursor) {
            self.last = Some(cursor);
            self.moved_at = now;
        }
        let (dx, dy) = (cursor.x - head.x, cursor.y - head.y);
        if dx.hypot(dy) > 260. {
            self.frame = 0;
            self.horizontal = 0;
            self.up = false;
            return None;
        }
        // A 1.5s pause is followed by a 2s attentive hold, then the return.
        let target = if now - self.moved_at >= 3.5 {
            0
        } else {
            if first {
                self.horizontal = if dx.abs() < 30. {
                    0
                } else if dx.abs() < 90. {
                    dx.signum() as i8
                } else {
                    2 * dx.signum() as i8
                };
                self.up = dy < -40.;
            }
            // Schmitt thresholds: a boundary must be crossed by 15 DIP.
            while self.horizontal < 2 && dx >= boundary(self.horizontal) + 15. {
                self.horizontal += 1;
            }
            while self.horizontal > -2 && dx < boundary(self.horizontal - 1) - 15. {
                self.horizontal -= 1;
            }
            if self.up {
                self.up = dy < -25.;
            } else {
                self.up = dy < -55.;
            }
            direction(self.horizontal, self.up)
        };
        if self.frame != target && now >= self.step_at {
            let clockwise = (target + 8 - self.frame) % 8;
            self.frame = (self.frame + if clockwise <= 4 { 1 } else { 7 }) % 8;
            self.step_at = now + 0.060;
        }
        Some(self.frame)
    }
}
#[cfg(test)]
fn boundary(bin: i8) -> f32 {
    match bin {
        -2 => -90.,
        -1 => -30.,
        0 => 30.,
        _ => 90.,
    }
}
#[cfg(test)]
pub fn direction(horizontal: i8, up: bool) -> u8 {
    if up {
        if horizontal < 0 {
            3
        } else if horizontal > 0 {
            5
        } else {
            4
        }
    } else {
        match horizontal {
            -2 => 2,
            -1 => 1,
            0 => 0,
            1 => 7,
            _ => 6,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Click {
    Priority(super::cues::Click),
    Chat,
    Confirm,
}

pub fn click(priority: Option<super::cues::Click>, connected: bool) -> Click {
    match priority {
        Some(action) => Click::Priority(action),
        None if connected => Click::Chat,
        None => Click::Confirm,
    }
}

/// Immediate press feedback, then 50ms ease and a 120ms spring return.
#[derive(Default)]
pub struct Press {down:bool,at:f64,from:f32}
impl Press {
 pub fn set(&mut self,down:bool,now:f64){if self.down!=down {self.from=self.amount(now);self.down=down;self.at=now;}}
 fn amount(&self,now:f64)->f32 {
  let t=(now-self.at).max(0.) as f32;
  if self.down {let u=(t/super::timing::PRESS).clamp(0.,1.);self.from.max(0.12)+(1.-self.from.max(0.12))*u*(2.-u)}
  else {let u=(t/super::timing::RETURN).clamp(0.,1.);self.from*(1.-u)*(u*std::f32::consts::PI*1.5).cos()}
 }
 pub fn apply(&self,p:&mut super::motion::Pose,now:f64,enabled:bool){let k=if enabled{self.amount(now)}else if self.down{1.}else{0.};if k!=0. {p.sx*=1.+0.04*k;p.sy*=1.-0.06*k;p.continuous=true;}}
 pub fn active(&self,now:f64)->bool {self.down||now-self.at<(super::timing::RETURN as f64)&&self.from!=0.}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shutdown_and_agenda_take_priority_and_repeated_clicks_stay_consistent() {
        use super::super::cues::Click as Priority;
        for connected in [false, true] {
            for action in [Priority::CancelShutdown(7), Priority::Agenda(123)] {
                assert_eq!(click(Some(action.clone()), connected), Click::Priority(action));
            }
        }
        for _ in 0..4 {
            assert_eq!(click(None, false), Click::Confirm);
            assert_eq!(click(None, true), Click::Chat);
        }
    }
    #[test] fn press_changes_the_first_frame_and_eases_back() {
        let mut press=Press::default();let mut p=super::super::Motion::new(0.).pose();
        press.set(true,10.);press.apply(&mut p,10.,true);assert!(p.sy<1.&&p.sx>1.);
        let mut p=super::super::Motion::new(0.).pose();press.apply(&mut p,10.05,true);assert!((p.sy-0.94).abs()<0.0001);
        press.set(false,10.1);assert!(press.active(10.11));assert!(!press.active(10.23));
    }
    #[test]
    fn gaze_mapping_hysteresis_and_adjacent_steps() {
        assert_eq!(
            (-2..=2).map(|x| direction(x, false)).collect::<Vec<_>>(),
            [2, 1, 0, 7, 6]
        );
        assert_eq!(
            (-2..=2).map(|x| direction(x, true)).collect::<Vec<_>>(),
            [3, 3, 4, 5, 5]
        );
        let mut g = Gaze::default();
        let head = Point::default();
        assert_eq!(g.sample(0., Point::new(-100., 0.), head), Some(1));
        assert_eq!(g.sample(0.03, Point::new(-106., 0.), head), Some(1));
        assert_eq!(g.sample(0.061, Point::new(-106., 0.), head), Some(2));
        assert_eq!(g.sample(0.2, Point::new(-80., 0.), head), Some(2));
        assert_eq!(g.sample(0.3, Point::new(-74., 0.), head), Some(1));
        assert_eq!(g.sample(0.4, Point::new(261., 0.), head), None);
        assert_eq!(g.frame, 0);
        assert_eq!(g.sample(0.5, Point::new(0., -56.), head), Some(1));
        g.sample(0.6, Point::new(0., -41.), head);
        assert!(g.up);
        g.sample(0.7, Point::new(0., -24.), head);
        assert!(!g.up);
    }
    #[test]
    fn gaze_waits_after_stationary_then_returns() {
        let mut g = Gaze::default();
        let p = Point::new(-110., 0.);
        g.sample(0., p, Point::default());
        g.sample(0.1, p, Point::default());
        assert_eq!(g.sample(1.6, p, Point::default()), Some(2));
        assert_eq!(g.sample(3.49, p, Point::default()), Some(2));
        assert_eq!(g.sample(3.5, p, Point::default()), Some(1));
        assert_eq!(g.sample(3.57, p, Point::default()), Some(0));
    }
}
