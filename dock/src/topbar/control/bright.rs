//! Brightness of the display the Control Center is open on.
//!
//! The PC's own panel is dimmed through WMI (WmiMonitorBrightness, the
//! panel's `wmi` module); any other monitor through DDC/CI (dxva2: the
//! monitor's physical handles, VCP code 0x10, luminance), the way Twinkle
//! Tray and Monitorian do it. A display mirrored onto several screens
//! (Duplicate) is one slider for all of them: every screen that answers
//! takes the level, the first one's is shown.
//!
//! DDC/CI goes over the monitor cable and takes tens to hundreds of ms a
//! call (a monitor that does not take part can take longer to fail), so
//! all of it runs on the panel's brightness worker, never the UI thread:
//! a drag's values are coalesced (the latest wins) and written at most
//! once per DDC_GAP (MCCS asks for 50 ms between commands; some monitors
//! need more), then read back once the drag stops.
//!
//! Nothing is changed on a guess: a display Windows would not describe
//! (its active paths, its monitors) gets no slider, and WMI (which dims
//! the first active panel it knows, not a given display) is used only
//! while the display is the one PC panel there is.
//!
//! What a display answered is kept for the process (CACHE), by its
//! monitors (their device paths, so another monitor on the same output or
//! a screen mirrored onto it is another display): the level last read
//! shows at once the next time (no slider popping in late), and monitors
//! that refused DDC/CI are not asked again for REFUSED_FOR from when they
//! were last asked. Physical monitor handles live only as long as the
//! worker's job and are destroyed with it; a closed panel's job writes to
//! no further screen.
//!
//! The decisions are pure and tested here with stand-in screens; nothing
//! in the tests touches a real monitor.

use std::sync::Mutex;
use std::time::{Duration, Instant};

pub type Res<T> = std::result::Result<T, String>;

/// One screen whose brightness can be read and set, in percent.
pub trait Dimmer {
    fn read(&mut self) -> Res<u8>;
    fn write(&mut self, level: u8) -> Res<()>;
    /// Over DDC/CI (an external monitor), not WMI.
    fn ddc(&self) -> bool;
    /// For the log.
    fn name(&self) -> String;
}

// ---- which way to a display's brightness --------------------------------------

/// The ways to try for one display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Plan {
    pub wmi: bool,
    pub ddc: bool,
}

/// The screens showing one display, as DisplayConfig has them.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Screens {
    /// One of them is the PC's own panel (an embedded connector).
    pub internal: bool,
    /// One of them is another monitor.
    pub external: bool,
    /// The PC's own panels showing anything now, on any display.
    pub internal_total: usize,
    /// Who they are: their monitors' device paths (or adapter and
    /// target), sorted.
    pub ident: String,
}

/// Pure: the ways to try for a display from its screens (None: Windows
/// would not say) and whether DDC/CI refused there a moment ago. Nothing
/// on a guess: an unknown display gets nothing. WMI dims the first active
/// panel it knows, not this display, so only a display showing the one
/// PC panel there is takes it (never another monitor's Control Center,
/// nor one of two built-in screens).
pub fn plan(screens: Option<&Screens>, ddc_refused: bool) -> Plan {
    let Some(sc) = screens else { return Plan::default() };
    Plan { wmi: sc.internal && sc.internal_total == 1, ddc: sc.external && !ddc_refused }
}

// ---- DDC/CI values -----------------------------------------------------------------

/// Pure: a monitor's luminance (VCP 0x10: `current` of `max`) in percent;
/// None for a monitor that gives no range.
pub fn to_percent(current: u32, max: u32) -> Option<u8> {
    if max == 0 {
        return None;
    }
    let (c, m) = (current.min(max) as u64, max as u64);
    Some(((c * 100 + m / 2) / m) as u8)
}

/// Pure: percent `level` as the monitor's own value (0..=`max`).
pub fn to_raw(level: u8, max: u32) -> u32 {
    ((level.min(100) as u64 * max as u64 + 50) / 100) as u32
}

// ---- writing at the monitor's pace ---------------------------------------------------

/// The least time between two DDC/CI commands to one monitor.
pub const DDC_GAP: Duration = Duration::from_millis(100);

/// What the worker does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do: wait for the panel.
    Idle,
    /// Something to do, not before then.
    Wait(Instant),
    /// Write this level.
    Write(u8),
    /// Read back what the screens really did.
    Confirm,
}

