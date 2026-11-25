//! Automatic updates for the direct-download build (installed or
//! portable; never a Store build): download the portable zip a
//! v2 manifest lists, check it, stage it, and install it at a quiet moment
//! (or when asked) through the updater process (`apply.rs`).
//! docs/updates.md, "Automatic updates".
//!
//! - Two routes ([`Route`]): an **installed** copy (its folder is the
//!   installer's `InstallLocation`) updates with the new installer
//!   (`downloads.setup`, run silently by the updater, the previous one kept
//!   for rollback); a **portable** copy (or an installed one whose manifest
//!   lists no installer) with the portable zip (`downloads.zip`).
//! - The dock's update planner (crate::updates) calls [`tick`] after each
//!   look at update.json: a newer version is downloaded into
//!   `%LOCALAPPDATA%\NopBar\updates\<version>\` (a `.part` file, exact
//!   size, SHA-256). A zip is extracted with the strict reader (`unzip.rs`)
//!   into `staged\`; the new nopbar.exe, or the setup exe, must have the
//!   same Authenticode signer as the running exe (when that one is signed).
//! - "Install updates automatically" on: installed once NopBar is quiet
//!   ([`is_quiet`]); off ("Ask first"): a toast offers Install now / Later.
//! - "Restart to update" (Settings), the toast's Install now and
//!   `--update-now` ask the dock over its pipe ([`request`]); with no dock
//!   running, the command-line process does it itself.
//! - Progress and outcome go to update.json (`State::pending`), which the
//!   settings window shows. update.json is only bookkeeping: what is
//!   installed, from where and with which hash always comes from the
//!   signed manifest kept in `updates\manifest.json` (+ `.sig`), checked
//!   again right before anything runs ([`trust`]).

pub mod apply;
pub mod inflate;
pub mod sha256;
pub mod trust;
pub mod unzip;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use crate::updates::{self, Phase, State, CURRENT};

/// The whole download may take this long.
const DOWNLOAD_TOTAL: Duration = Duration::from_secs(15 * 60);
/// Each network step of the download.
const DOWNLOAD_STEP_MS: i32 = 30_000;
/// No keyboard or mouse input for this long before a quiet install.
pub const QUIET_IDLE: Duration = Duration::from_secs(120);
/// While an update waits for a quiet moment, look this often.
pub const QUIET_POLL: Duration = Duration::from_secs(60);
/// A download that failed is tried again after this long (seconds).
const RETRY_FAILED: u64 = 3600;
/// A download or install that says it is running but has not moved for
/// this long died with its process (seconds).
const STALE: u64 = 30 * 60;

/// %LOCALAPPDATA%\NopBar\updates
pub fn updates_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("NopBar").join("updates"))
}

/// A version string that is safe as a folder name (it already parsed).
fn version_dir_name(v: &str) -> Option<String> {
    updates::Version::parse(v)?;
    let v = v.trim().trim_start_matches('v');
    (v.len() <= 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))).then(|| v.to_string())
}

// ---------------------------------------------------------------- can this copy update itself?

/// `exe` is a cargo build (target\debug or target\release).
pub fn is_dev_build(exe: &Path) -> bool {
    let mut up = exe.ancestors().skip(1);
    let (Some(profile), Some(target)) = (up.next(), up.next()) else { return false };
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(name(profile).as_str(), "debug" | "release") && name(target) == "target"
}

/// How this copy updates itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The folder nopbar.exe runs from.
    pub install: PathBuf,
    /// It is the installer's copy (`InstallLocation` of the Apps & features
    /// entry): updated with the installer when the manifest lists one.
    pub installed: bool,
}

/// What a version is installed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The installer (`downloads.setup`), run silently.
    Setup,
    /// The portable zip (`downloads.zip`), files swapped by the updater.
    Zip,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Setup => "setup",
            Kind::Zip => "zip",
        }
    }
}

/// The route for `exe`: Err (with the reason) for a Store build, a
/// renamed exe or a cargo build. `install_location` is the installer's
/// registered folder, if any.
pub fn route_for(exe: &Path, channel: updates::Channel, install_location: Option<&Path>) -> Result<Route, &'static str> {
    let install = path_eligibility(exe, channel)?;
    let installed = install_location.is_some_and(|loc| apply::norm(loc) == apply::norm(&install));
    Ok(Route { install, installed })
}

/// Whether this copy may replace itself; the reason when not.
pub fn path_eligibility(exe: &Path, channel: updates::Channel) -> Result<PathBuf, &'static str> {
    if channel != updates::Channel::Direct {
        return Err("the store updates this build");
    }
    if !exe.file_name().is_some_and(|n| n.eq_ignore_ascii_case("nopbar.exe")) {
        return Err("the exe is not called nopbar.exe");
    }
    if is_dev_build(exe) {
        return Err("a development build");
    }
    let dir = exe.parent().ok_or("no folder")?;
    if !dir.is_absolute() {
        return Err("not an absolute path");
    }
    Ok(dir.to_path_buf())
}

/// The install folder can be written (a file made and removed again).
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".nopbar-write-test-{}", std::process::id()));
    let ok = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// This copy's route when it can update itself (checked once per process;
/// the reason is logged).
pub fn eligible() -> Option<Route> {
    static E: std::sync::OnceLock<Option<Route>> = std::sync::OnceLock::new();
    E.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let loc = crate::release::install_location();
        let r = route_for(&exe, updates::channel(), loc.as_deref())
            .and_then(|r| if trust::embedded_key().is_some() { Ok(r) } else { Err("no update-signing key is built in") })
            .and_then(|r| if trust::elevated() { Err("running elevated") } else { Ok(r) })
            .and_then(|r| if writable(&r.install) { Ok(r) } else { Err("its folder is not writable") });
        match r {
            Ok(r) => {
                crate::log!("autoupdate: on for this copy ({}, {})", if r.installed { "installed" } else { "portable" }, r.install.display());
                Some(r)
            }
            Err(why) => {
                crate::log!("autoupdate: off for this copy ({why}); updates are only announced");
                None
            }
        }
    })
    .clone()
}

