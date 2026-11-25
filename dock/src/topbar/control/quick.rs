//! Dark mode, the quick tiles' power, tethering and display queries.

use super::*;

// ---- dark mode ------------------------------------------------------------------------

pub(super) const PERSONALIZE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
pub(super) const DARK_VALUES: [&str; 2] = ["AppsUseLightTheme", "SystemUsesLightTheme"];

pub(super) fn reg_get(name: &str) -> Option<u32> {
    let key = windows::core::HSTRING::from(PERSONALIZE);
    let name = windows::core::HSTRING::from(name);
    crate::syslook::dword(windows::core::PCWSTR(key.as_ptr()), windows::core::PCWSTR(name.as_ptr()))
}

/// Sets (Some) or removes (None) one Personalize value.
pub(super) fn reg_put(name: &str, v: Option<u32>) -> bool {
    use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD};
    let key = windows::core::HSTRING::from(PERSONALIZE);
    let name = windows::core::HSTRING::from(name);
    unsafe {
        match v {
            Some(v) => RegSetKeyValueW(HKEY_CURRENT_USER, &key, &name, REG_DWORD.0, Some(&v as *const u32 as *const _), 4).is_ok(),
            None => RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &name).is_ok(),
        }
    }
}

/// Apps and Windows both use the dark mode.
pub(super) fn read_dark() -> bool {
    DARK_VALUES.iter().all(|n| reg_get(n) == Some(0))
}

/// Puts both values to dark (or light), like Settings. If one write fails,
/// the old values go back (never apps and Windows half switched). Tells
/// every window ("ImmersiveColorSet") only if something changed. Returns
/// the state as it is now.
pub(super) fn apply_dark(dark: bool) -> bool {
    if cfg!(test) { return dark; }
    let want = if dark { 0 } else { 1 };
    let old = DARK_VALUES.map(reg_get);
    if old.iter().all(|v| *v == Some(want)) {
        return dark;
    }
    let mut written = 0;
    for name in DARK_VALUES {
        if !reg_put(name, Some(want)) {
            crate::log!("control: dark mode: {name} not written; restoring");
            break;
        }
        written += 1;
    }
    if written < DARK_VALUES.len() {
        for (name, v) in DARK_VALUES.iter().zip(old).take(written) {
            if !reg_put(name, v) {
                crate::log!("control: dark mode: {name} not restored");
            }
        }
    }
    let now = DARK_VALUES.map(reg_get);
    if now != old {
        let what = w!("ImmersiveColorSet");
        unsafe {
            let _ = SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(what.as_ptr() as isize), SMTO_ABORTIFHUNG, 3000, None);
        }
    }
    now.iter().all(|v| *v == Some(0))
}

/// Requests for the dark-mode worker: only the latest is kept.
pub(super) struct DarkQueue {
    pub(super) want: Option<(u64, bool, Arc<Shared>)>,
    pub(super) running: bool,
}

pub(super) static DARK: Mutex<DarkQueue> = Mutex::new(DarkQueue { want: None, running: false });

/// Switches dark mode on one serialized worker (the broadcast waits on
/// every app): requests made meanwhile collapse into the latest. The state
/// it finds afterwards goes back to the panel with `seq`.
pub(super) fn set_dark(seq: u64, dark: bool, shared: &Arc<Shared>) {
    let Ok(mut q) = DARK.lock() else { return };
    q.want = Some((seq, dark, shared.clone()));
    if q.running {
        return;
    }
    q.running = true;
    drop(q);
    let started = std::thread::Builder::new().name("cc-dark".into()).spawn(|| loop {
        let job = match DARK.lock() {
            Ok(mut q) => match q.want.take() {
                Some(j) => j,
                None => {
                    q.running = false;
                    break;
                }
            },
            Err(_) => break,
        };
        let (seq, dark, shared) = job;
        let now = apply_dark(dark);
        if now != dark {
            crate::log!("control: dark mode stays {}", if now { "on" } else { "off" });
        }
        shared.update(|r| r.dark = Some((seq, now)));
    });
    if started.is_err() {
        if let Ok(mut q) = DARK.lock() {
            q.running = false;
        }
    }
}

// ---- quick tiles: energy saver, hotspot, displays ------------------------------------

use windows::core::GUID;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, UnregisterPowerSettingNotification,
    HPOWERNOTIFY, POWERBROADCAST_SETTING, SYSTEM_POWER_STATUS,
};

