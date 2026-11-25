use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Retry {
    failures: u32,
    next: Option<Instant>,
}
impl Retry {
    pub fn failed(&mut self, now: Instant) -> Option<u32> {
        self.failures += 1;
        let delay = (self.failures <= 5).then(|| 1000 << (self.failures - 1));
        self.next = delay.map(|ms| now + Duration::from_millis(ms as u64));
        delay
    }
    pub fn ready(&self, now: Instant) -> bool {
        self.failures <= 5 && self.next.is_none_or(|t| now >= t)
    }
    pub fn remaining(&self, now: Instant) -> Option<u32> {
        self.next
            .map(|t| t.saturating_duration_since(now).as_millis().max(1) as u32)
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_recovers_with_bounded_backoff_and_stops() {
        let t = Instant::now();
        let mut r = Retry::default();
        for ms in [1000, 2000, 4000, 8000, 16000] {
            assert_eq!(r.failed(t), Some(ms));
            assert!(!r.ready(t));
            assert_eq!(r.remaining(t), Some(ms));
            assert!(r.ready(t + Duration::from_millis(ms as u64)));
        }
        assert_eq!(r.failed(t), None);
        assert!(!r.ready(t + Duration::from_secs(100)));
        r.reset();
        assert!(r.ready(t));
        assert_eq!(r.failed(t), Some(1000));
    }
}
