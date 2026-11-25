//! Time zones for calendar feeds (crate::agenda): which UTC instant a
//! wall-clock time in a named zone is, and the PC's own zone both ways.
//!
//! Feeds name zones two ways: IANA names ("Europe/Berlin", Google) and
//! Windows names ("W. Europe Standard Time", Outlook). Both go through the
//! ICU that ships with Windows (icu.dll), which knows each zone's whole
//! history of offsets and maps Windows names to IANA ones. icu.dll is
//! loaded by hand (never imported), so a PC without it still starts: the
//! parser then falls back to the feed's own VTIMEZONE rules, and the PC's
//! own zone to Windows' current rules.
//!
//! A wall time that happens twice (the hour the clocks fall back) is the
//! first one; a time that never happens (the hour the clocks spring
//! forward) uses the offset from before the gap (RFC 5545 3.3.5): 02:30 on
//! the US spring day is 03:30 daylight time.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCSTR};
use windows::Win32::Foundation::{HMODULE, SYSTEMTIME};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

const DAY: i64 = 86_400;

/// Time zone lookups for the parser.
pub trait Zones {
    /// UTC seconds since 1970 for the wall-clock time `local` (seconds
    /// since 1970 as if it were UTC) in zone `tzid` (an IANA name like
    /// "Europe/Berlin" or a Windows name like "W. Europe Standard Time").
    /// None: the zone is unknown.
    fn to_utc(&self, tzid: &str, local: i64) -> Option<i64>;
    /// The same for the PC's own zone (floating times).
    fn local_to_utc(&self, local: i64) -> i64;
    /// The wall-clock time in zone `tzid` at UTC instant `utc`. None: the
    /// zone is unknown, or this source cannot tell (then every wall time
    /// counts as one that happens).
    fn utc_to_zone(&self, _tzid: &str, _utc: i64) -> Option<i64> {
        None
    }
    /// The same for the PC's own zone.
    fn utc_to_local(&self, _utc: i64) -> Option<i64> {
        None
    }
}

// ---- civil dates ---------------------------------------------------------------

/// Days since 1970-01-01 of a date (proleptic Gregorian).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let (y, m, d) = (y as i64, m as i64, d as i64);
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day since 1970-01-01 (year, month 1-12, day 1-31).
pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y.clamp(i32::MIN as i64, i32::MAX as i64) as i32, m as u32, d as u32)
}

/// Day of the week of a day since 1970-01-01: 0 = Sunday.
pub fn weekday(days: i64) -> u32 {
    // 1970-01-01 was a Thursday.
    (days + 4).rem_euclid(7) as u32
}

/// Days in month `m` (1-12) of year `y`.
pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// ---- ICU -------------------------------------------------------------------------

type UCal = *mut c_void;

/// The few ICU calendar calls used, found in icu.dll at run time.
struct Icu {
    open: unsafe extern "C" fn(*const u16, i32, *const u8, i32, *mut i32) -> UCal,
    close: unsafe extern "C" fn(UCal),
    set_attribute: unsafe extern "C" fn(UCal, i32, i32),
    set_date_time: unsafe extern "C" fn(UCal, i32, i32, i32, i32, i32, i32, *mut i32),
    get_millis: unsafe extern "C" fn(UCal, *mut i32) -> f64,
    set_millis: unsafe extern "C" fn(UCal, f64, *mut i32),
    get: unsafe extern "C" fn(UCal, i32, *mut i32) -> i32,
    clear: unsafe extern "C" fn(UCal),
    for_windows_id: unsafe extern "C" fn(*const u16, i32, *const u8, *mut u16, i32, *mut i32) -> i32,
    canonical: unsafe extern "C" fn(*const u16, i32, *mut u16, i32, *mut i8, *mut i32) -> i32,
}

// ICU's constants (ucal.h).
const UCAL_GREGORIAN: i32 = 1;
const UCAL_REPEATED_WALL_TIME: i32 = 3;
const UCAL_SKIPPED_WALL_TIME: i32 = 4;
const UCAL_WALLTIME_FIRST: i32 = 1;
const UCAL_WALLTIME_LAST: i32 = 0;
const UCAL_ZONE_OFFSET: i32 = 15;
const UCAL_DST_OFFSET: i32 = 16;

/// Function `name` (ending in a NUL) of module `h`, as the type it has.
unsafe fn proc_at<T: Copy>(h: HMODULE, name: &str) -> Option<T> {
    let f = GetProcAddress(h, PCSTR(name.as_ptr()))?;
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of_val(&f));
    Some(std::mem::transmute_copy(&f))
}

