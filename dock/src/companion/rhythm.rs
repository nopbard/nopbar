//! Local day gate, shared by the morning briefing. No empty greetings.
use super::model::State;
#[derive(Clone, Copy)]
pub struct Calendar {
    pub date: u32,
    pub minute: u16,
}
#[derive(Default)]
pub struct Rhythm {
    away: bool,
}
impl Rhythm {
    pub fn away(&mut self, away: bool, _now: f64) {
        self.away = away;
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn morning_due(
        &self,
        calendar: Calendar,
        last: Option<u32>,
        available: Option<State>,
    ) -> bool {
        !self.away
            && calendar.minute >= 5 * 60
            && last != Some(calendar.date)
            && matches!(available, Some(State::Idle | State::Sit | State::Sleep))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn morning_gate_reuses_five_oclock_and_away_rules() {
        let mut r = Rhythm::default();
        let mut c = Calendar {
            date: 20261117,
            minute: 299,
        };
        assert!(!r.morning_due(c, None, Some(State::Idle)));
        c.minute = 300;
        assert!(r.morning_due(c, None, Some(State::Idle)));
        assert!(!r.morning_due(c, Some(c.date), Some(State::Idle)));
        r.away(true, 0.);
        assert!(!r.morning_due(c, None, Some(State::Idle)));
        r.away(false, 8000.);
        assert!(!r.morning_due(c, None, None));
        assert!(r.morning_due(c, None, Some(State::Idle)));
    }
}
