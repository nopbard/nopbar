//! System switches, read and confirmed on a panel worker.
#![allow(non_snake_case)]

use super::*;
use std::ffi::c_void;
use windows::core::{interface, IUnknown, IUnknown_Vtbl, GUID, HRESULT, HSTRING};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::{
        Com::CLSCTX_LOCAL_SERVER,
        Registry::{
            RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
            HKEY_LOCAL_MACHINE, KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET,
        },
        Threading::{CreateEventW, WaitForSingleObject},
    },
};
use windows::UI::Notifications::{
    ToastNotificationManager, ToastNotificationManagerForUser, ToastNotificationMode,
};

#[interface("db3afbfb-08e6-46c6-aa70-bf9a34c30ab7")]
unsafe trait IRadioManager: IUnknown {
    fn IsRMSupported(&self, state: *mut u32) -> HRESULT;
    fn GetUIRadioInstances(&self, list: *mut *mut c_void) -> HRESULT;
    fn GetSystemRadioState(
        &self,
        radios_on: *mut BOOL,
        changeable: *mut BOOL,
        reason: *mut u32,
    ) -> HRESULT;
    fn SetSystemRadioState(&self, radios_on: BOOL) -> HRESULT;
}

const RADIO_CLSID: GUID = GUID::from_u128(0x581333f6_28db_41be_bc7a_ff201f12f3f6);
const RADIO_KEY: &str = r"SYSTEM\CurrentControlSet\Control\RadioManagement\SystemRadioState";
const REFRESH: Duration = Duration::from_secs(2);
const CHECK: Duration = Duration::from_millis(250);
const BROKER_SETTLE: Duration = Duration::from_millis(750);

