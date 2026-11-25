//! Other launchers and previewers NopBar hands work to instead of competing
//! with them (roadmap item 4; docs/integrations.md has every tool's source).
//!
//! - "Search with" (Settings > General): the top bar's search cell and the
//!   search shortcut open the chosen launcher (Flow Launcher, Everything,
//!   ...) instead of NopBar's panel. Only installed ones are offered; one
//!   that is gone opens NopBar's panel.
//! - Quick Look: Space on a selected folder stack item previews it with
//!   QuickLook or PowerToys Peek (their documented command lines); without
//!   either, a tooltip says once what to install.
//!
//! Only documented, non-keyboard ways are used: a tool's command line or a
//! documented URI. NEVER keys sent to another app (no SendInput, no hotkey
//! replay) and no undocumented named events. A tool with no such way is
//! only opened ("opens the app" in its option's name).
//!
//! Finding them reads the registry (uninstall entries, the packaged app
//! repository), checks a few known paths and the running processes: a few
//! milliseconds, always on a helper thread (`refresh_soon`), the result
//! cached for the UI thread (`cached`). The parsing (`detect`) is pure and
//! tested with fixtures; the launch lines (`search_launch`,
//! `preview_launch`) too.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    PowerToysRun,
    CmdPal,
    Flow,
    Raycast,
    Everything,
    WindowsSearch,
    QuickLook,
    Peek,
}

/// The "Search with" choices other than NopBar's own, in the list's order.
pub const SEARCH_TOOLS: [Tool; 6] = [Tool::PowerToysRun, Tool::CmdPal, Tool::Flow, Tool::Raycast, Tool::Everything, Tool::WindowsSearch];
/// Previewers, best first.
pub const PREVIEWERS: [Tool; 2] = [Tool::QuickLook, Tool::Peek];
/// Config value of NopBar's own search (config::SearchCfg::with).
pub const NOPBAR: &str = "nopbar";

impl Tool {
    /// Its config value.
    pub fn id(self) -> &'static str {
        match self {
            Tool::PowerToysRun => "powertoys_run",
            Tool::CmdPal => "cmdpal",
            Tool::Flow => "flow",
            Tool::Raycast => "raycast",
            Tool::Everything => "everything",
            Tool::WindowsSearch => "windows_search",
            Tool::QuickLook => "quicklook",
            Tool::Peek => "peek",
        }
    }

    pub fn from_id(id: &str) -> Option<Tool> {
        SEARCH_TOOLS.iter().chain(PREVIEWERS.iter()).copied().find(|t| t.id().eq_ignore_ascii_case(id.trim()))
    }

    /// What people know it as (product names stay untranslated).
    pub fn name(self) -> &'static str {
        match self {
            Tool::PowerToysRun => "PowerToys Run",
            Tool::CmdPal => "Command Palette",
            Tool::Flow => "Flow Launcher",
            Tool::Raycast => "Raycast",
            Tool::Everything => "Everything",
            Tool::WindowsSearch => "Windows Search",
            Tool::QuickLook => "QuickLook",
            Tool::Peek => "Peek",
        }
    }

    /// Its option in the "Search with" list (crate::settings::model): a
    /// text key when it says more than the name ("opens the app"), else the
    /// name as is.
    pub fn option(self) -> &'static str {
        match self {
            Tool::PowerToysRun => "search.with.powertoys_run",
            Tool::CmdPal => "search.with.cmdpal",
            Tool::Raycast => "search.with.raycast",
            Tool::WindowsSearch => "search.with.windows",
            Tool::Flow => "raw:Flow Launcher",
            Tool::Everything => "raw:Everything",
            Tool::QuickLook => "raw:QuickLook",
            Tool::Peek => "raw:PowerToys Peek",
        }
    }
}

/// An installed tool and what to start for it: an exe path, or a shell
/// name ("shell:AppsFolder\..." for a packaged app, "search-ms:" for
/// Windows Search).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub tool: Tool,
    pub file: String,
}

/// One uninstall entry (HKLM / HKCU ...\Uninstall\*).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Uninstall {
    pub name: String,
    pub location: String,
    pub icon: String,
}

/// What detection reads from the machine (filled by `snapshot`, or by a
/// test's fixture).
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub uninstall: Vec<Uninstall>,
    /// Packaged apps' full names (the AppModel repository's keys), e.g.
    /// "Microsoft.CommandPalette_0.5.0.0_x64__8wekyb3d8bbwe".
    pub packages: Vec<String>,
    /// Full paths of running processes we care about (PROCESS_NAMES).
    pub processes: Vec<String>,
    /// %ProgramFiles% and %ProgramFiles(x86)% (either may be empty).
    pub program_files: Vec<String>,
    /// %LOCALAPPDATA%.
    pub local_appdata: String,
}

/// Exe names looked for among the running processes (lowercase).
pub const PROCESS_NAMES: [&str; 5] = ["flow.launcher.exe", "everything.exe", "quicklook.exe", "raycast.exe", "powertoys.exe"];

fn join(dir: &str, rel: &str) -> String {
    let d = dir.trim().trim_matches('"').trim_end_matches(['\\', '/']);
    if d.is_empty() {
        return String::new();
    }
    format!("{d}\\{rel}")
}

/// The folder of an uninstall entry's DisplayIcon ("C:\x\a.exe,0" and
/// quoted forms -> "C:\x").
fn icon_dir(icon: &str) -> String {
    let mut p = icon.trim();
    if let Some(rest) = p.strip_prefix('"') {
        p = rest.split('"').next().unwrap_or("");
    } else if let Some(i) = p.rfind(',') {
        if p[i + 1..].trim().trim_start_matches('-').chars().all(|c| c.is_ascii_digit()) {
            p = &p[..i];
        }
    }
    match p.rfind(['\\', '/']) {
        Some(i) => p[..i].to_string(),
        None => String::new(),
    }
}

/// The file name of a path, lowercase.
fn file_name_lc(p: &str) -> String {
    p.rsplit(['\\', '/']).next().unwrap_or(p).to_ascii_lowercase()
}

