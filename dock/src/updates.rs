//! Update checks for the direct-download build (docs/updates.md).
//!
//! A small JSON manifest at [`MANIFEST_URL`] names the newest version. The
//! dock fetches it 30 s after startup and then once a day (when
//! `updates.auto_check` is on), and shows a Windows toast once per new
//! version. The settings window's "Check now" runs the same check. Both
//! processes share what they learned through `%APPDATA%\NopBar\update.json`.
//!
//! A Store (MSIX) build never checks: the Store updates it.
//!
//! Privacy: a plain HTTPS GET with the User-Agent "NopBar/<version>", no
//! cookies, no query string, nothing about the user or the PC.
//!
//! Toasts: NopBar is unpackaged and has no Start-menu shortcut, so its
//! AppUserModelID is registered under
//! `HKCU\Software\Classes\AppUserModelId\NopBar.NopBar` (display name and an
//! icon written to `%LOCALAPPDATA%\NopBar\icon.png`), which is enough for
//! `ToastNotificationManager::CreateToastNotifierWithId`. Every activation is
//! a protocol activation (the https download page), so no COM activator is
//! needed and nothing has to be running when the toast is clicked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const MANIFEST_URL: &str = "https://nopbard.com/nopbar/version.json";
/// The AppUserModelID toasts are shown under.
pub const AUMID: &str = "NopBar.NopBar";
const AUMID_KEY: &str = "Software\\Classes\\AppUserModelId\\NopBar.NopBar";
/// A manifest larger than this is refused.
pub const MAX_BYTES: usize = 64 * 1024;
/// WinHTTP timeout for each step (resolve, connect, send, receive).
const TIMEOUT_MS: i32 = 5_000;
/// The first check after startup waits this long (seconds).
pub const FIRST_DELAY: u64 = 30;
/// Between successful checks (seconds).
pub const INTERVAL: u64 = 24 * 3600;
/// After a failed check (offline, 404), try again sooner (seconds).
pub const RETRY: u64 = 3600;
/// Longest single sleep of the scheduler: timed waits do not count time the
/// PC spent asleep, so it looks at the wall clock again at least this often.
const MAX_WAIT: u64 = 3600;

// ---------------------------------------------------------------- versions

/// A semantic version (semver.org 2.0): MAJOR.MINOR.PATCH[-pre][+build].
/// A leading "v" is accepted; build metadata is ignored when comparing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pre: Vec<Pre>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Pre {
    Num(u64),
    Text(String),
}

fn number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let s = match s.split_once('+') {
            Some((v, build)) if build.split('.').all(ident_ok) => v,
            Some(_) => return None,
            None => s,
        };
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut parts = core.split('.');
        let (major, minor, patch) = (number(parts.next()?)?, number(parts.next()?)?, number(parts.next()?)?);
        if parts.next().is_some() {
            return None;
        }
        let mut out = Vec::new();
        if let Some(p) = pre {
            for id in p.split('.') {
                if !ident_ok(id) {
                    return None;
                }
                if id.bytes().all(|b| b.is_ascii_digit()) {
                    out.push(Pre::Num(number(id)?));
                } else {
                    out.push(Pre::Text(id.to_string()));
                }
            }
        }
        Some(Version { major, minor, patch, pre: out })
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        let core = (self.major, self.minor, self.patch).cmp(&(o.major, o.minor, o.patch));
        if core != Equal {
            return core;
        }
        // A pre-release sorts before its release.
        match (self.pre.is_empty(), o.pre.is_empty()) {
            (true, true) => return Equal,
            (true, false) => return Greater,
            (false, true) => return Less,
            _ => {}
        }
        for (a, b) in self.pre.iter().zip(&o.pre) {
            let c = match (a, b) {
                (Pre::Num(x), Pre::Num(y)) => x.cmp(y),
                (Pre::Num(_), Pre::Text(_)) => Less,
                (Pre::Text(_), Pre::Num(_)) => Greater,
                (Pre::Text(x), Pre::Text(y)) => x.cmp(y),
            };
            if c != Equal {
                return c;
            }
        }
        self.pre.len().cmp(&o.pre.len())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// `candidate` is a newer version than `current` (false if either is not
/// a version).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------- manifest

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub version: String,
    pub url: String,
    /// Language tag -> release notes.
    pub notes: BTreeMap<String, String>,
    /// Oldest Windows the version runs on (major, minor, build).
    pub min_os: Option<[u32; 3]>,
    /// `downloads.zip`: the portable zip automatic updates install (None
    /// when missing or incomplete: then only the toast and the page).
    pub zip: Option<Download>,
    /// `downloads.setup` (the installer; parsed, not used by the app yet).
    pub setup: Option<Download>,
}

/// One file of `downloads` that the app can fetch and check.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Download {
    /// https only.
    pub url: String,
    /// 64 lowercase hex digits.
    pub sha256: String,
    /// Exact size in bytes (1 ..= MAX_DOWNLOAD).
    pub size: u64,
}

/// The largest download an update may be.
pub const MAX_DOWNLOAD: u64 = 100 * 1024 * 1024;

/// A release file name as the site takes it (site/nopbar/assets/i18n.js
/// `fileUrl`): letters, digits, `.`, `_`, `-`, not starting with a dot or
/// dash, no `..`.
fn release_file_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !s.contains("..")
}

/// `https://host[:port]` of an https URL.
fn origin(url: &str) -> Option<String> {
    let rest = url.get(..8).filter(|p| p.eq_ignore_ascii_case("https://")).map(|_| &url[8..])?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    (!host.is_empty()).then(|| format!("https://{host}"))
}

/// One `downloads` entry. `file` (the site's field) or `url`: a full https
/// URL, or a file name under `<manifest origin>/nopbar/releases/` like
/// the site resolves it.
pub fn parse_download(v: &serde_json::Value, manifest_url: &str) -> Result<Download, String> {
    let obj = v.as_object().ok_or("not an object")?;
    let file = obj.get("file").or_else(|| obj.get("url")).and_then(|f| f.as_str()).map(str::trim).ok_or("no \"file\"")?;
    let url = if is_https_url(file) {
        file.to_string()
    } else if release_file_name(file) {
        format!("{}/nopbar/releases/{file}", origin(manifest_url).ok_or("no origin")?)
    } else {
        return Err(format!("{file:?} is neither an https URL nor a release file name"));
    };
    let sha = obj.get("sha256").and_then(|h| h.as_str()).and_then(crate::autoupdate::sha256::parse_hex).ok_or("no 64-hex \"sha256\"")?;
    let size = obj.get("size").and_then(|n| n.as_u64()).ok_or("no \"size\"")?;
    if size == 0 || size > MAX_DOWNLOAD {
        return Err(format!("size {size} is not 1..={MAX_DOWNLOAD}"));
    }
    Ok(Download { url, sha256: crate::autoupdate::sha256::hex(&sha), size })
}

/// An https URL that is safe to hand to the shell and to put in a toast.
pub fn is_https_url(u: &str) -> bool {
    let Some(rest) = u.get(..8).filter(|p| p.eq_ignore_ascii_case("https://")).map(|_| &u[8..]) else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty() && u.len() <= 2048 && !u.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '\\' | '`'))
}

fn parse_os(s: &str) -> Option<[u32; 3]> {
    let mut out = [0u32; 3];
    let parts: Vec<&str> = s.trim().split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        out[i] = p.parse().ok()?;
    }
    Some(out)
}

#[cfg(test)]
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    parse_manifest_at(bytes, MANIFEST_URL)
}

/// The manifest fetched from `manifest_url` (release file names resolve
/// against its origin).
pub fn parse_manifest_at(bytes: &[u8], manifest_url: &str) -> Result<Manifest, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("manifest too big ({} bytes)", bytes.len()));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "manifest is not UTF-8".to_string())?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("manifest: {e}"))?;
    let obj = v.as_object().ok_or("manifest is not a JSON object")?;
    let version = obj.get("version").and_then(|v| v.as_str()).ok_or("manifest has no \"version\"")?.trim().to_string();
    if Version::parse(&version).is_none() {
        return Err(format!("manifest version {version:?} is not a version"));
    }
    let url = obj.get("url").and_then(|v| v.as_str()).ok_or("manifest has no \"url\"")?.trim().to_string();
    if !is_https_url(&url) {
        return Err(format!("manifest url {url:?} is not an https URL"));
    }
    let mut notes = BTreeMap::new();
    match obj.get("notes") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Object(m)) => {
            for (k, v) in m {
                if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    notes.insert(k.to_ascii_lowercase(), s.to_string());
                }
            }
        }
        Some(_) => return Err("manifest \"notes\" is not an object".into()),
    }
    let min_os = match obj.get("min_os") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(v.as_str().and_then(parse_os).ok_or("manifest \"min_os\" is not like \"10.0.19041\"")?),
    };
    // `downloads` (v2) is optional and lenient: an entry the app cannot use
    // leaves the manifest as good as a v1 one (toast and page only).
    let (mut zip, mut setup) = (None, None);
    if let Some(d) = obj.get("downloads").and_then(|d| d.as_object()) {
        let take = |key: &str| match d.get(key) {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => parse_download(v, manifest_url).map_err(|e| crate::log!("updates: downloads.{key} not usable: {e}")).ok(),
        };
        zip = take("zip");
        setup = take("setup").or_else(|| take("installer"));
    }
    Ok(Manifest { version, url, notes, min_os, zip, setup })
}

