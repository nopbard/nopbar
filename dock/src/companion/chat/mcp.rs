//! Presentation queue in the running dock. The standalone shim owns stdio.
pub use nopbar_mcp::{run, Output};
#[cfg(test)]
use nopbar_mcp::{schema, Server, Sink};
use serde_json::{json, Value};
use std::{sync::Mutex, time::Instant};
#[derive(Default)]
pub struct Queue {
    pending: Option<Output>,
    last: Option<u64>,
}
impl Queue {
    pub fn push(&mut self, o: Output) {
        self.pending = Some(o);
    }
    pub fn take(&mut self, now_ms: u64, blocked: bool) -> Option<Output> {
        if blocked || self.last.is_some_and(|t| now_ms.saturating_sub(t) < 5000) {
            return None;
        }
        let o = self.pending.take()?;
        self.last = Some(now_ms);
        Some(o)
    }
    fn next_delay(&self, now: u64, blocked: bool) -> Option<u32> {
        self.pending.as_ref()?;
        Some(if blocked {
            250
        } else {
            self.last.map_or(1, |last| {
                5000u64.saturating_sub(now.saturating_sub(last)).max(1)
            }) as u32
        })
    }
}
static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    pending: None,
    last: None,
});
static REACTION: Mutex<Option<(String, Instant)>> = Mutex::new(None);
pub const TIMER: usize = 0x434b;
pub const WAKE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 183;
pub fn wake() {
    let owner = super::super::OWNER.load(std::sync::atomic::Ordering::SeqCst);
    if owner != 0 {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(windows::Win32::Foundation::HWND(owner as *mut _)),
                WAKE,
                Default::default(),
                Default::default(),
            );
        }
    }
}
pub fn ingest(v: &Value) {
    if let Ok(o) = serde_json::from_value::<Output>(v["output"].clone()) {
        // Revalidate at the trust boundary, including callers bypassing stdio.
        let (name, args) = match o {
            Output::Say { text, mood } => {
                let mut a = json!({"text":text});
                if let Some(m) = mood {
                    a["mood"] = m.into();
                }
                ("say", a)
            }
            Output::Status { state, label } => {
                let mut a = json!({"state":state});
                if let Some(l) = label {
                    a["label"] = l.into();
                }
                ("status", a)
            }
        };
        if let Ok(o) = Output::parse(name, &args) {
            QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push(o);
            wake();
        }
    }
}
pub fn poll() {
    super::poll_history();
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{KillTimer, SetTimer},
    };
    let owner = HWND(super::super::OWNER.load(std::sync::atomic::Ordering::SeqCst) as *mut _);
    unsafe {
        let _ = KillTimer(Some(owner), TIMER);
    }
    super::hint::poll();
    super::super::refresh();
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let now = START.get_or_init(Instant::now).elapsed().as_millis() as u64;
    let blocked = super::is_open()
        || !super::super::visible()
        || crate::dock::companion_mode() != super::super::visibility::Mode::Run
        || super::super::living::away()
        || super::super::APP.with(|a| {
            a.try_borrow().ok().is_some_and(|a| {
                a.as_ref()
                    .is_some_and(|a| a.ground.is_some_and(|g| g.covered))
            })
        });
    let o = QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take(now, blocked);
    if let Some(o) = o {
        *REACTION.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((o.clip().into(), Instant::now()));
        match o {
            Output::Say { text, .. } => super::say(text),
            Output::Status {
                label: Some(label), ..
            } if !label.is_empty() => super::say(label),
            _ => {}
        }
        super::super::refresh();
    }
    let queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(wait) = queue.next_delay(now, blocked).or_else(|| super::hint::waiting_to_show().then_some(500)) {
        unsafe {
            SetTimer(Some(owner), TIMER, wait, None);
        }
    }
}
pub fn frame() -> Option<String> {
    let r = REACTION.lock().ok()?;
    let (clip, at) = r.as_ref()?;
    let t = at.elapsed().as_secs_f32();
    if t >= 6. {
        return None;
    }
    let c = super::super::clips::catalog().clip(clip);
    Some(c.frame(t.min(c.duration())).into())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        seen: Vec<Output>,
        offline: bool,
    }
    impl Sink for Fake {
        fn send(&mut self, o: &Output) -> Result<(), &'static str> {
            self.seen.push(o.clone());
            if self.offline {
                Err("NopBar가 꺼져 있음")
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn lifecycle_schema_unicode_and_offline() {
        let mut s = Server::default();
        let mut sink = Fake {
            seen: vec![],
            offline: false,
        };
        assert!(s
            .handle(
                json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
                &mut sink
            )
            .unwrap()
            .get("error")
            .is_some());
        let r=s.handle(json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),&mut sink).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
        assert!(s
            .handle(
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                &mut sink
            )
            .is_none());
        assert_eq!(schema()["tools"].as_array().unwrap().len(), 2);
        let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"say","arguments":{"text":"안🧙".repeat(100),"mood":"happy"}}});
        assert_eq!(
            s.handle(call.clone(), &mut sink).unwrap()["result"]["isError"],
            false
        );
        if let Output::Say { text, .. } = &sink.seen[0] {
            assert_eq!(text.chars().count(), 140);
        } else {
            panic!();
        }
        sink.offline = true;
        let r = s.handle(call, &mut sink).unwrap();
        assert_eq!(r["result"]["content"][0]["text"], "NopBar가 꺼져 있음");
        assert_eq!(r["result"]["isError"], true);
    }
    #[test]
    fn global_queue_keeps_only_latest_and_waits_for_all_suppression() {
        let output = |s: &str| Output::Say {
            text: s.into(),
            mood: None,
        };
        let mut q = Queue::default();
        assert_eq!(q.next_delay(0, false), None);
        q.push(output("first"));
        assert_eq!(q.take(0, false), Some(output("first")));
        q.push(output("second"));
        q.push(output("last"));
        assert_eq!(q.next_delay(1000, false), Some(4000));
        assert_eq!(q.next_delay(1000, true), Some(250));
        assert!(q.take(4999, false).is_none());
        assert!(q.take(5000, true).is_none());
        assert_eq!(q.take(6000, false), Some(output("last")));
        assert!(q.take(12000, false).is_none());
        assert_eq!(
            q.next_delay(12000, true),
            None,
            "idle queue never arms a timer"
        );
    }
}
