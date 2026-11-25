//! The settings window's end of the dock's pipe: one JSON line out, one in.
//! Without a running dock it works offline (Commit writes the file).

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use super::model;
use crate::config::Config;

/// Longest wait for one answer from the dock (it answers within ~1 s even
/// when its UI is busy; audit A08: this used to be unbounded).
const CALL_DEADLINE: Duration = Duration::from_secs(5);
/// Longest answer accepted (the dock's own request limit).
const MAX_REPLY: u64 = 1 << 20;

/// The pipe, served by its own thread: the window waits for an answer only
/// up to CALL_DEADLINE, and a dock that stops answering leaves the window
/// working offline instead of frozen.
struct Link {
    /// None once closing (the thread's recv then ends).
    tx: Option<Sender<String>>,
    rx: Receiver<Option<String>>,
    /// Set before the thread is told to stop: it starts no new I/O.
    closing: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Link {
    /// Ends the pipe thread for sure (audit fix review): no new I/O after
    /// `closing`, and a read or write already blocked is cancelled again
    /// and again until the thread is gone (CancelSynchronousIo only hits
    /// I/O that is pending at that moment). A helper does the waiting, so
    /// the window never does; the thread owns the pipe, so the pipe closes
    /// (and the dock drops a preview) as soon as it ends.
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        self.tx = None;
        let Some(thread) = self.thread.take() else { return };
        let reap = move || {
            use std::os::windows::io::AsRawHandle;
            while !thread.is_finished() {
                unsafe {
                    let _ = windows::Win32::System::IO::CancelSynchronousIo(windows::Win32::Foundation::HANDLE(thread.as_raw_handle()));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = thread.join();
        };
        if let Err(e) = std::thread::Builder::new().name("settings-pipe-close".into()).spawn(reap) {
            crate::log!("settings: no thread to close the pipe ({e}); it closes with the window");
        }
    }
}

pub struct Client {
    link: Option<Link>,
    /// What the dock last said about its global hotkeys (registered, or
    /// held by another app): (config key, spec, status). Kept for the
    /// notes once the link drops.
    pub hotkeys: Vec<(String, String, crate::hotkeys::Status)>,
    /// What the desktop widgets use, as the dock measured it (None: not told).
    pub widgets: Option<crate::desklayer::load::Status>,
}

/// One request line out, one answer line (at most MAX_REPLY bytes) in.
/// Nothing new starts once `closing` is set.
fn exchange(w: &mut std::fs::File, r: &mut BufReader<std::fs::File>, line: &str, closing: &AtomicBool) -> Option<String> {
    if closing.load(Ordering::SeqCst) {
        return None;
    }
    writeln!(w, "{line}").ok()?;
    w.flush().ok()?;
    if closing.load(Ordering::SeqCst) {
        return None;
    }
    let mut reply = String::new();
    let n = r.by_ref().take(MAX_REPLY + 1).read_line(&mut reply).ok()?;
    (n > 0 && n as u64 <= MAX_REPLY && reply.ends_with('\n')).then_some(reply)
}

impl Client {
    pub fn connect() -> Self {
        let name = crate::worker::ipc::pipe_name();
        // Opening a pipe never waits (all instances busy: it fails at once).
        let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(&name) else { return Self::offline() };
        let Ok(mut w) = file.try_clone() else { return Self::offline() };
        let mut r = BufReader::new(file);
        let (tx, req_rx) = channel::<String>();
        let (rep_tx, rx) = channel::<Option<String>>();
        let closing = Arc::new(AtomicBool::new(false));
        let stop = closing.clone();
        let thread = std::thread::Builder::new().name("settings-pipe".into()).spawn(move || {
            while let Ok(line) = req_rx.recv() {
                let reply = exchange(&mut w, &mut r, &line, &stop);
                let failed = reply.is_none();
                if rep_tx.send(reply).is_err() || failed {
                    return;
                }
            }
        });
        match thread {
            Ok(thread) => Self::with_link(Link { tx: Some(tx), rx, closing, thread: Some(thread) }),
            Err(_) => Self::offline(),
        }
    }

    fn with_link(link: Link) -> Self {
        Self { link: Some(link), ..Self::offline() }
    }

    pub(crate) fn offline() -> Self {
        Self { link: None, hotkeys: Vec::new(), widgets: None }
    }

    pub fn online(&self) -> bool {
        self.link.is_some()
    }

    fn call(&mut self, v: serde_json::Value) -> Option<serde_json::Value> {
        let link = self.link.as_ref()?;
        let sent = link.tx.as_ref().is_some_and(|tx| tx.send(v.to_string()).is_ok());
        let reply = if sent {
            match link.rx.recv_timeout(CALL_DEADLINE) {
                Ok(r) => r,
                Err(RecvTimeoutError::Timeout) => {
                    crate::log!("settings: the dock did not answer within {} s; working offline", CALL_DEADLINE.as_secs());
                    None
                }
                Err(RecvTimeoutError::Disconnected) => None,
            }
        } else {
            None
        };
        let Some(line) = reply else {
            // The dock went away (or stopped answering): carry on offline.
            // Dropping the link ends the pipe thread (see Link's Drop).
            self.link = None;
            return None;
        };
        let v: Option<serde_json::Value> = serde_json::from_str(&line).ok();
        if let Some(h) = v.as_ref().and_then(|v| v.get("hotkeys")) {
            self.hotkeys = crate::hotkeys::parse(h);
        }
        if let Some(w) = v.as_ref().and_then(|v| v.get("widgets")) {
            self.widgets = crate::desklayer::load::Status::from_json(w);
        }
        v
    }
    fn ok(reply: Option<serde_json::Value>) -> bool {
        reply.and_then(|r| r.get("type").and_then(|t| t.as_str()).map(|t| t == "Ok")).unwrap_or(false)
    }

    /// The dock's live config, or the file's.
    pub fn state(&mut self) -> Config {
        let live = self
            .call(serde_json::json!({"type": "GetState"}))
            .and_then(|r| r.get("config").cloned())
            .and_then(|c| serde_json::from_value::<Config>(c).ok());
        live.unwrap_or_else(Config::load)
    }

    pub fn preview(&mut self, cfg: &Config, keys: &[String]) -> bool {
        Self::ok(self.call(serde_json::json!({"type": "Preview", "config": cfg, "keys": keys})))
    }

    /// Asks the dock to quit (it answers first, then exits).
    pub fn new_note(&mut self) -> bool {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(windows::Win32::UI::WindowsAndMessaging::ASFW_ANY);
        }
        Self::ok(self.call(serde_json::json!({"type": "NewNote"})))
    }

    pub fn quit(&mut self) {
        let _ = self.call(serde_json::json!({"type": "Quit"}));
    }

    /// Asks the dock to install the update it staged now (`check`: look for
    /// a newer one first). False: no dock, or it said no.
    pub fn install_update(&mut self, check: bool) -> bool {
        self.online() && Self::ok(self.call(serde_json::json!({"type": "InstallUpdate", "check": check})))
    }

    /// Asks the dock to pause (`on`) or resume its own shortcuts while a
    /// shortcut is recorded here. False: not online (nothing paused), or
    /// the dock did not say Ok.
    pub fn suspend_keys(&mut self, on: bool) -> bool {
        self.online() && Self::ok(self.call(serde_json::json!({"type": "SuspendKeys", "on": on})))
    }

    /// Shows each display's number on it (Settings > Dock > Identify): by
    /// the dock, or from this window when no dock is running.
    pub fn identify(&mut self) {
        if !(self.online() && Self::ok(self.call(serde_json::json!({"type": "Identify"})))) {
            crate::displays::identify();
        }
    }

    /// Saves the edited fields (`keys`) of `cfg`: through the dock, or
    /// straight into the file when no dock is running. Ok(false) = saved,
    /// but "start with Windows" could not be changed.
    pub fn commit(&mut self, cfg: &Config, keys: &[String]) -> Result<bool, String> {
        if self.online() {
            let r = self.call(serde_json::json!({"type": "Commit", "config": cfg, "keys": keys})).ok_or("the dock did not answer")?;
            if r.get("type").and_then(|t| t.as_str()) != Some("Ok") {
                return Err(r.get("msg").and_then(|m| m.as_str()).unwrap_or("?").to_string());
            }
            return Ok(r.get("startup_ok").and_then(|b| b.as_bool()).unwrap_or(true));
        }
        // Offline: edit the file as it is now; refuse a newer or unreadable one.
        // Under the dock's write lock: read, change and save as one step.
        let _lock = crate::config::WriteLock::take().ok_or("the settings file is busy")?;
        let mut latest = if crate::config::file().exists() {
            match crate::config::read_current() {
                Some(c) if c.writable() => c,
                _ => return Err("the settings file cannot be changed (newer version or unreadable)".into()),
            }
        } else {
            Config::default()
        };
        let keys: Vec<String> = keys.to_vec();
        model::apply_keys(cfg, &mut latest, &keys);
        latest.save()?;
        if keys.iter().any(|k| k == "start_with_windows") {
            return Ok(super::set_run_at_login(latest.start_with_windows));
        }
        Ok(true)
    }

    /// Instant apply: saves the edited fields (`touched`) of `cfg` and,
    /// once they are on disk, forgets them. A dock that stopped answering
    /// mid-save is not waited for: the file is written directly instead
    /// (the link dropped, so the retry goes offline). An error answer keeps
    /// `touched` for the next try: nothing is lost.
    pub fn save(&mut self, cfg: &Config, touched: &mut Vec<String>) -> Result<bool, String> {
        if touched.is_empty() {
            return Ok(true);
        }
        let was_online = self.online();
        let mut r = self.commit(cfg, touched);
        if r.is_err() && retry_offline(was_online, self.online()) {
            r = self.commit(cfg, touched);
        }
        if r.is_ok() {
            touched.clear();
        }
        r
    }
}

/// Pure: a save that failed is tried again offline only when the dock
/// went away during it (not when it answered with an error).
fn retry_offline(was_online: bool, online: bool) -> bool {
    was_online && !online
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client on a scripted dock: each request line is passed on to the
    /// returned receiver and answered with the next of `replies` in order
    /// (None: no answer, the link drops).
    fn scripted(replies: Vec<Option<serde_json::Value>>) -> (Client, Receiver<String>) {
        let (tx, req_rx) = channel::<String>();
        let (rep_tx, rx) = channel::<Option<String>>();
        let (seen_tx, seen) = channel::<String>();
        let mut replies = replies.into_iter();
        std::thread::spawn(move || {
            while let Ok(line) = req_rx.recv() {
                let _ = seen_tx.send(line);
                let next = replies.next().flatten().map(|v| v.to_string() + "\n");
                if rep_tx.send(next).is_err() {
                    return;
                }
            }
        });
        let link = Link { tx: Some(tx), rx, closing: Arc::new(AtomicBool::new(false)), thread: None };
        (Client::with_link(link), seen)
    }

    /// Instant apply: a change is committed and forgotten once the dock
    /// says it is on disk; an error answer keeps it for the next try.
    #[test]
    fn instant_apply_saves_on_change_and_keeps_what_failed() {
        let (mut c, seen) = scripted(vec![Some(serde_json::json!({"type": "Ok", "startup_ok": true})), Some(serde_json::json!({"type": "Error", "msg": "save failed: disk"}))]);
        let mut cfg = Config::default();
        cfg.dock.autohide = !cfg.dock.autohide;
        let mut touched = vec!["dock.autohide".to_string()];
        assert_eq!(c.save(&cfg, &mut touched), Ok(true));
        let line = seen.recv().unwrap();
        assert!(line.contains("\"Commit\"") && line.contains("dock.autohide"), "{line}");
        assert!(touched.is_empty(), "saved: nothing left to save on close");
        // Nothing touched: nothing sent.
        assert_eq!(c.save(&cfg, &mut touched), Ok(true));
        assert!(seen.try_recv().is_err());
        // The dock refused: kept (and not written behind its back).
        touched.push("tray_icon".into());
        assert!(c.save(&cfg, &mut touched).is_err());
        assert_eq!(touched, vec!["tray_icon".to_string()]);
        assert!(c.online());
    }

    #[test]
    fn only_a_dock_gone_mid_save_is_retried_offline() {
        assert!(retry_offline(true, false));
        assert!(!retry_offline(true, true), "an error answer is not retried");
        assert!(!retry_offline(false, false), "already offline: the file write itself failed");
    }

    #[test]
    fn answers_are_size_capped() {
        let dir = std::env::temp_dir().join(format!("nopbar-client-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (out, inp) = (dir.join("out.txt"), dir.join("in.txt"));
        let mut w = std::fs::File::create(&out).unwrap();
        let open = AtomicBool::new(false);
        // A normal answer.
        std::fs::write(&inp, "{\"type\":\"Ok\"}\n").unwrap();
        let mut r = BufReader::new(std::fs::File::open(&inp).unwrap());
        assert_eq!(exchange(&mut w, &mut r, "x", &open).as_deref(), Some("{\"type\":\"Ok\"}\n"));
        // Too long, or cut off before its end: refused.
        std::fs::write(&inp, "a".repeat(MAX_REPLY as usize + 10) + "\n").unwrap();
        let mut r = BufReader::new(std::fs::File::open(&inp).unwrap());
        assert_eq!(exchange(&mut w, &mut r, "x", &open), None);
        std::fs::write(&inp, "{\"type\"").unwrap();
        let mut r = BufReader::new(std::fs::File::open(&inp).unwrap());
        assert_eq!(exchange(&mut w, &mut r, "x", &open), None);
        // Closing: nothing more is written or read.
        std::fs::write(&inp, "{\"type\":\"Ok\"}\n").unwrap();
        let mut r = BufReader::new(std::fs::File::open(&inp).unwrap());
        assert_eq!(exchange(&mut w, &mut r, "x", &AtomicBool::new(true)), None);
        drop(w);
        let _ = std::fs::remove_dir_all(dir);
    }
}