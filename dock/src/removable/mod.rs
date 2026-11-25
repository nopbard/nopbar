//! Removable storage: what Windows' "Safely Remove Hardware and Eject
//! Media" would offer (USB sticks and drives, SD cards, hot-plug disks),
//! for the Control Center's section and the dot on its bar button.
//!
//! - model.rs: the rules (which devnode is ejected and whether it is plain
//!   storage, volumes per device, connections, what a veto means, what
//!   each row says), pure and tested;
//! - scan.rs: reading the devices from Windows and ejecting them.
//!
//! One helper thread ("removable"), started with the main bar, does every
//! blocking call: a scan at its start and whenever a disk, CD drive or
//! volume interface comes or goes (CM_Register_Notification; a burst of
//! them is read once, SETTLE after the last) or drive letters change (the
//! bar's WM_DEVICECHANGE volume broadcast, `letters_changed`), and each
//! eject (CM_Request_Device_EjectW). No polling: when a volume has no
//! drive letter yet (the mount manager assigns it a moment after the
//! volume arrives) it looks again at most three times (model::LATE_LOOKS).
//!
//! Each device's connection has a number (model::assign_gens): unplugged
//! and plugged in again, or given another medium, it is a new connection,
//! and nothing asked of the old one (a "Safe to remove", a queued eject)
//! applies to it. A connection ends with a removal notice for one of its
//! disk or volume interfaces, another set of volumes (a medium swapped for
//! one of the same size) or another size. Right before an eject the
//! helper scans again (relations included) and refuses the eject unless
//! that same connection is still offered and no notice for it came in
//! while that scan ran.
//!
//! Without device notices nothing is offered: if registering them fails,
//! nothing is scanned and the section stays hidden (logged once), and the
//! registration is tried again after 5 s, 30 s and 5 minutes
//! (model::LISTEN_RETRIES), then not again. Notices past MAX_REMOVED are
//! marked lost: every device becomes a new connection at the next scan,
//! and an eject whose pre-eject scan saw notices lost is refused.
//!
//! What it found is kept here (STATE, only ever held to copy or swap
//! values: no I/O, no logging under it) and the bar is told
//! (WM_REMOVABLE); the bar redraws its button and an open Control Center
//! its section. Between events the thread sleeps on its channel.

pub mod model;
mod scan;

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Register_Notification, CM_Unregister_Notification, CM_NOTIFY_ACTION, CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL, CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL,
    CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_0, CM_NOTIFY_FILTER_0_0, CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CR_SUCCESS,
    HCMNOTIFICATION,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Ioctl::{GUID_DEVINTERFACE_CDROM, GUID_DEVINTERFACE_DISK, GUID_DEVINTERFACE_VOLUME};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use model::{Conn, Device, Shown, Status};

/// Posted to the main bar when the devices or a row's note changed.
pub const WM_REMOVABLE: u32 = WM_APP + 51;
/// A burst of arrivals (disk, then its volumes) is read once, this long
/// after the last of them.
const SETTLE: Duration = Duration::from_millis(400);
/// Removal notices kept for the next scan at most (a device unplugged and
/// back within one burst); past it they are marked lost, and every device
/// is taken as a new connection.
const MAX_REMOVED: usize = 64;

enum Cmd {
    /// A device interface came or went, or drive letters changed.
    Changed,
    /// Eject this connection of a device.
    Eject(Conn),
}

struct State {
    present: Vec<Device>,
    status: Vec<(Conn, Status)>,
}

static STATE: Mutex<State> = Mutex::new(State { present: Vec::new(), status: Vec::new() });
static TX: Mutex<Option<Sender<Cmd>>> = Mutex::new(None);
/// Removal notices since the last scan: their interface paths
/// (lowercase), and whether any was lost (past MAX_REMOVED).
static REMOVED: Mutex<model::Notices> = Mutex::new(model::Notices { paths: Vec::new(), overflowed: false });
/// The main bar's window (0: none, nothing is posted).
static NOTIFY: AtomicIsize = AtomicIsize::new(0);

/// The main bar is up (its window gets WM_REMOVABLE); starts the helper
/// thread the first time.
pub fn start(notify: HWND) {
    NOTIFY.store(notify.0 as isize, Ordering::SeqCst);
    let Ok(mut tx) = TX.lock() else { return };
    if tx.is_some() {
        drop(tx);
        post();
        return;
    }
    let (s, r) = channel();
    let spawned = std::thread::Builder::new().name("removable".into()).spawn(move || run(r));
    let ok = spawned.is_ok();
    if ok {
        *tx = Some(s);
    }
    drop(tx);
    if !ok {
        crate::log!("removable: no helper thread");
    }
}

/// The main bar went away: nothing is posted until the next one starts.
pub fn detach() {
    NOTIFY.store(0, Ordering::SeqCst);
}

/// The bar heard a WM_DEVICECHANGE broadcast (`event`, and the broadcast
/// header's device type): drive letters that came or went are read again.
pub fn device_broadcast(event: usize, devicetype: u32) {
    if model::letters_changed(event, devicetype) {
        send(Cmd::Changed);
    }
}

