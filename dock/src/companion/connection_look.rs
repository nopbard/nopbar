//! One sign-in acknowledgement and a bounded cache of original body rasters.
use super::{clips, render::Stamp};

#[derive(Default)]
pub struct Look {
    awakening: Option<f64>,
}
impl Look {
    pub fn animating(&self) -> bool {
        self.awakening.is_some()
    }

    pub fn update(&mut self, linked: bool, now: f64, motion: bool, eligible: bool) -> Option<String> {
        if !linked {
            self.awakening = None;
            return None;
        }
        if eligible && super::chat::hint::take_awaken() {
            self.awakening = Some(now);
        }
        let elapsed = (now - self.awakening?) as f32;
        let stretch = clips::catalog().clip("stretch");
        if !motion || elapsed >= stretch.duration() {
            self.awakening = None;
            super::chat::hint::show_ready();
            None
        } else {
            Some(stretch.frame(elapsed).into())
        }
    }
}

#[derive(Default)]
pub struct BodyCache(std::collections::VecDeque<(Stamp, Vec<u8>)>);
impl BodyCache {
    pub fn restore(&self, stamp: &Stamp, pixels: &mut [u8]) -> bool {
        if let Some((_, data)) = self.0.iter().find(|(s, p)| s == stamp && p.len() == pixels.len()) {
            pixels.copy_from_slice(data);
            true
        } else {
            false
        }
    }

    pub fn store(&mut self, stamp: &Stamp, pixels: &[u8]) {
        self.0.push_back((stamp.clone(), pixels.to_vec()));
        while self.0.len() > 16 || self.0.iter().map(|(_, p)| p.len()).sum::<usize>() > 8 * 1024 * 1024 {
            self.0.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_cache_preserves_original_colour_and_alpha() {
        let pose = super::super::Motion::new(0.).pose();
        let stamp = Stamp::new(&pose, 0.56, 1., false, false);
        let original = [8, 30, 190, 200];
        let mut cache = BodyCache::default();
        cache.store(&stamp, &original);
        let mut restored = [0; 4];
        assert!(cache.restore(&stamp, &mut restored));
        assert_eq!(restored, original);
    }

    #[test]
    fn first_wake_waits_for_visibility_then_runs_once() {
        use super::super::chat::hint;
        hint::queue_awaken_for_test();
        let mut look = Look::default();
        assert!(look.update(true, 0., true, false).is_none());
        let stretch = clips::catalog().clip("stretch");
        assert_eq!(look.update(true, 5., true, true).as_deref(), Some(stretch.frame(0.)));
        assert!(look.animating());
        assert!(!hint::waiting_to_show());
        assert!(look.update(true, 5. + stretch.duration() as f64, true, true).is_none());
        assert!(!look.animating());
        assert!(hint::waiting_to_show());
        assert!(look.update(true, 30., true, true).is_none());
        hint::queue_awaken_for_test();
        assert!(look.update(true, 31., false, true).is_none());
        assert!(hint::waiting_to_show());
    }
}
