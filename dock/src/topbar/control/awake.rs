//! Keep Awake: the Control Center's tile that keeps the display on and the
//! PC from sleeping (what people install PowerToys Awake or Caffeine for).
//!
//! One power request for the process (PowerCreateRequest), set for the
//! display and the system while on (PowerRequestDisplayRequired +
//! PowerRequestSystemRequired), cleared when off. A request is a kernel
//! object held by its handle: not tied to a thread (unlike
//! SetThreadExecutionState), so the tile switches it from the UI thread in
//! a moment, and Windows drops it with the handle when the process ends,
//! however it ends (a crash too). A clean quit releases it first anyway.
//!
//! Off at every start, never saved (like Caffeine). Windows lists it as
//! NopBar's (`powercfg /requests`).
//!
//! Sleep the user asks for (Start's Sleep, the lid) still happens, and
//! Windows ends the request for it. Back from sleep, a Keep Awake the user
//! turned on is made again (a new request, the old handle closed): it is
//! never turned off behind their back; only if Windows will not take it
//! again does the tile go off, so it always says what is in force.
//!
//! The switching (Keeper) is pure over a stand-in request in the tests:
//! nothing in them touches the real power state.

use std::sync::Mutex;

pub type Res<T> = std::result::Result<T, String>;

/// The power request: on asks Windows to keep the display and the system
/// awake, off stops asking.
pub trait Request {
    fn set(&mut self, on: bool) -> Res<()>;
}

/// The tile's state and its request (made on first use).
pub struct Keeper<R> {
    req: Option<R>,
    on: bool,
}

impl<R: Request> Keeper<R> {
    pub const fn new() -> Self {
        Keeper { req: None, on: false }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    /// Switches to `on`, making the request with `make` the first time;
    /// the state after (as it was if Windows refused).
    pub fn switch(&mut self, on: bool, make: impl FnOnce() -> Res<R>) -> Res<bool> {
        if on == self.on {
            return Ok(on);
        }
        if self.req.is_none() {
            self.req = Some(make()?);
        }
        if let Some(r) = self.req.as_mut() {
            r.set(on)?;
        }
        self.on = on;
        Ok(on)
    }

    /// Back from sleep (which ended the request): while on, a new request
    /// replaces the old one; the state after (off if it could not be
    /// made, so the tile never says on for a request not in force).
    pub fn rearm(&mut self, make: impl FnOnce() -> Res<R>) -> Res<bool> {
        if !self.on {
            return Ok(false);
        }
        // The old one closed first (its handle with it).
        self.req = None;
        let made = make().and_then(|mut r| r.set(true).map(|()| r));
        match made {
            Ok(r) => {
                self.req = Some(r);
                Ok(true)
            }
            Err(e) => {
                self.on = false;
                Err(e)
            }
        }
    }

    /// Off, and the request gone (quitting).
    pub fn release(&mut self) {
        if self.on {
            if let Some(r) = self.req.as_mut() {
                let _ = r.set(false);
            }
        }
        self.req = None;
        self.on = false;
    }
}

/// The process's power request (its handle as isize: Send).
struct Power(isize);

impl Power {
    fn create() -> Res<Self> {
        use windows::core::{w, PWSTR};
        use windows::Win32::System::Power::PowerCreateRequest;
        use windows::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};
        let ctx = REASON_CONTEXT {
            // POWER_REQUEST_CONTEXT_VERSION.
            Version: 0,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(w!("NopBar: Keep Awake").as_ptr() as *mut _) },
        };
        unsafe { PowerCreateRequest(&ctx) }.map(|h| Power(h.0 as isize)).map_err(|e| format!("request: {e}"))
    }

    fn handle(&self) -> windows::Win32::Foundation::HANDLE {
        windows::Win32::Foundation::HANDLE(self.0 as *mut _)
    }
}

impl Request for Power {
    fn set(&mut self, on: bool) -> Res<()> {
        use windows::Win32::System::Power::{PowerClearRequest, PowerRequestDisplayRequired, PowerRequestSystemRequired, PowerSetRequest};
        let h = self.handle();
        unsafe {
            if on {
                PowerSetRequest(h, PowerRequestDisplayRequired).map_err(|e| format!("display: {e}"))?;
                if let Err(e) = PowerSetRequest(h, PowerRequestSystemRequired) {
                    let _ = PowerClearRequest(h, PowerRequestDisplayRequired);
                    return Err(format!("system: {e}"));
                }
                Ok(())
            } else {
                let d = PowerClearRequest(h, PowerRequestDisplayRequired);
                let s = PowerClearRequest(h, PowerRequestSystemRequired);
                d.and(s).map_err(|e| format!("clear: {e}"))
            }
        }
    }
}

impl Drop for Power {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle());
        }
    }
}

static KEEPER: Mutex<Keeper<Power>> = Mutex::new(Keeper::new());

/// Keep Awake is on.
pub fn on() -> bool {
    KEEPER.lock().map(|k| k.on()).unwrap_or(false)
}