const ENERGY_SAVER: GUID = GUID::from_u128(0x550e8400_e29b_41d4_a716_446655440000);
const POWER_SAVING: GUID = GUID::from_u128(0xe00958c0_c213_4ace_ac77_fecced2eeea5);
const BATTERY_PERCENT: GUID = GUID::from_u128(0xa7ad8041_b45a_4cae_87a3_eecbb468a9e1);

pub(super) fn read_power() -> (Option<(u8, bool)>, Option<bool>) {
    let mut s = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut s) }.is_err() {
        return (None, None);
    }
    let battery = (s.BatteryFlag != 128 && s.BatteryLifePercent <= 100)
        .then_some((s.BatteryLifePercent, s.ACLineStatus == 1));
    (battery, Some(s.SystemStatusFlag == 1))
}

#[derive(Default)]
pub(super) struct PowerWatch(Vec<HPOWERNOTIFY>);

impl PowerWatch {
    pub(super) fn new(hwnd: HWND) -> Self {
        if cfg!(test) { return Self::default(); }
        let register = |key| unsafe {
            RegisterPowerSettingNotification(HANDLE(hwnd.0), key, DEVICE_NOTIFY_WINDOW_HANDLE).ok()
        };
        Self([register(&ENERGY_SAVER).or_else(|| register(&POWER_SAVING)), register(&BATTERY_PERCENT)]
            .into_iter().flatten().collect())
    }

    pub(super) unsafe fn update(shared: &Shared, data: LPARAM) {
        let Some(change) = (data.0 as *const POWERBROADCAST_SETTING).as_ref() else { return };
        let (battery, _) = read_power();
        shared.set_quiet(|r| {
            r.battery = battery;
            if [ENERGY_SAVER, POWER_SAVING].contains(&change.PowerSetting) && change.DataLength == 4 {
                let state = std::ptr::read_unaligned(change.Data.as_ptr().cast::<u32>());
                r.energy = Some(state != 0);
            }
        });
    }
}

impl Drop for PowerWatch {
    fn drop(&mut self) {
        for notify in self.0.drain(..) {
            unsafe { let _ = UnregisterPowerSettingNotification(notify); }
        }
    }
}

pub(super) fn open_studio(camera: String) {
    let _ = std::thread::Builder::new().name("cc-studio".into()).spawn(move || {
        use crate::launch::{open_here, report_later, Source, Target};
        let mut settings = reqwest::Url::parse("ms-settings:camera").expect("valid settings URI");
        settings.query_pairs_mut().append_pair("cameraId", &camera);
        let settings = settings.as_str();
        let last = Target::new("ms-settings:camera", "ms-settings:camera", Source::TopBar);
        crate::launch::with_shell_com(|| {
            if open_here(&Target::new("ms-controlcenter:studioeffects", "ms-controlcenter:studioeffects", Source::TopBar)).is_err()
                && open_here(&Target::new(settings, settings, Source::TopBar)).is_err()
            {
                if let Err(code) = open_here(&last) {
                    report_later(last, code);
                }
            }
        });
    });
}

/// The tethering manager for the connection Windows would share (None: no
/// internet connection, or one Windows cannot share).
pub(super) fn tethering() -> Option<Tethering> {
    let profile = NetworkInformation::GetInternetConnectionProfile().ok()?;
    match Tethering::GetTetheringCapabilityFromConnectionProfile(&profile) {
        Ok(TetheringCapability::Enabled) => {}
        Ok(c) => {
            crate::log!("control: no hotspot tile (tethering capability {})", c.0);
            return None;
        }
        Err(e) => {
            crate::log!("control: no hotspot tile: {e}");
            return None;
        }
    }
    match Tethering::CreateFromConnectionProfile(&profile) {
        Ok(m) => Some(m),
        Err(e) => {
            crate::log!("control: no hotspot tile: {e}");
            None
        }
    }
}

