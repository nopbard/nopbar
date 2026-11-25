//! Publish Explorer readings atomically; retain one grid for the whole drag.
use super::grid::Desk;
use crate::monitors::Monitor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
static DEFERRED: AtomicBool = AtomicBool::new(false);
static DISPLAY_SETTLING: AtomicBool = AtomicBool::new(false);
pub fn display_settling() -> bool { DISPLAY_SETTLING.load(Ordering::Acquire) }
pub fn begin_display_change() { DISPLAY_SETTLING.store(true, Ordering::Release); }
pub fn end_display_change() { DISPLAY_SETTLING.store(false, Ordering::Release); }
pub fn defer() {
    DEFERRED.store(true, Ordering::Relaxed);
}

#[derive(Default)]
struct Cache {
    current: Option<Desk>,
    pending: Option<(Desk, Vec<Monitor>)>,
    frozen: Option<(Desk, Vec<Monitor>)>,
    holds: usize,
}
impl Cache {
    fn publish(&mut self, mut desk: Desk, mons: &[Monitor]) -> bool {
        if self.holds > 0 {
            self.pending = Some((desk, mons.to_vec()));
            return false;
        }
        super::grid::stabilize(&mut desk, self.current.as_ref(), mons);
        let changed = self.current.as_ref() != Some(&desk);
        self.current = Some(desk);
        changed
    }
    fn release(&mut self) -> bool {
        self.holds = self.holds.saturating_sub(1);
        if self.holds != 0 {
            return false;
        }
        self.frozen = None;
        self.pending.take().is_some_and(|(desk, mons)| self.publish(desk, &mons))
    }
}
static CACHE: Mutex<Cache> = Mutex::new(Cache {
    current: None,
    pending: None,
    frozen: None,
    holds: 0,
});
pub fn cached() -> Option<Desk> {
    let c = CACHE.lock().ok()?;
    c.frozen
        .as_ref()
        .map(|f| f.0.clone())
        .or_else(|| c.current.clone())
}
pub fn monitors() -> Vec<Monitor> {
    CACHE
        .lock()
        .ok()
        .and_then(|c| c.frozen.as_ref().map(|f| f.1.clone()))
        .unwrap_or_else(crate::monitors::all)
}
pub fn publish(desk: Desk) {
    let changed = CACHE.lock().is_ok_and(|mut c| c.publish(desk, &crate::monitors::all()));
    if changed && !display_settling() {
        super::edit::grid_changed();
    }
}
pub fn held() -> bool {
    CACHE.lock().is_ok_and(|c| c.holds > 0)
}
pub struct Frozen;
impl Frozen {
    pub fn new() -> Self {
        super::icons::refresh();
        let mons = crate::monitors::all();
        let fallback = super::spacing::fallback(1.);
        if let Ok(mut c) = CACHE.lock() {
            if c.holds == 0 {
                c.frozen = Some((c.current.clone().unwrap_or(fallback), mons));
            }
            c.holds += 1;
        }
        Self
    }
}
impl Drop for Frozen {
    fn drop(&mut self) {
        let changed = CACHE.lock().is_ok_and(|mut c| c.release());
        if !held() && !display_settling() {
            let deferred = DEFERRED.swap(false, Ordering::Relaxed);
            if changed || deferred { super::edit::grid_changed(); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_unaligned_icon_move_keeps_published_origin() {
        use windows::Win32::Foundation::{POINT, RECT};
        let r = RECT { left: 0, top: 0, right: 800, bottom: 600 };
        let mons = [Monitor { handle: 0, rect: r, work: r, primary: true, dpi: 96 }];
        let mut c = Cache::default();
        let first = Desk { cell: (80,100), icons: vec![POINT { x: 7, y: 11 }], ..Default::default() };
        c.publish(first.clone(), &mons);
        let before = super::super::grid::grids(c.current.as_ref().unwrap(), &mons);
        c.publish(Desk { icons: vec![POINT { x: 37, y: 49 }], ..first }, &mons);
        assert_eq!(super::super::grid::grids(c.current.as_ref().unwrap(), &mons), before);
    }
    #[test]
    fn newest_reading_waits_until_all_drags_end() {
        let registry = Desk {
            cell: (76, 96),
            ..Default::default()
        };
        let actual = Desk {
            cell: (80, 100),
            align: true,
            ..Default::default()
        };
        let latest = Desk {
            auto_arrange: true,
            ..actual.clone()
        };
        let mut c = Cache {
            current: Some(registry.clone()),
            holds: 2,
            ..Default::default()
        };
        assert!(!c.publish(actual, &[]));
        assert!(!c.publish(latest.clone(), &[]));
        assert_eq!(c.current, Some(registry));
        assert!(!c.release());
        assert!(c.release());
        assert_eq!(c.current, Some(latest.clone()));
        assert!(!c.publish(latest, &[]));
    }
}
