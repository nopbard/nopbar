//! Independent transport state: Windows accepts toasts even while DND is on.
use super::reminders::{Delivery, Notice, Queue};
use std::time::{Duration, Instant};

const DEFER: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: u32 = 8;
struct Pending {
    notice: Notice,
    toast_at: Option<Instant>,
    attempts: u32,
    bubble_at: Option<Instant>,
    acknowledged: bool,
}
pub struct Dispatcher {
    queued: Queue,
    active: Vec<Pending>,
    last: Option<Delivery>,
}
impl Dispatcher {
    pub const fn new() -> Self { Self { queued: Queue::new(), active: Vec::new(), last: None } }
    pub fn push(&mut self, mut notice: Notice) {
        notice.ids.retain(|id| !self.active.iter().any(|p| p.notice.ids.contains(id)));
        self.queued.push(notice);
    }
    pub fn cancel(&mut self, id: &str) {
        self.queued.cancel(id);
        for p in &mut self.active {
            p.notice.ids.retain(|i| i != id);
            if p.notice.grouped && !p.notice.ids.is_empty() {
                p.notice = Notice::summary(p.notice.ids.clone(), p.notice.missed);
            }
        }
        self.active.retain(|p| !p.notice.ids.is_empty());
    }
    /// Repaint remaining members of an already delivered bubble after a cancel.
    pub fn push_bubble(&mut self, mut notice: Notice, now: Instant) {
        for p in &mut self.active {
            if notice.ids.iter().any(|id| p.notice.ids.contains(id)) {
                p.bubble_at = Some(now);
                notice.ids.retain(|id| !p.notice.ids.contains(id));
            }
        }
        if !notice.ids.is_empty() {
            if notice.grouped { notice = Notice::summary(notice.ids, notice.missed); }
            self.active.push(Pending { notice, toast_at: None, attempts: MAX_ATTEMPTS,
                bubble_at: Some(now), acknowledged: true });
        }
    }
    pub fn poll(&mut self, now: Instant, delivery: Delivery,
        mut toast: impl FnMut(&Notice) -> bool,
        mut bubble: impl FnMut(&Notice) -> bool,
        mut acknowledge: impl FnMut(Notice),
    ) -> Option<Duration> {
        while let Some(notice) = self.queued.next() {
            self.queued.acknowledge(&notice);
            self.active.push(Pending { notice, toast_at: Some(now), attempts: 0,
                bubble_at: Some(now), acknowledged: false });
        }
        let changed = self.last.replace(delivery) != Some(delivery);
        for p in &mut self.active {
            let mut delivered = false;
            if p.toast_at.is_some_and(|at| at <= now) {
                p.attempts += 1;
                delivered = toast(&p.notice);
                p.toast_at = if delivered || p.attempts >= MAX_ATTEMPTS { None }
                    else { Some(now + Duration::from_secs((5u64 << (p.attempts - 1)).min(300))) };
            }
            if p.bubble_at.is_some_and(|at| changed || at <= now) {
                match delivery {
                    Delivery::Defer => p.bubble_at = Some(now + DEFER),
                    Delivery::Toast => p.bubble_at = None,
                    Delivery::Bubble => {
                        let shown = bubble(&p.notice);
                        delivered |= shown;
                        p.bubble_at = if shown { None } else { Some(now + DEFER) };
                    }
                }
            }
            if delivered && !p.acknowledged {
                p.acknowledged = true;
                acknowledge(p.notice.clone());
            }
        }
        self.active.retain(|p| p.toast_at.is_some() || p.bubble_at.is_some());
        self.active.iter().flat_map(|p| [p.toast_at, p.bubble_at]).flatten()
            .min().map(|at| at.saturating_duration_since(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn notice(id: &str) -> Notice { Notice::single(id.into(), id.into()) }
    #[test]
    fn quiet_delivers_toast_now_and_bubble_once_after_release() {
        let mut d = Dispatcher::new(); let t = Instant::now();
        d.push(notice("a"));
        let (mut toasts, mut bubbles, mut acks) = (0, 0, 0);
        for (secs, route) in [(0, Delivery::Defer), (1, Delivery::Defer),
            (2, Delivery::Bubble), (3, Delivery::Bubble)] {
            let wake = d.poll(t + Duration::from_secs(secs), route,
                |_| {toasts += 1; true}, |_| {bubbles += 1; true}, |_| acks += 1);
            if secs == 0 { assert_eq!(wake, Some(DEFER)); }
            if secs <= 1 { assert_eq!(bubbles, 0); }
        }
        assert_eq!((toasts, bubbles, acks), (1, 1, 1));
    }
    #[test]
    fn toast_action_cancels_deferred_bubble_and_failed_transport() {
        let mut d = Dispatcher::new(); let t = Instant::now();
        d.push(notice("a")); d.push(notice("b"));
        d.poll(t, Delivery::Defer, |_| true, |_| panic!(), |_| {});
        d.cancel("a");
        d.poll(t, Delivery::Bubble, |_| panic!(), |n| {assert_eq!(n.ids, ["b"]); true}, |_| panic!());
        assert!(d.active.is_empty());
        d.push(notice("c"));
        d.poll(t, Delivery::Defer, |_| false, |_| panic!(), |_| panic!());
        d.cancel("c");
        assert!(d.poll(t + DEFER, Delivery::Bubble, |_| panic!(), |_| panic!(), |_| panic!()).is_none());
    }
    #[test]
    fn toast_failure_still_shows_bubble_and_backs_off_to_a_bounded_stop() {
        let mut d = Dispatcher::new(); let t = Instant::now(); d.push(notice("a"));
        let (mut toasts, mut bubbles, mut acks) = (0, 0, 0);
        let mut at = t;
        let expected = [Some(5), Some(10), Some(20), Some(40), Some(80), Some(160), Some(300), None];
        for seconds in expected {
            let wake = d.poll(at, Delivery::Bubble, |_| {toasts += 1; false},
                |_| {bubbles += 1; true}, |_| acks += 1);
            assert_eq!(wake, seconds.map(Duration::from_secs));
            if let Some(wait) = wake {
                d.poll(at + Duration::from_secs(1), Delivery::Bubble, |_| panic!(), |_| panic!(), |_| panic!());
                at += wait;
            }
        }
        assert_eq!((toasts, bubbles, acks), (8, 1, 1));
        assert!(d.active.is_empty());
    }
    #[test]
    fn new_notice_is_not_blocked_by_an_old_quiet_deadline() {
        let mut d = Dispatcher::new(); let t = Instant::now(); d.push(notice("a"));
        d.poll(t, Delivery::Defer, |_| true, |_| panic!(), |_| {});
        d.push(notice("b"));
        d.poll(t + Duration::from_secs(1), Delivery::Defer,
            |n| {assert_eq!(n.ids, ["b"]); true}, |_| panic!(), |_| {});
    }
    #[test]
    fn busy_bubble_retries_without_duplicate_toast_or_ack() {
        let mut d = Dispatcher::new(); let t = Instant::now(); d.push(notice("a"));
        assert_eq!(d.poll(t, Delivery::Bubble, |_| true, |_| false, |_| {}), Some(DEFER));
        assert!(d.poll(t + DEFER, Delivery::Bubble, |_| panic!(), |_| true, |_| panic!()).is_none());
    }
    #[test]
    fn partial_bubble_cancellation_never_resends_an_already_delivered_toast() {
        let mut d = Dispatcher::new(); let t = Instant::now(); d.push(notice("a")); d.push(notice("b"));
        d.poll(t, Delivery::Bubble, |_| true, |_| true, |_| {});
        d.cancel("a"); d.push_bubble(notice("b"), t);
        d.poll(t, Delivery::Bubble, |_| panic!("duplicate toast"), |_| true, |_| panic!("duplicate ack"));
        assert!(d.active.is_empty());
    }
}