/// Switches Keep Awake; the state after (unchanged when Windows refused).
pub fn set(on: bool) -> bool {
    if cfg!(test) { return on; }
    let Ok(mut k) = KEEPER.lock() else { return false };
    match k.switch(on, Power::create) {
        Ok(now) => {
            crate::log!("control: keep awake {}", if now { "on" } else { "off" });
            now
        }
        Err(e) => {
            crate::log!("control: keep awake -> {on} refused: {e}");
            k.on()
        }
    }
}

/// PBT_APMRESUMESUSPEND / PBT_APMRESUMEAUTOMATIC: back from sleep.
const RESUMED: [usize; 2] = [0x7, 0x12];

/// WM_POWERBROADCAST: back from sleep, Keep Awake (if on) is made again.
/// Every top bar hears it; making it again twice is harmless.
pub fn on_power(event: usize) {
    if cfg!(test) || !RESUMED.contains(&event) {
        return;
    }
    let Ok(mut k) = KEEPER.lock() else { return };
    match k.rearm(Power::create) {
        Ok(true) => crate::log!("control: keep awake made again after sleep"),
        Ok(false) => {}
        Err(e) => crate::log!("control: keep awake lost in sleep, now off: {e}"),
    }
}

/// Quitting: the request goes (Windows would drop it with the process).
pub fn release() {
    if cfg!(test) { return; }
    if let Ok(mut k) = KEEPER.lock() {
        k.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A stand-in request: logs what it is asked, refuses when told to.
    struct Fake {
        log: Rc<RefCell<Vec<bool>>>,
        refuse: Rc<RefCell<bool>>,
    }

    impl Request for Fake {
        fn set(&mut self, on: bool) -> Res<()> {
            if *self.refuse.borrow() {
                return Err("refused".into());
            }
            self.log.borrow_mut().push(on);
            Ok(())
        }
    }

    #[test]
    fn keep_awake_switches_and_lets_go() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let refuse = Rc::new(RefCell::new(false));
        let made = Rc::new(RefCell::new(0));
        let make = || {
            *made.borrow_mut() += 1;
            Ok(Fake { log: log.clone(), refuse: refuse.clone() })
        };
        let mut k = Keeper::<Fake>::new();
        // Off at start; off again asks nothing (no request made).
        assert!(!k.on());
        assert_eq!(k.switch(false, make), Ok(false));
        assert_eq!(*made.borrow(), 0);
        // On, then off: one request, set and cleared.
        assert_eq!(k.switch(true, make), Ok(true));
        assert!(k.on());
        assert_eq!(k.switch(true, make), Ok(true), "already on: nothing asked");
        assert_eq!(k.switch(false, make), Ok(false));
        assert_eq!(*log.borrow(), vec![true, false]);
        assert_eq!(*made.borrow(), 1, "the request is made once");
        // Windows refuses: the tile stays as it was.
        *refuse.borrow_mut() = true;
        assert!(k.switch(true, make).is_err());
        assert!(!k.on());
        *refuse.borrow_mut() = false;
        // Quitting while on: cleared and dropped.
        k.switch(true, make).unwrap();
        k.release();
        assert!(!k.on());
        assert_eq!(log.borrow().last(), Some(&false));
        assert!(k.req.is_none());
        // No request could be made: still off.
        let mut k = Keeper::<Fake>::new();
        assert!(k.switch(true, || Err("no request".into())).is_err());
        assert!(!k.on());
    }

    /// A stand-in request that says when it is let go.
    struct Owned {
        id: u32,
        set: Rc<RefCell<Vec<(u32, bool)>>>,
        closed: Rc<RefCell<Vec<u32>>>,
    }

    impl Request for Owned {
        fn set(&mut self, on: bool) -> Res<()> {
            self.set.borrow_mut().push((self.id, on));
            Ok(())
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            self.closed.borrow_mut().push(self.id);
        }
    }

    #[test]
    fn back_from_sleep_keep_awake_is_what_the_tile_says() {
        let set = Rc::new(RefCell::new(Vec::new()));
        let closed = Rc::new(RefCell::new(Vec::new()));
        let next = Rc::new(RefCell::new(0u32));
        let make = || {
            *next.borrow_mut() += 1;
            Ok(Owned { id: *next.borrow(), set: set.clone(), closed: closed.clone() })
        };
        let mut k = Keeper::<Owned>::new();
        // Off: sleep changes nothing, nothing is made.
        assert_eq!(k.rearm(make), Ok(false));
        assert_eq!(*next.borrow(), 0);
        // On, then sleep (Windows ended request 1): request 2 replaces it,
        // set again; the tile stays on, as the user left it.
        k.switch(true, make).unwrap();
        assert_eq!(k.rearm(make), Ok(true));
        assert!(k.on());
        assert_eq!(*set.borrow(), vec![(1, true), (2, true)]);
        assert_eq!(*closed.borrow(), vec![1], "the old handle is closed");
        // Windows will not take it again: the tile goes off with it, and
        // nothing is left open.
        assert!(k.rearm(|| Err("refused".into())).is_err());
        assert!(!k.on());
        assert_eq!(*closed.borrow(), vec![1, 2]);
        assert!(k.req.is_none());
        // Turned on again later: works as ever.
        assert_eq!(k.switch(true, make), Ok(true));
        assert!(k.on());
        assert!(RESUMED.contains(&0x7) && RESUMED.contains(&0x12) && !RESUMED.contains(&0x4));
    }
}
