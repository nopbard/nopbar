//! Brightness: WMI for the PC's own panel, DDC/CI through bright.rs.

use super::*;

// ---- brightness (WMI root\wmi for the PC's own panel; DDC/CI: bright.rs) ----------

pub(super) mod wmi {
    use windows::core::{w, Interface, BSTR, HRESULT, PCWSTR};
    use windows::Win32::System::Com::{CoCreateInstance, CoSetProxyBlanket, CLSCTX_INPROC_SERVER, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::System::Wmi::*;

    /// How long one WMI call may take.
    const MS: i32 = 5000;
    /// WBEM_S_TIMEDOUT: a success code, so it needs checking by hand.
    const TIMED_OUT: i32 = 0x40004;
    /// WBEM_E_NOT_SUPPORTED / WBEM_E_INVALID_CLASS: no brightness here.
    fn unsupported(hr: HRESULT) -> bool {
        matches!(hr.0 as u32, 0x8004_100C | 0x8004_1010)
    }

    pub type Res<T> = std::result::Result<T, String>;

    pub struct Wmi {
        svc: IWbemServices,
        /// WmiMonitorBrightnessMethods instance path and the
        /// WmiSetBrightness in-parameters (fetched on first use).
        setter: Option<(BSTR, IWbemClassObject)>,
    }

    /// Waits (bounded) for a semisynchronous call; Err on timeout or failure.
    fn finish(cr: &IWbemCallResult, what: &str) -> Res<()> {
        let mut status = 0i32;
        let hr = unsafe { (Interface::vtable(cr).GetCallStatus)(Interface::as_raw(cr), MS, &mut status) };
        if hr.0 == TIMED_OUT {
            return Err(format!("{what}: no answer in {MS} ms"));
        }
        if hr.is_err() {
            return Err(format!("{what}: {hr:?}"));
        }
        if status != 0 {
            return Err(format!("{what}: failed 0x{:08X}", status as u32));
        }
        Ok(())
    }

    impl Wmi {
        pub fn connect() -> Res<Self> {
            unsafe {
                let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("locator: {e}"))?;
                let empty = BSTR::new();
                // USE_MAX_WAIT: the connection gives up instead of hanging.
                let svc = loc
                    .ConnectServer(&BSTR::from("ROOT\\WMI"), &empty, &empty, &empty, WBEM_FLAG_CONNECT_USE_MAX_WAIT.0, &empty, None)
                    .map_err(|e| format!("connect: {e}"))?;
                // RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE.
                CoSetProxyBlanket(&svc, 10, 0, PCWSTR::null(), RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE).map_err(|e| format!("proxy: {e}"))?;
                Ok(Self { svc, setter: None })
            }
        }

        /// First object of a WQL query; Ok(None) if there is none or the
        /// class is not supported here.
        fn first(&self, query: &str) -> Res<Option<IWbemClassObject>> {
            unsafe {
                let e = match self.svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from(query), WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY, None) {
                    Ok(e) => e,
                    Err(e) if unsupported(e.code()) => return Ok(None),
                    Err(e) => return Err(format!("query: {e}")),
                };
                let mut objs = [None];
                let mut n = 0u32;
                let hr = e.Next(MS, &mut objs, &mut n);
                if hr.0 == TIMED_OUT {
                    return Err(format!("query: no answer in {MS} ms"));
                }
                if hr.is_err() {
                    return if unsupported(hr) { Ok(None) } else { Err(format!("query: {hr:?}")) };
                }
                Ok(if n == 0 { None } else { objs[0].take() })
            }
        }

        /// Built-in panel brightness (0..100); Ok(None) if unsupported.
        pub fn read(&self) -> Res<Option<u8>> {
            let Some(obj) = self.first("SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active=TRUE")? else {
                return Ok(None);
            };
            let mut v = VARIANT::default();
            unsafe { obj.Get(w!("CurrentBrightness"), 0, &mut v, None, None).map_err(|e| format!("read: {e}"))? };
            let b = u32::try_from(&v).map_err(|e| format!("read: {e}"))?;
            Ok(Some(b.min(100) as u8))
        }