/// How many displays are connected (QDC_ALL_PATHS: every target with a
/// monitor on it, in use or not) and the mode the active paths show
/// (QDC_ONLY_ACTIVE_PATHS, see active_topology).
pub(super) fn read_displays() -> Displays {
    use windows::Win32::Devices::Display::{
        GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
        DISPLAYCONFIG_PATH_INFO, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, QUERY_DISPLAY_CONFIG_FLAGS,
    };
    use windows::Win32::Foundation::{ERROR_SUCCESS, LUID};
    let query = |flags: QUERY_DISPLAY_CONFIG_FLAGS| -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
        unsafe {
            let (mut np, mut nm) = (0u32, 0u32);
            if GetDisplayConfigBufferSizes(flags, &mut np, &mut nm) != ERROR_SUCCESS || np > 4096 || nm > 8192 {
                return None;
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            if QueryDisplayConfig(flags, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
                return None;
            }
            paths.truncate(np as usize);
            Some(paths)
        }
    };
    // The PC's own screen: the embedded connectors.
    let internal = |p: &DISPLAYCONFIG_PATH_INFO| {
        let t = p.targetInfo.outputTechnology;
        t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
            || t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
            || t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED
            || t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS
    };
    let key = |a: LUID, id: u32| ((a.LowPart as u64) << 40) ^ ((a.HighPart as u32 as u64) << 20) ^ id as u64;
    let Some(all) = query(QDC_ALL_PATHS) else { return Displays::Single };
    let mut targets: Vec<(u64, bool)> =
        all.iter().filter(|p| p.targetInfo.targetAvailable.as_bool()).map(|p| (key(p.targetInfo.adapterId, p.targetInfo.id), internal(p))).collect();
    targets.sort_unstable();
    targets.dedup();
    let internal_connected = targets.iter().any(|t| t.1);
    let current = query(QDC_ONLY_ACTIVE_PATHS).and_then(|paths| {
        let active: Vec<(u64, bool)> = paths.iter().map(|p| (key(p.sourceInfo.adapterId, p.sourceInfo.id), internal(p))).collect();
        active_topology(&active, internal_connected)
    });
    displays_of(targets.len(), current)
}

/// Puts the displays in mode `t`, as Win+P does (SDC_TOPOLOGY_*: Windows
/// picks the paths and remembers the choice).
pub(super) fn set_topology(t: Topology) {
    if cfg!(test) { return; }
    use windows::Win32::Devices::Display::{SetDisplayConfig, SDC_APPLY, SET_DISPLAY_CONFIG_FLAGS};
    let r = unsafe { SetDisplayConfig(None, None, SET_DISPLAY_CONFIG_FLAGS(SDC_APPLY.0 | t.sdc())) };
    if r != 0 {
        crate::log!("control: display mode {t:?} refused ({r})");
    }
}

pub(super) fn quick_worker(shared: &Shared, rx: Receiver<QuickCmd>, tx: Sender<QuickCmd>) {
    let d = read_displays();
    shared.update(|r| r.displays = d);
    if shared.quitting() {
        return;
    }
    let mut _subs = Vec::new();
    // Any network change (a new connection, the hotspot going on or off,
    // by us, by Windows' own switch or its idle timeout): the tile is
    // worked out again from the connection Windows would share now.
    {
        let tx = tx.clone();
        let handler = windows::Networking::Connectivity::NetworkStatusChangedEventHandler::new(move |_| {
            let _ = tx.send(QuickCmd::Network);
            Ok(())
        });
        match NetworkInformation::NetworkStatusChanged(&handler) {
            Ok(t) => _subs.push(Defer::new(move || {
                let _ = NetworkInformation::RemoveNetworkStatusChanged(t);
            })),
            Err(e) => crate::log!("control: network changes: {e}"),
        }
    }
    let mut hotspot = HotspotWatch::default();
    hotspot.reconnect(shared);
    loop {
        // Woken by a command, or when a switching hotspot is due a look.
        let cmd = match hotspot.settling.map(|s| s.next) {
            Some(next) => match rx.recv_timeout(next.saturating_duration_since(std::time::Instant::now())) {
                Ok(c) => Some(c),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            },
        };
        if shared.quitting() {
            break;
        }
        let Some(cmd) = cmd else {
            hotspot.recheck(shared);
            continue;
        };
        match cmd {
            QuickCmd::Quit => break,
            QuickCmd::Displays => {
                let d = read_displays();
                shared.update(|r| r.displays = d);
            }
            QuickCmd::Network => hotspot.reconnect(shared),
            QuickCmd::Hotspot(on) => {
                if !hotspot.switch(shared, on) {
                    break;
                }
            }
        }
        // Commands coming one after another do not hold off a due look.
        if hotspot.settling.is_some_and(|s| std::time::Instant::now() >= s.next) {
            hotspot.recheck(shared);
        }
    }
}