/// Packaged apps' copies are not started by path (Windows refuses).
fn in_windows_apps(p: &str) -> bool {
    p.to_ascii_lowercase().contains("\\windowsapps\\")
}

/// Is `p` inside folder `dir` (both full paths, any case)?
fn under(p: &str, dir: &str) -> bool {
    let d = dir.trim().trim_end_matches(['\\', '/']);
    // (get: a non-ASCII path may not split at d.len()).
    !d.is_empty() && p.len() > d.len() + 1 && p.get(..d.len()).is_some_and(|h| h.to_lowercase() == d.to_lowercase()) && matches!(p.as_bytes()[d.len()], b'\\' | b'/')
}

/// Installed for the whole machine (Program Files: only administrators
/// write there).
fn per_machine(p: &str, s: &Snapshot) -> bool {
    s.program_files.iter().any(|d| under(p, d))
}

/// Candidates for `exe`, per-machine ones first: uninstall entries named
/// `prefix...` (InstallLocation, then the DisplayIcon's folder), `defaults`,
/// then running copies. `check` says which is really that tool (and gives
/// its full path); the first that passes wins.
fn find_exe(s: &Snapshot, tool: Tool, prefix: &str, exe: &str, defaults: &[String], check: &dyn Fn(Tool, &str) -> Option<String>) -> Option<String> {
    let mut cands: Vec<String> = Vec::new();
    for u in s.uninstall.iter().filter(|u| u.name.trim().to_ascii_lowercase().starts_with(prefix)) {
        cands.push(join(&u.location, exe));
        cands.push(join(&icon_dir(&u.icon), exe));
    }
    cands.extend(defaults.iter().map(|d| join(d, exe)));
    let leaf = file_name_lc(exe);
    cands.extend(s.processes.iter().filter(|p| file_name_lc(p) == leaf).cloned());
    cands.retain(|c| !c.is_empty() && !in_windows_apps(c));
    // A name-only match elsewhere never goes before a per-machine install.
    cands.sort_by_key(|c| !per_machine(c, s));
    cands.into_iter().find_map(|c| check(tool, &c))
}

/// What an exe says about itself: its version resource and, when it has a
/// valid Authenticode signature, the signer's name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileId {
    pub product: String,
    pub company: String,
    /// Signer (certificate subject's display name) of a valid signature.
    pub signer: Option<String>,
}

/// What each tool's exe must say. `signer` set: the vendor signs its
/// builds, so a valid signature by it is required (anywhere on disk).
/// Unset: the exe must also sit in a per-machine folder or the tool's own
/// per-user install folder (`user_dirs`, under %LOCALAPPDATA%).
struct Expect {
    product: &'static str,
    company: &'static str,
    signer: &'static str,
    user_dirs: &'static [&'static str],
}

fn expect(tool: Tool) -> Option<Expect> {
    Some(match tool {
        Tool::PowerToysRun | Tool::Peek => Expect { product: "PowerToys", company: "Microsoft", signer: "Microsoft Corporation", user_dirs: &[] },
        Tool::Everything => Expect { product: "Everything", company: "voidtools", signer: "voidtools", user_dirs: &[] },
        Tool::Flow => Expect { product: "Flow Launcher", company: "", signer: "", user_dirs: &["FlowLauncher"] },
        Tool::Raycast => Expect { product: "Raycast", company: "", signer: "", user_dirs: &["Programs\\Raycast"] },
        Tool::QuickLook => Expect { product: "QuickLook", company: "", signer: "", user_dirs: &["Programs\\QuickLook"] },
        // Not exe files (a package, a URI).
        Tool::CmdPal | Tool::WindowsSearch => return None,
    })
}

fn has(text: &str, want: &str) -> bool {
    want.is_empty() || text.to_lowercase().contains(&want.to_lowercase())
}

/// Pure: is the exe at `path` (full, local, links resolved) with `id`
/// really `tool`? Version names must match; then the vendor's signature,
/// or (unsigned tools) a per-machine or the tool's own per-user folder.
/// Network paths are refused (they could hang, and could be anything).
pub fn trusted(tool: Tool, path: &str, id: &FileId, s: &Snapshot) -> bool {
    let Some(e) = expect(tool) else { return false };
    let b = path.as_bytes();
    let local = b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    if !local || !has(&id.product, e.product) || !has(&id.company, e.company) {
        return false;
    }
    if !e.signer.is_empty() {
        return id.signer.as_deref().is_some_and(|n| n.trim().eq_ignore_ascii_case(e.signer));
    }
    let lad = s.local_appdata.trim();
    per_machine(path, s) || (!lad.is_empty() && e.user_dirs.iter().any(|d| under(path, &join(lad, d))))
}

/// The family name of a package full name ("Name_Version_Arch_Res_Pub" ->
/// "Name_Pub").
pub fn family_of(full: &str) -> Option<String> {
    let parts: Vec<&str> = full.split('_').collect();
    if parts.len() < 5 || parts[0].is_empty() || parts[parts.len() - 1].is_empty() {
        return None;
    }
    Some(format!("{}_{}", parts[0], parts[parts.len() - 1]))
}

