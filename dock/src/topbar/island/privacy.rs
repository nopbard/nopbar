//! Which apps use the camera, the microphone or screen capture right now,
//! from Windows' own privacy records (the ones Settings > Privacy shows):
//!
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\
//! ConsentStore\<capability>\<app>` with QWORD values `LastUsedTimeStart`
//! and `LastUsedTimeStop`. An app that has started and not stopped (Stop is
//! 0) is using it now. Packaged apps are keyed by their family name, other
//! apps under `NonPackaged` by their exe path with `#` for `\`.
//!
//! Event driven: one `service` watch arms RegNotifyChangeKeyValue on each
//! capability key (with its subtree) and reads them again only when
//! Windows writes there; it posts WM_ISLAND_PRIVACY to the primary top bar
//! when the list changed. Nothing is polled; the camera and microphone are
//! never touched.

use std::sync::Mutex;
use std::time::Duration;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ,
    REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, RRF_RT_REG_QWORD,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::model::{Sensor, Use};

/// Posted to the primary bar: `current()` changed.
pub const WM_ISLAND_PRIVACY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 47;

const STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";
/// The capabilities watched, with what they are.
const CAPS: [(&str, Sensor); 3] = [("webcam", Sensor::Camera), ("microphone", Sensor::Microphone), ("graphicsCaptureProgrammatic", Sensor::Screen)];
/// Apps read per capability at most (a registry that grew huge cannot
/// make one read slow).
const MAX_APPS: usize = 512;

// ---------------------------------------------------------------- pure

/// One app's record under a capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub sensor: Sensor,
    /// The subkey's name (family name, or the exe path with '#').
    pub key: String,
    pub packaged: bool,
    pub start: Option<u64>,
    pub stop: Option<u64>,
}

/// In use now: started, and its stop time is 0.
pub fn in_use(start: Option<u64>, stop: Option<u64>) -> bool {
    start.is_some_and(|s| s > 0) && stop == Some(0)
}

/// A readable name for a record's key: the exe's name without ".exe" for
/// other apps ("C:#Program Files#Zoom#bin#Zoom.exe" -> "Zoom"), the
/// package's own name split into words for packaged ones
/// ("Microsoft.WindowsCamera_8wekyb3d8bbwe" -> "Windows Camera").
pub fn app_name(key: &str, packaged: bool) -> String {
    if !packaged {
        let file = key.rsplit(['#', '\\', '/']).next().unwrap_or(key);
        let stem = match file.len().checked_sub(4) {
            Some(n) if file.is_char_boundary(n) && file[n..].eq_ignore_ascii_case(".exe") => &file[..n],
            _ => file,
        };
        return if stem.is_empty() { key.to_string() } else { stem.to_string() };
    }
    // Family name: <name>_<publisher hash>; the name's last dotted part.
    let name = key.rsplit_once('_').map(|(n, _)| n).unwrap_or(key);
    let last = name.rsplit('.').next().filter(|s| !s.is_empty()).unwrap_or(name);
    words(last)
}

/// "WindowsCamera" -> "Windows Camera", "MSTeams" -> "MS Teams".
fn words(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &ch) in c.iter().enumerate() {
        if i > 0 && ch.is_uppercase() {
            let prev = c[i - 1];
            let next_lower = c.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || (prev.is_uppercase() && next_lower) {
                out.push(' ');
            }
        }
        out.push(ch);
    }
    out
}

