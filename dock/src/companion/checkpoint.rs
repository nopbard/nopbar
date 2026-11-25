//! Batches the local usage state independently of UI configuration application.
#[derive(Default)]
pub struct Checkpoint {
    pub at: u64,
    pub day: i64,
}
impl Checkpoint {
    pub fn due(&self, now: u64, day: i64, dirty: bool, shutdown: bool) -> bool {
        dirty && (shutdown || day != self.day || now.saturating_sub(self.at) >= 1800)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_3_batches_minutes_caps_date_and_shutdown() {
        let mut c = Checkpoint { at: 0, day: 10 };
        for now in 1..1800 {
            assert!(!c.due(now, 10, true, false));
        }
        assert!(c.due(1800, 10, true, false));
        c.at = 1800;
        assert!(!c.due(1801, 10, true, false));
        assert!(c.due(1801, 11, true, false));
        assert!(c.due(1801, 9, true, false));
        assert!(c.due(1801, 10, true, true));
        assert!(!c.due(9000, 10, false, true));
    }
}