/// Pure: every installed tool in `s`. `check(tool, path)` gives the full
/// path when the file is there and really is that tool (real: `verify`;
/// tests: fixtures). Order: SEARCH_TOOLS, then PREVIEWERS.
pub fn detect(s: &Snapshot, check: &dyn Fn(Tool, &str) -> Option<String>) -> Vec<Found> {
    let pf = |rel: &str| s.program_files.iter().filter(|p| !p.trim().is_empty()).map(|p| join(p, rel)).collect::<Vec<_>>();
    let lad = |rel: &str| if s.local_appdata.trim().is_empty() { Vec::new() } else { vec![join(&s.local_appdata, rel)] };
    // PowerToys: machine-wide (Program Files) or per user (two documented
    // places: ...\Local\PowerToys and ...\Local\Programs\PowerToys).
    let mut pt_dirs = pf("PowerToys");
    pt_dirs.extend(lad("PowerToys"));
    pt_dirs.extend(lad("Programs\\PowerToys"));
    let powertoys = find_exe(s, Tool::PowerToysRun, "powertoys", "PowerToys.exe", &pt_dirs, check);
    let mut out = Vec::new();
    for tool in SEARCH_TOOLS.iter().chain(PREVIEWERS.iter()).copied() {
        let file = match tool {
            Tool::PowerToysRun => powertoys.clone(),
            Tool::Peek => powertoys.as_deref().and_then(|p| check(Tool::Peek, &join(&icon_dir(p), "WinUI3Apps\\PowerToys.Peek.UI.exe"))),
            Tool::CmdPal => s
                .packages
                .iter()
                .filter(|p| p.to_ascii_lowercase().starts_with("microsoft.commandpalette_"))
                .find_map(|p| family_of(p))
                // Microsoft's publisher id: a package of that name from
                // anyone else is not it.
                .filter(|fam| fam.to_ascii_lowercase().ends_with("_8wekyb3d8bbwe"))
                .map(|fam| format!("shell:AppsFolder\\{fam}!App")),
            Tool::Flow => {
                // Squirrel install: the stub in %LOCALAPPDATA%\FlowLauncher
                // starts the current app-x.y.z copy.
                find_exe(s, tool, "flow launcher", "Flow.Launcher.exe", &lad("FlowLauncher"), check)
            }
            Tool::Raycast => {
                let mut d = lad("Programs\\Raycast");
                d.extend(pf("Raycast"));
                find_exe(s, tool, "raycast", "Raycast.exe", &d, check)
            }
            Tool::Everything => find_exe(s, tool, "everything", "Everything.exe", &pf("Everything"), check),
            Tool::WindowsSearch => Some("search-ms:".to_string()),
            Tool::QuickLook => {
                let mut d = lad("Programs\\QuickLook");
                d.extend(pf("QuickLook"));
                find_exe(s, tool, "quicklook", "QuickLook.exe", &d, check)
            }
        };
        if let Some(file) = file {
            out.push(Found { tool, file });
        }
    }
    out
}

// ---------------------------------------------------------------- launching

/// What to start: a file (exe, URI or shell name) and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub file: String,
    pub args: String,
}