fn post() {
    let h = NOTIFY.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_REMOVABLE, WPARAM(0), LPARAM(0));
        }
    }
}

fn send(c: Cmd) -> bool {
    TX.lock().ok().and_then(|t| t.as_ref().map(|t| t.send(c).is_ok())).unwrap_or(false)
}

/// Some removable storage is connected (the bar button's dot).
pub fn present() -> bool {
    STATE.lock().map(|s| !s.present.is_empty()).unwrap_or(false)
}

/// The Control Center's rows now (empty: no section).
pub fn rows() -> Vec<Shown> {
    let now = Instant::now();
    match STATE.lock() {
        Ok(mut s) => {
            let State { present, status } = &mut *s;
            model::prune(status, present, now);
            model::shown(present, status, now)
        }
        Err(_) => Vec::new(),
    }
}

/// How long until a row's note runs out (the panel redraws then).
pub fn next_change() -> Option<Duration> {
    STATE.lock().ok().and_then(|s| model::next_change(&s.status, Instant::now()))
}

/// Ejects connection `c` of a device on the helper thread; its row says
/// "Ejecting…" at once (WM_REMOVABLE follows with the answer). Nothing
/// while one is on its way, or when that connection is not present.
pub fn eject(c: &Conn) {
    let Ok(mut s) = STATE.lock() else { return };
    if s.status.iter().any(|(k, st)| k == c && *st == Status::Ejecting) || !model::still_there(&s.present, c) {
        return;
    }
    s.status.retain(|(k, _)| k != c);
    s.status.push((c.clone(), Status::Ejecting));
    drop(s);
    if !send(Cmd::Eject(c.clone())) {
        set_status(c, Some(Status::Refused { at: Instant::now(), why: model::Refusal::Failed }));
    }
    post();
}

/// Sets (or with None clears) the status of connection `c`.
fn set_status(c: &Conn, st: Option<Status>) {
    if let Ok(mut s) = STATE.lock() {
        s.status.retain(|(k, _)| k != c);
        if let Some(st) = st {
            s.status.push((c.clone(), st));
        }
    }
}

/// The symbolic link a device interface notice names (lowercase).
unsafe fn notice_path(data: *const CM_NOTIFY_EVENT_DATA, size: u32) -> Option<String> {
    let head = std::mem::offset_of!(CM_NOTIFY_EVENT_DATA, u) + std::mem::offset_of!(windows::Win32::Devices::DeviceAndDriverInstallation::CM_NOTIFY_EVENT_DATA_0_0, SymbolicLink);
    if data.is_null() || (size as usize) <= head {
        return None;
    }
    let chars = (size as usize - head) / 2;
    let p = (data as *const u8).add(head) as *const u16;
    let s = std::slice::from_raw_parts(p, chars);
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    Some(String::from_utf16_lossy(&s[..n]).to_lowercase())
}

/// Called by cfgmgr32 on its own thread: notes a removal's path (or that
/// one was lost: too many, or its path unreadable), wakes the helper.
unsafe extern "system" fn on_change(_: HCMNOTIFICATION, _: *const core::ffi::c_void, action: CM_NOTIFY_ACTION, data: *const CM_NOTIFY_EVENT_DATA, size: u32) -> u32 {
    if action == CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL {
        if let Ok(mut r) = REMOVED.lock() {
            match notice_path(data, size) {
                Some(p) if r.paths.len() < MAX_REMOVED => r.paths.push(p),
                _ => r.overflowed = true,
            }
        }
    }
    if action == CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL || action == CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL {
        send(Cmd::Changed);
    }
    0
}

/// Listens for disks, CD drives and volumes coming and going (for the
/// process's life). False when any of the three could not be registered
/// (then none is kept).
fn listen() -> bool {
    let mut kept: Vec<HCMNOTIFICATION> = Vec::new();
    for class in [GUID_DEVINTERFACE_DISK, GUID_DEVINTERFACE_CDROM, GUID_DEVINTERFACE_VOLUME] {
        let filter = CM_NOTIFY_FILTER {
            cbSize: std::mem::size_of::<CM_NOTIFY_FILTER>() as u32,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            u: CM_NOTIFY_FILTER_0 { DeviceInterface: CM_NOTIFY_FILTER_0_0 { ClassGuid: class } },
            ..Default::default()
        };
        let mut h = HCMNOTIFICATION::default();
        let cr = unsafe { CM_Register_Notification(&filter, None, Some(on_change), &mut h) };
        if cr != CR_SUCCESS {
            for k in kept {
                unsafe {
                    let _ = CM_Unregister_Notification(k);
                }
            }
            return false;
        }
        kept.push(h);
    }
    true
}

/// The helper's own memory between scans.
struct Helper {
    /// Device notices are registered: without them nothing is offered
    /// (and nothing is scanned).
    notifying: bool,
    /// Connection numbers given so far.
    counter: u64,
    /// The last scan's notes (logged again only when they change).
    notes: Vec<String>,
}