fn switch_result(on: Option<bool>, target: bool, elapsed: Duration, settle: Duration, timeout: Duration) -> Option<bool> {
    let reached = on == Some(target) && elapsed >= settle;
    (reached || elapsed >= timeout).then_some(!reached)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Setting {
    Airplane,
    Night,
    Dnd,
}

impl Setting {
    const ALL: [Self; 3] = [Self::Airplane, Self::Night, Self::Dnd];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SettingState {
    pub(super) on: Option<bool>,
    pub(super) selected: Option<bool>,
    pub(super) writable: bool,
    pub(super) hidden: bool,
}

impl SettingState {
    fn available(on: bool) -> Self {
        Self {
            on: Some(on),
            writable: true,
            ..Default::default()
        }
    }

    pub(super) fn automatic_dnd(self) -> bool {
        self.on == Some(true) && self.selected == Some(false)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SystemTile {
    pub(super) state: SettingState,
    pub(super) ready: bool,
    pub(super) busy: bool,
    pub(super) failed: Option<Instant>,
}

pub(super) enum SystemCmd {
    Refresh(Setting),
    Set(Setting, bool),
    AccessRefresh,
    Access(Access, bool),
    Quit,
}

struct RegistryWatch(HKEY, HANDLE);

impl RegistryWatch {
    fn new(root: HKEY, path: &str) -> Option<Self> {
        let event = unsafe { CreateEventW(None, false, false, None) }.ok()?;
        let mut key = HKEY::default();
        if unsafe { RegOpenKeyExW(root, &HSTRING::from(path), None, KEY_NOTIFY, &mut key) }.is_err()
        {
            unsafe {
                let _ = CloseHandle(event);
            }
            return None;
        }
        let watch = Self(key, event);
        watch.arm().then_some(watch)
    }

    fn arm(&self) -> bool {
        unsafe {
            RegNotifyChangeKeyValue(
                self.0,
                false,
                REG_NOTIFY_CHANGE_LAST_SET,
                Some(self.1),
                true,
            )
        }
        .is_ok()
    }

    fn changed(slot: &mut Option<Self>) -> bool {
        let Some(watch) = slot else { return false };
        if unsafe { WaitForSingleObject(watch.1, 0) } != WAIT_OBJECT_0 {
            return false;
        }
        if !watch.arm() {
            *slot = None;
        }
        true
    }
}

impl Drop for RegistryWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
            let _ = CloseHandle(self.1);
        }
    }
}

fn airplane_manager() -> Option<IRadioManager> {
    let manager: IRadioManager =
        unsafe { CoCreateInstance(&RADIO_CLSID, None, CLSCTX_LOCAL_SERVER) }.ok()?;
    let mut supported = 0;
    unsafe { manager.IsRMSupported(&mut supported) }.ok().ok()?;
    (supported != 0).then_some(manager)
}

fn airplane_state(manager: &IRadioManager) -> Option<SettingState> {
    let (mut radios, mut changeable, mut reason) = (BOOL(0), BOOL(0), 0);
    unsafe { manager.GetSystemRadioState(&mut radios, &mut changeable, &mut reason) }
        .ok()
        .ok()?;
    Some(SettingState {
        on: Some(!radios.as_bool()),
        writable: changeable.as_bool(),
        ..Default::default()
    })
}

fn dnd_status(selected: Option<bool>, effective: Option<bool>) -> SettingState {
    let mut state = SettingState { on: effective.or(selected), selected, ..Default::default() };
    state.writable = selected.is_some() && !state.automatic_dnd();
    state
}

fn dnd_state(manager: Option<&ToastNotificationManagerForUser>) -> SettingState {
    use crate::focus_timer::quiet_hours::{Settings, OFF};
    let selected = Settings::new().and_then(|s| s.get()).ok().map(|p| p != OFF);
    let effective = manager.and_then(|m| m.NotificationMode().ok())
        .map(|m| m != ToastNotificationMode::Unrestricted);
    dnd_status(selected, effective)
}

pub(super) fn system_worker(shared: &Shared, rx: Receiver<SystemCmd>, tx: Sender<SystemCmd>) {
    if cfg!(test) {
        return;
    }
    let radio = airplane_manager();
    let toast = ToastNotificationManager::GetDefault().ok();
    let _toast_watch = toast.clone().and_then(|m| {
        let token = m
            .NotificationModeChanged(&TypedEventHandler::new(move |_, _| {
                let _ = tx.send(SystemCmd::Refresh(Setting::Dnd));
                Ok(())
            }))
            .ok()?;
        Some(Defer::new(move || {
            let _ = m.RemoveNotificationModeChanged(token);
        }))
    });
    let mut registry = [
        RegistryWatch::new(HKEY_LOCAL_MACHINE, RADIO_KEY),
        RegistryWatch::new(HKEY_CURRENT_USER, night_light::KEY),
    ];
    let paths = [
        r"Software\Microsoft\ColorFiltering",
        r"Software\Microsoft\Multimedia\Audio",
    ];
    let mut access_registry = paths.map(|path| RegistryWatch::new(HKEY_CURRENT_USER, path));
    let read = |setting| match setting {
        Setting::Airplane => radio.as_ref().and_then(airplane_state).unwrap_or_default(),
        Setting::Night => match night_light::state() {
            Some(Some(on)) => SettingState::available(on),
            Some(None) => SettingState {
                hidden: true,
                ..Default::default()
            },
            None => SettingState::default(),
        },
        Setting::Dnd => dnd_state(toast.as_ref()),
    };
    let publish = |setting, state| {
        shared.update(|r| {
            let tile = &mut r.system[setting as usize];
            tile.state = state;
            tile.ready = true;
        });
    };
    for setting in Setting::ALL {
        if shared.quitting() { return; }
        publish(setting, read(setting));
    }
    let mut pending: [Option<(bool, Instant)>; Setting::ALL.len()] = Default::default();
    let mut access_pending: [Option<(bool, Instant)>; Access::ALL.len()] = Default::default();
    let mut nexts = Instant::now();
    while !shared.quitting() {
        let mut dirty = [false; Setting::ALL.len()];
        let mut access_dirty = false;
        let cmd = rx.recv_timeout(CHECK);
        if shared.quitting() {
            break;
        }
        match cmd {
            Ok(SystemCmd::Quit) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(SystemCmd::Refresh(setting)) => dirty[setting as usize] = true,
            Ok(SystemCmd::AccessRefresh) => access_dirty = true,
            Ok(SystemCmd::Access(access, on)) => {
                let i = access as usize;
                if read_accessibility()[i] == Some(on) || set_accessibility(access, on) {
                    access_pending[i] = Some((on, Instant::now()));
                } else {
                    shared.update(|r| {
                        r.access[i].busy = false;
                        r.access[i].failed = Some(Instant::now());
                    });
                }
                access_dirty = true;
            }
            Ok(SystemCmd::Set(setting, on)) => {
                let accepted = match setting {
                    Setting::Airplane => radio
                        .as_ref()
                        .is_some_and(|m| unsafe { m.SetSystemRadioState(BOOL::from(!on)) }.is_ok()),
                    Setting::Night => night_light::set(on).is_some(),
                    Setting::Dnd => crate::focus_timer::set_dnd_manually(on)
                        .map_err(|e| crate::log!("control: DND switch: {e}"))
                        .is_ok(),
                };
                if accepted {
                    pending[setting as usize] = Some((on, Instant::now()));
                } else {
                    let state = read(setting);
                    shared.update(|r| {
                        r.system[setting as usize] = SystemTile {
                            state,
                            ready: true,
                            busy: false,
                            failed: Some(Instant::now()),
                        }
                    });
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        let refresh = now >= nexts;
        if refresh {
            nexts = now + REFRESH;
            if registry[0].is_none() {
                registry[0] = RegistryWatch::new(HKEY_LOCAL_MACHINE, RADIO_KEY);
            }
            if registry[1].is_none() {
                registry[1] = RegistryWatch::new(HKEY_CURRENT_USER, night_light::KEY);
            }
            for (slot, path) in access_registry.iter_mut().zip(paths) {
                if slot.is_none() {
                    *slot = RegistryWatch::new(HKEY_CURRENT_USER, path);
                }
            }
        }
        for (i, watch) in registry.iter_mut().enumerate() {
            dirty[i] |= RegistryWatch::changed(watch);
        }
        dirty[Setting::Airplane as usize] |= shared.airplane_refresh.swap(false, Ordering::AcqRel);
        for setting in Setting::ALL {
            let i = setting as usize;
            if !refresh && !dirty[i] && pending[i].is_none() {
                continue;
            }
            let state = read(setting);
            if let Some((target, started)) = pending[i] {
                let wait = if setting == Setting::Night { BROKER_SETTLE } else { Duration::ZERO };
                let confirmed = if setting == Setting::Dnd { state.selected } else { state.on };
                if let Some(failed) = switch_result(confirmed, target, now.saturating_duration_since(started), wait, Duration::from_secs(4)) {
                    pending[i] = None;
                    shared.update(|r| {
                        r.system[i] = SystemTile {
                            state,
                            ready: true,
                            busy: false,
                            failed: failed.then_some(now),
                        }
                    });
                }
            } else if shared.snapshot().system[i].state != state {
                publish(setting, state);
            }
        }
        for watch in &mut access_registry {
            access_dirty |= RegistryWatch::changed(watch);
        }
        if refresh || access_dirty || access_pending.iter().any(Option::is_some) {
            let states = read_accessibility();
            let current = shared.remote.lock().map(|r| r.access).unwrap_or_default();
            if states
                .iter()
                .zip(current)
                .any(|(state, old)| *state != old.on)
                || access_pending.iter().any(Option::is_some)
            {
                shared.update(|r| {
                    for (i, on) in states.into_iter().enumerate() {
                        let view = &mut r.access[i];
                        view.on = on;
                        if let Some((target, started)) = access_pending[i] {
                            let elapsed = now.saturating_duration_since(started);
                            let wait = if i == Access::ColorFilters as usize {
                                BROKER_SETTLE
                            } else {
                                Duration::ZERO
                            };
                            if let Some(failed) = switch_result(on, target, elapsed, wait, Duration::from_secs(5)) {
                                access_pending[i] = None;
                                view.busy = false;
                                view.failed = failed.then_some(now);
                            }
                        }
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_dnd_is_lit_but_routes_to_settings() {
        let automatic = dnd_status(Some(false), Some(true));
        assert!(automatic.automatic_dnd() && !automatic.writable);
        assert!(dnd_status(Some(true), Some(true)).writable);
        assert!(dnd_status(Some(false), None).writable);
    }

    #[test]
    fn a_registry_echo_before_the_settle_period_is_not_confirmation() {
        let limit = Duration::from_secs(4);
        assert_eq!(switch_result(Some(true), true, CHECK, BROKER_SETTLE, limit), None);
        assert_eq!(switch_result(Some(true), true, BROKER_SETTLE, BROKER_SETTLE, limit), Some(false));
        assert_eq!(switch_result(Some(false), true, limit, BROKER_SETTLE, limit), Some(true));
    }
}
