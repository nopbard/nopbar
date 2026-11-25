//! Passive, local notifications: no model/provider prompt.
use crate::notes::reminders::Delivery;
use std::{collections::VecDeque, sync::Mutex};
struct PendingToast {
    id: String,
    result: std::sync::mpsc::Receiver<bool>,
}
fn toast_result(pending: &mut Option<PendingToast>) -> Option<(String, bool)> {
    match pending.as_ref()?.result.try_recv() {
        Ok(ok) => pending.take().map(|p| (p.id, ok)),
        Err(std::sync::mpsc::TryRecvError::Disconnected) => pending.take().map(|p| (p.id, false)),
        Err(std::sync::mpsc::TryRecvError::Empty) => None,
    }
}
thread_local! {
    static TOAST: std::cell::RefCell<Option<PendingToast>> = const { std::cell::RefCell::new(None) };
    static RETRY: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}
static QUEUE: Mutex<VecDeque<(String, String)>> = Mutex::new(VecDeque::new());
pub fn enqueue(id: String, text: String) {
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = q.iter_mut().find(|(k, _)| *k == id) {
        n.1 = text;
    } else {
        q.push_back((id, text));
    }
}
pub fn clear() {
    TOAST.with(|t| *t.borrow_mut() = None);
    RETRY.with(|t| t.set(None));
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
pub fn retain(ids: &[String]) {
    TOAST.with(|t| {
        let mut t = t.borrow_mut();
        if t.as_ref().is_some_and(|p| !ids.contains(&p.id)) {
            *t = None;
        }
    });
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(id, _)| ids.contains(id));
}
fn poll_queue(
    queue: &mut VecDeque<(String, String)>,
    delivery: impl FnOnce() -> Delivery,
    send: impl FnOnce(Delivery, String) -> bool,
) -> Option<String> {
    if queue.is_empty() {
        return None;
    }
    let delivery = delivery();
    if delivery == Delivery::Defer {
        return None;
    }
    let (id, text) = queue.front()?.clone();
    if !send(delivery, text) {
        return None;
    }
    queue.pop_front();
    Some(id)
}
pub fn poll() -> Option<String> {
    let completed = TOAST.with(|t| toast_result(&mut t.borrow_mut()));
    if let Some((id, ok)) = completed {
        if ok {
            QUEUE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|(k, _)| *k != id);
            return Some(id);
        }
        RETRY.with(|t| {
            t.set(Some(
                std::time::Instant::now() + std::time::Duration::from_secs(30),
            ))
        });
    }
    if TOAST.with(|t| t.borrow().is_some())
        || RETRY.with(|t| t.get().is_some_and(|at| at > std::time::Instant::now()))
    {
        return None;
    }
    // Do not hold the queue mutex while the say path runs native UI code.
    let mut next: VecDeque<_> = QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .front()
        .cloned()
        .into_iter()
        .collect();
    let toast_id = next.front().map(|(id, _)| id.clone());
    let delivered = poll_queue(&mut next, super::remind::delivery, |delivery, text| {
        dispatch(delivery, text, super::chat::say_queued, |text| {
            let (tx, rx) = std::sync::mpsc::channel();
            let title = crate::i18n::t("widgets.device_battery").to_owned();
            if std::thread::Builder::new()
                .name("battery-notice".into())
                .spawn(move || {
                    let _ = tx.send(crate::updates::try_notice_now(&title, &text));
                })
                .is_ok()
            {
                if let Some(id) = toast_id {
                    TOAST.with(|t| *t.borrow_mut() = Some(PendingToast { id, result: rx }));
                }
            }
            false // Acknowledge only after the toast API reports successful delivery.
        })
    });
    if let Some(id) = &delivered {
        QUEUE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(k, _)| k != id);
    }
    delivered
}
fn dispatch(
    delivery: Delivery,
    text: String,
    say: impl FnOnce(String) -> bool,
    toast: impl FnOnce(String) -> bool,
) -> bool {
    match delivery {
        Delivery::Bubble => say(text),
        Delivery::Toast => toast(text),
        Delivery::Defer => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_09_toast_ack_waits_for_success_and_detects_failure() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut pending = Some(PendingToast {
            id: "a".into(),
            result: rx,
        });
        assert!(toast_result(&mut pending).is_none());
        assert!(pending.is_some());
        tx.send(true).unwrap();
        assert_eq!(toast_result(&mut pending), Some(("a".into(), true)));
        assert!(pending.is_none());
        let (tx, rx) = std::sync::mpsc::channel();
        pending = Some(PendingToast {
            id: "b".into(),
            result: rx,
        });
        tx.send(false).unwrap();
        assert_eq!(toast_result(&mut pending), Some(("b".into(), false)));
        let (tx, rx) = std::sync::mpsc::channel();
        pending = Some(PendingToast {
            id: "c".into(),
            result: rx,
        });
        drop(tx);
        assert_eq!(toast_result(&mut pending), Some(("c".into(), false)));
    }
    #[test]
    fn review_05_empty_queue_skips_monitor_and_delivery_checks() {
        let mut queue = VecDeque::new();
        assert!(poll_queue(
            &mut queue,
            || panic!("empty queue inspected fullscreen/monitors"),
            |_, _| panic!("sent empty queue")
        )
        .is_none());
    }
    #[test]
    fn review_09_delivery_failure_and_deferral_keep_pending_notice() {
        let mut queue = VecDeque::from([("a".into(), "a 18%".into())]);
        assert!(poll_queue(&mut queue, || Delivery::Defer, |_, _| panic!("deferred")).is_none());
        assert!(poll_queue(&mut queue, || Delivery::Bubble, |_, _| false).is_none());
        assert_eq!(queue.len(), 1);
        assert_eq!(
            poll_queue(&mut queue, || Delivery::Bubble, |_, _| true),
            Some("a".into())
        );
        assert!(queue.is_empty());
    }
    #[test]
    fn disabled_companion_uses_toast_and_deferral_delivers_neither() {
        use std::cell::Cell;
        let bubble = Cell::new(false);
        let toast = Cell::new(false);
        let d = crate::notes::reminders::delivery_for(false, false, false, false, false);
        dispatch(
            d,
            "이어폰 18%".into(),
            |_| {
                bubble.set(true);
                true
            },
            |_| {
                toast.set(true);
                true
            },
        );
        assert!(!bubble.get() && toast.get());
        toast.set(false);
        dispatch(
            Delivery::Defer,
            "x".into(),
            |_| {
                bubble.set(true);
                true
            },
            |_| {
                toast.set(true);
                true
            },
        );
        assert!(!bubble.get() && !toast.get());
    }
    #[test]
    fn reduced_motion_uses_the_same_local_say_route() {
        use crate::companion::chat::bubbles::{Bubbles, Phase};
        let mut b = Bubbles::default();
        b.say("이어폰 18%".into(), 0.);
        assert_eq!(b.phase, Phase::Reply);
        assert_eq!(b.alpha(0., false), 1.);
        assert_eq!(b.alpha(0., true), 0.);
        b.tick(b.deadline().unwrap(), false);
        assert_eq!(b.phase, Phase::Hidden);
    }
}