// ---------------------------------------------------------------- the signed manifest

fn kept_manifest() -> Option<(PathBuf, PathBuf)> {
    updates_dir().map(|d| (d.join("manifest.json"), d.join("manifest.json.sig")))
}

/// After a check: fetch `<manifest url>.sig` and, if it verifies with the
/// built-in key, keep the manifest's exact bytes and the signature for the
/// automatic installs; otherwise forget any kept one. Only for a manifest
/// that lists downloads, and only for a copy that updates itself.
pub fn keep_if_signed(url: &str, bytes: &[u8], m: &updates::Manifest) {
    let Some((mpath, spath)) = kept_manifest() else { return };
    if (m.zip.is_none() && m.setup.is_none()) || eligible().is_none() {
        return;
    }
    let sig_url = format!("{}.sig", url.split(['?', '#']).next().unwrap_or(url));
    let r = updates::fetch_capped(&sig_url, trust::MAX_SIG_BYTES)
        .and_then(|sig| String::from_utf8(sig).map_err(|_| "the signature is not text".to_string()))
        .and_then(|sig| trust::verify(bytes, &sig).map(|_| sig));
    match r {
        Ok(sig) => {
            let write = |p: &Path, b: &[u8]| -> Result<(), String> {
                let tmp = p.with_extension(format!("{}.tmp", std::process::id()));
                std::fs::write(&tmp, b).and_then(|_| std::fs::rename(&tmp, p)).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    e.to_string()
                })
            };
            let _ = std::fs::create_dir_all(mpath.parent().unwrap_or(Path::new(".")));
            if let Err(e) = write(&mpath, bytes).and_then(|_| write(&spath, sig.as_bytes())) {
                crate::log!("autoupdate: could not keep the signed manifest: {e}");
            } else {
                crate::log!("autoupdate: manifest {} is signed", m.version);
            }
        }
        Err(e) => {
            crate::log!("autoupdate: {} not installed automatically: {e} ({})", m.version, trust::redact_url(&sig_url));
            let _ = std::fs::remove_file(&mpath);
            let _ = std::fs::remove_file(&spath);
        }
    }
}

/// The kept manifest, read through locked handles and verified again with
/// the built-in key (None: nothing kept, or it does not verify).
pub fn verified_manifest() -> Option<updates::Manifest> {
    let (mpath, spath) = kept_manifest()?;
    let mut m = trust::Locked::open(&mpath).ok()?;
    let mut sg = trust::Locked::open(&spath).ok()?;
    let bytes = m.read_all(updates::MAX_BYTES as u64).ok()?;
    let sig = String::from_utf8(sg.read_all(trust::MAX_SIG_BYTES as u64).ok()?).ok()?;
    if let Err(e) = trust::verify(&bytes, &sig) {
        crate::log!("autoupdate: the kept manifest does not verify ({e}); ignored");
        return None;
    }
    // Release file names resolve against the official origin; a test
    // manifest names full URLs.
    updates::parse_manifest_at(&bytes, &manifest_base()).ok()
}

fn manifest_base() -> String {
    crate::config::read_current().and_then(|c| c.updates.manifest_url).filter(|u| u.starts_with("https://")).unwrap_or_else(|| updates::MANIFEST_URL.to_string())
}

/// `st` with the latest version and its files taken from the signed
/// manifest `m` (none when there is no signed manifest): what automatic
/// installs decide on. Everything else (pending, bad) stays.
pub fn signed_view(st: &State, m: Option<&updates::Manifest>) -> State {
    let mut v = st.clone();
    match m {
        Some(m) => {
            v.latest = m.version.clone();
            v.zip = m.zip.clone();
            v.setup = m.setup.clone();
        }
        None => {
            v.zip = None;
            v.setup = None;
        }
    }
    v
}

/// The configured install policy: install quietly (true) or ask first.
fn auto_install() -> bool {
    crate::config::read_current().map(|c| c.updates.auto_install).unwrap_or(true)
}

// ---------------------------------------------------------------- policy

/// What to do about the state in update.json.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    Nothing,
    /// Fetch and stage this version, this way.
    Download(String, updates::Download, Kind),
    /// Staged; install at the next quiet moment.
    InstallWhenQuiet,
    /// Staged; show the "ready" toast (Ask first).
    Ask,
    /// A run that died left the state "installing": give up on it.
    GiveUp,
}

/// Which file of the latest version this copy installs: the installer for
/// the installed copy (when listed), else the zip.
pub fn pick(st: &State, installed: bool) -> Option<(updates::Download, Kind)> {
    let ok = |d: &Option<updates::Download>| d.as_ref().filter(|d| updates::is_https_url(&d.url)).cloned();
    match (installed, ok(&st.setup), ok(&st.zip)) {
        (true, Some(setup), _) => Some((setup, Kind::Setup)),
        (_, _, Some(zip)) => Some((zip, Kind::Zip)),
        _ => None,
    }
}

/// The version automatic updates would install: newer than `current`,
/// listed with a file this copy can use, not one that failed before.
pub fn target(st: &State, current: &str, installed: bool) -> Option<(String, updates::Download, Kind)> {
    let (dl, kind) = pick(st, installed)?;
    if !updates::is_newer(&st.latest, current) || st.latest == st.bad {
        return None;
    }
    Some((st.latest.clone(), dl, kind))
}

/// [`target`] for this copy (None when it does not update itself).
pub fn target_here(st: &State) -> Option<(String, updates::Download, Kind)> {
    let route = eligible()?;
    target(&signed_view(st, verified_manifest().as_ref()), CURRENT, route.installed)
}