/// The uses in a set of records, sorted, each app and sensor once.
pub fn scan(entries: &[Entry]) -> Vec<Use> {
    // Identified by the record's key (package or full exe path), named after.
    let mut out: Vec<Use> = entries.iter().filter(|e| in_use(e.start, e.stop)).map(|e| Use { sensor: e.sensor, id: e.key.to_lowercase(), app: app_name(&e.key, e.packaged) }).collect();
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------- reading

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open(parent: HKEY, path: &str, access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<Key> {
    let mut k = HKEY::default();
    unsafe { RegOpenKeyExW(parent, &HSTRING::from(path), None, access, &mut k) }.ok().ok()?;
    Some(Key(k))
}

/// Subkey names of `k` (at most MAX_APPS).
fn subkeys(k: &Key) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = [0u16; 512];
    for i in 0..MAX_APPS as u32 {
        let mut len = buf.len() as u32;
        let r = unsafe { RegEnumKeyExW(k.0, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
        if r != ERROR_SUCCESS {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}

fn qword(k: &Key, sub: &str, name: &str) -> Option<u64> {
    let mut v = 0u64;
    let mut size = 8u32;
    let r = unsafe {
        RegGetValueW(k.0, &HSTRING::from(sub), &HSTRING::from(name), RRF_RT_REG_QWORD, None, Some(&mut v as *mut u64 as *mut _), Some(&mut size))
    };
    (r == ERROR_SUCCESS).then_some(v)
}

/// Every record of the watched capabilities.
fn read_entries() -> Vec<Entry> {
    let mut out = Vec::new();
    for (cap, sensor) in CAPS {
        let Some(k) = open(HKEY_CURRENT_USER, &format!(r"{STORE}\{cap}"), KEY_READ) else { continue };
        for name in subkeys(&k) {
            if name.eq_ignore_ascii_case("NonPackaged") {
                let Some(np) = open(k.0, &name, KEY_READ) else { continue };
                for app in subkeys(&np) {
                    out.push(Entry { sensor, start: qword(&np, &app, "LastUsedTimeStart"), stop: qword(&np, &app, "LastUsedTimeStop"), key: app, packaged: false });
                }
            } else {
                out.push(Entry { sensor, start: qword(&k, &name, "LastUsedTimeStart"), stop: qword(&k, &name, "LastUsedTimeStop"), key: name, packaged: true });
            }
        }
    }
    out
}

// ---------------------------------------------------------------- watching

/// What is in use now, and the generation it belongs to: a watch writes it
/// only while it is the current one (checked under this lock, which
/// `start` / `stop` take to move to a new generation).
struct State {
    gen: u64,
    uses: Vec<Use>,
    target: isize,
    /// The current watch (to wake it on stop).
    id: Option<crate::service::Id>,
}

static STATE: Mutex<State> = Mutex::new(State { gen: 0, uses: Vec::new(), target: 0, id: None });

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// What is in use now (as last read).
pub fn current() -> Vec<Use> {
    state().uses.clone()
}

/// One armed key and its (manual-reset) event.
struct Armed(HKEY, HANDLE);

impl Drop for Armed {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
            let _ = CloseHandle(self.1);
        }
    }
}

/// One notification of `filter` on `path` (and its subtree if asked).
fn arm(path: &str, subtree: bool, filter: windows::Win32::System::Registry::REG_NOTIFY_FILTER) -> Option<Armed> {
    let k = open(HKEY_CURRENT_USER, path, KEY_NOTIFY)?;
    let ev = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok()?;
    // Owned from here (dropped together).
    let raw = k.0;
    std::mem::forget(k);
    let armed = Armed(raw, ev);
    unsafe { RegNotifyChangeKeyValue(raw, subtree, filter, Some(ev), true) }.ok().ok()?;
    Some(armed)
}

/// The parent of a registry path ("" at the top).
fn parent(path: &str) -> &str {
    path.rsplit_once('\\').map(|(p, _)| p).unwrap_or("")
}

/// What to watch: each capability key that exists (values, with its
/// subtree), and the store itself for capability keys created or deleted
/// later (its subkeys' names); if the store does not exist yet, its
/// nearest existing ancestor (for a subkey appearing). Pure over `exists`.
pub fn plan(exists: impl Fn(&str) -> bool) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = CAPS.iter().map(|(cap, _)| format!(r"{STORE}\{cap}")).filter(|p| exists(p)).map(|p| (p, true)).collect();
    let mut p = STORE;
    while !p.is_empty() {
        if exists(p) {
            out.push((p.to_string(), false));
            break;
        }
        p = parent(p);
    }
    out
}

struct Watch {
    gen: u64,
    armed: Vec<Armed>,
}

// Registry keys and events are kernel handles: usable from any thread.
unsafe impl Send for Watch {}

impl Watch {
    fn current(&self) -> bool {
        state().gen == self.gen
    }

    fn arm_and_read(&mut self) {
        self.armed.clear();
        let exists = |p: &str| open(HKEY_CURRENT_USER, p, KEY_NOTIFY).is_some();
        for (path, values) in plan(exists) {
            let filter = if values { REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_CHANGE_NAME } else { REG_NOTIFY_CHANGE_NAME };
            self.armed.extend(arm(&path, values, filter));
        }
        // Read after arming: a write in between is not missed.
        let now = scan(&read_entries());
        crate::companion::calm(crate::companion::Calm::Meeting,now.iter().any(|u|matches!(u.sensor,Sensor::Camera|Sensor::Microphone)));
        let target = {
            let mut st = state();
            if st.gen != self.gen || st.uses == now {
                return;
            }
            st.uses = now;
            st.target
        };
        if target != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(target as *mut _)), WM_ISLAND_PRIVACY, WPARAM(0), LPARAM(0));
            }
        }
    }
}