        /// WmiSetBrightness, checked: the call's status and, if the method
        /// reports one, its ReturnValue.
        pub fn set(&mut self, level: u8) -> Res<()> {
            if self.setter.is_none() {
                self.setter = Some(self.prepare()?);
            }
            let Some((path, params)) = &self.setter else { return Err("no setter".into()) };
            unsafe {
                params.Put(w!("Timeout"), 0, &VARIANT::from(1i32), 0).map_err(|e| format!("Timeout: {e}"))?;
                params.Put(w!("Brightness"), 0, &VARIANT::from(level.min(100)), 0).map_err(|e| format!("Brightness: {e}"))?;
                let mut cr = None;
                self.svc
                    .ExecMethod(path, &BSTR::from("WmiSetBrightness"), WBEM_FLAG_RETURN_IMMEDIATELY, None, params, None, Some(&mut cr))
                    .map_err(|e| format!("WmiSetBrightness: {e}"))?;
                let cr = cr.ok_or("WmiSetBrightness: no call result")?;
                finish(&cr, "WmiSetBrightness")?;
                if let Ok(out) = cr.GetResultObject(0) {
                    let mut v = VARIANT::default();
                    if out.Get(w!("ReturnValue"), 0, &mut v, None, None).is_ok() && !v.is_empty() {
                        let rv = u32::try_from(&v).unwrap_or(0);
                        if rv != 0 {
                            return Err(format!("WmiSetBrightness returned {rv}"));
                        }
                    }
                }
                Ok(())
            }
        }

        fn prepare(&self) -> Res<(BSTR, IWbemClassObject)> {
            let inst = self.first("SELECT __PATH FROM WmiMonitorBrightnessMethods WHERE Active=TRUE")?.ok_or("no WmiMonitorBrightnessMethods")?;
            unsafe {
                let mut v = VARIANT::default();
                inst.Get(w!("__PATH"), 0, &mut v, None, None).map_err(|e| format!("__PATH: {e}"))?;
                let path = BSTR::try_from(&v).map_err(|e| format!("__PATH: {e}"))?;
                let mut cr = None;
                self.svc
                    .GetObject(&BSTR::from("WmiMonitorBrightnessMethods"), WBEM_FLAG_RETURN_IMMEDIATELY, None, None, Some(&mut cr))
                    .map_err(|e| format!("class: {e}"))?;
                let cr = cr.ok_or("class: no call result")?;
                finish(&cr, "class")?;
                let class = cr.GetResultObject(0).map_err(|e| format!("class: {e}"))?;
                let mut sig = None;
                class.GetMethod(w!("WmiSetBrightness"), 0, &mut sig, std::ptr::null_mut()).map_err(|e| format!("method: {e}"))?;
                let params = sig.ok_or("method: no parameters")?.SpawnInstance(0).map_err(|e| format!("parameters: {e}"))?;
                Ok((path, params))
            }
        }
    }
}

/// The PC's own panel through WMI, as one of a display's screens.
impl bright::Dimmer for wmi::Wmi {
    fn read(&mut self) -> bright::Res<u8> {
        wmi::Wmi::read(self)?.ok_or_else(|| "no WMI brightness here".to_string())
    }
    fn write(&mut self, level: u8) -> bright::Res<()> {
        self.set(level)
    }
    fn ddc(&self) -> bool {
        false
    }
    fn name(&self) -> String {
        "built-in panel (WMI)".into()
    }
}