pub fn next(st: &State, current: &str, installed: bool, auto_install: bool, now: u64) -> Next {
    let Some((v, dl, kind)) = target(st, current, installed) else { return Next::Nothing };
    let p = &st.pending;
    let age = now.saturating_sub(p.since);
    // Another version, or the same one as the other file (e.g. an
    // installer listed after the zip was staged): start over.
    if p.version != v || (p.kind != kind.name() && p.phase != Phase::Installing) {
        return Next::Download(v, dl, kind);
    }
    match p.phase {
        Phase::None => Next::Download(v, dl, kind),
        Phase::Downloading | Phase::Verifying if age >= STALE || p.since > now + 600 => Next::Download(v, dl, kind),
        Phase::Downloading | Phase::Verifying => Next::Nothing,
        Phase::Failed if age >= RETRY_FAILED || p.since > now + 600 => Next::Download(v, dl, kind),
        Phase::Failed => Next::Nothing,
        Phase::Ready if auto_install => Next::InstallWhenQuiet,
        Phase::Ready if p.asked != v => Next::Ask,
        Phase::Ready => Next::Nothing,
        Phase::Installing if age >= STALE => Next::GiveUp,
        Phase::Installing => Next::Nothing,
    }
}

/// The dock's checker found a version this copy will download and install
/// itself (so the "is out" toast is not shown).
pub fn will_install(st: &State) -> bool {
    target_here(st).is_some()
}

/// A full-screen app, a presentation or Do not disturb for games
/// (SHQueryUserNotificationState): not a moment to restart NopBar.
pub fn notification_busy(state: i32) -> bool {
    // QUNS_BUSY, QUNS_RUNNING_D3D_FULL_SCREEN, QUNS_PRESENTATION_MODE, QUNS_APP.
    matches!(state, 2 | 3 | 4 | 7)
}

/// What a quiet moment needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quiet {
    /// Since the last keyboard or mouse input.
    pub idle: Duration,
    /// SHQueryUserNotificationState said busy.
    pub busy: bool,
    /// A settings window is open.
    pub settings_open: bool,
    /// The dock has a panel, menu or drag going (or did not answer).
    pub ui_busy: bool,
}

pub fn is_quiet(q: &Quiet) -> bool {
    q.idle >= QUIET_IDLE && !q.busy && !q.settings_open && !q.ui_busy
}

fn quiet_now() -> Quiet {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows::Win32::UI::Shell::SHQueryUserNotificationState;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    let idle = unsafe {
        let mut li = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if GetLastInputInfo(&mut li).as_bool() {
            Duration::from_millis(GetTickCount().wrapping_sub(li.dwTime) as u64)
        } else {
            Duration::ZERO
        }
    };
    let busy = unsafe { SHQueryUserNotificationState() }.map(|s| notification_busy(s.0)).unwrap_or(true);
    let settings_open = unsafe { FindWindowW(windows::core::w!("NopBarSettings"), windows::core::PCWSTR::null()) }.is_ok();
    // Asked last: it is the only one that reaches the UI thread.
    let ui_busy = if idle >= QUIET_IDLE && !busy && !settings_open { !crate::dock::ui_quiet() } else { true };
    Quiet { idle, busy, settings_open, ui_busy }
}

// ---------------------------------------------------------------- requests

/// 0 = none, 1 = install now, 2 = check first, then install now.
static REQUEST: AtomicU8 = AtomicU8::new(0);

/// Install now (from the pipe: Settings, the toast, --update-now). The
/// planner thread does it.
pub fn request(check: bool) {
    REQUEST.fetch_max(if check { 2 } else { 1 }, Ordering::SeqCst);
    updates::request_plan();
}

/// The pending request: Some(check first).
pub fn take_request() -> Option<bool> {
    match REQUEST.swap(0, Ordering::SeqCst) {
        0 => None,
        n => Some(n == 2),
    }
}

// ---------------------------------------------------------------- the dock's step

/// Called by the update planner (its own thread) with the current state.
/// Some(d) = look again within `d` (waiting for a quiet moment).
pub fn tick(st: &State) -> Option<Duration> {
    static CLEANED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let route = eligible()?;
    if !CLEANED.load(Ordering::SeqCst) && startup_cleanup(&route.install, st) {
        CLEANED.store(true, Ordering::SeqCst);
    }
    apply::recover_stale_journal();
    let signed = verified_manifest();
    let view = signed_view(st, signed.as_ref());
    let st = &view;
    let auto = auto_install();
    match next(st, CURRENT, route.installed, auto, updates::unix_now()) {
        Next::Nothing => None,
        Next::GiveUp => {
            crate::log!("autoupdate: the install of {} never finished; not trying it again", st.pending.version);
            let v = st.pending.version.clone();
            let _ = updates::transact(|s| {
                if s.pending.phase == Phase::Installing && s.pending.version == v {
                    s.pending.phase = Phase::Failed;
                    s.pending.error = "the updater did not finish".into();
                    s.bad = v.clone();
                }
            });
            None
        }
        Next::Download(v, dl, kind) => {
            if let Err(e) = prepare(&v, &dl, kind) {
                crate::log!("autoupdate: {v} not staged: {e}");
                return None;
            }
            // Staged: decide again right away (quiet install or toast).
            let st = signed_view(&updates::load_state(), signed.as_ref());
            tick_ready(&st, route.installed, auto)
        }
        _ => tick_ready(st, route.installed, auto),
    }
}