/// Notes in `lang` ("pt-br"), else its base language ("pt"), else English.
pub fn pick_notes<'a>(notes: &'a BTreeMap<String, String>, lang: &str) -> Option<&'a str> {
    let lang = lang.to_ascii_lowercase();
    let base = lang.split('-').next().unwrap_or("");
    notes.get(&lang).or_else(|| notes.get(base)).or_else(|| notes.get("en")).map(String::as_str)
}

// ---------------------------------------------------------------- state

/// What the last checks learned (shared by the dock and the settings
/// window through update.json).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct State {
    /// Unix seconds of the last attempt (successful or not; 0 = never).
    pub last_check: u64,
    /// The last attempt failed (offline, HTTP error, bad manifest).
    pub failed: bool,
    /// The newest version the last successful check offered ("" = none
    /// that runs on this Windows).
    pub latest: String,
    pub url: String,
    pub notes: BTreeMap<String, String>,
    /// The newest version the user was already told about (toast or the
    /// settings window): never toasted again.
    pub notified: String,
    /// The latest version's portable zip (manifest v2), if listed.
    pub zip: Option<Download>,
    /// The latest version's installer (manifest v2), if listed: what an
    /// installed copy updates with.
    pub setup: Option<Download>,
    /// The installer of the version installed now, kept by the last
    /// installer update: what a failed next update rolls back with.
    pub cached_setup: Option<CachedSetup>,
    /// The automatic update in progress (crate::autoupdate).
    pub pending: Pending,
    /// A version whose install failed (rolled back): never installed
    /// automatically again; a newer one is.
    pub bad: String,
    /// The version the last automatic update installed.
    pub installed: String,
}

/// Where an automatic update is.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    None,
    Downloading,
    Verifying,
    Ready,
    Installing,
    Failed,
}

/// A kept installer (crate::autoupdate::apply).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct CachedSetup {
    pub version: String,
    pub path: String,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Pending {
    pub version: String,
    /// What was downloaded: "setup" (the installer) or "zip".
    pub kind: String,
    pub phase: Phase,
    /// Downloading: 0..=100.
    pub progress: u8,
    /// Unix seconds the phase began.
    pub since: u64,
    /// Ready: the staged folder and its nopbar.exe's SHA-256 (zip), or the
    /// downloaded setup exe and its SHA-256 (setup).
    pub staged: String,
    pub exe_sha256: String,
    /// Failed: why (for dock.log and Settings).
    pub error: String,
    /// "Ask first": the version the ready toast was shown for.
    pub asked: String,
}

pub fn state_file() -> PathBuf {
    crate::config::app_dir().join("update.json")
}

pub fn load_state_from(path: &Path) -> State {
    std::fs::read(path)
        .ok()
        .filter(|b| b.len() <= MAX_BYTES * 2)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_state_to(path: &Path, st: &State) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

pub fn load_state() -> State {
    load_state_from(&state_file())
}

/// How long a read-modify-write waits for the other process.
const LOCK_WAIT_MS: u32 = 3_000;

/// A per-user named mutex around update.json's read-modify-write (the dock
/// and the settings window are separate processes). Named after the file,
/// so tests with their own file never meet the real one.
struct StateLock(windows::Win32::Foundation::HANDLE);

impl StateLock {
    fn take(path: &Path) -> Option<StateLock> {
        use windows::core::HSTRING;
        use windows::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        let name = format!("Local\\NopBar.UpdateState.{:016x}", crate::icons::fnv64(path.to_string_lossy().to_lowercase().as_bytes()));
        unsafe {
            let m = CreateMutexW(None, false, &HSTRING::from(name)).ok()?;
            // Abandoned = the other process died holding it: ours now.
            let r = WaitForSingleObject(m, LOCK_WAIT_MS);
            if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
                Some(StateLock(m))
            } else {
                let _ = CloseHandle(m);
                None
            }
        }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::ReleaseMutex;
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// Reads `path`, lets `f` change the state, and saves it if it changed, all
/// under the cross-process lock. None (and nothing written) if the lock
/// could not be had.
pub fn transact_at<R>(path: &Path, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let Some(_lock) = StateLock::take(path) else {
        crate::log!("updates: update.json is busy; not updated");
        return None;
    };
    let before = load_state_from(path);
    let mut st = before.clone();
    let r = f(&mut st);
    if st != before {
        if let Err(e) = save_state_to(path, &st) {
            crate::log!("updates: could not save state: {e}");
        }
    }
    Some(r)
}

pub(crate) fn transact<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    transact_at(&state_file(), f)
}

/// Marks `version` as told about. Only ever moves forward (a stale writer
/// cannot take it back to an older version).
pub fn mark_notified(st: &mut State, version: &str) {
    if Version::parse(version).is_some() && (Version::parse(&st.notified).is_none() || is_newer(version, &st.notified)) {
        st.notified = version.to_string();
    }
}

/// Folds the outcome of a check that started at `started` into the state.
/// A check that started before the one already recorded is dropped (false).
/// `os`: this Windows (a version that needs a newer one is not offered).
pub fn record(st: &mut State, started: u64, result: Result<Manifest, String>, os: [u32; 3]) -> bool {
    if started < st.last_check && st.last_check <= started.saturating_add(INTERVAL) {
        return false;
    }
    st.last_check = started;
    match result {
        Ok(m) if m.min_os.is_some_and(|min| os < min) => {
            crate::log!("updates: {} needs Windows {:?}, this is {os:?}", m.version, m.min_os);
            st.failed = false;
            st.latest.clear();
            st.url.clear();
            st.notes.clear();
            st.zip = None;
            st.setup = None;
        }
        Ok(m) => {
            st.failed = false;
            st.latest = m.version;
            st.url = m.url;
            st.notes = m.notes;
            st.zip = m.zip;
            st.setup = m.setup;
        }
        Err(e) => {
            crate::log!("updates: check failed: {e}");
            st.failed = true;
        }
    }
    true
}

/// The state offers a version newer than `current` that the user has not
/// been told about yet.
pub fn should_notify(st: &State, current: &str) -> bool {
    if !is_newer(&st.latest, current) || !is_https_url(&st.url) {
        return false;
    }
    // Only a version newer than the last one told about (an unreadable
    // record does not block it).
    Version::parse(&st.notified).is_none() || is_newer(&st.latest, &st.notified)
}

// ---------------------------------------------------------------- schedule

/// Seconds until the next automatic check: not before `FIRST_DELAY` after
/// startup (`uptime` = seconds the process has run), then `INTERVAL` after
/// the last check (`RETRY` after a failed one). A last check in the future
/// (the clock was moved back) counts as long ago.
pub fn wait_secs(now: u64, last_check: u64, failed: bool, uptime: u64) -> u64 {
    let startup = FIRST_DELAY.saturating_sub(uptime);
    let due = if last_check == 0 || last_check > now.saturating_add(600) {
        0
    } else {
        last_check.saturating_add(if failed { RETRY } else { INTERVAL }).saturating_sub(now)
    };
    startup.max(due)
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------- status

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Checking,
    Available(String),
    Failed,
    Never,
    UpToDate,
}

pub fn status(st: &State, checking: bool, current: &str) -> Status {
    if checking {
        Status::Checking
    } else if is_newer(&st.latest, current) && is_https_url(&st.url) {
        Status::Available(st.latest.clone())
    } else if st.failed {
        Status::Failed
    } else if st.last_check == 0 {
        Status::Never
    } else {
        Status::UpToDate
    }
}

// ---------------------------------------------------------------- channel

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Downloaded from nopbard.com: checks for itself.
    Direct,
    /// The Microsoft Store (MSIX) build.
    Store,
}