/// icu.dll from System32, loaded once; None where Windows has none.
fn icu() -> Option<&'static Icu> {
    static ICU: OnceLock<Option<Icu>> = OnceLock::new();
    ICU.get_or_init(|| unsafe {
        let h = LoadLibraryExW(w!("icu.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        macro_rules! sym {
            ($n:literal) => {
                proc_at(h, concat!($n, "\0"))?
            };
        }
        Some(Icu {
            open: sym!("ucal_open"),
            close: sym!("ucal_close"),
            set_attribute: sym!("ucal_setAttribute"),
            set_date_time: sym!("ucal_setDateTime"),
            get_millis: sym!("ucal_getMillis"),
            set_millis: sym!("ucal_setMillis"),
            get: sym!("ucal_get"),
            clear: sym!("ucal_clear"),
            for_windows_id: sym!("ucal_getTimeZoneIDForWindowsID"),
            canonical: sym!("ucal_getCanonicalTimeZoneID"),
        })
    })
    .as_ref()
}

/// An ICU calendar set to one zone.
struct Cal(UCal);

// An ICU calendar is plain memory, usable from any one thread at a time.
unsafe impl Send for Cal {}

impl Drop for Cal {
    fn drop(&mut self) {
        if let Some(i) = icu() {
            unsafe { (i.close)(self.0) };
        }
    }
}

impl Cal {
    /// A calendar in zone `id` (a name ICU knows, see `resolve`).
    fn open(id: &str) -> Option<Cal> {
        let i = icu()?;
        let wide: Vec<u16> = id.encode_utf16().collect();
        let mut st = 0i32;
        let c = unsafe { (i.open)(wide.as_ptr(), wide.len() as i32, std::ptr::null(), UCAL_GREGORIAN, &mut st) };
        if c.is_null() {
            return None;
        }
        let cal = Cal(c);
        if st > 0 {
            return None;
        }
        unsafe {
            (i.set_attribute)(c, UCAL_REPEATED_WALL_TIME, UCAL_WALLTIME_FIRST);
            (i.set_attribute)(c, UCAL_SKIPPED_WALL_TIME, UCAL_WALLTIME_LAST);
        }
        Some(cal)
    }

    fn wall_to_utc(&self, local: i64) -> Option<i64> {
        let i = icu()?;
        let (y, m, d) = civil_from_days(local.div_euclid(DAY));
        let s = local.rem_euclid(DAY) as i32;
        let mut st = 0i32;
        let ms = unsafe {
            (i.clear)(self.0);
            (i.set_date_time)(self.0, y, m as i32 - 1, d as i32, s / 3600, s / 60 % 60, s % 60, &mut st);
            (i.get_millis)(self.0, &mut st)
        };
        (st <= 0 && ms.is_finite()).then(|| (ms / 1000.0).floor() as i64)
    }

    fn utc_to_wall(&self, utc: i64) -> Option<i64> {
        let i = icu()?;
        let mut st = 0i32;
        let off = unsafe {
            (i.set_millis)(self.0, utc as f64 * 1000.0, &mut st);
            (i.get)(self.0, UCAL_ZONE_OFFSET, &mut st) as i64 + (i.get)(self.0, UCAL_DST_OFFSET, &mut st) as i64
        };
        (st <= 0).then(|| utc + off / 1000)
    }
}