fn tick_ready(st: &State, installed: bool, auto: bool) -> Option<Duration> {
    match next(st, CURRENT, installed, auto, updates::unix_now()) {
        Next::InstallWhenQuiet => {
            let q = quiet_now();
            if !is_quiet(&q) {
                crate::debug_log!("autoupdate: {} waits for a quiet moment ({q:?})", st.pending.version);
                return Some(QUIET_POLL);
            }
            crate::log!("autoupdate: quiet moment; installing {}", st.pending.version);
            if let Err(e) = launch(st, std::process::id(), crate::taskbar::guardian_pid()) {
                crate::log!("autoupdate: {e}");
            }
            None
        }
        Next::Ask => {
            let v = st.pending.version.clone();
            match updates::show_ready_toast(&v, &st.notes) {
                Ok(()) => {
                    crate::log!("autoupdate: {v} is ready; asked");
                    let _ = updates::transact(|s| s.pending.asked = v.clone());
                }
                Err(e) => crate::log!("autoupdate: {v} is ready; toast failed ({e})"),
            }
            None
        }
        _ => None,
    }
}

/// "Install now" on the planner thread (or the command line with no dock:
/// `dock_pid` 0): check first if asked, download if needed, then start the
/// updater whatever the time.
pub fn install_now(check: bool) {
    install_now_for(check, std::process::id(), crate::taskbar::guardian_pid());
}

fn install_now_for(check: bool, dock_pid: u32, guardian_pid: u32) {
    let Some(route) = eligible() else {
        crate::log!("autoupdate: install asked for, but this copy cannot update itself");
        return;
    };
    let st = if check { updates::check_now() } else { updates::load_state() };
    let mut st = signed_view(&st, verified_manifest().as_ref());
    let Some((v, dl, kind)) = target(&st, CURRENT, route.installed) else {
        crate::log!("autoupdate: install asked for, but there is nothing newer to install (latest {:?}, bad {:?})", st.latest, st.bad);
        return;
    };
    if !(st.pending.version == v && st.pending.kind == kind.name() && st.pending.phase == Phase::Ready) {
        if let Err(e) = prepare(&v, &dl, kind) {
            crate::log!("autoupdate: {v} not staged: {e}");
            return;
        }
        st = updates::load_state();
    }
    if let Err(e) = launch(&st, dock_pid, guardian_pid) {
        crate::log!("autoupdate: {e}");
    }
}

// ---------------------------------------------------------------- download and stage

fn set_phase(v: &str, kind: Kind, phase: Phase, progress: u8) {
    let _ = updates::transact(|s| {
        if s.pending.version != v || s.pending.kind != kind.name() {
            s.pending = updates::Pending { version: v.to_string(), kind: kind.name().to_string(), ..Default::default() };
        }
        if s.pending.phase != phase {
            s.pending.since = updates::unix_now();
        }
        s.pending.phase = phase;
        s.pending.progress = progress;
        s.pending.error.clear();
    });
}

fn fail(v: &str, e: String) -> String {
    let _ = updates::transact(|s| {
        if s.pending.version == v {
            s.pending.phase = Phase::Failed;
            s.pending.since = updates::unix_now();
            s.pending.error = e.chars().take(300).collect();
        }
    });
    e
}