/// One argument, quoted for CommandLineToArgvW / the C runtime: inside
/// double quotes, a quote becomes \" and backslashes before a quote (or
/// the closing one) are doubled. Control characters (a line break could
/// not be typed in a search box anyway) become spaces.
pub fn quote_arg(a: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0usize;
    for c in a.chars() {
        let c = if c.is_control() { ' ' } else { c };
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

/// Percent-encoding for a URI parameter (RFC 3986 unreserved stay).
pub fn encode_uri(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Pure: how `f` shows its search box, with `query` typed in when the tool
/// documents a way to pass one ("" = none). See docs/integrations.md.
pub fn search_launch(f: &Found, query: &str, user_profile: &str) -> Launch {
    let q = query.trim();
    let args = match f.tool {
        // -search <text> (voidtools command line options); without text the
        // window just opens.
        Tool::Everything if !q.is_empty() => format!("-search {}", quote_arg(q)),
        // A second start shows the running Flow's window; it takes no query.
        _ => String::new(),
    };
    let file = match f.tool {
        // search-ms: (documented): File Explorer's search, in the user's folder.
        Tool::WindowsSearch => {
            let mut uri = String::from("search-ms:");
            if !q.is_empty() {
                uri.push_str(&format!("query={}&", encode_uri(q)));
            }
            if !user_profile.trim().is_empty() {
                uri.push_str(&format!("crumb=location:{}", encode_uri(user_profile.trim())));
            }
            uri
        }
        _ => f.file.clone(),
    };
    Launch { file, args }
}

/// Is `p` a full path to hand a previewer (drive or UNC; never something
/// that could read as an option, like "-x" or "/x")?
pub fn previewable(p: &str) -> bool {
    let b = p.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    let unc = p.starts_with("\\\\") && p.len() > 2;
    (drive || unc) && !p.chars().any(|c| c.is_control() || c == '"')
}

/// Pure: the previewer `f` showing `path` (QuickLook.exe <path>, which
/// closes it again when the same path comes twice; PowerToys.Peek.UI.exe
/// <path>). None for a path that is not a plain full path.
pub fn preview_launch(f: &Found, path: &str) -> Option<Launch> {
    if !previewable(path) || !matches!(f.tool, Tool::QuickLook | Tool::Peek) {
        return None;
    }
    Some(Launch { file: f.file.clone(), args: quote_arg(path) })
}

/// Pure: what the search cell / shortcut does with config value `with`
/// and the tools found: Some(tool) to hand off, None = NopBar's panel.
pub fn search_choice<'a>(with: &str, found: &'a [Found]) -> Option<&'a Found> {
    let tool = Tool::from_id(with).filter(|t| SEARCH_TOOLS.contains(t))?;
    found.iter().find(|f| f.tool == tool)
}

// ---------------------------------------------------------------- the cache

/// The last detection (None until the first one is done).
static CACHE: Mutex<Option<Arc<Vec<Found>>>> = Mutex::new(None);
/// A detection is running.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// The chosen launcher (config search.with), for the dock's UI thread.
static CHOICE: Mutex<String> = Mutex::new(String::new());
/// Told (on the detection thread) each time a detection is done.
static ON_FOUND: Mutex<Option<fn()>> = Mutex::new(None);
/// The chosen launcher failed to start: the next search press (sent right
/// away) opens NopBar's own panel. Kept for a moment only.
static FALL_BACK: Mutex<Option<std::time::Instant>> = Mutex::new(None);
const FALL_BACK_FOR: std::time::Duration = std::time::Duration::from_secs(3);

/// What was found last time (empty before the first detection is done).
/// Never detects: safe on any UI thread.
pub fn cached() -> Arc<Vec<Found>> {
    CACHE.lock().ok().and_then(|c| c.clone()).unwrap_or_default()
}

/// `f` runs (on the detection thread) after every detection: the settings
/// window posts itself a message to rebuild its "Search with" list.
pub fn on_found(f: fn()) {
    if let Ok(mut o) = ON_FOUND.lock() {
        *o = Some(f);
    }
}

/// Detects again on a helper thread (unless one is running already).
pub fn refresh_soon() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("launchers".into()).spawn(|| {
        let found = Arc::new(detect_now());
        if let Ok(mut c) = CACHE.lock() {
            *c = Some(found);
        }
        RUNNING.store(false, Ordering::SeqCst);
        if let Some(f) = ON_FOUND.lock().ok().and_then(|o| *o) {
            f();
        }
    });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Drops `tool` from the cache (it failed to start or is not what it was)
/// and looks again: another copy may be found.
fn invalidate(tool: Tool) {
    if let Ok(mut c) = CACHE.lock() {
        if let Some(list) = c.as_ref() {
            *c = Some(Arc::new(list.iter().filter(|f| f.tool != tool).cloned().collect()));
        }
    }
    refresh_soon();
}

fn detect_now() -> Vec<Found> {
    let snap = snapshot();
    let found = detect(&snap, &|tool, p| verify(tool, p, &snap));
    crate::log!("launchers: found {}", found.iter().map(|f| f.tool.id()).collect::<Vec<_>>().join(", "));
    found
}

/// The full local path of `p` with links resolved, if it is a file.
fn full_path(p: &str) -> Option<String> {
    let c = std::fs::canonicalize(p).ok()?;
    if !c.is_file() {
        return None;
    }
    let s = c.to_string_lossy().to_string();
    // A plain drive path: "\\?\C:\x" -> "C:\x"; "\\?\UNC\..." stays (and
    // is refused by trusted()).
    Some(s.strip_prefix("\\\\?\\").filter(|r| r.as_bytes().get(1) == Some(&b':')).map(str::to_string).unwrap_or(s))
}

/// Is the file at `p` really `tool` (trusted() on what the file says)?
/// Its full path if so. Helper threads only (reads the file, checks a
/// signature).
fn verify(tool: Tool, p: &str, s: &Snapshot) -> Option<String> {
    let full = full_path(p)?;
    // Network paths are refused before the file is read.
    let b = full.as_bytes();
    if !(b.len() > 3 && b[1] == b':' && b[2] == b'\\') {
        return None;
    }
    let id = unsafe { file_id(&full) };
    if trusted(tool, &full, &id, s) {
        Some(full)
    } else {
        crate::log!("launchers: {} not used for {} ({:?})", full, tool.id(), id);
        None
    }
}

/// Checks a cached entry again just before it is started (helper thread):
/// still there and still the tool. Packages and URIs need no check.
fn still_good(f: &Found) -> bool {
    if expect(f.tool).is_none() {
        return true;
    }
    let s = Snapshot { program_files: program_files(), local_appdata: std::env::var("LOCALAPPDATA").unwrap_or_default(), ..Default::default() };
    verify(f.tool, &f.file, &s).is_some_and(|p| p.eq_ignore_ascii_case(&f.file))
}

/// The config's choice (dock UI thread, on load and on every change). A
/// new choice detects again, so a tool installed since startup is seen.
pub fn set_choice(with: &str) {
    let changed = CHOICE.lock().map(|mut c| {
        let changed = *c != with;
        *c = with.to_string();
        changed
    });
    if changed.unwrap_or(true) {
        refresh_soon();
    }
}

/// The search cell or the search shortcut was used: true if the chosen
/// launcher is being started (NopBar's panel is not opened), false to
/// open NopBar's own. Call on the UI thread. The start runs on a helper
/// thread, which first checks the file again; if that or the start fails,
/// the entry leaves the cache, the shared failure notice tells why (a
/// failed start) and NopBar's own search opens instead (a search press is
/// posted to the dock, which then lands here and falls back).
pub fn redirect_search() -> bool {
    if FALL_BACK.lock().ok().and_then(|mut f| f.take()).is_some_and(|t| t.elapsed() < FALL_BACK_FOR) {
        return false;
    }
    let with = CHOICE.lock().map(|c| c.clone()).unwrap_or_default();
    if with.is_empty() || with == NOPBAR {
        return false;
    }
    let found = cached();
    let Some(f) = search_choice(&with, &found).cloned() else {
        crate::log!("launchers: \"{with}\" not found; NopBar search opens");
        refresh_soon();
        return false;
    };
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    let l = search_launch(&f, "", &profile);
    let target = crate::launch::Target::new(&l.file, f.tool.name(), crate::launch::Source::Search).args(&l.args, "");
    let owner = crate::search::hotkey::owner();
    crate::launch::prepare();
    crate::log!("launchers: search with {} ({})", f.tool.id(), l.file);
    let spawned = std::thread::Builder::new().name("launch".into()).spawn(move || {
        let result = if still_good(&f) {
            crate::launch::with_shell_com(|| crate::launch::open_here(&target))
        } else {
            crate::log!("launchers: {} changed since it was found; not started", f.file);
            Err(0)
        };
        if let Err(code) = result {
            invalidate(f.tool);
            if code != 0 {
                crate::launch::report_later(target, code);
            }
            if let Ok(mut fb) = FALL_BACK.lock() {
                *fb = Some(std::time::Instant::now());
            }
            if owner != 0 {
                unsafe {
                    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
                    let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                        Some(HWND(owner as *mut _)),
                        windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY,
                        WPARAM(crate::search::hotkey::ID as usize),
                        LPARAM(0),
                    );
                }
            }
        }
    });
    if spawned.is_err() {
        return false;
    }
    true
}