/// A drag's levels coalesced: only the latest is written, at most one
/// command per `gap`, and once it is written the level is read back
/// (unless a newer one came meanwhile).
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    gap: Duration,
    last: Option<Instant>,
    want: Option<u8>,
    confirm: bool,
}

impl Pace {
    pub fn new(gap: Duration) -> Self {
        Pace { gap, last: None, want: None, confirm: false }
    }

    /// The panel asked for `level` (an older one not written yet is
    /// dropped).
    pub fn push(&mut self, level: u8) {
        self.want = Some(level);
        self.confirm = false;
    }

    /// A level waits to be written.
    pub fn pending(&self) -> bool {
        self.want.is_some()
    }

    pub fn next(&self, now: Instant) -> Step {
        if self.want.is_none() && !self.confirm {
            return Step::Idle;
        }
        let ready = self.last.map_or(now, |l| l + self.gap);
        if now < ready {
            return Step::Wait(ready);
        }
        match self.want {
            Some(v) => Step::Write(v),
            None => Step::Confirm,
        }
    }

    /// The level was written (or tried) at `now`.
    pub fn wrote(&mut self, now: Instant) {
        self.last = Some(now);
        self.want = None;
        self.confirm = true;
    }

    /// The level was read back at `now`.
    pub fn confirmed(&mut self, now: Instant) {
        self.last = Some(now);
        self.confirm = false;
    }
}

// ---- what each display answered ------------------------------------------------------

/// A display whose monitors refused DDC/CI is not asked again for this
/// long (a monitor just woken, or DDC/CI just switched on in its menu,
/// gets its slider soon after).
pub const REFUSED_FOR: Duration = Duration::from_secs(120);
/// Displays remembered (the oldest goes).
const CACHE_MAX: usize = 16;

/// What a display answered last.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    /// Where it is (key_of: what the panel knows at once).
    place: String,
    /// Its level then (None: no screen of it could be dimmed).
    level: Option<u8>,
    /// When DDC/CI was last asked there and no monitor answered (None:
    /// it answered, or was never asked).
    ddc_refused: Option<Instant>,
}

/// Displays by key (display_key: where they are and their monitors) and
/// what each answered, the latest last.
pub struct Cache {
    entries: Vec<(String, Seen)>,
}

impl Cache {
    pub const fn new() -> Self {
        Cache { entries: Vec::new() }
    }

    fn find(&self, key: &str) -> Option<&Seen> {
        self.entries.iter().find(|e| e.0 == key).map(|e| &e.1)
    }

    /// The level to show at once on the display at `place` (key_of): the
    /// last one read there, whatever monitors it had (the worker corrects
    /// it, or hides the slider).
    pub fn shown(&self, place: &str) -> Option<u8> {
        if place.is_empty() {
            return None;
        }
        self.entries.iter().rev().find(|e| e.1.place == place).and_then(|e| e.1.level)
    }

    /// DDC/CI refused on display `key` less than REFUSED_FOR ago.
    pub fn ddc_refused(&self, key: &str, now: Instant) -> bool {
        self.find(key).and_then(|s| s.ddc_refused).is_some_and(|t| now.saturating_duration_since(t) < REFUSED_FOR)
    }

    /// Display `key` (at `place`) was opened with `plan`: its level, and
    /// whether a DDC/CI monitor answered. DDC/CI's refusal (and its time)
    /// changes only when DDC/CI was asked: one skipped for a refusal keeps
    /// it as it was, so REFUSED_FOR runs from the last real refusal.
    pub fn opened(&mut self, place: &str, key: &str, plan: Plan, level: Option<u8>, ddc_answered: bool, now: Instant) {
        if key.is_empty() {
            return;
        }
        let before = self.find(key).and_then(|s| s.ddc_refused);
        let ddc_refused = if plan.ddc { (!ddc_answered).then_some(now) } else { before };
        self.entries.retain(|e| e.0 != key);
        if self.entries.len() >= CACHE_MAX {
            self.entries.remove(0);
        }
        self.entries.push((key.to_string(), Seen { place: place.to_string(), level, ddc_refused }));
    }

    /// A level confirmed after a write (the display's other answers kept).
    pub fn set_level(&mut self, key: &str, level: u8) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.0 == key) {
            e.1.level = Some(level);
        }
    }
}