/// The brightness worker: finds how to dim the panel's display (bright::
/// plan: WMI for the PC's own panel, DDC/CI for another monitor), reports
/// the level (Some(None): nothing to dim, no slider), then writes the
/// panel's levels at the screens' pace (bright::Pace), the latest only,
/// and reads back what they did.
pub(super) fn bright_worker(shared: &Shared, rx: Receiver<BrightCmd>) {
    let mon = shared.monitor.load(Ordering::SeqCst);
    let place = bright::key_of(mon);
    // Not sure which screens these are: nothing is touched (no slider).
    let screens_now = if mon == 0 || place.is_empty() { None } else { bright::screens_of(mon) };
    let key = screens_now.as_ref().map(|sc| bright::display_key(&place, sc)).unwrap_or_default();
    let refused = bright::CACHE.lock().map(|c| c.ddc_refused(&key, Instant::now())).unwrap_or(false);
    let plan = bright::plan(screens_now.as_ref().filter(|_| !key.is_empty()), refused);
    let mut screens: Vec<Box<dyn bright::Dimmer>> = Vec::new();
    if plan.wmi {
        match wmi::Wmi::connect() {
            Ok(w) => screens.push(Box::new(w)),
            Err(e) => crate::log!("control: brightness: WMI error: {e}"),
        }
    }
    if plan.ddc && mon != 0 && !shared.quitting() {
        screens.extend(bright::ddc_screens(mon));
    }
    let (mut group, level) = bright::Group::open(screens, || shared.quitting());
    if shared.quitting() {
        return;
    }
    if group.is_empty() {
        crate::log!("control: brightness: nothing to dim on this display ({plan:?})");
    }
    if let Ok(mut c) = bright::CACHE.lock() {
        c.opened(&place, &key, plan, level, group.has_ddc(), Instant::now());
    }
    let mut pace = bright::Pace::new(if group.has_ddc() { bright::DDC_GAP } else { Duration::ZERO });
    // Levels the panel sent meanwhile (the slider shows at once from the
    // cache): the drag wins over what was read.
    let mut quit = drain_bright(&rx, &mut pace);
    if group.is_empty() || !pace.pending() {
        shared.update(|r| r.brightness = Some(level));
    }
    // The last level the screens confirmed (shown after a failure).
    let mut confirmed = level;
    while !quit && !shared.quitting() && !group.is_empty() {
        let now = Instant::now();
        let wait = match pace.next(now) {
            bright::Step::Write(v) => {
                // A closed panel writes no further screen.
                if !group.write(v, || shared.quitting()) && !shared.quitting() {
                    crate::log!("control: brightness {v} not set");
                }
                pace.wrote(Instant::now());
                None
            }
            bright::Step::Confirm => {
                if let Some(v) = group.read() {
                    confirmed = Some(v);
                    if let Ok(mut c) = bright::CACHE.lock() {
                        c.set_level(&key, v);
                    }
                }
                pace.confirmed(Instant::now());
                // A newer level came in while reading: the knob follows
                // the pointer, so the result shows once the user stops.
                quit |= drain_bright(&rx, &mut pace);
                if !pace.pending() {
                    shared.update(|r| r.brightness = Some(confirmed));
                }
                None
            }
            bright::Step::Wait(t) => Some(Some(t.saturating_duration_since(now))),
            bright::Step::Idle => Some(None),
        };
        let cmd = match wait {
            None => continue,
            Some(Some(d)) => match rx.recv_timeout(d) {
                Ok(c) => Some(c),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            },
            Some(None) => match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            },
        };
        match cmd {
            Some(BrightCmd::Set(v)) => pace.push(v),
            Some(BrightCmd::Quit) => break,
            None => {}
        }
        quit |= drain_bright(&rx, &mut pace);
    }
}

/// Takes the levels waiting (a drag sends many: only the latest counts);
/// true if the panel said Quit.
pub(super) fn drain_bright(rx: &Receiver<BrightCmd>, pace: &mut bright::Pace) -> bool {
    let mut quit = false;
    while let Ok(c) = rx.try_recv() {
        match c {
            BrightCmd::Set(v) => pace.push(v),
            BrightCmd::Quit => quit = true,
        }
    }
    quit
}