/// Reads a UTF-16 answer of `n` units, if the call worked.
fn answer(buf: &[u16], n: i32, st: i32) -> Option<String> {
    (st <= 0 && n > 0 && (n as usize) <= buf.len()).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// The ICU zone id for a name a feed gives: a Windows name mapped to its
/// IANA zone, or an IANA name ICU knows. Quotes and path prefixes such as
/// "/mozilla.org/20050126_1/America/New_York" are taken off. None: unknown.
fn resolve(name: &str) -> Option<String> {
    let i = icu()?;
    let name = name.trim().trim_matches('"').trim();
    if name.is_empty() || name.len() > 200 {
        return None;
    }
    // The name itself, then what follows each '/' (a path prefix).
    let mut cands = vec![name];
    cands.extend(name.match_indices('/').map(|(k, _)| &name[k + 1..]).filter(|s| !s.is_empty()));
    for c in cands {
        let wide: Vec<u16> = c.encode_utf16().collect();
        let mut buf = [0u16; 128];
        let mut st = 0i32;
        let n = unsafe { (i.for_windows_id)(wide.as_ptr(), wide.len() as i32, std::ptr::null(), buf.as_mut_ptr(), buf.len() as i32, &mut st) };
        if let Some(id) = answer(&buf, n, st) {
            return Some(id);
        }
        let mut st = 0i32;
        let mut system = 0i8;
        let n = unsafe { (i.canonical)(wide.as_ptr(), wide.len() as i32, buf.as_mut_ptr(), buf.len() as i32, &mut system, &mut st) };
        if let Some(id) = answer(&buf, n, st) {
            if id != "Etc/Unknown" {
                return Some(id);
            }
        }
    }
    None
}

/// The real zones: ICU, one calendar per zone name, opened when first
/// asked for. Lives on one worker thread at a time.
#[derive(Default)]
pub struct SystemZones {
    cals: RefCell<HashMap<String, Option<Cal>>>,
}

impl SystemZones {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Zones for SystemZones {
    fn to_utc(&self, tzid: &str, local: i64) -> Option<i64> {
        let mut cals = self.cals.borrow_mut();
        if !cals.contains_key(tzid) {
            // A feed names a handful of zones; a junk feed is kept small.
            if cals.len() > 256 {
                cals.clear();
            }
            cals.insert(tzid.to_string(), resolve(tzid).and_then(|id| Cal::open(&id)));
        }
        cals.get(tzid)?.as_ref()?.wall_to_utc(local)
    }

    fn local_to_utc(&self, local: i64) -> i64 {
        utc_from_local(local)
    }

    fn utc_to_zone(&self, tzid: &str, utc: i64) -> Option<i64> {
        // Asked after to_utc for the same zone: its calendar is open.
        self.cals.borrow().get(tzid)?.as_ref()?.utc_to_wall(utc)
    }

    fn utc_to_local(&self, utc: i64) -> Option<i64> {
        Some(local_from_utc(utc))
    }
}

// ---- the PC's own zone ---------------------------------------------------------------

/// Windows' DYNAMIC_TIME_ZONE_INFORMATION (declared here: the windows
/// crate's System_Time feature is not built).
#[repr(C)]
struct DynamicTzi {
    bias: i32,
    standard_name: [u16; 32],
    standard_date: SYSTEMTIME,
    standard_bias: i32,
    daylight_name: [u16; 32],
    daylight_date: SYSTEMTIME,
    daylight_bias: i32,
    key_name: [u16; 128],
    dynamic_disabled: u8,
}

/// kernel32's time zone calls, found at run time.
struct Kernel {
    dynamic: unsafe extern "system" fn(*mut DynamicTzi) -> u32,
    to_local: unsafe extern "system" fn(*const c_void, *const SYSTEMTIME, *mut SYSTEMTIME) -> i32,
    to_utc: unsafe extern "system" fn(*const c_void, *const SYSTEMTIME, *mut SYSTEMTIME) -> i32,
}

fn kernel() -> Option<&'static Kernel> {
    static K: OnceLock<Option<Kernel>> = OnceLock::new();
    K.get_or_init(|| unsafe {
        let h = GetModuleHandleW(w!("kernel32.dll")).ok()?;
        macro_rules! sym {
            ($n:literal) => {
                proc_at(h, concat!($n, "\0"))?
            };
        }
        Some(Kernel { dynamic: sym!("GetDynamicTimeZoneInformation"), to_local: sym!("SystemTimeToTzSpecificLocalTime"), to_utc: sym!("TzSpecificLocalTimeToSystemTime") })
    })
    .as_ref()
}

/// The Windows name of the PC's zone now ("Korea Standard Time").
fn windows_zone() -> Option<String> {
    let k = kernel()?;
    let mut tzi: DynamicTzi = unsafe { std::mem::zeroed() };
    if unsafe { (k.dynamic)(&mut tzi) } == u32::MAX {
        return None;
    }
    let n = tzi.key_name.iter().position(|&c| c == 0).unwrap_or(tzi.key_name.len());
    (n > 0).then(|| String::from_utf16_lossy(&tzi.key_name[..n]))
}

/// The PC's zone as an ICU calendar, opened again when Windows' zone
/// changes (the user moved, or set another one).
fn with_local<R>(f: impl FnOnce(&Cal) -> Option<R>) -> Option<R> {
    static LOCAL: Mutex<Option<(String, Option<Cal>)>> = Mutex::new(None);
    let name = windows_zone()?;
    let mut g = LOCAL.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().map(|(n, _)| n != &name).unwrap_or(true) {
        let cal = resolve(&name).and_then(|id| Cal::open(&id));
        *g = Some((name, cal));
    }
    g.as_ref()?.1.as_ref().and_then(f)
}

fn to_systemtime(t: i64) -> SYSTEMTIME {
    let (y, m, d) = civil_from_days(t.div_euclid(DAY));
    let s = t.rem_euclid(DAY);
    SYSTEMTIME {
        wYear: y.clamp(1601, 30000) as u16,
        wMonth: m as u16,
        wDay: d as u16,
        wDayOfWeek: weekday(t.div_euclid(DAY)) as u16,
        wHour: (s / 3600) as u16,
        wMinute: (s / 60 % 60) as u16,
        wSecond: (s % 60) as u16,
        wMilliseconds: 0,
    }
}

fn from_systemtime(st: &SYSTEMTIME) -> i64 {
    days_from_civil(st.wYear as i32, st.wMonth as u32, st.wDay as u32) * DAY + st.wHour as i64 * 3600 + st.wMinute as i64 * 60 + st.wSecond as i64
}

/// Without ICU: Windows' own conversion with the zone's current rules.
fn win32(t: i64, to_local: bool) -> Option<i64> {
    let k = kernel()?;
    let input = to_systemtime(t);
    let mut out = SYSTEMTIME::default();
    let f = if to_local { k.to_local } else { k.to_utc };
    (unsafe { f(std::ptr::null(), &input, &mut out) } != 0).then(|| from_systemtime(&out))
}

/// The PC's wall-clock time (seconds since 1970 as if it were UTC) at UTC
/// instant `utc`.
pub fn local_from_utc(utc: i64) -> i64 {
    with_local(|c| c.utc_to_wall(utc)).or_else(|| win32(utc, true)).unwrap_or(utc)
}

/// The UTC instant of the PC's wall-clock time `local`.
pub fn utc_from_local(local: i64) -> i64 {
    with_local(|c| c.wall_to_utc(local)).or_else(|| win32(local, false)).unwrap_or(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, m, d) * DAY + h * 3600 + mi * 60
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(weekday(days_from_civil(2026, 11, 1)), 0, "a Sunday");
        assert_eq!(weekday(days_from_civil(2026, 11, 5)), 4, "a Thursday");
        for d in (-800_000..800_000).step_by(37) {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
            assert!(dd >= 1 && dd <= days_in_month(y, m));
        }
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
    }

    #[test]
    fn iana_and_windows_names_agree() {
        let z = SystemZones::new();
        let l = at(2026, 7, 1, 12, 0);
        let berlin = z.to_utc("Europe/Berlin", l).expect("ICU knows Berlin");
        assert_eq!(berlin, l - 2 * 3600, "summer time");
        assert_eq!(z.to_utc("W. Europe Standard Time", l), Some(berlin));
        assert_eq!(z.to_utc("\"Europe/Berlin\"", l), Some(berlin), "quoted");
        assert_eq!(z.to_utc("/mozilla.org/20050126_1/Europe/Berlin", l), Some(berlin), "a path prefix");
        assert_eq!(z.to_utc("Europe/Berlin", at(2026, 1, 1, 12, 0)), Some(at(2026, 1, 1, 11, 0)), "winter");
        assert_eq!(z.to_utc("Korea Standard Time", l), Some(l - 9 * 3600));
        assert_eq!(z.to_utc("Asia/Seoul", l), Some(l - 9 * 3600));
        assert_eq!(z.to_utc("UTC", l), Some(l));
        assert_eq!(z.to_utc("Atlantis/Lost_City", l), None);
        assert_eq!(z.to_utc("Customized Time Zone", l), None);
        assert_eq!(z.to_utc("", l), None);
    }

    #[test]
    fn dst_edges_follow_rfc5545() {
        let z = SystemZones::new();
        let ny = |l| z.to_utc("America/New_York", l).unwrap();
        // Spring forward: 02:30 never happens; it is 03:30 EDT.
        assert_eq!(ny(at(2026, 3, 8, 2, 30)), at(2026, 3, 8, 7, 30));
        assert_eq!(ny(at(2026, 3, 8, 1, 59)), at(2026, 3, 8, 6, 59));
        assert_eq!(ny(at(2026, 3, 8, 3, 0)), at(2026, 3, 8, 7, 0));
        // Fall back: 01:30 happens twice; the first (EDT) wins.
        assert_eq!(ny(at(2026, 11, 1, 1, 30)), at(2026, 11, 1, 5, 30));
        assert_eq!(ny(at(2026, 11, 1, 2, 0)), at(2026, 11, 1, 7, 0));
        // The Windows name gives the same.
        assert_eq!(z.to_utc("Eastern Standard Time", at(2026, 11, 1, 1, 30)), Some(at(2026, 11, 1, 5, 30)));
    }

    #[test]
    fn the_pc_zone_goes_both_ways() {
        // Whatever zone this PC is in: around a year, local -> UTC -> local.
        for k in 0..60 {
            let utc = at(2026, 1, 1, 0, 0) + k * 6 * DAY + 3 * 3600;
            let l = local_from_utc(utc);
            assert!((l - utc).abs() <= 14 * 3600);
            assert_eq!(utc_from_local(l), utc);
        }
        assert_eq!(win32(at(2026, 6, 1, 0, 0), true).map(|l| (l - at(2026, 6, 1, 0, 0)).abs() <= 14 * 3600), Some(true));
    }
}