/// Pure: a display's key for CACHE: where it is and who its monitors
/// are (empty when either is unknown: nothing is kept for it).
pub fn display_key(place: &str, sc: &Screens) -> String {
    if place.is_empty() || sc.ident.is_empty() {
        return String::new();
    }
    format!("{place}|{}", sc.ident)
}

pub static CACHE: Mutex<Cache> = Mutex::new(Cache::new());

// ---- the screens of one display, as one slider --------------------------------------

/// The screens of a display that answered.
pub struct Group {
    dimmers: Vec<Box<dyn Dimmer>>,
}

impl Group {
    /// Reads every candidate (stopping early when `quit` says so): keeps
    /// those that answer (the others are dropped, their handles with
    /// them); the level is the first one's.
    pub fn open(cands: Vec<Box<dyn Dimmer>>, quit: impl Fn() -> bool) -> (Group, Option<u8>) {
        let mut level = None;
        let mut dimmers = Vec::with_capacity(cands.len());
        for mut d in cands {
            if quit() {
                break;
            }
            match d.read() {
                Ok(v) => {
                    level.get_or_insert(v);
                    dimmers.push(d);
                }
                Err(e) => crate::log!("control: brightness: {} does not answer: {e}", d.name()),
            }
        }
        (Group { dimmers }, level)
    }

    pub fn is_empty(&self) -> bool {
        self.dimmers.is_empty()
    }

    pub fn has_ddc(&self) -> bool {
        self.dimmers.iter().any(|d| d.ddc())
    }

    /// Every screen takes `level`, unless `quit` says the panel is gone
    /// (no further screen is written then); true if any did.
    pub fn write(&mut self, level: u8, quit: impl Fn() -> bool) -> bool {
        let mut any = false;
        for d in &mut self.dimmers {
            if quit() {
                break;
            }
            match d.write(level) {
                Ok(()) => any = true,
                Err(e) => crate::log!("control: brightness {level} on {}: {e}", d.name()),
            }
        }
        any
    }

    /// The level now: the first screen that answers.
    pub fn read(&mut self) -> Option<u8> {
        self.dimmers.iter_mut().find_map(|d| d.read().ok())
    }
}

// ---- Windows ------------------------------------------------------------------------

/// Where a display is: its GDI name and its rectangle (at once, for the
/// level shown when the panel opens; display_key adds its monitors).
pub fn key_of(hmon: isize) -> String {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO, MONITORINFOEXW};
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(HMONITOR(hmon as *mut _), &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
        return String::new();
    }
    let r = mi.monitorInfo.rcMonitor;
    format!("{}@{},{},{},{}", wide(&mi.szDevice), r.left, r.top, r.right, r.bottom)
}

fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// The screens showing display `hmon` (its active paths): whether one is
/// the PC's own panel (an embedded connector), whether one is another
/// monitor, how many PC panels show anything at all, and who the screens
/// are. None when Windows would not say (asked twice: the configuration
/// can change between sizing and reading it). DisplayConfig, a few ms.
pub fn screens_of(hmon: isize) -> Option<Screens> {
    let gdi = key_of(hmon).split('@').next().unwrap_or_default().to_string();
    if gdi.is_empty() {
        return None;
    }
    (0..2).find_map(|_| read_screens(&gdi))
}

fn read_screens(gdi: &str) -> Option<Screens> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
        DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;
    let mut sc = Screens::default();
    let mut idents = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS || np == 0 || np > 64 || nm > 128 {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return None;
        }
        paths.truncate(np as usize);
        for p in &paths {
            let t = p.targetInfo.outputTechnology;
            let own = t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
                || t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
                || t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED
                || t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS;
            if own {
                sc.internal_total += 1;
            }
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                // A path we cannot place: not sure which display is which.
                return None;
            }
            if !wide(&src.viewGdiDeviceName).eq_ignore_ascii_case(gdi) {
                continue;
            }
            if own {
                sc.internal = true;
            } else {
                sc.external = true;
            }
            let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            let path = if DisplayConfigGetDeviceInfo(&mut tgt.header) == 0 { wide(&tgt.monitorDevicePath) } else { String::new() };
            idents.push(if path.is_empty() {
                format!("{:x}:{:x}:{}", p.targetInfo.adapterId.HighPart, p.targetInfo.adapterId.LowPart, p.targetInfo.id)
            } else {
                path.to_ascii_lowercase()
            });
        }
    }
    if !(sc.internal || sc.external) {
        return None;
    }
    idents.sort();
    sc.ident = idents.join(";");
    Some(sc)
}