/// Downloads, checks and stages `v` (the zip extracted, or the setup exe
/// as it is). On success update.json says Ready.
pub fn prepare(v: &str, dl: &updates::Download, kind: Kind) -> Result<PathBuf, String> {
    let name = version_dir_name(v).ok_or_else(|| format!("{v:?} is not a usable version"))?;
    let root = updates_dir().ok_or("no %LOCALAPPDATA%")?;
    let dir = root.join(&name);
    std::fs::create_dir_all(&dir).map_err(|e| fail(v, format!("{}: {e}", dir.display())))?;
    set_phase(v, kind, Phase::Downloading, 0);
    crate::log!("autoupdate: downloading {v} ({}) from {} ({} bytes)", kind.name(), trust::redact_url(&dl.url), dl.size);
    // Kept as downloaded (zip or setup exe); checked now and again at
    // install time (launch).
    let file = dir.join(match kind {
        Kind::Zip => format!("NopBar-{name}-portable.zip"),
        Kind::Setup => format!("NopBar-{name}-setup.exe"),
    });
    download(v, kind, dl, &file).map_err(|e| fail(v, e))?;
    set_phase(v, kind, Phase::Verifying, 100);
    let checked = match kind {
        Kind::Zip => {
            // Taken apart in a scratch folder: names, version, signer.
            let scratch = dir.join(format!("check-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&scratch);
            let r = trust::locked_with_hash(&file, &dl.sha256).and_then(|mut l| l.read_all(updates::MAX_DOWNLOAD)).and_then(|bytes| stage(&bytes, &scratch, v)).map(|_| ());
            let _ = std::fs::remove_dir_all(&scratch);
            r
        }
        Kind::Setup => check_new_file(&file, v),
    };
    if let Err(e) = checked {
        let _ = std::fs::remove_file(&file);
        return Err(fail(v, e));
    }
    let (staged, exe_sha) = (file, dl.sha256.clone());
    let _ = updates::transact(|s| {
        if s.pending.version == v {
            s.pending.phase = Phase::Ready;
            s.pending.since = updates::unix_now();
            s.pending.staged = staged.display().to_string();
            s.pending.exe_sha256 = exe_sha.clone();
            s.pending.progress = 100;
        }
    });
    crate::log!("autoupdate: {v} is ready ({})", staged.display());
    crate::companion::cue(crate::companion::Cue::Update(v.into()));
    Ok(staged)
}

/// What a download writes: the exact size and hash or an error.
pub struct Sink {
    file: std::fs::File,
    hash: sha256::Sha256,
    got: u64,
    size: u64,
}

impl Sink {
    pub fn new(file: std::fs::File, size: u64) -> Result<Sink, String> {
        Ok(Sink { file, hash: sha256::Sha256::new()?, got: 0, size })
    }


    pub fn write(&mut self, chunk: &[u8]) -> Result<(), String> {
        use std::io::Write;
        self.got += chunk.len() as u64;
        if self.got > self.size {
            return Err(format!("more than the {} bytes the manifest says", self.size));
        }
        self.hash.update(chunk)?;
        self.file.write_all(chunk).map_err(|e| format!("write: {e}"))
    }

    /// Size and SHA-256 must be exactly the manifest's.
    pub fn finish(mut self, want_sha: &str) -> Result<(), String> {
        use std::io::Write;
        if self.got != self.size {
            return Err(format!("got {} bytes, the manifest says {}", self.got, self.size));
        }
        self.file.flush().and_then(|_| self.file.sync_all()).map_err(|e| format!("write: {e}"))?;
        let got = self.hash.finish()?;
        if !sha256::matches(&got, want_sha) {
            return Err(format!("SHA-256 mismatch (got {}, the manifest says {want_sha})", sha256::hex(&got)));
        }
        Ok(())
    }
}

/// The Content-Length the server announced must be the manifest's size.
pub fn check_length(len: u64, size: u64) -> Result<(), String> {
    if len != size {
        return Err(format!("the server says {len} bytes, the manifest {size}"));
    }
    Ok(())
}

/// Streams `zip.url` to `<file>.part`, checks it, renames it to `file`.
fn download(v: &str, kind: Kind, zip: &updates::Download, file: &Path) -> Result<(), String> {
    let mut part = file.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let _ = std::fs::remove_file(&part);
    let f = std::fs::OpenOptions::new().write(true).create_new(true).open(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut sink = Sink::new(f, zip.size)?;
    let mut last = std::time::Instant::now();
    let lim = updates::Limits { total: DOWNLOAD_TOTAL, step_ms: DOWNLOAD_STEP_MS, same_site: false };
    let r = updates::http_get(&zip.url, &lim, &mut |len| check_length(len, zip.size), &mut |chunk| {
        sink.write(chunk)?;
        if last.elapsed() >= Duration::from_millis(700) {
            last = std::time::Instant::now();
            set_phase(v, kind, Phase::Downloading, (sink.got * 100 / sink.size.max(1)).min(99) as u8);
        }
        Ok(())
    });
    let r = r.and_then(|_| sink.finish(&zip.sha256));
    if let Err(e) = r {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let _ = std::fs::remove_file(file);
    std::fs::rename(&part, file).map_err(|e| format!("{}: {e}", file.display()))
}

/// Extracts the checked zip bytes into `staged` (which must not exist)
/// and checks the new exe ([`check_new_file`]). Returns the SHA-256 of the
/// extracted nopbar.exe.
fn stage(bytes: &[u8], staged: &Path, version: &str) -> Result<String, String> {
    unzip::extract(bytes, staged, apply::FILES, &["nopbar.exe"], updates::MAX_DOWNLOAD, updates::MAX_DOWNLOAD)?;
    let new_exe = staged.join("nopbar.exe");
    check_new_file(&new_exe, version)?;
    Ok(sha256::hex(&sha256::of_file(&new_exe)?))
}

/// The new nopbar.exe or setup exe says it is `version` (ProductVersion),
/// newer than this one, and is signed like this exe (same key).
fn check_new_file(file: &Path, version: &str) -> Result<(), String> {
    trust::version_ok(trust::product_version(file).as_deref(), version, CURRENT)?;
    trust::same_publisher(file)
}

// ---------------------------------------------------------------- launch the updater

/// Starts the updater. Nothing from update.json is trusted here but the
/// version and kind to install: the file and its hash come from the
/// signed manifest (verified again now), the file is opened so that it
/// cannot be changed, checked, and held until the updater has started:
/// - zip: the zip is checked and extracted into a fresh folder, whose
///   nopbar.exe (version, signer checked again) runs as the updater
///   (`apply::run`);
/// - setup: the setup exe is checked; a copy of this exe in a fresh
///   folder runs as the updater (`apply::run_setup`) and checks the setup
///   exe again before it runs it.
fn launch(st: &State, dock_pid: u32, guardian_pid: u32) -> Result<(), String> {
    let v = st.pending.version.clone();
    if st.pending.phase != Phase::Ready || !updates::is_newer(&v, CURRENT) || v == st.bad {
        return Err(format!("{v} is not ready to install"));
    }
    // Desktop notes saved first; put off once while a text can be kept
    // nowhere (the next try goes on).
    if !crate::notes::prepare_quit(crate::notes::Quit::Update) {
        return Err("desktop notes could not be saved; the update waits for the next try".into());
    }
    let route = eligible().ok_or("this copy cannot update itself")?;
    let signed = verified_manifest().ok_or("no signed manifest")?;
    let view = signed_view(st, Some(&signed));
    let Some((sv, dl, kind)) = target(&view, CURRENT, route.installed) else { return Err(format!("the signed manifest offers nothing newer than {CURRENT}")) };
    if sv != v || kind.name() != st.pending.kind {
        let _ = updates::transact(|s| s.pending.phase = Phase::None);
        return Err(format!("the signed manifest offers {sv} ({}), not the staged {v}; downloading again", kind.name()));
    }
    let file = PathBuf::from(&st.pending.staged);
    let name = version_dir_name(&v).ok_or("bad version")?;
    let dir = updates_dir().ok_or("no %LOCALAPPDATA%")?.join(&name);
    // Must be our own download, not a path someone wrote into update.json.
    if apply::norm(file.parent().unwrap_or(Path::new(""))) != apply::norm(&dir) {
        let _ = updates::transact(|s| s.pending.phase = Phase::None);
        return Err("the staged file is not in the updates folder; downloading again".into());
    }
    let mut zip = match trust::locked_with_hash(&file, &dl.sha256) {
        Ok(l) => l,
        Err(e) => {
            let _ = updates::transact(|s| s.pending.phase = Phase::None);
            return Err(format!("{e}; downloading again"));
        }
    };
    let fresh = |what: &str| -> Result<PathBuf, String> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let d = dir.join(format!("{what}-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        Ok(d)
    };
    // Held until the updater runs: nothing it uses can be swapped.
    let mut held: Vec<trust::Locked> = Vec::new();
    let (exe, args, cwd) = match kind {
        Kind::Zip => {
            let bytes = zip.read_all(updates::MAX_DOWNLOAD)?;
            let run = fresh("run")?;
            stage(&bytes, &run, &v)?;
            for f in apply::FILES {
                let p = run.join(f);
                if p.is_file() {
                    held.push(trust::Locked::open(&p)?);
                }
            }
            let exe = run.join("nopbar.exe");
            let a = apply::Args { staged: run.clone(), install: route.install, dock_pid, guardian_pid, from: CURRENT.to_string() };
            (exe, apply::command_args(&a), run)
        }
        Kind::Setup => {
            check_new_file(&file, &v)?;
            // The installer replaces the installed nopbar.exe: the updater
            // is a copy of this exe outside the install folder.
            let updater = fresh("updater")?;
            let me = std::env::current_exe().map_err(|e| e.to_string())?;
            // (A running exe may refuse a read-only-share open; it cannot
            // be written while it runs anyway.)
            let my_hash = match trust::Locked::open(&me) {
                Ok(mut l) => {
                    let h = l.sha256()?;
                    held.push(l);
                    h
                }
                Err(_) => sha256::hex(&sha256::of_file(&me)?),
            };
            let copy = updater.join("nopbar.exe");
            std::fs::copy(&me, &copy).map_err(|e| format!("{}: {e}", copy.display()))?;
            held.push(trust::locked_with_hash(&copy, &my_hash)?);
            let a = apply::SetupArgs { setup: file.clone(), sha256: dl.sha256.clone(), install: route.install, dock_pid, guardian_pid, from: CURRENT.to_string(), to: v.clone() };
            (copy, apply::setup_command_args(&a), updater)
        }
    };
    let _ = updates::transact(|s| {
        s.pending.phase = Phase::Installing;
        s.pending.since = updates::unix_now();
    });
    let r = std::process::Command::new(&exe).args(args).current_dir(&cwd).spawn();
    drop(held);
    drop(zip);
    match r {
        Ok(child) => {
            crate::log!("autoupdate: updater for {v} started (pid {})", child.id());
            Ok(())
        }
        Err(e) => {
            let _ = updates::transact(|s| s.pending.phase = Phase::Ready);
            Err(format!("could not start the updater: {e}"))
        }
    }
}

/// Once per dock run, when no update is moving: temporary files a past
/// update left in the install folder, and staged folders of versions no
/// longer pending. False = not now.
fn startup_cleanup(install: &Path, st: &State) -> bool {
    if matches!(st.pending.phase, Phase::Installing | Phase::Downloading | Phase::Verifying) || apply::journal_path().is_some_and(|j| j.exists()) {
        return false;
    }
    apply::clean_install_dir(install);
    let Some(root) = updates_dir() else { return true };
    let keep = version_dir_name(&st.pending.version).filter(|_| st.pending.phase == Phase::Ready);
    for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == "backup" || name == "setup-cache" || Some(&name) == keep.as_ref() || version_dir_name(&name).is_none() {
            continue;
        }
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    true
}

// ---------------------------------------------------------------- command line

/// `--update-now` (check, download, install now) and `--install-update`
/// (the toast's Install now: install what is staged). With a dock running
/// the dock does it (asked over its pipe); otherwise this process does.
/// Everything goes to dock.log. Returns the exit code.
pub fn cli(check: bool) -> i32 {
    crate::i18n::init(&crate::config::read_current().map(|c| c.language).unwrap_or_default());
    if updates::channel() != updates::Channel::Direct {
        crate::log!("autoupdate: {:?} build; its store updates it", updates::channel());
        return 1;
    }
    let mut client = crate::settings::client::Client::connect();
    if client.online() {
        let ok = client.install_update(check);
        crate::log!("autoupdate: asked the dock to {} ({})", if check { "check and install" } else { "install" }, if ok { "accepted" } else { "refused" });
        return if ok { 0 } else { 1 };
    }
    crate::log!("autoupdate: no dock running; updating from this process");
    install_now_for(check, 0, 0);
    0
}

#[cfg(test)]
pub(crate) mod testutil {
    /// A scratch folder under target\test-scratch (never outside the repo),
    /// removed on drop.
    pub struct Scratch(pub std::path::PathBuf);

    impl Scratch {
        pub fn new(name: &str) -> Scratch {
            let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(r"..\target\test-scratch").join(format!("{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Scratch(d)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updates::{Download, Pending};

    fn zip() -> Download {
        Download { url: "https://nopbard.com/nopbar/releases/NopBar-0.2.0-portable.zip".into(), sha256: "ab".repeat(32), size: 3_592_623 }
    }

    fn st(latest: &str, phase: Phase, pending_v: &str) -> State {
        State { latest: latest.into(), url: "https://nopbard.com/nopbar/download".into(), zip: Some(zip()), pending: Pending { version: pending_v.into(), kind: "zip".into(), phase, since: 1000, ..Default::default() }, ..Default::default() }
    }

    #[test]
    fn never_the_same_older_or_failed_version() {
        let now = 2000;
        // Newer: downloaded.
        assert!(matches!(next(&st("0.2.0", Phase::None, ""), "0.1.0", false, true, now), Next::Download(v, _, _) if v == "0.2.0"));
        // Equal or older: nothing, whatever the pending state says.
        for cur in ["0.2.0", "0.3.0", "0.2.1-rc.1"] {
            for phase in [Phase::None, Phase::Ready, Phase::Failed] {
                assert_eq!(next(&st("0.2.0", phase, "0.2.0"), cur, false, true, now), Next::Nothing, "{cur} {phase:?}");
            }
        }
        // A pre-release newer than the running one counts.
        assert!(matches!(next(&st("0.2.0-rc.1", Phase::None, ""), "0.1.0", false, true, now), Next::Download(..)));
        // The version that failed to install: never again; a newer one is.
        let mut s = st("0.2.0", Phase::Ready, "0.2.0");
        s.bad = "0.2.0".into();
        assert_eq!(next(&s, "0.1.0", false, true, now), Next::Nothing);
        s.latest = "0.2.1".into();
        assert!(matches!(next(&s, "0.1.0", false, true, now), Next::Download(v, _, _) if v == "0.2.1"));
        // No zip (v1 manifest) or a bad one: nothing (the toast covers it).
        let mut s = st("0.2.0", Phase::None, "");
        s.zip = None;
        assert_eq!(next(&s, "0.1.0", false, true, now), Next::Nothing);
        let mut s = st("0.2.0", Phase::None, "");
        s.zip.as_mut().unwrap().url = "http://x/y.zip".into();
        assert_eq!(next(&s, "0.1.0", false, true, now), Next::Nothing);
        assert!(updates::Version::parse("garbage").is_none());
        assert_eq!(next(&st("garbage", Phase::None, ""), "0.1.0", false, true, now), Next::Nothing);
    }

    #[test]
    fn pending_phases_lead_on() {
        let now = 1000 + 60;
        // A newer version than the one staged: download that one.
        assert!(matches!(next(&st("0.3.0", Phase::Ready, "0.2.0"), "0.1.0", false, true, now), Next::Download(v, _, _) if v == "0.3.0"));
        // Ready: quiet install, or ask once.
        assert_eq!(next(&st("0.2.0", Phase::Ready, "0.2.0"), "0.1.0", false, true, now), Next::InstallWhenQuiet);
        assert_eq!(next(&st("0.2.0", Phase::Ready, "0.2.0"), "0.1.0", false, false, now), Next::Ask);
        let mut asked = st("0.2.0", Phase::Ready, "0.2.0");
        asked.pending.asked = "0.2.0".into();
        assert_eq!(next(&asked, "0.1.0", false, false, now), Next::Nothing);
        // In progress: leave it; stale (process died): start over / give up.
        assert_eq!(next(&st("0.2.0", Phase::Downloading, "0.2.0"), "0.1.0", false, true, now), Next::Nothing);
        assert!(matches!(next(&st("0.2.0", Phase::Downloading, "0.2.0"), "0.1.0", false, true, 1000 + STALE), Next::Download(..)));
        assert_eq!(next(&st("0.2.0", Phase::Installing, "0.2.0"), "0.1.0", false, true, now), Next::Nothing);
        assert_eq!(next(&st("0.2.0", Phase::Installing, "0.2.0"), "0.1.0", false, true, 1000 + STALE), Next::GiveUp);
        // A failed download: again after an hour.
        assert_eq!(next(&st("0.2.0", Phase::Failed, "0.2.0"), "0.1.0", false, true, now), Next::Nothing);
        assert!(matches!(next(&st("0.2.0", Phase::Failed, "0.2.0"), "0.1.0", false, true, 1000 + RETRY_FAILED), Next::Download(..)));
        // The clock went back a long way: not stuck.
        assert!(matches!(next(&st("0.2.0", Phase::Failed, "0.2.0"), "0.1.0", false, true, 10), Next::Download(..)));
    }

    #[test]
    fn installed_copies_take_the_installer_portable_ones_the_zip() {
        let setup = Download { url: "https://nopbard.com/nopbar/releases/NopBar-0.2.0-setup.exe".into(), sha256: "cd".repeat(32), size: 4_000_000 };
        let mut s = st("0.2.0", Phase::None, "");
        s.setup = Some(setup.clone());
        // Installed: the installer; portable: the zip.
        assert_eq!(pick(&s, true), Some((setup.clone(), Kind::Setup)));
        assert_eq!(pick(&s, false), Some((zip(), Kind::Zip)));
        assert!(matches!(next(&s, "0.1.0", true, true, 2000), Next::Download(v, d, Kind::Setup) if v == "0.2.0" && d == setup));
        assert!(matches!(next(&s, "0.1.0", false, true, 2000), Next::Download(_, _, Kind::Zip)));
        // Installed, but the manifest lists no installer: the zip.
        let mut z = s.clone();
        z.setup = None;
        assert_eq!(pick(&z, true), Some((zip(), Kind::Zip)));
        // Portable with only an installer listed: nothing (an installer
        // would install somewhere else).
        let mut o = s.clone();
        o.zip = None;
        assert_eq!(pick(&o, false), None);
        assert_eq!(next(&o, "0.1.0", false, true, 2000), Next::Nothing);
        // A non-https installer is not used.
        let mut h = s.clone();
        h.setup.as_mut().unwrap().url = "http://x/s.exe".into();
        assert_eq!(pick(&h, true), Some((zip(), Kind::Zip)));
        // The zip was staged, then an installer appeared: download that.
        let mut r = s.clone();
        r.pending = Pending { version: "0.2.0".into(), kind: "zip".into(), phase: Phase::Ready, since: 1000, ..Default::default() };
        assert!(matches!(next(&r, "0.1.0", true, true, 2000), Next::Download(_, _, Kind::Setup)));
        r.pending.kind = "setup".into();
        assert_eq!(next(&r, "0.1.0", true, true, 2000), Next::InstallWhenQuiet);
        // Installing either way is left alone.
        r.pending.phase = Phase::Installing;
        r.pending.kind = "zip".into();
        assert_eq!(next(&r, "0.1.0", true, true, 2000), Next::Nothing);
    }

    #[test]
    fn routes_follow_the_install_location() {
        use updates::Channel::*;
        let loc = Path::new(r"C:\Users\u\AppData\Local\Programs\NopBar");
        let r = route_for(Path::new(r"C:\Users\u\AppData\Local\Programs\NopBar\nopbar.exe"), Direct, Some(loc)).unwrap();
        assert!(r.installed);
        assert_eq!(r.install, loc);
        // Any case, a trailing backslash in the registry.
        assert!(route_for(Path::new(r"c:\users\u\appdata\local\programs\nopbar\NopBar.exe"), Direct, Some(Path::new(r"C:\Users\u\AppData\Local\Programs\NopBar\"))).unwrap().installed);
        // Portable: another folder, or no installer at all.
        assert!(!route_for(Path::new(r"D:\Tools\NopBar\nopbar.exe"), Direct, Some(loc)).unwrap().installed);
        assert!(!route_for(Path::new(r"D:\Tools\NopBar\nopbar.exe"), Direct, None).unwrap().installed);
        // Dev builds and Store builds never update themselves.
        assert!(route_for(Path::new(r"V:\src\nopbar\target\release\nopbar.exe"), Direct, Some(Path::new(r"V:\src\nopbar\target\release"))).is_err());
        assert!(route_for(Path::new(r"C:\x\nopbar.exe"), Store, Some(Path::new(r"C:\x"))).is_err());
    }

    #[test]
    fn quiet_moments() {
        let q = Quiet { idle: QUIET_IDLE, busy: false, settings_open: false, ui_busy: false };
        assert!(is_quiet(&q));
        assert!(!is_quiet(&Quiet { idle: QUIET_IDLE - Duration::from_secs(1), ..q }), "someone is typing");
        assert!(!is_quiet(&Quiet { busy: true, ..q }), "fullscreen game / presentation");
        assert!(!is_quiet(&Quiet { settings_open: true, ..q }));
        assert!(!is_quiet(&Quiet { ui_busy: true, ..q }), "a panel, menu or drag");
        assert!(is_quiet(&Quiet { idle: Duration::from_secs(3600), ..q }));
        // SHQueryUserNotificationState: busy, D3D fullscreen, presentation, app.
        for s in [2, 3, 4, 7] {
            assert!(notification_busy(s), "{s}");
        }
        // Not present (locked), accepts notifications, quiet time.
        for s in [1, 5, 6] {
            assert!(!notification_busy(s), "{s}");
        }
    }

    #[test]
    fn which_copies_update_themselves() {
        use updates::Channel::*;
        let ok = |p: &str| path_eligibility(Path::new(p), Direct);
        assert_eq!(ok(r"C:\Users\u\AppData\Local\Programs\NopBar\nopbar.exe"), Ok(PathBuf::from(r"C:\Users\u\AppData\Local\Programs\NopBar")));
        assert_eq!(ok(r"D:\Tools\NopBar-0.1.0\NOPBAR.EXE"), Ok(PathBuf::from(r"D:\Tools\NopBar-0.1.0")), "portable, any case");
        assert!(ok(r"V:\src\nopbar\target\release\nopbar.exe").is_err(), "dev build");
        assert!(ok(r"V:\src\nopbar\target\debug\nopbar.exe").is_err(), "dev build");
        assert!(ok(r"D:\Tools\nopbar-renamed.exe").is_err());
        assert!(ok(r"nopbar.exe").is_err(), "relative");
        assert!(path_eligibility(Path::new(r"C:\x\nopbar.exe"), Store).is_err());
        assert!(is_dev_build(Path::new(r"C:\a\TARGET\Release\nopbar.exe")));
        assert!(!is_dev_build(Path::new(r"C:\release\nopbar.exe")));
    }


    #[test]
    fn version_folders_are_plain() {
        assert_eq!(version_dir_name("0.2.0").as_deref(), Some("0.2.0"));
        assert_eq!(version_dir_name("v1.0.0-rc.1+b5").as_deref(), Some("1.0.0-rc.1+b5"));
        assert_eq!(version_dir_name("../0.2.0"), None);
        assert_eq!(version_dir_name("backup"), None);
    }

    #[test]
    fn downloads_are_checked_exactly() {
        let s = testutil::Scratch::new("au-sink");
        let data = b"portable zip bytes".to_vec();
        let sha = sha256::hex(&sha256::of(&data).unwrap());
        let open = |n: &str| std::fs::OpenOptions::new().write(true).create_new(true).open(s.0.join(n)).unwrap();
        // Right size and hash.
        let mut k = Sink::new(open("a"), data.len() as u64).unwrap();
        check_length(data.len() as u64, data.len() as u64).unwrap();
        k.write(&data[..5]).unwrap();
        k.write(&data[5..]).unwrap();
        assert!(k.finish(&sha).is_ok());
        assert_eq!(std::fs::read(s.0.join("a")).unwrap(), data);
        // Server announces another size.
        assert!(check_length(1 << 30, data.len() as u64).is_err());
        // More than announced: stopped at once.
        let mut k = Sink::new(open("c"), 4).unwrap();
        assert!(k.write(&data).is_err());
        // Short.
        let mut k = Sink::new(open("d"), data.len() as u64).unwrap();
        k.write(&data[..3]).unwrap();
        assert!(k.finish(&sha).unwrap_err().contains("bytes"));
        // One bit off.
        let mut bad = data.clone();
        bad[0] ^= 1;
        let mut k = Sink::new(open("e"), data.len() as u64).unwrap();
        k.write(&bad).unwrap();
        assert!(k.finish(&sha).unwrap_err().contains("SHA-256"));
    }

    #[test]
    fn requests_are_taken_once() {
        assert_eq!(take_request(), None);
        request(false);
        request(true);
        assert_eq!(take_request(), Some(true), "check-first wins");
        assert_eq!(take_request(), None);
        request(false);
        assert_eq!(take_request(), Some(false));
    }
}
