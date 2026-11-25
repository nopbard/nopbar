//! Messages arriving during a reminder are presented when its bubble has closed.
#[derive(Default)]
pub struct SayQueue(std::collections::VecDeque<String>);
impl SayQueue {
    pub fn push(&mut self, text: String) {
        self.0.push_back(text);
    }
    pub fn take(&mut self, ready: bool) -> Option<String> {
        if ready {
            self.0.pop_front()
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_say_during_reminder_waits_and_keeps_fifo() {
        let mut q = SayQueue::default();
        q.push("first".into());
        q.push("second".into());
        assert!(q.take(false).is_none());
        assert_eq!(q.take(true).as_deref(), Some("first"));
        assert_eq!(q.take(true).as_deref(), Some("second"));
        assert!(q.take(true).is_none());
    }
}