mod dxva2 {
    use windows::core::BOOL;
    use windows::Win32::Devices::Display::PHYSICAL_MONITOR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Gdi::HMONITOR;
    // In the windows crate only behind its Direct3D9 feature.
    windows_core::link!("dxva2.dll" "system" fn GetPhysicalMonitorsFromHMONITOR(hmonitor: HMONITOR, size: u32, array: *mut PHYSICAL_MONITOR) -> BOOL);
    windows_core::link!("dxva2.dll" "system" fn GetVCPFeatureAndVCPFeatureReply(monitor: HANDLE, code: u8, kind: *mut i32, current: *mut u32, maximum: *mut u32) -> i32);
}

/// MCCS luminance.
const VCP_BRIGHTNESS: u8 = 0x10;
/// Tries per read (DDC/CI drops a reply now and then) and the pause
/// between them.
const DDC_TRIES: u32 = 2;
const DDC_RETRY: Duration = Duration::from_millis(50);

/// One physical monitor over DDC/CI; its handle is destroyed with it.
struct Ddc {
    handle: isize,
    /// Its luminance range's top (0 until read).
    max: u32,
    name: String,
}

impl Ddc {
    fn handle(&self) -> windows::Win32::Foundation::HANDLE {
        windows::Win32::Foundation::HANDLE(self.handle as *mut _)
    }
}

impl Dimmer for Ddc {
    fn read(&mut self) -> Res<u8> {
        let mut last = String::new();
        for i in 0..DDC_TRIES {
            if i > 0 {
                std::thread::sleep(DDC_RETRY);
            }
            let (mut cur, mut max) = (0u32, 0u32);
            let ok = unsafe { dxva2::GetVCPFeatureAndVCPFeatureReply(self.handle(), VCP_BRIGHTNESS, std::ptr::null_mut(), &mut cur, &mut max) } != 0;
            if !ok {
                last = format!("no reply ({})", windows::core::Error::from_thread());
                continue;
            }
            match to_percent(cur, max) {
                Some(p) => {
                    self.max = max;
                    return Ok(p);
                }
                None => return Err("no luminance range".into()),
            }
        }
        Err(last)
    }

    fn write(&mut self, level: u8) -> Res<()> {
        if self.max == 0 {
            return Err("range unknown".into());
        }
        let ok = unsafe { windows::Win32::Devices::Display::SetVCPFeature(self.handle(), VCP_BRIGHTNESS, to_raw(level, self.max)) } != 0;
        if ok {
            Ok(())
        } else {
            Err(format!("refused ({})", windows::core::Error::from_thread()))
        }
    }

    fn ddc(&self) -> bool {
        true
    }

    fn name(&self) -> String {
        if self.name.is_empty() { "monitor (DDC/CI)".into() } else { format!("{} (DDC/CI)", self.name) }
    }
}

impl Drop for Ddc {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Devices::Display::DestroyPhysicalMonitor(self.handle());
        }
    }
}