pub fn channel() -> Channel {
    if crate::release::is_packaged() {
        Channel::Store
    } else {
        Channel::Direct
    }
}

// ---------------------------------------------------------------- fetch

/// The whole request (resolve, connect, TLS, redirects, headers, body) must
/// be done within this; each step also gets at most TIMEOUT_MS.
pub const TOTAL_MS: u64 = 10_000;

/// Set on shutdown: no new requests, the scheduler ends.
static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Requests in flight, by token: closing a WinHTTP request handle from
/// another thread cancels the call blocked on it.
static INFLIGHT: Mutex<Vec<(u64, usize)>> = Mutex::new(Vec::new());
static NEXT_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn close_handle(h: usize) {
    unsafe {
        let _ = windows::Win32::Networking::WinHttp::WinHttpCloseHandle(h as *mut _);
    }
}

/// Closes request `token` if it is still in flight; true if it was.
fn cancel_request(token: u64) -> bool {
    let h = {
        let mut v = INFLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        let Some(i) = v.iter().position(|(t, _)| *t == token) else { return false };
        v.swap_remove(i).1
    };
    close_handle(h);
    true
}

/// Shutdown: cancels requests in flight and ends the scheduler.
pub fn cancel() {
    STOP.store(true, std::sync::atomic::Ordering::SeqCst);
    let all: Vec<u64> = INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(t, _)| *t).collect();
    for t in all {
        cancel_request(t);
    }
    poke_scheduler();
}

fn stopping() -> bool {
    STOP.load(std::sync::atomic::Ordering::SeqCst)
}

struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            close_handle(self.0 as usize);
        }
    }
}

fn open(h: *mut core::ffi::c_void, what: &str) -> Result<Handle, String> {
    if h.is_null() {
        Err(format!("{what}: {}", windows::core::Error::from_thread()))
    } else {
        Ok(Handle(h))
    }
}

/// Milliseconds the next step may take before `deadline` (at most
/// TIMEOUT_MS), or an error once it has passed.
#[cfg(test)]
pub fn step_ms(now: std::time::Instant, deadline: std::time::Instant) -> Result<i32, String> {
    step_within(now, deadline, TIMEOUT_MS, TOTAL_MS)
}

/// Like [`step_ms`] for any step limit (`total_ms` only for the message).
pub fn step_within(now: std::time::Instant, deadline: std::time::Instant, step_max: i32, total_ms: u64) -> Result<i32, String> {
    let left = deadline.saturating_duration_since(now).as_millis() as u64;
    if left == 0 {
        return Err(format!("timed out after {total_ms} ms"));
    }
    Ok(left.min(step_max.max(1) as u64) as i32)
}

/// The request handle, registered for cancellation (by shutdown or the
/// deadline watchdog). Dropping it unregisters and closes it, unless it
/// was cancelled (then it is closed already).
struct Request {
    token: u64,
    h: *mut core::ffi::c_void,
}

impl Drop for Request {
    fn drop(&mut self) {
        let mine = {
            let mut v = INFLIGHT.lock().unwrap_or_else(|e| e.into_inner());
            v.iter().position(|(t, _)| *t == self.token).map(|i| v.swap_remove(i))
        };
        if mine.is_some() {
            close_handle(self.h as usize);
        }
    }
}

/// Tells the deadline watchdog the request is over (on drop).
struct Over(std::sync::Arc<(Mutex<bool>, Condvar)>);

impl Drop for Over {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.0 .1.notify_all();
    }
}
/// The manifest to check: `updates.manifest_url` from config.json when it
/// is a plain https URL (testing), else the official one.
fn manifest_url() -> String {
    let custom = crate::config::read_current().and_then(|c| c.updates.manifest_url);
    pick_manifest_url(custom.as_deref())
}

fn pick_manifest_url(custom: Option<&str>) -> String {
    match custom.map(str::trim) {
        Some(u) if u.len() < 512 && u.starts_with("https://") && u.len() > 8 && !u.contains(char::is_whitespace) => {
            crate::log!("updates: using the test manifest {}", crate::autoupdate::trust::redact_url(u));
            u.to_string()
        }
        _ => MANIFEST_URL.to_string(),
    }
}


/// GETs the manifest: HTTPS only, at most MAX_BYTES, each step at most
/// TIMEOUT_MS and the whole request at most TOTAL_MS (a watchdog closes the
/// request when the deadline passes, so a server trickling bytes cannot
/// hold it longer). [`cancel`] ends it early.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    fetch_capped(url, MAX_BYTES)
}

/// A small GET (the manifest, its signature): at most `max` bytes.
pub(crate) fn fetch_capped(url: &str, max: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let lim = Limits { total: Duration::from_millis(TOTAL_MS), step_ms: TIMEOUT_MS, same_site: false };
    http_get(url, &lim, &mut |_| Ok(()), &mut |chunk| {
        body.extend_from_slice(chunk);
        if body.len() > max {
            return Err(format!("{} too big (over {max} bytes)", crate::autoupdate::trust::redact_url(url)));
        }
        Ok(())
    })?;
    Ok(body)
}

/// How long a GET may take.
pub(crate) struct Limits {
    /// The whole request, body included.
    pub total: Duration,
    /// Each step (resolve, connect, send, receive, each read).
    pub step_ms: i32,
    /// Redirects stay on the same site: https, the same host and port
    /// (checked here, at most MAX_HOPS); any other one is an error. Off:
    /// WinHTTP follows https -> https anywhere (a CDN for downloads).
    pub same_site: bool,
}

/// Most redirects a same-site request follows.
const MAX_HOPS: usize = 5;

/// Pure: where a same-site redirect from `from` (https://host[:port]/...)
/// to `location` goes, or why it is refused. A path ("/a.ics") stays on
/// the host; a full link must be https on the same host and port.
pub fn same_site_redirect(from: &str, location: &str) -> Result<String, String> {
    let (host, port, _) = split_https(from).ok_or("bad link")?;
    let loc = location.trim();
    if loc.starts_with('/') && !loc.starts_with("//") {
        let authority = if port == 443 { host } else { format!("{host}:{port}") };
        return Ok(format!("https://{authority}{loc}"));
    }
    match split_https(loc) {
        Some((h, p, _)) if h.eq_ignore_ascii_case(&host) && p == port => Ok(loc.to_string()),
        Some(_) => Err("redirect to another site refused".into()),
        None => Err("redirect off https refused".into()),
    }
}

/// `https://host[:port]/path?query` -> (host, port, path with query). No
/// user info, no fragment.
pub fn split_https(url: &str) -> Option<(String, u16, String)> {
    if !is_https_url(url) {
        return None;
    }
    let rest = &url[8..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.contains(['@', '[', ']']) {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok().filter(|p| *p != 0)?),
        None => (authority, 443),
    };
    if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')) {
        return None;
    }
    let tail = tail.split('#').next().unwrap_or("");
    let path = if tail.starts_with('/') { tail.to_string() } else { format!("/{tail}") };
    Some((host.to_string(), port, path))
}

/// A GET over HTTPS: redirects only to https (https -> https, e.g. a CDN),
/// no cookies, each step bounded by `lim.step_ms` and the whole request by
/// `lim.total` (a watchdog closes the request at the deadline), [`cancel`]
/// ends it early. `on_length` hears the Content-Length if the server sent
/// one (it may refuse: too big); `on_chunk` gets the body piece by piece.
pub(crate) fn http_get(url: &str, lim: &Limits, on_length: &mut dyn FnMut(u64) -> Result<(), String>, on_chunk: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
    // One deadline for every hop.
    let deadline = std::time::Instant::now() + lim.total;
    let mut at = url.to_string();
    for _ in 0..=MAX_HOPS {
        match http_get_once(&at, lim, deadline, on_length, on_chunk)? {
            None => return Ok(()),
            Some(location) => at = same_site_redirect(&at, &location)?,
        }
    }
    Err("too many redirects".into())
}