/// Space on a stack item (`path`): previews it with QuickLook, else Peek.
/// The helper thread checks each again and tries the next when one fails
/// (it leaves the cache); when none starts, the shared failure notice
/// tells why. With no previewer at all nothing happens. Returns true if a
/// previewer is being tried.
pub fn preview(path: &str) -> bool {
    let found = cached();
    let tries: Vec<(Found, Launch)> = PREVIEWERS
        .iter()
        .filter_map(|t| found.iter().find(|f| f.tool == *t))
        .filter_map(|f| preview_launch(f, path).map(|l| (f.clone(), l)))
        .collect();
    if tries.is_empty() {
        refresh_soon();
        return false;
    }
    crate::launch::prepare();
    let spawned = std::thread::Builder::new().name("launch".into()).spawn(move || {
        crate::launch::with_shell_com(|| {
            let mut last: Option<(crate::launch::Target, u32)> = None;
            for (f, l) in tries {
                let target = crate::launch::Target::new(&l.file, f.tool.name(), crate::launch::Source::Stack).args(&l.args, "");
                if !still_good(&f) {
                    invalidate(f.tool);
                    continue;
                }
                crate::log!("launchers: preview with {}", f.tool.id());
                match crate::launch::open_here(&target) {
                    Ok(()) => return,
                    Err(code) => {
                        invalidate(f.tool);
                        last = Some((target, code));
                    }
                }
            }
            if let Some((target, code)) = last {
                crate::launch::report_later(target, code);
            }
        });
    });
    spawned.is_ok()
}

// ---------------------------------------------------------------- reading the machine

/// Reads what `detect` needs. Registry and process reads only (nothing is
/// written, nothing is started).
pub fn snapshot() -> Snapshot {
    Snapshot {
        uninstall: unsafe { reg::uninstall_entries() },
        packages: unsafe { reg::packages() },
        processes: unsafe { running(&PROCESS_NAMES) },
        program_files: program_files(),
        local_appdata: std::env::var("LOCALAPPDATA").unwrap_or_default(),
    }
}

/// %ProgramFiles%, %ProgramW6432%, %ProgramFiles(x86)% (the per-machine
/// folders), without blanks and repeats.
fn program_files() -> Vec<String> {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let mut v = vec![env("ProgramFiles"), env("ProgramW6432"), env("ProgramFiles(x86)")];
    v.retain(|p| !p.is_empty());
    v.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    v
}

/// What the exe at `path` says: ProductName and CompanyName of its version
/// resource (first language listed), and the signer of a valid Authenticode
/// signature (no revocation check, cached URLs only: never the network).
pub(crate) unsafe fn file_id(path: &str) -> FileId {
    FileId { product: version_string(path, "ProductName"), company: version_string(path, "CompanyName"), signer: signer(path) }
}

pub(crate) unsafe fn version_string(path: &str, name: &str) -> String {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    let file = HSTRING::from(path);
    let size = GetFileVersionInfoSizeW(&file, None);
    if size == 0 || size > (16 << 20) {
        return String::new();
    }
    let mut data = vec![0u8; size as usize];
    if GetFileVersionInfoW(&file, None, size, data.as_mut_ptr().cast()).is_err() {
        return String::new();
    }
    // \VarFileInfo\Translation: (language, code page) pairs.
    let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    let mut sub = String::from("\\StringFileInfo\\040904b0\\");
    if VerQueryValueW(data.as_ptr().cast(), &HSTRING::from("\\VarFileInfo\\Translation"), &mut ptr, &mut len).as_bool() && len >= 4 && !ptr.is_null() {
        let pair = std::slice::from_raw_parts(ptr as *const u16, 2);
        sub = format!("\\StringFileInfo\\{:04x}{:04x}\\", pair[0], pair[1]);
    }
    let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    if !VerQueryValueW(data.as_ptr().cast(), &HSTRING::from(format!("{sub}{name}")), &mut ptr, &mut len).as_bool() || ptr.is_null() || len == 0 {
        return String::new();
    }
    let s = std::slice::from_raw_parts(ptr as *const u16, len as usize);
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..n]).trim().to_string()
}

/// The signer's display name, when `path` has a valid signature.
unsafe fn signer(path: &str) -> Option<String> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Security::Cryptography::CertGetNameStringW;
    use windows::Win32::Security::WinTrust::*;
    let file = HSTRING::from(path);
    let mut fi = WINTRUST_FILE_INFO { cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32, pcwszFilePath: windows::core::PCWSTR(file.as_ptr()), ..Default::default() };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };
    data.Anonymous.pFile = &mut fi;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let ok = WinVerifyTrust(HWND(-1isize as *mut _), &mut action, (&mut data as *mut WINTRUST_DATA).cast()) == 0;
    let mut name = None;
    if ok {
        let prov = WTHelperProvDataFromStateData(data.hWVTStateData);
        if !prov.is_null() {
            let sgnr = WTHelperGetProvSignerFromChain(prov, 0, false, 0);
            if !sgnr.is_null() && !(*sgnr).pasCertChain.is_null() && (*sgnr).csCertChain > 0 {
                let cert = (*(*sgnr).pasCertChain).pCert;
                if !cert.is_null() {
                    // CERT_NAME_SIMPLE_DISPLAY_TYPE
                    let mut buf = [0u16; 256];
                    let n = CertGetNameStringW(cert, 4, 0, None, Some(&mut buf)) as usize;
                    if n > 1 {
                        name = Some(String::from_utf16_lossy(&buf[..n - 1]));
                    }
                }
            }
        }
    }
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    let _ = WinVerifyTrust(HWND(-1isize as *mut _), &mut action, (&mut data as *mut WINTRUST_DATA).cast());
    name
}

/// Full paths of running processes named one of `names` (lowercase).
unsafe fn running(names: &[&str]) -> Vec<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    let mut out = Vec::new();
    let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
    let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut ok = Process32FirstW(snap, &mut e).is_ok();
    while ok {
        let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
        let name = String::from_utf16_lossy(&e.szExeFile[..n]).to_ascii_lowercase();
        if names.contains(&name.as_str()) {
            if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID) {
                let mut buf = [0u16; 1024];
                let mut len = buf.len() as u32;
                if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                    out.push(String::from_utf16_lossy(&buf[..len as usize]));
                }
                let _ = CloseHandle(h);
            }
        }
        ok = Process32NextW(snap, &mut e).is_ok();
    }
    let _ = CloseHandle(snap);
    out
}