/// The physical monitors behind display `hmon`, each as a DDC/CI screen
/// (not asked anything yet: Group::open reads them).
pub fn ddc_screens(hmon: isize) -> Vec<Box<dyn Dimmer>> {
    use windows::Win32::Devices::Display::{GetNumberOfPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR};
    use windows::Win32::Graphics::Gdi::HMONITOR;
    let h = HMONITOR(hmon as *mut _);
    let mut n = 0u32;
    if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n) }.is_err() || n == 0 || n > 16 {
        return Vec::new();
    }
    let mut list = vec![PHYSICAL_MONITOR::default(); n as usize];
    if !unsafe { dxva2::GetPhysicalMonitorsFromHMONITOR(h, n, list.as_mut_ptr()) }.as_bool() {
        crate::log!("control: brightness: no physical monitors ({})", windows::core::Error::from_thread());
        return Vec::new();
    }
    list.iter()
        .map(|p| {
            // Packed: copied out, never borrowed in place.
            let (handle, desc) = (p.hPhysicalMonitor, p.szPhysicalMonitorDescription);
            Box::new(Ddc { handle: handle.0 as isize, max: 0, name: wide(&desc) }) as Box<dyn Dimmer>
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn screens(internal: bool, external: bool, internal_total: usize, ident: &str) -> Screens {
        Screens { internal, external, internal_total, ident: ident.into() }
    }

    #[test]
    fn each_display_gets_its_own_way() {
        // The PC's own panel: WMI, never DDC/CI.
        assert_eq!(plan(Some(&screens(true, false, 1, "p")), false), Plan { wmi: true, ddc: false });
        // Another monitor: DDC/CI only (WMI would dim the laptop's panel).
        assert_eq!(plan(Some(&screens(false, true, 1, "m")), false), Plan { wmi: false, ddc: true });
        // Mirrored onto both: both, one slider.
        assert_eq!(plan(Some(&screens(true, true, 1, "m;p")), false), Plan { wmi: true, ddc: true });
        // DDC/CI refused there a moment ago: not asked again.
        assert_eq!(plan(Some(&screens(false, true, 1, "m")), true), Plan { wmi: false, ddc: false });
        assert_eq!(plan(Some(&screens(true, true, 1, "m;p")), true), Plan { wmi: true, ddc: false });
    }

    #[test]
    fn nothing_changes_on_a_guess() {
        // Windows would not describe the display (the external monitor's
        // Control Center with DisplayConfig failing): nothing at all, so
        // the built-in panel answering WMI is never written.
        assert_eq!(plan(None, false), Plan::default());
        let log = Rc::new(RefCell::new(Vec::new()));
        let p = plan(None, false);
        let cands = if p.wmi { vec![fake('w', Some(60), false, &log)] } else { Vec::new() };
        let (mut g, level) = Group::open(cands, || false);
        assert!(g.is_empty() && level.is_none());
        assert!(!g.write(10, || false));
        assert!(log.borrow().is_empty(), "no WMI write");
        // Two built-in screens: WMI cannot say which it dims, so neither.
        assert_eq!(plan(Some(&screens(true, false, 2, "p1")), false), Plan::default());
        // Nothing kept for a display that is not known.
        assert_eq!(display_key("", &screens(true, false, 1, "p")), "");
        assert_eq!(display_key("\\\\.\\DISPLAY1@0,0,10,10", &screens(true, false, 1, "")), "");
        let mut c = Cache::new();
        c.opened("A", "", Plan { wmi: true, ddc: true }, Some(5), false, Instant::now());
        assert_eq!(c.shown("A"), None);
        assert_eq!(c.shown(""), None);
    }

    #[test]
    fn ddc_values_map_to_percent_and_back() {
        assert_eq!(to_percent(0, 100), Some(0));
        assert_eq!(to_percent(37, 100), Some(37));
        assert_eq!(to_percent(255, 255), Some(100));
        assert_eq!(to_percent(128, 255), Some(50));
        // A value past the range is the top; no range, no slider.
        assert_eq!(to_percent(300, 100), Some(100));
        assert_eq!(to_percent(5, 0), None);
        assert_eq!(to_raw(0, 100), 0);
        assert_eq!(to_raw(42, 100), 42);
        assert_eq!(to_raw(100, 255), 255);
        assert_eq!(to_raw(200, 100), 100);
        // Every percent comes back as itself on a range of 100 or more; on
        // a coarser one every step of the monitor's does.
        for max in [100u32, 255, 1000] {
            for p in 0..=100u8 {
                assert_eq!(to_percent(to_raw(p, max), max), Some(p), "{p} of {max}");
            }
        }
        for max in [10u32, 50] {
            for raw in 0..=max {
                assert_eq!(to_raw(to_percent(raw, max).unwrap(), max), raw, "{raw} of {max}");
            }
        }
    }

    #[test]
    fn a_drag_writes_only_the_latest_at_the_monitors_pace() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut p = Pace::new(DDC_GAP);
        assert_eq!(p.next(t0), Step::Idle);
        // A drag: three levels before the worker looks; only the last.
        p.push(10);
        p.push(20);
        p.push(30);
        assert_eq!(p.next(t0), Step::Write(30));
        p.wrote(t0);
        // Nothing new: read back, but not before the gap.
        assert_eq!(p.next(ms(10)), Step::Wait(ms(100)));
        assert_eq!(p.next(ms(100)), Step::Confirm);
        // A new level within the gap waits for it, and replaces the
        // read-back.
        p.push(40);
        assert_eq!(p.next(ms(50)), Step::Wait(ms(100)));
        p.push(45);
        assert_eq!(p.next(ms(120)), Step::Write(45));
        p.wrote(ms(120));
        assert_eq!(p.next(ms(220)), Step::Confirm);
        p.confirmed(ms(230));
        assert_eq!(p.next(ms(240)), Step::Idle);
        assert!(!p.pending());
        // WMI (no gap): written as soon as asked.
        let mut w = Pace::new(Duration::ZERO);
        w.push(5);
        w.wrote(t0);
        w.push(6);
        assert_eq!(w.next(t0), Step::Write(6));
    }

    #[test]
    fn displays_are_remembered() {
        let t0 = Instant::now();
        let ddc = Plan { wmi: false, ddc: true };
        let mut c = Cache::new();
        assert_eq!(c.shown("A"), None);
        assert!(!c.ddc_refused("A|m1", t0));
        c.opened("A", "A|m1", ddc, Some(70), true, t0);
        c.opened("B", "B|m2", ddc, None, false, t0);
        assert_eq!(c.shown("A"), Some(70));
        assert_eq!(c.shown("B"), None);
        // Refused: not asked again for a while, then asked afresh.
        assert!(c.ddc_refused("B|m2", t0 + Duration::from_secs(5)));
        assert!(!c.ddc_refused("B|m2", t0 + REFUSED_FOR));
        assert!(!c.ddc_refused("A|m1", t0));
        // A confirmed write moves the level; an unknown display stays so.
        c.set_level("A|m1", 30);
        c.set_level("C|m3", 30);
        assert_eq!(c.shown("A"), Some(30));
        assert_eq!(c.shown("C"), None);
        // DDC/CI answering again clears the refusal.
        c.opened("B", "B|m2", ddc, Some(10), true, t0 + REFUSED_FOR);
        assert_eq!(c.shown("B"), Some(10));
        assert!(!c.ddc_refused("B|m2", t0 + REFUSED_FOR));
        // Bounded: the oldest goes.
        for i in 0..CACHE_MAX {
            c.opened(&format!("x{i}"), &format!("x{i}|m"), ddc, Some(1), true, t0);
        }
        assert_eq!(c.entries.len(), CACHE_MAX);
        assert_eq!(c.shown("A"), None);
    }

    /// Opening a display as the worker does: the plan from the cache, the
    /// screens asked (`answers`: whether a DDC/CI monitor answers), the
    /// answer kept. Whether DDC/CI was asked.
    fn open_as_worker(c: &mut Cache, sc: &Screens, ddc_answers: bool, now: Instant) -> bool {
        let key = display_key("D", sc);
        let p = plan(Some(sc), c.ddc_refused(&key, now));
        let answered = p.ddc && ddc_answers;
        c.opened("D", &key, p, answered.then_some(50), answered, now);
        p.ddc
    }

    #[test]
    fn a_refusal_holds_for_its_whole_time() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let sc = screens(false, true, 0, "monitor-a");
        let mut c = Cache::new();
        // Refused at 0; opened again at 5 s and 10 s: not asked either
        // time (skipping it does not wipe the refusal).
        assert!(open_as_worker(&mut c, &sc, false, t0));
        assert!(!open_as_worker(&mut c, &sc, true, s(5)));
        assert!(!open_as_worker(&mut c, &sc, true, s(10)));
        assert!(!open_as_worker(&mut c, &sc, true, t0 + REFUSED_FOR - Duration::from_millis(1)));
        // REFUSED_FOR after the real refusal: asked again.
        assert!(open_as_worker(&mut c, &sc, true, t0 + REFUSED_FOR));
        assert!(!c.ddc_refused(&display_key("D", &sc), t0 + REFUSED_FOR));
    }

    #[test]
    fn another_monitor_on_the_same_output_is_another_display() {
        let t0 = Instant::now();
        let mut c = Cache::new();
        // Monitor A refuses DDC/CI; B on the same output, same place and
        // size: asked all the same.
        let a = screens(false, true, 0, "monitor-a");
        let b = screens(false, true, 0, "monitor-b");
        assert!(open_as_worker(&mut c, &a, false, t0));
        assert!(c.ddc_refused(&display_key("D", &a), t0));
        assert!(open_as_worker(&mut c, &b, true, t0 + Duration::from_secs(1)));
        // A mirrored screen added: another display too.
        let ab = screens(false, true, 0, "monitor-a;monitor-b");
        assert!(open_as_worker(&mut c, &ab, true, t0 + Duration::from_secs(2)));
        // The level shown at once is the place's latest.
        assert_eq!(c.shown("D"), Some(50));
    }

    /// A stand-in screen: answers with `level` (None: refuses), logs writes.
    struct Fake {
        level: Option<u8>,
        ddc: bool,
        writes: Rc<RefCell<Vec<(char, u8)>>>,
        id: char,
        refuse_writes: bool,
    }

    impl Dimmer for Fake {
        fn read(&mut self) -> Res<u8> {
            self.level.ok_or_else(|| "refused".into())
        }
        fn write(&mut self, level: u8) -> Res<()> {
            if self.refuse_writes {
                return Err("refused".into());
            }
            self.writes.borrow_mut().push((self.id, level));
            self.level = Some(level);
            Ok(())
        }
        fn ddc(&self) -> bool {
            self.ddc
        }
        fn name(&self) -> String {
            self.id.to_string()
        }
    }

    fn fake(id: char, level: Option<u8>, ddc: bool, log: &Rc<RefCell<Vec<(char, u8)>>>) -> Box<dyn Dimmer> {
        Box::new(Fake { level, ddc, writes: log.clone(), id, refuse_writes: false })
    }

    #[test]
    fn one_slider_for_the_screens_that_answer() {
        let log = Rc::new(RefCell::new(Vec::new()));
        // A refusing monitor first: the level is the first that answers.
        let (mut g, level) = Group::open(vec![fake('a', None, true, &log), fake('b', Some(40), true, &log), fake('c', Some(90), false, &log)], || false);
        assert_eq!(level, Some(40));
        assert!(!g.is_empty() && g.has_ddc());
        // A write goes to every screen that answered, never the refuser.
        assert!(g.write(55, || false));
        assert_eq!(*log.borrow(), vec![('b', 55), ('c', 55)]);
        assert_eq!(g.read(), Some(55));
        // Nobody answers: no slider.
        let (g, level) = Group::open(vec![fake('a', None, true, &log)], || false);
        assert!(g.is_empty() && level.is_none() && !g.has_ddc());
        // WMI alone.
        let (g, _) = Group::open(vec![fake('w', Some(10), false, &log)], || false);
        assert!(!g.has_ddc());
        // The panel closed: nothing more is asked.
        let (g, level) = Group::open(vec![fake('b', Some(40), true, &log)], || true);
        assert!(g.is_empty() && level.is_none());
        // Writes all refused: said so.
        let mut g = Group { dimmers: vec![Box::new(Fake { level: Some(1), ddc: true, writes: log.clone(), id: 'r', refuse_writes: true })] };
        assert!(!g.write(5, || false));
    }

    /// A stand-in that closes the panel while it is written, and counts
    /// how often it is dropped.
    struct Slow {
        id: char,
        writes: Rc<RefCell<Vec<char>>>,
        quit: Rc<std::cell::Cell<bool>>,
        drops: Rc<RefCell<Vec<char>>>,
    }

    impl Dimmer for Slow {
        fn read(&mut self) -> Res<u8> {
            Ok(50)
        }
        fn write(&mut self, _: u8) -> Res<()> {
            self.writes.borrow_mut().push(self.id);
            self.quit.set(true);
            Ok(())
        }
        fn ddc(&self) -> bool {
            true
        }
        fn name(&self) -> String {
            self.id.to_string()
        }
    }

    impl Drop for Slow {
        fn drop(&mut self) {
            self.drops.borrow_mut().push(self.id);
        }
    }

    #[test]
    fn a_closed_panel_writes_no_further_screen() {
        let writes = Rc::new(RefCell::new(Vec::new()));
        let drops = Rc::new(RefCell::new(Vec::new()));
        let quit = Rc::new(std::cell::Cell::new(false));
        let slow = |id| Box::new(Slow { id, writes: writes.clone(), quit: quit.clone(), drops: drops.clone() }) as Box<dyn Dimmer>;
        let (mut g, _) = Group::open(vec![slow('a'), slow('b')], || false);
        // The panel closes while A is written: B is not written at all.
        let q = quit.clone();
        assert!(g.write(30, move || q.get()));
        assert_eq!(*writes.borrow(), vec!['a']);
        // Each screen (its handle) is let go once, with the job.
        drop(g);
        let mut d = drops.borrow().clone();
        d.sort();
        assert_eq!(d, vec!['a', 'b']);
    }
}