/// One request of http_get. Some(location): a redirect to follow (only
/// with `lim.same_site`, which turns WinHTTP's own following off).
fn http_get_once(url: &str, lim: &Limits, deadline: std::time::Instant, on_length: &mut dyn FnMut(u64) -> Result<(), String>, on_chunk: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<Option<String>, String> {
    use std::time::Instant;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Networking::WinHttp::*;
    if stopping() {
        return Err("shutting down".into());
    }
    let (host, port, path) = split_https(url).ok_or_else(|| format!("{} is not a plain https URL", crate::autoupdate::trust::redact_url(url)))?;
    let total_ms = lim.total.as_millis() as u64;
    unsafe {
        let agent = HSTRING::from(format!("NopBar/{CURRENT}"));
        let session = open(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0), "open")?;
        let conn = open(WinHttpConnect(session.0, &HSTRING::from(host), port, 0), "connect")?;
        let raw = WinHttpOpenRequest(conn.0, &HSTRING::from("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE);
        if raw.is_null() {
            return Err(format!("request: {}", windows::core::Error::from_thread()));
        }
        let token = NEXT_TOKEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).push((token, raw as usize));
        let req = Request { token, h: raw };
        // Declared after `req`, so dropped before it: the watchdog stands
        // down first, then the request is closed.
        let done = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
        let _over = Over(done.clone());
        let _ = std::thread::Builder::new().name("update-deadline".into()).spawn(move || {
            let (m, cv) = &*done;
            let g = m.lock().unwrap_or_else(|e| e.into_inner());
            let left = deadline.saturating_duration_since(Instant::now());
            let (g, _) = cv.wait_timeout_while(g, left, |over| !*over).unwrap_or_else(|e| e.into_inner());
            if !*g && cancel_request(token) {
                crate::log!("updates: request cancelled at the {total_ms} ms deadline");
            }
        });
        let step = |what: &str| -> Result<(), String> {
            if stopping() {
                return Err("shutting down".into());
            }
            // Closed by the watchdog: the handle must not be touched again.
            if !INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(t, _)| *t == token) {
                return Err(format!("{what}: timed out after {total_ms} ms"));
            }
            let ms = step_within(Instant::now(), deadline, lim.step_ms, total_ms).map_err(|e| format!("{what}: {e}"))?;
            WinHttpSetTimeouts(req.h, ms, ms, ms, ms).map_err(|e| format!("{what}: timeouts: {e}"))
        };
        // No cookies; never follow a redirect off https.
        let _ = WinHttpSetOption(Some(req.h), WINHTTP_OPTION_DISABLE_FEATURE, Some(&WINHTTP_DISABLE_COOKIES.to_le_bytes()));
        if lim.same_site {
            // Every redirect is ours to check: the request must not fail open.
            WinHttpSetOption(Some(req.h), WINHTTP_OPTION_REDIRECT_POLICY, Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_le_bytes())).map_err(|e| format!("redirect policy: {e}"))?;
        } else {
            let _ = WinHttpSetOption(Some(req.h), WINHTTP_OPTION_REDIRECT_POLICY, Some(&WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP.to_le_bytes()));
        }
        step("send")?;
        WinHttpSendRequest(req.h, None, None, 0, 0, 0).map_err(|e| format!("send: {e}"))?;
        step("receive")?;
        WinHttpReceiveResponse(req.h, std::ptr::null_mut()).map_err(|e| format!("receive: {e}"))?;
        let mut code = 0u32;
        let mut len = 4u32;
        WinHttpQueryHeaders(req.h, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut code as *mut u32 as *mut _), &mut len, std::ptr::null_mut())
            .map_err(|e| format!("status: {e}"))?;
        if lim.same_site && matches!(code, 301 | 302 | 303 | 307 | 308) {
            let mut buf = vec![0u16; 2048];
            let mut blen = (buf.len() * 2) as u32;
            WinHttpQueryHeaders(req.h, WINHTTP_QUERY_LOCATION, PCWSTR::null(), Some(buf.as_mut_ptr() as *mut _), &mut blen, std::ptr::null_mut()).map_err(|_| format!("HTTP {code} without a location"))?;
            let n = (blen as usize / 2).min(buf.len());
            return Ok(Some(String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string()));
        }
        if code != 200 {
            return Err(format!("HTTP {code}"));
        }
        let mut buf = [0u16; 32];
        let mut blen = std::mem::size_of_val(&buf) as u32;
        if WinHttpQueryHeaders(req.h, WINHTTP_QUERY_CONTENT_LENGTH, PCWSTR::null(), Some(buf.as_mut_ptr() as *mut _), &mut blen, std::ptr::null_mut()).is_ok() {
            let n = (blen as usize / 2).min(buf.len());
            if let Ok(v) = String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').trim().parse::<u64>() {
                on_length(v)?;
            }
        }
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            step("read")?;
            let mut got = 0u32;
            WinHttpReadData(req.h, chunk.as_mut_ptr() as *mut _, chunk.len() as u32, &mut got).map_err(|e| format!("read: {e}"))?;
            if got == 0 {
                break;
            }
            on_chunk(&chunk[..got as usize])?;
        }
        Ok(None)
    }
}

/// This Windows as (major, minor, build).
fn os_version() -> [u32; 3] {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    let key = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let dword = |name: windows::core::PCWSTR| -> Option<u32> {
        let mut v = 0u32;
        let mut size = 4u32;
        unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut size)).ok().ok()? };
        Some(v)
    };
    let build = {
        let mut buf = [0u16; 32];
        let mut size = std::mem::size_of_val(&buf) as u32;
        let ok = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, w!("CurrentBuild"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).is_ok() };
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if ok { String::from_utf16_lossy(&buf[..n]).trim().parse().unwrap_or(0) } else { 0 }
    };
    [dword(w!("CurrentMajorVersionNumber")).unwrap_or(10), dword(w!("CurrentMinorVersionNumber")).unwrap_or(0), build]
}

/// Fetches the manifest now and records the outcome in update.json (under
/// the state lock; a check that started before one already recorded is
/// dropped). Returns the state as saved.
pub fn check_now() -> State {
    let started = unix_now();
    let url = manifest_url();
    let result = fetch(&url).and_then(|b| {
        let m = parse_manifest_at(&b, &url)?;
        // Automatic installs take only a manifest signed with the built-in
        // key; its exact bytes are kept for them (crate::autoupdate).
        crate::autoupdate::keep_if_signed(&url, &b, &m);
        Ok(m)
    });
    let os = os_version();
    transact(|st| {
        if !record(st, started, result, os) {
            crate::log!("updates: a newer check was recorded meanwhile; keeping it");
        }
        st.clone()
    })
    .unwrap_or_else(load_state)
}

/// "Check now" in the settings window: the user sees the answer there, so
/// that version is not toasted afterwards.
pub fn check_from_settings() -> State {
    check_now();
    transact(|st| {
        if should_notify(st, CURRENT) {
            let v = st.latest.clone();
            mark_notified(st, &v);
        }
        st.clone()
    })
    .unwrap_or_else(load_state)
}

/// The dock's check: toasts a new version once. The version is claimed
/// (marked as told) under the lock before the toast, so the dock and the
/// settings window can never both announce it; if the toast fails the
/// claim is given back.
fn check_and_toast() -> State {
    let st = check_now();
    // A version automatic updates will download says so itself (installed
    // quietly, or the "ready" toast when asking first).
    if crate::autoupdate::will_install(&st) {
        return st;
    }
    let claim = transact(|st| {
        if !should_notify(st, CURRENT) {
            return None;
        }
        let prev = st.notified.clone();
        let v = st.latest.clone();
        mark_notified(st, &v);
        Some((st.clone(), prev))
    })
    .flatten();
    let Some((st, prev)) = claim else { return st };
    match show_toast(&st) {
        Ok(()) => crate::log!("updates: {} is out, toast shown", st.latest),
        Err(e) => {
            crate::log!("updates: {} is out, {e}", st.latest);
            let _ = transact(|now| {
                if now.notified == st.latest {
                    now.notified = prev.clone();
                }
            });
        }
    }
    st
}

// ---------------------------------------------------------------- toast

pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 has no other control characters.
            c if c.is_control() && !matches!(c, '\n' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

/// At most `max` characters, cut at a character boundary with "…".
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// The toast: clicking it or "Download" opens `url` (protocol activation),
/// "Later" dismisses it.
pub fn toast_xml(title: &str, body: &str, url: &str, download: &str, later: &str) -> String {
    let (title, body, url, download, later) = (xml_escape(&clip(title, 120)), xml_escape(&clip(body, 400)), xml_escape(url), xml_escape(download), xml_escape(later));
    format!(
        "<toast activationType=\"protocol\" launch=\"{url}\"><visual><binding template=\"ToastGeneric\"><text>{title}</text><text>{body}</text></binding></visual>\
<actions><action content=\"{download}\" activationType=\"protocol\" arguments=\"{url}\"/><action content=\"{later}\" activationType=\"system\" arguments=\"dismiss\"/></actions></toast>"
    )
}

/// The icon the toast shows, next to the app's other local data.
fn icon_file() -> Option<PathBuf> {
    const ICON: &[u8] = include_bytes!("../../assets/brand/icon-256.png");
    let dir = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?.join("NopBar");
    let path = dir.join("icon.png");
    if std::fs::read(&path).ok().as_deref() != Some(ICON) {
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join(format!("icon.png.{}.tmp", std::process::id()));
        std::fs::write(&tmp, ICON).ok()?;
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
    }
    Some(path)
}

/// Registers the AppUserModelID toasts need (per user, no admin).
pub(crate) fn register_aumid() -> Result<(), String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Registry::*;
    let set = |key: HKEY, name: &str, value: &str| -> Result<(), String> {
        let v: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let bytes = unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2) };
        unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes)).ok().map_err(|e| format!("{name}: {e}")) }
    };
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(AUMID_KEY), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None)
            .ok()
            .map_err(|e| format!("AppUserModelId key: {e}"))?;
        let mut r = set(key, "DisplayName", "NopBar");
        if let (Ok(()), Some(icon)) = (&r, icon_file()) {
            r = set(key, "IconUri", &icon.to_string_lossy());
        }
        let _ = RegCloseKey(key);
        r
    }
}

/// Removes the AppUserModelID and the install scheme (uninstall).
pub fn unregister_aumid() {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(AUMID_KEY));
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(SCHEME_KEY));
    }
}

/// A plain toast (title and text, no actions) for one-time notices from
/// other parts of NopBar. Shown from its own short-lived thread (COM in
/// MTA there), so the caller never waits; failures are logged.
pub fn notice(title: String, body: String) {
    let _ = std::thread::Builder::new().name("notice".into()).spawn(move || notice_now(&title, &body));
}

/// [`notice`], shown before it returns (the updater, which exits next).
pub fn notice_now(title: &str, body: &str) {
    let _ = try_notice_now(title, body);
}
/// Same toast path, with a delivery result for callers that persist a one-shot latch.
pub fn try_notice_now(title: &str, body: &str) -> bool {
    let com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) }.is_ok();
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(&clip(title, 120)),
        xml_escape(&clip(body, 400))
    );
    let result = register_aumid().and_then(|_| show_xml(&xml));
    if let Err(e) = &result {
        crate::log!("notice: not shown ({e})");
    }
    if com {
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
    result.is_ok()
}

pub(crate) fn show_xml(xml: &str) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    let show = || -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
    };
    show().map_err(|e| format!("toast: {e}"))
}

/// The URL scheme the "Install now" button of the ready toast opens. It
/// runs `"<this exe>" --install-update` (the URL itself is never passed on),
/// which asks the running dock to install the update it has staged.
pub const INSTALL_SCHEME: &str = "nopbar-update";
const SCHEME_KEY: &str = "Software\\Classes\\nopbar-update";

/// Registers [`INSTALL_SCHEME`] for this exe (per user, no admin).
fn register_scheme() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    register_protocol(INSTALL_SCHEME, &format!("\"{}\" --install-update", exe.display()))
}

pub(crate) fn register_protocol(scheme: &str, command: &str) -> Result<(), String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Registry::*;
    let scheme_key = format!("Software\\Classes\\{scheme}");
    let set = |key: HKEY, name: Option<&str>, value: &str| -> Result<(), String> {
        let v: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let bytes = unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2) };
        let name = name.map(HSTRING::from);
        let pname = name.as_ref().map(|n| PCWSTR(n.as_ptr())).unwrap_or(PCWSTR::null());
        unsafe { RegSetValueExW(key, pname, None, REG_SZ, Some(bytes)).ok().map_err(|e| format!("scheme: {e}")) }
    };
    let create = |path: &str| -> Result<HKEY, String> {
        let mut key = HKEY::default();
        unsafe {
            RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None)
                .ok()
                .map_err(|e| format!("scheme key: {e}"))?;
        }
        Ok(key)
    };
    let root = create(&scheme_key)?;
    let r = set(root, None, "URL:NopBar").and_then(|_| set(root, Some("URL Protocol"), ""));
    unsafe {
        let _ = RegCloseKey(root);
    }
    r?;
    let cmd = create(&format!("{scheme_key}\\shell\\open\\command"))?;
    let r = set(cmd, None, command);
    unsafe {
        let _ = RegCloseKey(cmd);
    }
    r
}

/// "NopBar {v} is ready": Install now (runs the install through
/// [`INSTALL_SCHEME`]) or Later.
pub fn ready_toast_xml(title: &str, body: &str, install: &str, later: &str) -> String {
    let (title, body, install, later) = (xml_escape(&clip(title, 120)), xml_escape(&clip(body, 400)), xml_escape(install), xml_escape(later));
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{title}</text><text>{body}</text></binding></visual>\
<actions><action content=\"{install}\" activationType=\"protocol\" arguments=\"{INSTALL_SCHEME}:install\"/><action content=\"{later}\" activationType=\"system\" arguments=\"dismiss\"/></actions></toast>"
    )
}

/// Shows the "ready to install" toast for `version` ("Ask first").
pub fn show_ready_toast(version: &str, notes: &BTreeMap<String, String>) -> Result<(), String> {
    use crate::i18n::{t, tf};
    register_aumid()?;
    register_scheme()?;
    let title = tf("update.ready.title", &[("v", version)]);
    let body = pick_notes(notes, crate::i18n::current()).unwrap_or_else(|| t("update.ready.body"));
    show_xml(&ready_toast_xml(&title, body, t("update.install_now"), t("updates.toast.later")))
}

fn show_toast(st: &State) -> Result<(), String> {
    use crate::i18n::{t, tf};
    register_aumid()?;
    let title = tf("updates.toast.title", &[("v", &st.latest)]);
    let body = pick_notes(&st.notes, crate::i18n::current()).unwrap_or_else(|| t("updates.toast.body"));
    show_xml(&toast_xml(&title, body, &st.url, t("updates.toast.download"), t("updates.toast.later")))
}

// ---------------------------------------------------------------- scheduler

/// `updates.auto_check` as the dock last applied it (None = not yet known).
static AUTO: Mutex<Option<bool>> = Mutex::new(None);
/// Bumped whenever AUTO changes: a plan made before is stale.
static AUTO_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The scheduler's watch on the service thread.
static SCHED: Mutex<Option<crate::service::Id>> = Mutex::new(None);

/// Something asked for an update now: a plan made before is stale.
pub(crate) fn request_plan() {
    AUTO_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    poke_scheduler();
}

fn poke_scheduler() {
    if let Some(id) = *SCHED.lock().unwrap_or_else(|e| e.into_inner()) {
        crate::service::poke(id);
    }
}

/// The dock's config changed: wakes the scheduler when the switch flipped.
pub fn set_auto(on: bool) {
    let mut g = AUTO.lock().unwrap_or_else(|e| e.into_inner());
    if *g != Some(on) {
        *g = Some(on);
        AUTO_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        drop(g);
        poke_scheduler();
    }
}

/// What the planner decided: how long to sleep (None = until the switch
/// changes), for AUTO_GEN `gen`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Plan {
    sleep: Option<Duration>,
    gen: u64,
}

/// Starts the dock's update scheduler (direct-download build only). A
/// watch on the service thread only keeps time: until the switch is turned
/// on when it is off, else until the next check is due (waking at most
/// hourly to look at the wall clock again). Everything that touches the
/// disk or the network (config, update.json, the check) runs on a planner
/// thread that exists only while it works, so a slow disk never holds up
/// the service thread.
pub fn start() {
    let ch = channel();
    if ch != Channel::Direct {
        crate::log!("updates: {ch:?} build, the store updates it");
        return;
    }
    let result: std::sync::Arc<Mutex<Option<Plan>>> = Default::default();
    let out = result.clone();
    let started = std::time::Instant::now();
    // This planner's own last attempt, in case update.json cannot be
    // written (it would otherwise look like "never checked" forever).
    let mine = Mutex::new((0u64, false));
    let planner = crate::idle::IdleQueue::new("updates", PLANNER_IDLE, 0, move |jobs: &mut crate::idle::Jobs<()>| {
        let com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) }.is_ok();
        while jobs.next().is_some() {
            while jobs.try_next().is_some() {}
            let mut m = mine.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(p) = plan(started, &mut m) {
                *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
                poke_scheduler();
            }
        }
        if com {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    });
    // SCHED is held while adding: a planner that answers at once waits in
    // poke_scheduler until the id is there (no lost first answer).
    let mut g = SCHED.lock().unwrap_or_else(|e| e.into_inner());
    *g = crate::service::add(Box::new(Scheduler { planner, result }));
}