impl crate::service::Watch for Watch {
    fn name(&self) -> &'static str {
        "island privacy"
    }
    fn handles(&self) -> Vec<HANDLE> {
        self.armed.iter().map(|a| a.1).collect()
    }
    fn wake(&mut self, why: crate::service::Wake) -> crate::service::Next {
        use crate::service::{Next, Wake};
        // An older generation ends (a newer watch, or none, took over).
        if !self.current() {
            return Next::Done;
        }
        match why {
            Wake::Start | Wake::Timer => {
                self.arm_and_read();
                if self.armed.is_empty() {
                    // Not even HKCU\Software could be opened: look again
                    // now and then, rarely.
                    return Next::Sleep(Duration::from_secs(300));
                }
                Next::Wait
            }
            // An app starts or stops with a few writes in a row (a key
            // created: the next arming includes it): read once they are in.
            Wake::Signaled(_) | Wake::Poked => {
                self.armed.clear();
                Next::Sleep(Duration::from_millis(120))
            }
        }
    }
}

/// Starts watching (a new generation; an older watch ends by itself) and
/// posts WM_ISLAND_PRIVACY to `hwnd`. Already running for `hwnd`: nothing.
pub fn start(hwnd: HWND) {
    let (gen, old) = {
        let mut st = state();
        if st.id.is_some() && st.target == hwnd.0 as isize {
            return;
        }
        st.gen += 1;
        st.target = hwnd.0 as isize;
        st.uses.clear();
        (st.gen, st.id.take())
    };
    if let Some(old) = old {
        crate::service::poke(old);
    }
    let id = crate::service::add(Box::new(Watch { gen, armed: Vec::new() }));
    let mut st = state();
    if st.gen == gen {
        st.id = id;
    } else if let Some(id) = id {
        // Stopped or restarted meanwhile: this one ends on its next wake.
        crate::service::poke(id);
    }
}