mod reg {
    use super::Uninstall;
    use windows::core::{HSTRING, PCWSTR, PWSTR};
    use windows::Win32::System::Registry::*;

    const UNINSTALL: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
    const PACKAGES: &str = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages";

    /// Subkey names of `root\path` (read access in `view`).
    unsafe fn subkeys(root: HKEY, path: &str, view: REG_SAM_FLAGS) -> (Option<HKEY>, Vec<String>) {
        let mut key = HKEY::default();
        if RegOpenKeyExW(root, &HSTRING::from(path), None, KEY_READ | view, &mut key).is_err() {
            return (None, Vec::new());
        }
        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            if RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
            i += 1;
            if i > 4096 {
                break;
            }
        }
        (Some(key), out)
    }

    /// A string value of `key\sub` ("" when missing).
    unsafe fn string(key: HKEY, sub: &str, value: PCWSTR) -> String {
        let sub = HSTRING::from(sub);
        let mut size = 0u32;
        if RegGetValueW(key, &sub, value, RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ, None, None, Some(&mut size)).is_err() || size == 0 || size > 65536 {
            return String::new();
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        if RegGetValueW(key, &sub, value, RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)).is_err() {
            return String::new();
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    }

    /// Every uninstall entry with a name: machine-wide (both views) and
    /// this user's.
    pub(super) unsafe fn uninstall_entries() -> Vec<Uninstall> {
        let mut out = Vec::new();
        for (root, view) in [(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY), (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY), (HKEY_CURRENT_USER, REG_SAM_FLAGS(0))] {
            let (key, subs) = subkeys(root, UNINSTALL, view);
            let Some(key) = key else { continue };
            for s in subs {
                let name = string(key, &s, windows::core::w!("DisplayName"));
                if name.is_empty() {
                    continue;
                }
                let location = string(key, &s, windows::core::w!("InstallLocation"));
                let icon = string(key, &s, windows::core::w!("DisplayIcon"));
                out.push(Uninstall { name, location, icon });
            }
            let _ = RegCloseKey(key);
        }
        out
    }

    /// This user's packaged apps (full names).
    pub(super) unsafe fn packages() -> Vec<String> {
        let (key, subs) = subkeys(HKEY_CURRENT_USER, PACKAGES, REG_SAM_FLAGS(0));
        if let Some(key) = key {
            let _ = RegCloseKey(key);
        }
        subs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> Snapshot {
        Snapshot {
            uninstall: vec![
                Uninstall { name: "PowerToys (Preview) x64".into(), location: r"C:\Program Files\PowerToys\".into(), icon: String::new() },
                Uninstall { name: "Everything 1.4.1.1026 (x64)".into(), location: String::new(), icon: r#""C:\Program Files\Everything\Everything.exe",0"#.into() },
                Uninstall { name: "Flow Launcher".into(), location: r"C:\Users\u\AppData\Local\FlowLauncher".into(), icon: r"C:\Users\u\AppData\Local\FlowLauncher\app.ico".into() },
                Uninstall { name: "Some Other App".into(), location: r"C:\Other".into(), icon: String::new() },
            ],
            packages: vec!["Microsoft.WindowsCalculator_11.0.0.0_x64__8wekyb3d8bbwe".into(), "Microsoft.CommandPalette_0.5.0.0_x64__8wekyb3d8bbwe".into()],
            processes: vec![r"D:\Portable\QuickLook\QuickLook.exe".into()],
            program_files: vec![r"C:\Program Files".into(), r"C:\Program Files (x86)".into()],
            local_appdata: r"C:\Users\u\AppData\Local".into(),
        }
    }

    fn id(product: &str, company: &str, signer: Option<&str>) -> FileId {
        FileId { product: product.into(), company: company.into(), signer: signer.map(String::from) }
    }

    /// Files on the fixture disk: (path, what the file says).
    fn files() -> Vec<(&'static str, FileId)> {
        let ms = || id("PowerToys", "Microsoft Corporation", Some("Microsoft Corporation"));
        vec![
            (r"C:\Program Files\PowerToys\PowerToys.exe", ms()),
            (r"C:\Program Files\PowerToys\WinUI3Apps\PowerToys.Peek.UI.exe", ms()),
            (r"C:\Program Files\Everything\Everything.exe", id("Everything", "voidtools", Some("voidtools"))),
            (r"C:\Users\u\AppData\Local\FlowLauncher\Flow.Launcher.exe", id("Flow Launcher", "", None)),
            (r"D:\Portable\QuickLook\QuickLook.exe", id("QuickLook", "", None)),
        ]
    }

    /// The fixture check: the file is there and trusted() takes it.
    fn checker(disk: Vec<(&'static str, FileId)>, s: Snapshot) -> impl Fn(Tool, &str) -> Option<String> {
        move |tool, p| disk.iter().find(|(f, _)| f.eq_ignore_ascii_case(p)).filter(|(f, i)| trusted(tool, f, i, &s)).map(|(f, _)| f.to_string())
    }

    #[test]
    fn detects_from_uninstall_entries_paths_packages_and_processes() {
        let found = detect(&snap(), &checker(files(), snap()));
        let get = |t: Tool| found.iter().find(|f| f.tool == t).map(|f| f.file.as_str());
        assert_eq!(get(Tool::PowerToysRun), Some(r"C:\Program Files\PowerToys\PowerToys.exe"));
        assert_eq!(get(Tool::Peek), Some(r"C:\Program Files\PowerToys\WinUI3Apps\PowerToys.Peek.UI.exe"));
        assert_eq!(get(Tool::CmdPal), Some(r"shell:AppsFolder\Microsoft.CommandPalette_8wekyb3d8bbwe!App"));
        assert_eq!(get(Tool::Flow), Some(r"C:\Users\u\AppData\Local\FlowLauncher\Flow.Launcher.exe"), "its own per-user folder");
        assert_eq!(get(Tool::Everything), Some(r"C:\Program Files\Everything\Everything.exe"));
        assert_eq!(get(Tool::QuickLook), None, "an unsigned copy running from elsewhere is not trusted");
        assert_eq!(get(Tool::Raycast), None, "not there");
        assert_eq!(get(Tool::WindowsSearch), Some("search-ms:"), "always");
        // Order: the search list, then previewers.
        let order: Vec<Tool> = found.iter().map(|f| f.tool).collect();
        assert_eq!(order, [Tool::PowerToysRun, Tool::CmdPal, Tool::Flow, Tool::Everything, Tool::WindowsSearch, Tool::Peek]);
    }

    #[test]
    fn nothing_installed_finds_only_windows_search() {
        let found = detect(&Snapshot::default(), &|_, _| None);
        assert_eq!(found, vec![Found { tool: Tool::WindowsSearch, file: "search-ms:".into() }]);
        // An entry whose files are gone is not installed.
        let found = detect(&snap(), &|_, _| None);
        assert!(found.iter().all(|f| matches!(f.tool, Tool::WindowsSearch | Tool::CmdPal)), "{found:?}");
    }

    #[test]
    fn default_paths_and_packaged_copies() {
        let s = Snapshot { program_files: vec![r"C:\Program Files".into()], local_appdata: r"C:\L".into(), ..Default::default() };
        let disk = vec![
            (r"C:\L\Programs\PowerToys\PowerToys.exe", id("PowerToys", "Microsoft Corporation", Some("Microsoft Corporation"))),
            (r"C:\L\Programs\Raycast\Raycast.exe", id("Raycast", "Raycast Technologies Inc.", None)),
        ];
        let found = detect(&s, &checker(disk, s.clone()));
        assert!(found.iter().any(|f| f.tool == Tool::PowerToysRun && f.file == r"C:\L\Programs\PowerToys\PowerToys.exe"), "per-user PowerToys, signed");
        assert!(found.iter().any(|f| f.tool == Tool::Raycast), "its own per-user folder");
        assert!(!found.iter().any(|f| f.tool == Tool::Peek), "Peek's exe is not there");
        // A Store copy's running exe is not started by path.
        let s = Snapshot { processes: vec![r"C:\Program Files\WindowsApps\QL-Win.QuickLook_1.0\QuickLook.exe".into()], ..Default::default() };
        assert!(!detect(&s, &|_, p| Some(p.to_string())).iter().any(|f| f.tool == Tool::QuickLook));
        // A Command Palette package from another publisher is not it.
        let s = Snapshot { packages: vec!["Microsoft.CommandPalette_1.0.0.0_x64__abcdefghijklm".into()], ..Default::default() };
        assert!(!detect(&s, &|_, _| None).iter().any(|f| f.tool == Tool::CmdPal));
    }

    #[test]
    fn spoofed_entries_do_not_win() {
        // The review's case: an HKCU entry "QuickLook Helper" points at a
        // user folder holding another program named QuickLook.exe (even one
        // whose version says QuickLook). The real one in Program Files is
        // used; alone, the fake is refused.
        let mut s = snap();
        s.uninstall.insert(0, Uninstall { name: "QuickLook Helper".into(), location: r"C:\Users\u\Downloads\Fake".into(), icon: String::new() });
        s.uninstall.push(Uninstall { name: "QuickLook".into(), location: r"C:\Program Files\QuickLook".into(), icon: String::new() });
        let fake = (r"C:\Users\u\Downloads\Fake\QuickLook.exe", id("QuickLook", "", None));
        let real = (r"C:\Program Files\QuickLook\QuickLook.exe", id("QuickLook", "", None));
        let found = detect(&s, &checker(vec![fake.clone(), real.clone()], s.clone()));
        assert_eq!(found.iter().find(|f| f.tool == Tool::QuickLook).map(|f| f.file.as_str()), Some(real.0));
        let found = detect(&s, &checker(vec![fake], s.clone()));
        assert!(!found.iter().any(|f| f.tool == Tool::QuickLook));
        // Per-machine copies go first even when listed last.
        let mut cands_s = snap();
        // (An uninstall entry is tried before the default paths.)
        cands_s.uninstall = vec![Uninstall { name: "Everything".into(), location: r"C:\Users\u\Desktop".into(), icon: String::new() }];
        let everywhere = |_: Tool, p: &str| Some(p.to_string());
        let found = detect(&cands_s, &everywhere);
        assert_eq!(found.iter().find(|f| f.tool == Tool::Everything).map(|f| f.file.as_str()), Some(r"C:\Program Files\Everything\Everything.exe"));
    }

    #[test]
    fn identity_rules() {
        let s = snap();
        let pt = r"C:\Program Files\PowerToys\PowerToys.exe";
        assert!(trusted(Tool::PowerToysRun, pt, &id("PowerToys", "Microsoft Corporation", Some("Microsoft Corporation")), &s));
        assert!(!trusted(Tool::PowerToysRun, pt, &id("PowerToys", "Microsoft Corporation", None), &s), "Microsoft signs it: unsigned is not it");
        assert!(!trusted(Tool::PowerToysRun, pt, &id("PowerToys", "Microsoft Corporation", Some("Someone Else")), &s), "another signer");
        assert!(!trusted(Tool::PowerToysRun, pt, &id("Totally Not", "Microsoft Corporation", Some("Microsoft Corporation")), &s), "another product");
        let ev = r"D:\Tools\Everything.exe";
        assert!(trusted(Tool::Everything, ev, &id("Everything", "voidtools", Some("voidtools")), &s), "signed: anywhere local");
        assert!(!trusted(Tool::Everything, ev, &id("Everything", "voidtools", None), &s));
        // Unsigned tools: per-machine or their own per-user folder only.
        let ql = id("QuickLook", "", None);
        assert!(trusted(Tool::QuickLook, r"C:\Program Files\QuickLook\QuickLook.exe", &ql, &s));
        assert!(trusted(Tool::QuickLook, r"C:\Users\u\AppData\Local\Programs\QuickLook\QuickLook.exe", &ql, &s));
        assert!(!trusted(Tool::QuickLook, r"C:\Users\u\AppData\Local\Programs\QuickLookFake\QuickLook.exe", &ql, &s), "a folder that only starts the same");
        assert!(!trusted(Tool::QuickLook, r"C:\Users\u\Downloads\QuickLook.exe", &ql, &s));
        assert!(!trusted(Tool::Flow, r"C:\Users\u\AppData\Local\FlowLauncher\Flow.Launcher.exe", &id("Other", "", None), &s), "the version must say Flow Launcher");
        // Network paths are refused, signed or not.
        assert!(!trusted(Tool::Everything, r"\\nas\share\Everything.exe", &id("Everything", "voidtools", Some("voidtools")), &s));
        // Paths with non-ASCII names compare without panicking.
        assert!(!trusted(Tool::QuickLook, r"C:\Users\홍길동\QuickLook.exe", &ql, &s));
        assert!(under(r"C:\Program Files\X\a.exe", r"C:\program files\"));
        assert!(!under(r"C:\Program Files", r"C:\Program Files"));
    }

    #[test]
    fn icon_paths_and_package_families() {
        assert_eq!(icon_dir(r#""C:\a b\x.exe",0"#), r"C:\a b");
        assert_eq!(icon_dir(r"C:\a\x.exe,-101"), r"C:\a");
        assert_eq!(icon_dir(r"C:\a\x.ico"), r"C:\a");
        assert_eq!(icon_dir("x.ico"), "");
        assert_eq!(family_of("Microsoft.CommandPalette_0.5.0.0_x64__8wekyb3d8bbwe").as_deref(), Some("Microsoft.CommandPalette_8wekyb3d8bbwe"));
        assert_eq!(family_of("bad"), None);
    }

    #[test]
    fn arguments_are_quoted_not_injected() {
        assert_eq!(quote_arg(r"C:\a b\c.txt"), r#""C:\a b\c.txt""#);
        assert_eq!(quote_arg(r"C:\dir\"), r#""C:\dir\\""#, "a trailing backslash does not escape the closing quote");
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_arg("x\" -toggle-window \"y"), "\"x\\\" -toggle-window \\\"y\"", "one argument, still");
        assert_eq!(quote_arg("line\nbreak"), "\"line break\"");
        assert_eq!(quote_arg(""), "\"\"");
    }

    #[test]
    fn search_lines() {
        let ev = Found { tool: Tool::Everything, file: r"C:\Program Files\Everything\Everything.exe".into() };
        assert_eq!(search_launch(&ev, "", "").args, "");
        assert_eq!(search_launch(&ev, " report \"q3\" ", "").args, r#"-search "report \"q3\"""#);
        let flow = Found { tool: Tool::Flow, file: r"C:\F\Flow.Launcher.exe".into() };
        assert_eq!(search_launch(&flow, "ignored", ""), Launch { file: flow.file.clone(), args: String::new() }, "Flow takes no query");
        let ws = Found { tool: Tool::WindowsSearch, file: "search-ms:".into() };
        assert_eq!(search_launch(&ws, "", r"C:\Users\Kim Lee").file, "search-ms:crumb=location:C%3A%5CUsers%5CKim%20Lee");
        assert_eq!(search_launch(&ws, "a&b=c", "").file, "search-ms:query=a%26b%3Dc&");
        let cmd = Found { tool: Tool::CmdPal, file: r"shell:AppsFolder\X_y!App".into() };
        assert_eq!(search_launch(&cmd, "q", "").args, "", "opened only");
    }

    #[test]
    fn preview_lines() {
        let ql = Found { tool: Tool::QuickLook, file: r"C:\QL\QuickLook.exe".into() };
        assert_eq!(preview_launch(&ql, r"C:\Users\u\Downloads\a b.pdf"), Some(Launch { file: ql.file.clone(), args: r#""C:\Users\u\Downloads\a b.pdf""#.into() }));
        assert_eq!(preview_launch(&ql, r"\\nas\share\x.png").map(|l| l.args), Some(r#""\\nas\share\x.png""#.into()));
        assert_eq!(preview_launch(&ql, r"C:\dir\").map(|l| l.args), Some(r#""C:\dir\\""#.into()));
        // Not a plain full path: nothing is started.
        for bad in ["-x", "/x", "relative.txt", "", "C:\\a\"b", "C:\\a\nb", "shell:AppsFolder\\X"] {
            assert_eq!(preview_launch(&ql, bad), None, "{bad:?}");
        }
        let ev = Found { tool: Tool::Everything, file: "e.exe".into() };
        assert_eq!(preview_launch(&ev, r"C:\a.txt"), None, "not a previewer");
    }

    #[test]
    fn choices_fall_back_to_nopbar() {
        let found = vec![
            Found { tool: Tool::Flow, file: "f.exe".into() },
            Found { tool: Tool::Peek, file: "p.exe".into() },
            Found { tool: Tool::QuickLook, file: "q.exe".into() },
        ];
        assert_eq!(search_choice("flow", &found).map(|f| f.tool), Some(Tool::Flow));
        assert_eq!(search_choice("everything", &found), None, "not installed: NopBar's panel");
        assert_eq!(search_choice(NOPBAR, &found), None);
        assert_eq!(search_choice("quicklook", &found), None, "a previewer is not a launcher");
        assert_eq!(search_choice("bogus", &found), None);
    }

    #[test]
    fn ids_round_trip_and_texts_exist() {
        for t in SEARCH_TOOLS.iter().chain(PREVIEWERS.iter()) {
            assert_eq!(Tool::from_id(t.id()), Some(*t));
            let o = t.option();
            assert!(o.starts_with("raw:") || crate::i18n::get(o).is_some(), "{o}");
        }
        assert_eq!(Tool::from_id(NOPBAR), None);
    }
}
