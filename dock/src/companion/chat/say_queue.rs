//! Messages wait until chat can present the next reply.
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
    fn messages_wait_until_chat_is_ready_and_keep_fifo() {
        let mut q = SayQueue::default();
        q.push("first".into());
        q.push("second".into());
        assert!(q.take(false).is_none());
        assert_eq!(q.take(true).as_deref(), Some("first"));
        assert_eq!(q.take(true).as_deref(), Some("second"));
        assert!(q.take(true).is_none());
    }
}