/// The planner thread ends after this long without work.
const PLANNER_IDLE: Duration = Duration::from_secs(5);

/// Reads the switch and the last check, runs a check if one is due (on
/// this thread), and says how long to sleep. None when shutting down.
fn plan(started: std::time::Instant, mine: &mut (u64, bool)) -> Option<Plan> {
    loop {
        if stopping() {
            return None;
        }
        let gen = AUTO_GEN.load(std::sync::atomic::Ordering::SeqCst);
        let auto = {
            let mut g = AUTO.lock().unwrap_or_else(|e| e.into_inner());
            if g.is_none() {
                *g = Some(crate::config::read_current().map(|c| c.updates.auto_check).unwrap_or(true));
            }
            *g
        };
        // "Restart to update", the toast's Install now, --update-now.
        if let Some(check) = crate::autoupdate::take_request() {
            crate::autoupdate::install_now(check);
            continue;
        }
        if auto == Some(false) {
            return Some(Plan { sleep: None, gen });
        }
        let st = load_state();
        // Download / install a found version; Some = look again that soon
        // (waiting for a quiet moment).
        let soon = crate::autoupdate::tick(&st);
        let (last, failed) = if mine.0 > st.last_check { *mine } else { (st.last_check, st.failed) };
        let wait = wait_secs(unix_now(), last, failed, started.elapsed().as_secs());
        if wait > 0 {
            let mut sleep = Duration::from_secs(wait.min(MAX_WAIT));
            if let Some(s) = soon {
                sleep = sleep.min(s);
            }
            return Some(Plan { sleep: Some(sleep), gen });
        }
        let st = check_and_toast();
        *mine = (unix_now(), st.failed);
    }
}

/// What the scheduler does on a wake, given the planner's latest answer.
fn next_step(result: Option<Plan>, gen: u64) -> Option<crate::service::Next> {
    use crate::service::Next;
    match result {
        Some(p) if p.gen == gen => Some(match p.sleep {
            Some(d) => Next::Sleep(d),
            None => Next::Wait,
        }),
        // No answer, or one made before the switch changed: plan again.
        _ => None,
    }
}

struct Scheduler {
    planner: crate::idle::IdleQueue<()>,
    /// The planner's latest answer (taken by the scheduler).
    result: std::sync::Arc<Mutex<Option<Plan>>>,
}