/// Stops watching and forgets what was in use.
pub fn stop() {
    crate::companion::calm(crate::companion::Calm::Meeting,false);
    let old = {
        let mut st = state();
        st.gen += 1;
        st.target = 0;
        st.uses.clear();
        st.id.take()
    };
    if let Some(id) = old {
        crate::service::poke(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(sensor: Sensor, key: &str, packaged: bool, start: Option<u64>, stop: Option<u64>) -> Entry {
        Entry { sensor, key: key.into(), packaged, start, stop }
    }

    #[test]
    fn in_use_means_started_and_not_stopped() {
        assert!(in_use(Some(133_000_000_000_000_000), Some(0)));
        assert!(!in_use(Some(133_000_000_000_000_000), Some(133_000_000_000_000_100)), "stopped");
        assert!(!in_use(Some(0), Some(0)), "never used");
        assert!(!in_use(None, Some(0)));
        assert!(!in_use(Some(5), None), "no stop value: not known to be on");
    }

    #[test]
    fn names_from_keys() {
        assert_eq!(app_name(r"C:#Program Files#Zoom#bin#Zoom.exe", false), "Zoom");
        assert_eq!(app_name(r"C:#Users#me#AppData#Local#Discord#app-1.0#Discord.EXE", false), "Discord");
        assert_eq!(app_name("obs64", false), "obs64");
        assert_eq!(app_name("Microsoft.WindowsCamera_8wekyb3d8bbwe", true), "Windows Camera");
        assert_eq!(app_name("MSTeams_8wekyb3d8bbwe", true), "MS Teams");
        assert_eq!(app_name("Microsoft.SkypeApp_kzf8qxf38zg5c", true), "Skype App");
        assert_eq!(app_name("noseparator", true), "noseparator");
        assert_eq!(app_name("한글앱.exe", false), "한글앱");
    }

    /// A fixture shaped like a real ConsentStore: a few packaged apps, the
    /// NonPackaged ones, used before and in use now.
    #[test]
    fn scan_a_fixture() {
        let t0 = Some(133_700_000_000_000_000);
        let t1 = Some(133_700_000_100_000_000);
        let fixture = [
            e(Sensor::Camera, "Microsoft.WindowsCamera_8wekyb3d8bbwe", true, t0, t1),
            e(Sensor::Camera, r"C:#Program Files#Zoom#bin#Zoom.exe", false, t1, Some(0)),
            e(Sensor::Microphone, r"C:#Program Files#Zoom#bin#Zoom.exe", false, t1, Some(0)),
            e(Sensor::Microphone, r"C:#Program Files#Zoom#bin#Zoom.exe", false, t1, Some(0)),
            e(Sensor::Microphone, "MSTeams_8wekyb3d8bbwe", true, t0, Some(0)),
            e(Sensor::Microphone, "Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe", true, None, None),
            e(Sensor::Screen, r"C:#Program Files#obs-studio#bin#64bit#obs64.exe", false, t0, t1),
        ];
        let uses = scan(&fixture);
        let zoom = r"c:#program files#zoom#bin#zoom.exe";
        assert_eq!(
            uses,
            [
                Use { sensor: Sensor::Camera, id: zoom.into(), app: "Zoom".into() },
                Use { sensor: Sensor::Microphone, id: zoom.into(), app: "Zoom".into() },
                Use { sensor: Sensor::Microphone, id: "msteams_8wekyb3d8bbwe".into(), app: "MS Teams".into() },
            ]
        );
        // Two exes of the same name in different folders stay two.
        let two = [e(Sensor::Camera, r"C:#A#Zoom.exe", false, t1, Some(0)), e(Sensor::Camera, r"D:#B#Zoom.exe", false, t1, Some(0))];
        assert_eq!(scan(&two).len(), 2);
        assert!(scan(&fixture[..1]).is_empty());
    }

    #[test]
    fn the_plan_watches_the_store_for_keys_made_later() {
        let cap = |c: &str| format!(r"{STORE}\{c}");
        // Only the microphone key exists: it, and the store for the others.
        let some = plan(|p| p == cap("microphone") || p == STORE || STORE.starts_with(p));
        assert_eq!(some, [(cap("microphone"), true), (STORE.to_string(), false)]);
        // No store yet: its nearest existing ancestor.
        let cam = r"Software\Microsoft\Windows\CurrentVersion";
        let none = plan(|p| p == cam || cam.starts_with(p));
        assert_eq!(none, [(cam.to_string(), false)]);
        assert!(plan(|_| false).is_empty());
    }
}
