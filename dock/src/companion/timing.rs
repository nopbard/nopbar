//! Shared companion timings in seconds. Tool bubble design is intentionally separate.
pub const POP: f32 = 0.12;
pub const FADE: f32 = 0.15;
pub const CLIP_EASE: f32 = 0.12;
pub const PRESS: f32 = 0.05;
pub const RETURN: f32 = 0.12;

pub fn motion(setting: bool, system: bool, effects: bool) -> bool {
    setting && system && effects
}

/// Pointer movement never consumes this budget; only a full held frame does.
pub fn drag_due(elapsed:f64,battery:bool)->bool {elapsed+1e-9>=if battery{1./30.}else{1./60.}}

/// Walking windows move at 60fps, while their body is rendered at most 20fps.
#[derive(Default)]
pub struct BodyClock(Option<f64>);
impl BodyClock {
    pub fn due(&mut self, now: f64, walking: bool, battery: bool, force: bool) -> bool {
        if !walking { self.0 = None; return true; }
        let period=if battery {0.1}else{0.05};
        // Preserve the 20fps deadline across 60fps ticks. Resetting it to each
        // actual wake would turn a sub-ms early tick into a whole missed frame.
        let due = force || self.0.is_none_or(|at| now + 0.001 >= at);
        if due { self.0=Some(if force {now+period}else{self.0.map(|at|at+period).filter(|&at|at>now).unwrap_or(now+period)}); }
        due
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn reduced_motion_requires_all_three_switches() {
        for setting in [false,true] { for system in [false,true] { for effects in [false,true] {
            assert_eq!(motion(setting,system,effects), setting && system && effects);
        }}}
    }
    #[test] fn movement_ticks_do_not_force_body_raster_and_transitions_reset() {
        let mut clock=BodyClock::default();
        let frames=(0..60).filter(|&i|clock.due(i as f64/60.,true,false,false)).count();
        assert_eq!(frames,20);
        let mut clock=BodyClock::default();
        assert_eq!((0..60).filter(|&i|clock.due(i as f64/60.,true,true,false)).count(),10);
        assert!(clock.due(1.,false,false,false));assert!(clock.due(1.001,true,false,false));
        assert!(clock.due(1.002,true,false,true));assert!(!clock.due(1.003,true,false,false));
    }
    #[test] fn walking_body_deadline_does_not_drift_under_submillisecond_jitter() {
        for (battery,expected) in [(false,60),(true,30)] {
            let mut clock=BodyClock::default();
            let frames=(0..180).filter(|&i|clock.due(i as f64/60.+if i%2==0{0.0004}else{-0.0004},true,battery,false)).count();
            assert_eq!(frames,expected);
        }
    }
    #[test] fn approved_durations_are_unchanged() {
        assert_eq!([POP,FADE,CLIP_EASE,PRESS,RETURN],[0.12,0.15,0.12,0.05,0.12]);
    }
}

pub const FAN_OPEN:f32=0.14;
pub const FAN_CLOSE:f32=0.1;
pub const FAN_STAGGER:f32=0.03;
pub const FAN_HOVER:f32=0.08;
pub const FAN_TOOLTIP:f32=0.4;
pub const FAN_HOLD:f32=0.5;