impl crate::service::Watch for Scheduler {
    fn name(&self) -> &'static str {
        "update scheduler"
    }
    fn handles(&self) -> Vec<windows::Win32::Foundation::HANDLE> {
        Vec::new()
    }
    fn wake(&mut self, why: crate::service::Wake) -> crate::service::Next {
        use crate::service::{Next, Wake};
        if stopping() {
            return Next::Done;
        }
        let answer = self.result.lock().unwrap_or_else(|e| e.into_inner()).take();
        let gen = AUTO_GEN.load(std::sync::atomic::Ordering::SeqCst);
        // A timer means the sleep is over: plan again (and check if due).
        if why != Wake::Timer {
            if let Some(next) = next_step(answer, gen) {
                return next;
            }
        }
        let _ = self.planner.send(());
        // Until the planner pokes with its answer.
        Next::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} should parse"))
    }

    #[test]
    fn scheduler_follows_only_current_plans() {
        use crate::service::Next;
        let p = |sleep, gen| Some(Plan { sleep, gen });
        assert_eq!(next_step(p(Some(Duration::from_secs(60)), 4), 4), Some(Next::Sleep(Duration::from_secs(60))));
        assert_eq!(next_step(p(None, 4), 4), Some(Next::Wait), "switch off: wait for it");
        assert_eq!(next_step(p(None, 3), 4), None, "made before the switch changed: plan again");
        assert_eq!(next_step(None, 4), None);
    }

    #[test]
    fn versions_parse() {
        assert_eq!((v("1.2.3").major, v("1.2.3").minor, v("1.2.3").patch), (1, 2, 3));
        assert_eq!(v("v0.2.0"), v("0.2.0"));
        assert_eq!(v(" 0.2.0 "), v("0.2.0"));
        assert_eq!(v("1.0.0+build.5"), v("1.0.0"), "build metadata is ignored");
        assert!(Version::parse("1.0.0-rc.1").is_some());
        assert!(Version::parse("1.0.0-x-y-z.--").is_some());
    }

    #[test]
    fn malformed_versions_are_refused() {
        for s in ["", "1", "1.2", "1.2.3.4", "a.b.c", "1.2.x", "01.2.3", "1.02.3", "1.2.03", "1.2.3-", "1.2.3-a..b", "1.2.3-01", "1.2.3+", "1.2.3+a+b", "-1.2.3", "1.2.3-a b", "1..3", "99999999999999999999.0.0"] {
            assert!(Version::parse(s).is_none(), "{s:?} must not parse");
        }
    }

    #[test]
    fn versions_compare_like_semver() {
        // semver.org §11 example chain.
        let chain = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.1.0", "2.0.0", "10.0.0"];
        for w in chain.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
            assert!(is_newer(w[1], w[0]));
            assert!(!is_newer(w[0], w[1]));
        }
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("1.0.0+b2", "1.0.0+b1"));
        assert!(is_newer("0.10.0", "0.9.0"), "numeric, not text order");
        assert!(!is_newer("garbage", "0.1.0"));
        assert!(!is_newer("0.2.0", "garbage"));
        assert!(Version::parse(CURRENT).is_some(), "this build's version parses");
    }

    #[test]
    fn manifest_parses() {
        let m = parse_manifest(r#"{"version":"0.2.0","url":"https://nopbard.com/nopbar/download","notes":{"en":"Faster dock","ko":"더 빠른 독","PT-BR":"Mais rápido","x":5},"min_os":"10.0.19041"}"#.as_bytes()).unwrap();
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.url, "https://nopbard.com/nopbar/download");
        assert_eq!(m.min_os, Some([10, 0, 19041]));
        assert_eq!(m.notes.len(), 3, "non-text notes are dropped, tags lowercased");
        assert_eq!(pick_notes(&m.notes, "ko"), Some("더 빠른 독"));
        assert_eq!(pick_notes(&m.notes, "pt-br"), Some("Mais rápido"));
        assert_eq!(pick_notes(&m.notes, "de"), Some("Faster dock"), "falls back to English");
        // Only what is required.
        let m = parse_manifest("\u{feff}{\"version\":\"1.0.0\",\"url\":\"https://x.example/d\"}".as_bytes()).unwrap();
        assert!(m.notes.is_empty());
        assert_eq!(m.min_os, None);
        assert_eq!(pick_notes(&m.notes, "en"), None);
        // Base language.
        let mut n = BTreeMap::new();
        n.insert("pt".to_string(), "pt".to_string());
        assert_eq!(pick_notes(&n, "pt-PT"), Some("pt"));
    }

    #[test]
    fn bad_manifests_are_refused() {
        let bad: &[&[u8]] = &[
            b"",
            b"not json",
            b"[]",
            b"{}",
            br#"{"url":"https://nopbard.com/d"}"#,
            br#"{"version":"0.2.0"}"#,
            br#"{"version":2,"url":"https://nopbard.com/d"}"#,
            br#"{"version":"two","url":"https://nopbard.com/d"}"#,
            br#"{"version":"0.2.0","url":"http://nopbard.com/d"}"#,
            br#"{"version":"0.2.0","url":"https://"}"#,
            br#"{"version":"0.2.0","url":"https://a.com/x\" onload=\"y"}"#,
            br#"{"version":"0.2.0","url":"file:///C:/x.exe"}"#,
            br#"{"version":"0.2.0","url":"https://a.com/d","notes":"text"}"#,
            br#"{"version":"0.2.0","url":"https://a.com/d","min_os":"ten"}"#,
            br#"{"version":"0.2.0","url":"https://a.com/d","min_os":10}"#,
            b"\xff\xfe{}",
        ];
        for b in bad {
            assert!(parse_manifest(b).is_err(), "{:?} must be refused", String::from_utf8_lossy(b));
        }
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn manifest_v2_downloads() {
        // The site's format (site/README.md) plus "size".
        let text = format!(
            r#"{{"version":"0.1.2","url":"https://nopbard.com/nopbar/download",
            "downloads":{{"setup":{{"file":"NopBar-0.1.2-setup.exe","sha256":"{SHA}"}},
            "zip":{{"file":"NopBar-0.1.2-portable.zip","sha256":"{up}","size":3592623}},
            "winget":"NopBar.NopBar","store":"https://apps.microsoft.com/detail/x"}}}}"#,
            up = SHA.to_uppercase()
        );
        let m = parse_manifest(text.as_bytes()).unwrap();
        assert_eq!(m.zip, Some(Download { url: "https://nopbard.com/nopbar/releases/NopBar-0.1.2-portable.zip".into(), sha256: SHA.into(), size: 3_592_623 }));
        assert_eq!(m.setup, None, "no size: not usable by the app (the site still shows it)");
        // A test manifest elsewhere: names resolve against its origin;
        // full URLs (GitHub Releases, a CDN) are taken as they are; "url"
        // is accepted like "file".
        let m = parse_manifest_at(text.as_bytes(), "https://gist.githubusercontent.com/u/1/raw/version.json").unwrap();
        assert_eq!(m.zip.unwrap().url, "https://gist.githubusercontent.com/nopbar/releases/NopBar-0.1.2-portable.zip");
        let gh = format!(r#"{{"version":"0.1.2","url":"https://a.com/d","downloads":{{"zip":{{"url":"https://github.com/o/r/releases/download/v0.1.2/NopBar-0.1.2-portable.zip?x=1","sha256":"{SHA}","size":5}}}}}}"#);
        assert_eq!(parse_manifest(gh.as_bytes()).unwrap().zip.unwrap().url, "https://github.com/o/r/releases/download/v0.1.2/NopBar-0.1.2-portable.zip?x=1");
    }

    #[test]
    fn unusable_downloads_leave_a_v1_manifest() {
        let with = |zip: &str| parse_manifest(format!(r#"{{"version":"0.2.0","url":"https://a.com/d","downloads":{{"zip":{zip}}}}}"#).as_bytes());
        for bad in [
            format!(r#"{{"file":"http://a.com/z.zip","sha256":"{SHA}","size":5}}"#),
            format!(r#"{{"file":"../z.zip","sha256":"{SHA}","size":5}}"#),
            format!(r#"{{"file":"a/z.zip","sha256":"{SHA}","size":5}}"#),
            format!(r#"{{"file":".z.zip","sha256":"{SHA}","size":5}}"#),
            format!(r#"{{"file":"C:\\z.zip","sha256":"{SHA}","size":5}}"#),
            r#"{"file":"z.zip","sha256":"abc","size":5}"#.to_string(),
            r#"{"file":"z.zip","size":5}"#.to_string(),
            format!(r#"{{"file":"z.zip","sha256":"{SHA}"}}"#),
            format!(r#"{{"file":"z.zip","sha256":"{SHA}","size":0}}"#),
            format!(r#"{{"file":"z.zip","sha256":"{SHA}","size":104857601}}"#),
            format!(r#"{{"file":"z.zip","sha256":"{SHA}","size":"5"}}"#),
            r#""NopBar.zip""#.to_string(),
            "null".to_string(),
        ] {
            let m = with(&bad).unwrap_or_else(|e| panic!("{bad}: the manifest itself stays good ({e})"));
            assert_eq!(m.zip, None, "{bad}");
        }
        // The largest allowed size.
        assert!(with(&format!(r#"{{"file":"z.zip","sha256":"{SHA}","size":104857600}}"#)).unwrap().zip.is_some());
        // "downloads" that is not an object is ignored (v1 apps ignore it too).
        assert!(parse_manifest(br#"{"version":"0.2.0","url":"https://a.com/d","downloads":[1]}"#).unwrap().zip.is_none());
    }

    #[test]
    fn same_site_redirects_never_leave_the_host() {
        let from = "https://calendar.example/private/SECRET.ics";
        // A path stays on the host.
        assert_eq!(same_site_redirect(from, "/private/other.ics").as_deref(), Ok("https://calendar.example/private/other.ics"));
        assert_eq!(same_site_redirect("https://cal.example:8443/a.ics", "/b.ics").as_deref(), Ok("https://cal.example:8443/b.ics"));
        // The same host by a full link, any case.
        assert!(same_site_redirect(from, "https://CALENDAR.example/new/SECRET.ics").is_ok());
        // Another host, another port, plain http, a scheme-relative link: refused.
        assert!(same_site_redirect(from, "https://collector.example/private/SECRET.ics").is_err());
        assert!(same_site_redirect(from, "https://calendar.example:444/x.ics").is_err());
        assert!(same_site_redirect(from, "http://calendar.example/x.ics").is_err());
        assert!(same_site_redirect(from, "//collector.example/x.ics").is_err());
        assert!(same_site_redirect(from, "https://calendar.example.collector.example/x").is_err());
    }

    #[test]
    fn https_urls_split() {
        assert_eq!(split_https("https://nopbard.com/nopbar/version.json"), Some(("nopbard.com".into(), 443, "/nopbar/version.json".into())));
        assert_eq!(split_https("https://a.com:8443/x?y=1#frag"), Some(("a.com".into(), 8443, "/x?y=1".into())));
        assert_eq!(split_https("https://a.com?y=1"), Some(("a.com".into(), 443, "/?y=1".into())));
        assert_eq!(split_https("https://a.com"), Some(("a.com".into(), 443, "/".into())));
        for bad in ["http://a.com/x", "https://user@a.com/x", "https://a.com:0/x", "https://a.com:99999/", "https://[::1]/x", "https://a_b.com/", "https://:443/"] {
            assert_eq!(split_https(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_check_keeps_the_zip_with_its_version() {
        let os = [10, 0, 22631];
        let mut st = State::default();
        let m = parse_manifest(format!(r#"{{"version":"0.2.0","url":"https://a.com/d","downloads":{{"zip":{{"file":"z.zip","sha256":"{SHA}","size":5}}}}}}"#).as_bytes()).unwrap();
        record(&mut st, 100, Ok(m), os);
        assert_eq!(st.zip.as_ref().map(|z| z.size), Some(5));
        // A v1 manifest later: no zip any more.
        record(&mut st, 200, parse_manifest(br#"{"version":"0.2.1","url":"https://a.com/d"}"#), os);
        assert_eq!(st.zip, None);
        // Older update.json files (no pending fields) still load.
        let old: State = serde_json::from_str(r#"{"last_check":5,"latest":"0.2.0","url":"https://a.com/d","notified":""}"#).unwrap();
        assert_eq!(old.pending.phase, Phase::None);
        assert_eq!(old.bad, "");
    }

    #[test]
    fn ready_toast_runs_only_the_install_scheme() {
        let x = ready_toast_xml("NopBar 0.2.0 is ready", "notes <b>", "Install now", "Later");
        assert!(x.contains("activationType=\"protocol\" arguments=\"nopbar-update:install\""));
        assert!(x.contains("notes &lt;b&gt;"));
        assert!(x.contains("activationType=\"system\" arguments=\"dismiss\""));
        assert!(!x.contains("launch="), "clicking the body installs nothing");
    }

    #[test]
    fn oversized_manifest_is_refused() {
        let pad = " ".repeat(MAX_BYTES);
        let big = format!("{{\"version\":\"0.2.0\",\"url\":\"https://a.com/d\"{pad}}}");
        let err = parse_manifest(big.as_bytes()).unwrap_err();
        assert!(err.contains("too big"), "{err}");
        let small = format!("{{\"version\":\"0.2.0\",\"url\":\"https://a.com/d\"{}}}", " ".repeat(MAX_BYTES - 64));
        assert!(parse_manifest(small.as_bytes()).is_ok(), "just under the limit is fine");
    }

    #[test]
    fn schedule() {
        let now = 1_800_000_000;
        // Never checked: 30 s after startup, then right away.
        assert_eq!(wait_secs(now, 0, false, 0), FIRST_DELAY);
        assert_eq!(wait_secs(now, 0, false, 10), FIRST_DELAY - 10);
        assert_eq!(wait_secs(now, 0, false, 31), 0);
        // Checked an hour ago: the rest of the day.
        assert_eq!(wait_secs(now, now - 3600, false, 100), INTERVAL - 3600);
        // ... and never sooner than the startup delay.
        assert_eq!(wait_secs(now, now - INTERVAL - 5, false, 0), FIRST_DELAY);
        assert_eq!(wait_secs(now, now - INTERVAL, false, 1000), 0);
        // A failed check is retried after RETRY.
        assert_eq!(wait_secs(now, now - 600, true, 1000), RETRY - 600);
        assert_eq!(wait_secs(now, now - RETRY, true, 1000), 0);
        // A last check far in the future (clock moved back) is due now.
        assert_eq!(wait_secs(now, now + 86_400, false, 1000), 0);
        // A little in the future (clock jitter) still waits.
        assert_eq!(wait_secs(now, now + 60, false, 1000), INTERVAL + 60);
    }

    fn state(latest: &str, notified: &str) -> State {
        State { last_check: 1, latest: latest.into(), url: "https://nopbard.com/nopbar/download".into(), notified: notified.into(), ..Default::default() }
    }

    #[test]
    fn same_version_is_not_toasted_twice() {
        let mut st = state("0.2.0", "");
        assert!(should_notify(&st, "0.1.0"));
        st.notified = st.latest.clone();
        assert!(!should_notify(&st, "0.1.0"), "already told");
        // A newer one is.
        st.latest = "0.3.0".into();
        assert!(should_notify(&st, "0.1.0"));
        // A rollback to an older version than told about is not.
        assert!(!should_notify(&state("0.2.0", "0.3.0"), "0.1.0"));
        // Not newer than what runs: nothing.
        assert!(!should_notify(&state("0.1.0", ""), "0.1.0"));
        assert!(!should_notify(&state("", ""), "0.1.0"));
        // A pre-release newer than the running one.
        assert!(should_notify(&state("0.2.0-rc.1", ""), "0.1.0"));
        // An unreadable record does not block it.
        assert!(should_notify(&state("0.2.0", "junk"), "0.1.0"));
        // No usable URL: nothing to offer.
        let mut st = state("0.2.0", "");
        st.url = "http://x".into();
        assert!(!should_notify(&st, "0.1.0"));
    }

    #[test]
    fn record_and_status() {
        let os = [10, 0, 22631];
        let mut st = State::default();
        assert_eq!(status(&st, false, "0.1.0"), Status::Never);
        assert_eq!(status(&st, true, "0.1.0"), Status::Checking);
        let m = parse_manifest(br#"{"version":"0.2.0","url":"https://a.com/d","notes":{"en":"n"}}"#).unwrap();
        record(&mut st, 100, Ok(m.clone()), os);
        assert_eq!(st.last_check, 100);
        assert_eq!(status(&st, false, "0.1.0"), Status::Available("0.2.0".into()));
        assert_eq!(status(&st, false, "0.2.0"), Status::UpToDate);
        // Offline later: the update found before is still offered.
        record(&mut st, 200, Err("offline".into()), os);
        assert!(st.failed);
        assert_eq!(status(&st, false, "0.1.0"), Status::Available("0.2.0".into()));
        assert_eq!(status(&st, false, "0.2.0"), Status::Failed);
        // Needs a newer Windows: not offered.
        let mut m2 = m;
        m2.min_os = Some([10, 0, 26100]);
        record(&mut st, 300, Ok(m2), os);
        assert!(!st.failed);
        assert_eq!(st.latest, "");
        assert_eq!(status(&st, false, "0.1.0"), Status::UpToDate);
    }

    #[test]
    fn state_file_round_trips_and_tolerates_junk() {
        let dir = std::env::temp_dir().join(format!("nopbar-updates-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("update.json");
        assert_eq!(load_state_from(&p), State::default(), "missing file");
        let st = state("0.2.0", "0.2.0");
        save_state_to(&p, &st).unwrap();
        assert_eq!(load_state_from(&p), st);
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(load_state_from(&p), State::default());
        std::fs::write(&p, r#"{"latest":"0.3.0","future":true}"#).unwrap();
        assert_eq!(load_state_from(&p).latest, "0.3.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notified_only_moves_forward() {
        let mut st = State::default();
        mark_notified(&mut st, "0.2.0");
        assert_eq!(st.notified, "0.2.0");
        mark_notified(&mut st, "0.1.5");
        assert_eq!(st.notified, "0.2.0", "a stale writer cannot take it back");
        mark_notified(&mut st, "junk");
        assert_eq!(st.notified, "0.2.0");
        mark_notified(&mut st, "0.3.0-rc.1");
        assert_eq!(st.notified, "0.3.0-rc.1");
        st.notified = "junk".into();
        mark_notified(&mut st, "0.1.0");
        assert_eq!(st.notified, "0.1.0", "an unreadable record is replaced");
    }

    #[test]
    fn an_older_check_does_not_overwrite_a_newer_one() {
        let os = [10, 0, 22631];
        let m = |v: &str| parse_manifest(format!(r#"{{"version":"{v}","url":"https://a.com/d"}}"#).as_bytes());
        let mut st = State::default();
        assert!(record(&mut st, 200, m("0.3.0"), os));
        // Started earlier, finished later: dropped.
        assert!(!record(&mut st, 150, m("0.2.0"), os));
        assert!(!record(&mut st, 150, Err("offline".into()), os));
        assert_eq!((st.last_check, st.latest.as_str(), st.failed), (200, "0.3.0", false));
        // Same start or later: taken.
        assert!(record(&mut st, 200, Err("offline".into()), os));
        assert!(st.failed);
        // A last check far in the future (clock moved back) does not block.
        let mut st = State { last_check: 200 + INTERVAL + 1, ..Default::default() };
        assert!(record(&mut st, 100, m("0.2.0"), os));
        assert_eq!(st.last_check, 100);
    }

    #[test]
    fn transactions_do_not_lose_updates() {
        let dir = std::env::temp_dir().join(format!("nopbar-updates-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("update.json");
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        transact_at(&p, |st| st.last_check += 1).expect("lock");
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(load_state_from(&p).last_check, 100, "every read-modify-write landed");
        // Unchanged: not written.
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert_eq!(transact_at(&p, |st| st.last_check), Some(100));
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn request_steps_share_one_deadline() {
        use std::time::Instant;
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_millis(TOTAL_MS);
        assert_eq!(step_ms(t0, deadline), Ok(TIMEOUT_MS), "a step gets at most 5 s");
        assert_eq!(step_ms(t0 + Duration::from_millis(TOTAL_MS - 1200), deadline), Ok(1200), "then only what is left");
        assert!(step_ms(deadline, deadline).is_err());
        assert!(step_ms(deadline + Duration::from_secs(3), deadline).is_err());
        assert!(TOTAL_MS >= TIMEOUT_MS as u64);
    }

    #[test]
    fn toast_xml_is_escaped() {
        let x = toast_xml("NopBar 0.2.0 is out", "Fixes <b> & \"quotes\"\u{1}", "https://a.com/d?x=1&y=2", "Download", "Later");
        assert!(x.contains("launch=\"https://a.com/d?x=1&amp;y=2\""));
        assert!(x.contains("arguments=\"https://a.com/d?x=1&amp;y=2\""));
        assert!(x.contains("Fixes &lt;b&gt; &amp; &quot;quotes&quot;</text>"));
        assert!(x.contains("activationType=\"system\" arguments=\"dismiss\""));
        assert!(!x.contains('\u{1}'));
        let long = "가".repeat(1000);
        let x = toast_xml("t", &long, "https://a.com", "d", "l");
        assert!(x.contains(&format!("{}…", "가".repeat(399))));
    }

    #[test]
    fn https_urls() {
        assert!(is_https_url("https://nopbard.com/nopbar/download"));
        assert!(is_https_url("HTTPS://nopbard.com"));
        for u in ["http://nopbard.com", "https://", "https:///x", "javascript:alert(1)", "https://a.com/ x", "https://a.com/\"", "nopbar:about", "https://a.com\\x"] {
            assert!(!is_https_url(u), "{u}");
        }
    }
}

#[cfg(test)]
mod manifest_url_tests {
    #[test]
    fn only_plain_https_overrides() {
        use super::{pick_manifest_url, MANIFEST_URL};
        assert_eq!(pick_manifest_url(None), MANIFEST_URL);
        assert_eq!(pick_manifest_url(Some("http://x/v.json")), MANIFEST_URL);
        assert_eq!(pick_manifest_url(Some("file:///c:/v.json")), MANIFEST_URL);
        assert_eq!(pick_manifest_url(Some("https://a b/v.json")), MANIFEST_URL);
        assert_eq!(pick_manifest_url(Some(" https://gist.githubusercontent.com/u/1/raw/v.json ")), "https://gist.githubusercontent.com/u/1/raw/v.json");
    }
}