/// A hotspot switch Windows is still carrying out, read again until it
/// settles (see settle).
#[derive(Clone, Copy, Debug)]
pub(super) struct Settling {
    pub(super) target: Option<bool>,
    pub(super) until: std::time::Instant,
    pub(super) next: std::time::Instant,
}

/// The quick worker's hotspot: the tethering manager of the connection
/// Windows would share now, and a switch still being followed.
#[derive(Default)]
pub(super) struct HotspotWatch {
    pub(super) manager: Option<Tethering>,
    pub(super) settling: Option<Settling>,
}

impl HotspotWatch {
    pub(super) fn read(&self) -> (RadioView, bool) {
        hotspot_of(self.manager.as_ref().and_then(|m| m.TetheringOperationalState().ok()))
    }

    /// The connection may have changed: a manager for the current one (the
    /// old one is dropped), and the tile from its state. A switch still
    /// followed is followed on the new manager.
    pub(super) fn reconnect(&mut self, shared: &Shared) {
        self.manager = tethering();
        let target = self.settling.and_then(|s| s.target);
        self.look(shared, target);
    }

    /// Shows the state now; while Windows is still switching (or a switch
    /// of ours does not show yet), the tile says "Switching…" and is read
    /// again every SETTLE_EVERY for at most SETTLE_FOR. Busy only while so
    /// followed.
    pub(super) fn look(&mut self, shared: &Shared, target: Option<bool>) {
        let now = std::time::Instant::now();
        let read = self.read();
        match settle(read, target, false) {
            Some((v, failed)) => self.done(shared, v, failed),
            None => {
                let until = self.settling.map(|s| s.until).unwrap_or(now + SETTLE_FOR);
                self.settling = Some(Settling { target, until, next: now + SETTLE_EVERY });
                let v = read.0;
                shared.update(|r| {
                    r.hotspot = match target {
                        Some(t) => if t { RadioView::On } else { RadioView::Off },
                        None => v,
                    };
                    r.hotspot_busy = true;
                    r.hotspot_failed = None;
                });
            }
        }
    }

    /// A switching hotspot's time for another look.
    pub(super) fn recheck(&mut self, shared: &Shared) {
        let Some(s) = self.settling else { return };
        let now = std::time::Instant::now();
        match settle(self.read(), s.target, now >= s.until) {
            Some((v, failed)) => {
                if failed {
                    crate::log!("control: hotspot -> {:?} not shown within {} s; the tile says so", s.target, SETTLE_FOR.as_secs());
                }
                self.done(shared, v, failed);
            }
            None => self.settling = Some(Settling { next: now + SETTLE_EVERY, ..s }),
        }
    }

    pub(super) fn done(&mut self, shared: &Shared, v: RadioView, failed: bool) {
        self.settling = None;
        shared.update(|r| {
            r.hotspot = v;
            r.hotspot_busy = false;
            r.hotspot_failed = failed.then(std::time::Instant::now);
        });
    }

    /// Turns the hotspot on / off. False: the panel closed meanwhile.
    pub(super) fn switch(&mut self, shared: &Shared, on: bool) -> bool {
        let result = self.manager.as_ref().and_then(|m| {
            let op = if on { m.StartTetheringAsync() } else { m.StopTetheringAsync() };
            wait(op, 20_000, shared, Kind::Quick, "hotspot switch")
        });
        if shared.quitting() {
            return false;
        }
        let status = result.as_ref().and_then(|r| r.Status().ok());
        let ok = matches!(status, Some(TetheringOperationStatus::Success | TetheringOperationStatus::AlreadyOn));
        if ok {
            // Accepted is not done: followed until the state shows it.
            self.settling = None;
            self.look(shared, Some(on));
        } else {
            let why = result.as_ref().and_then(|r| r.AdditionalErrorMessage().ok()).map(|m| m.to_string()).unwrap_or_default();
            crate::log!("control: hotspot -> {on} refused ({:?}) {why}", status.map(|s| s.0));
            // As Windows has it (back where it was if it cannot be read).
            let v = match self.read() {
                (RadioView::Missing, _) => if on { RadioView::Off } else { RadioView::On },
                (v, _) => v,
            };
            self.done(shared, v, true);
        }
        true
    }
}
