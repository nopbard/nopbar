//! No sleep while an agent works (Settings > Agents, on by default).
//!
//! Power requests of NopBar's own for the system and display, set while
//! any session works and let go LINGER after the last stops working
//! (finished or waiting for its person: someone coming back to answer
//! within a few minutes finds the PC as they left it). Like Keep Awake's
//! (topbar/control/awake.rs), a request is a kernel object held by its
//! handle, not by a thread (unlike SetThreadExecutionState): Windows drops
//! it with the process however it ends, and `powercfg /requests` lists it
//! as NopBar's. Sleep the person asks for still happens.
//!
//! The decisions (Hold) are pure; the tests touch no power state.

use std::sync::Mutex;
use std::time::Duration;

/// How long the request stays after the last session stopped working.
pub const LINGER: Duration = Duration::from_secs(10 * 60);

/// What to do with the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Set,
    /// Let go after LINGER (the generation the timer must still find).
    LetGoLater(u64),
    LetGo,
    Nothing,
}

/// The request's state: held, and the generation of the pending let-go
/// (a newer change makes an older timer do nothing).
#[derive(Default, Debug)]
pub struct Hold {
    pub held: bool,
    pending: Option<u64>,
    gen: u64,
}

impl Hold {
    pub const fn new() -> Hold {
        Hold { held: false, pending: None, gen: 0 }
    }

    /// Pure: what `working` (any session works) under the switch `on`
    /// asks for.
    pub fn update(&mut self, working: bool, on: bool) -> Act {
        if !on {
            self.pending = None;
            return if std::mem::take(&mut self.held) { Act::LetGo } else { Act::Nothing };
        }
        if working {
            self.pending = None;
            if self.held {
                return Act::Nothing;
            }
            self.held = true;
            return Act::Set;
        }
        if !self.held || self.pending.is_some() {
            return Act::Nothing;
        }
        self.gen += 1;
        self.pending = Some(self.gen);
        Act::LetGoLater(self.gen)
    }

    /// Pure: LINGER is over for generation `gen`: let go if that is still
    /// what was asked.
    pub fn due(&mut self, gen: u64) -> bool {
        if self.pending == Some(gen) && self.held {
            self.pending = None;
            self.held = false;
            true
        } else {
            false
        }
    }
}

struct Req(isize);

impl Req {
    fn create() -> Option<Req> {
        use windows::core::{w, PWSTR};
        use windows::Win32::System::Power::{PowerCreateRequest, PowerRequestDisplayRequired, PowerRequestSystemRequired, PowerSetRequest};
        use windows::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};
        let ctx = REASON_CONTEXT {
            Version: 0,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(w!("NopBar: an AI agent is working").as_ptr() as *mut _) },
        };
        unsafe {
            let h = PowerCreateRequest(&ctx).ok()?;
            let r = Req(h.0 as isize);
            PowerSetRequest(h, PowerRequestSystemRequired).ok()?;
            PowerSetRequest(h, PowerRequestDisplayRequired).ok()?;
            Some(r)
        }
    }
}

impl Drop for Req {
    fn drop(&mut self) {
        unsafe {
            // Closing the handle ends the request.
            let _ = windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(self.0 as *mut _));
        }
    }
}

struct State {
    hold: Hold,
    req: Option<Req>,
}

static STATE: Mutex<State> = Mutex::new(State { hold: Hold::new(), req: None });

fn set(s: &mut State) {
    if s.req.is_none() {
        s.req = Req::create();
        crate::log!("agents: keeping the PC awake{}", if s.req.is_some() { "" } else { " refused by Windows" });
    }
}

fn let_go(s: &mut State) {
    if s.req.take().is_some() {
        crate::log!("agents: letting the PC sleep again");
    }
}

/// The board changed (or the switch did).
pub fn update(working: bool, on: bool) {
    let Ok(mut s) = STATE.lock() else { return };
    match s.hold.update(working, on) {
        Act::Set => set(&mut s),
        Act::LetGo => let_go(&mut s),
        Act::LetGoLater(gen) => {
            drop(s);
            crate::service::after("agents awake", LINGER, move || {
                if let Ok(mut s) = STATE.lock() {
                    if s.hold.due(gen) {
                        let_go(&mut s);
                    }
                }
            });
        }
        Act::Nothing => {}
    }
}

/// PBT_APMRESUMESUSPEND / PBT_APMRESUMEAUTOMATIC.
const RESUMED: [usize; 2] = [0x7, 0x12];

/// Back from sleep (which ended the request): made again while held.
pub fn on_power(event: usize) {
    if !RESUMED.contains(&event) {
        return;
    }
    if let Ok(mut s) = STATE.lock() {
        if s.hold.held {
            s.req = None;
            set(&mut s);
        }
    }
}

/// Quitting (Windows would drop it with the process anyway).
pub fn release() {
    if let Ok(mut s) = STATE.lock() {
        s.req = None;
        s.hold = Hold::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_while_working_and_let_go_later() {
        let mut h = Hold::new();
        assert_eq!(h.update(false, true), Act::Nothing, "nothing to hold yet");
        assert_eq!(h.update(true, true), Act::Set);
        assert_eq!(h.update(true, true), Act::Nothing);
        let Act::LetGoLater(g1) = h.update(false, true) else { panic!("let go later") };
        assert_eq!(h.update(false, true), Act::Nothing, "one timer at a time");
        // Working again before LINGER: the old timer does nothing.
        assert_eq!(h.update(true, true), Act::Nothing);
        assert!(!h.due(g1));
        assert!(h.held);
        let Act::LetGoLater(g2) = h.update(false, true) else { panic!() };
        assert!(h.due(g2));
        assert!(!h.held);
        assert!(!h.due(g2), "once");
    }

    #[test]
    fn the_switch_lets_go_at_once() {
        let mut h = Hold::new();
        h.update(true, true);
        assert_eq!(h.update(true, false), Act::LetGo);
        assert_eq!(h.update(true, false), Act::Nothing);
        assert_eq!(h.update(true, true), Act::Set);
    }
}
