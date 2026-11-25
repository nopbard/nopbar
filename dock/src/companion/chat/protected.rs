//! Detached credential work survives a cancelled request/runtime. Normal process
//! exit drains it after windows close, so a rotated refresh token reaches vault.
use std::sync::{Arc, Condvar, Mutex, OnceLock};
#[derive(Default)]
struct Jobs {
    state: Mutex<(usize, bool)>,
    done: Condvar,
}
impl Jobs {
    fn spawn<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Option<tokio::sync::oneshot::Receiver<T>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.1 {
            return None;
        }
        state.0 += 1;
        drop(state);
        let jobs = self.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            struct Finish(Arc<Jobs>);
            impl Drop for Finish {
                fn drop(&mut self) {
                    let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.0 -= 1;
                    self.0.done.notify_all();
                }
            }
            let _finish = Finish(jobs);
            let result = work();
            let _ = tx.send(result);
        });
        Some(rx)
    }
    fn drain(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.1 = true;
        while state.0 > 0 {
            state = self.done.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }
}
fn jobs() -> &'static Arc<Jobs> {
    static JOBS: OnceLock<Arc<Jobs>> = OnceLock::new();
    JOBS.get_or_init(Default::default)
}
pub async fn run<T: Send + 'static>(
    work: impl FnOnce() -> super::credentials::Result<T> + Send + 'static,
) -> super::credentials::Result<T> {
    jobs()
        .spawn(work)
        .ok_or(crate::i18n::t("chat.error.connection_closing"))?
        .await
        .map_err(|_| crate::i18n::t("chat.error.connection_stopped"))?
}
pub fn shutdown() {
    jobs().drain();
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_does_not_discard_rotation_and_shutdown_waits_for_commit() {
        let jobs = Arc::new(Jobs::default());
        let (finish, wait) = std::sync::mpsc::channel();
        let committed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = committed.clone();
        let rx = jobs
            .spawn(move || {
                wait.recv().unwrap();
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .unwrap();
        drop(rx);
        finish.send(()).unwrap();
        jobs.drain();
        assert!(committed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(jobs.spawn(|| ()).is_none());
    }
}