/// Scans, numbers the connections, keeps the result (dropping statuses of
/// connections that are over) and tells the bar; returns the devices.
/// Nothing slow or logged under the lock.
fn refresh(h: &mut Helper) -> Vec<Device> {
    let removed: model::Notices = REMOVED.lock().map(|mut r| std::mem::take(&mut *r)).unwrap_or_default();
    let scan = if h.notifying { scan::scan() } else { scan::Scan { devices: Vec::new(), notes: Vec::new() } };
    let mut devs = model::offered(scan.devices, h.notifying);
    let prev = STATE.lock().map(|s| s.present.clone()).unwrap_or_default();
    model::assign_gens(&prev, &mut devs, &removed, &mut h.counter);
    let changed = prev != devs;
    if let Ok(mut s) = STATE.lock() {
        s.present = devs.clone();
        let State { present, status } = &mut *s;
        model::prune(status, present, Instant::now());
    }
    if removed.overflowed {
        crate::log!("removable: removal notices were lost: every device is taken as new");
    }
    if changed {
        let list: Vec<String> = devs.iter().map(|d| format!("{} [{}]", d.name, d.letters().join(" "))).collect();
        crate::log!("removable: {} device(s): {}", list.len(), list.join(", "));
        post();
    }
    if scan.notes != h.notes {
        for n in &scan.notes {
            crate::log!("removable: {n}");
        }
        h.notes = scan.notes;
    }
    devs
}

/// The earlier of two optional deadlines.
fn earlier(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

fn run(rx: Receiver<Cmd>) {
    let mut h = Helper { notifying: listen(), counter: 0, notes: Vec::new() };
    // Registering again after a failure: when, and how many tries so far
    // (model::LISTEN_RETRIES; then not again).
    let mut tries = 0usize;
    let mut relisten = None;
    if !h.notifying {
        crate::log!("removable: no device notifications: nothing is offered until they can be registered");
        relisten = model::LISTEN_RETRIES.first().map(|d| Instant::now() + *d);
    }
    // Late looks for drive letters: when the next one is due, and how many
    // were made since the last event.
    let devs = refresh(&mut h);
    let mut looks = 0usize;
    let mut late = model::next_look(&devs, looks).map(|d| Instant::now() + d);
    let mut queue: Vec<Cmd> = Vec::new();
    loop {
        let cmd = match queue.pop() {
            Some(c) => c,
            None => match earlier(late, relisten) {
                Some(t) => match rx.recv_timeout(t.saturating_duration_since(Instant::now())) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => {
                        let now = Instant::now();
                        if relisten.is_some_and(|r| r <= now) {
                            tries += 1;
                            relisten = None;
                            if listen() {
                                h.notifying = true;
                                crate::log!("removable: device notifications registered (try {})", tries + 1);
                                let devs = refresh(&mut h);
                                looks = 0;
                                late = model::next_look(&devs, looks).map(|d| Instant::now() + d);
                            } else {
                                relisten = model::LISTEN_RETRIES.get(tries).map(|d| now + *d);
                            }
                        }
                        if late.is_some_and(|l| l <= now) {
                            looks += 1;
                            let devs = refresh(&mut h);
                            late = model::next_look(&devs, looks).map(|d| Instant::now() + d);
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(c) => c,
                    Err(_) => return,
                },
            },
        };
        match cmd {
            Cmd::Changed => {
                // The rest of the burst first (ejects asked meanwhile wait).
                loop {
                    match rx.recv_timeout(SETTLE) {
                        Ok(Cmd::Changed) => {}
                        Ok(c) => queue.insert(0, c),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                let devs = refresh(&mut h);
                looks = 0;
                late = model::next_look(&devs, looks).map(|d| Instant::now() + d);
            }
            Cmd::Eject(c) => {
                // The device as it is right now (a full scan: still the
                // same connection, still offered, its scope checked again),
                // and no removal notice for its disks or volumes, nor any
                // lost, while that scan ran. Else "Couldn't eject — try
                // again".
                let devs = refresh(&mut h);
                let mid_scan: model::Notices = REMOVED.lock().map(|r| r.clone()).unwrap_or_default();
                let device = match model::eject_go(&devs, &c, &mid_scan) {
                    Ok(d) => d,
                    Err(why) => {
                        crate::log!("removable: eject not done: the device changed or is no longer offered");
                        set_status(&c, Some(Status::Refused { at: Instant::now(), why }));
                        post();
                        continue;
                    }
                };
                let started = Instant::now();
                let out = scan::eject(&c.id);
                crate::log!("removable: eject of {}: {:?} after {} ms", device.name, out, started.elapsed().as_millis());
                let now = Instant::now();
                match out {
                    Ok(()) => {set_status(&c, Some(Status::Safe { at: now, device }));crate::companion::cue(crate::companion::Cue::Removable);},
                    Err(why) => set_status(&c, Some(Status::Refused { at: now, why })),
                }
                refresh(&mut h);
                post();
            }
        }
    }
}
