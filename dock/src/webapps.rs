//! Browsers' installed web apps (PWAs and Chrome apps: Chrome Remote
//! Desktop, YouTube Music...). They all run the browser's exe (chrome.exe,
//! msedge.exe, brave.exe), so the exe's icon and name are the browser's:
//! every one showed as Chrome. Each window carries the app's own
//! AppUserModelID ("Chrome._crx_<app>", measured on this PC), and the
//! Start-menu shortcut the browser made for the app ("Chrome 앱\Chrome 원격
//! 데스크톱.lnk") carries the same one, with the app's icon and name, and
//! launches the app: what the taskbar shows. Chrome set no Relaunch*
//! properties on its app windows (only on browser windows, for the
//! profile); a browser that does gives the icon, name and command (the
//! browser and the app's arguments) without a shortcut.
//!
//! A web app is never its browser's exe: without a shortcut its item is
//! `shell:AppsFolder\<AUMID>` (the shell opens that once a shortcut with
//! the AUMID exists), started meanwhile as the window's RelaunchCommand
//! says (`launch_for`), the browser and its arguments kept apart. A pin
//! keeps that (`WebPin`, config.rs) for the next run.
//!
//! Read on the window tracker's thread (`warm`), cached by AUMID; the UI
//! thread only reads the cache (`cached`, `read`). A pinned web app not
//! running now is read there too (`watch_pins`, `warm_pins`): its shortcut
//! renamed, moved or deleted meanwhile reaches its pin without a window.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

/// An installed web app's AUMID: "<browser id>._crx_<app>" (Chromium's
/// own scheme; Edge "MSEdge._crx_...", Brave "Brave._crx_..."). Same rule
/// as audio::session's WEB_APP.
pub fn is_web_app(aumid: &str) -> bool {
    const WEB_APP: &[u8] = b"._crx_";
    aumid.as_bytes().windows(WEB_APP.len()).any(|w| w.eq_ignore_ascii_case(WEB_APP))
}

/// The shell's path of an app by its AUMID.
const APPS: &str = "shell:AppsFolder\\";

/// Pure: the AUMID of a `shell:AppsFolder\<AUMID>` path (any case).
pub fn apps_folder_aumid(path: &str) -> Option<&str> {
    path.get(..APPS.len()).filter(|p| p.eq_ignore_ascii_case(APPS)).map(|_| &path[APPS.len()..]).filter(|a| !a.is_empty())
}

/// Pure: the item path of web app `aumid` when it has no shortcut.
pub fn apps_folder_path(aumid: &str) -> String {
    format!("{APPS}{aumid}")
}

/// How a web app starts without its shortcut: the browser and its
/// arguments (the app, the profile), from the window's RelaunchCommand.
/// Opened as a program and its parameters, never one command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub exe: String,
    pub args: String,
}

/// What a web app is shown and launched as.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebApp {
    /// The Start-menu shortcut with the app's AUMID: launches the app, its
    /// icon is the app's, a pin of it is the app's.
    pub link: Option<String>,
    /// Every Start-menu shortcut with the app's AUMID, in search order
    /// (`link` is the first): a pin of any of them is still the app's.
    pub links: Vec<String>,
    /// The icon key of the window's RelaunchIconResource (an .ico or a
    /// file's first icon: the file; another icon of a file: "file,index",
    /// icons::icon_location), when the window set one.
    pub icon: Option<String>,
    /// The app's name: the window's RelaunchDisplayNameResource, else the
    /// shortcut's name.
    pub name: Option<String>,
    /// The window's RelaunchCommand, when it set one.
    pub launch: Option<Launch>,
    /// What its icon is drawn from as it was when read (its shortcut's
    /// and the window's icon file's stamps, as one number: `icon_ver`):
    /// another when either file changed in place (items.rs draws it again).
    pub ver: u64,
}

/// A pinned web app's details kept with its pin (config.rs PinEntry): who
/// it is, how it starts and looks without its shortcut, and the name the
/// dock gave it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct WebPin {
    pub aumid: String,
    /// The browser the window's RelaunchCommand starts...
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub exe: String,
    /// ...and its arguments.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    /// The window's own icon (WebApp::icon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The app's name as the dock last gave it: a pin still called that is
    /// renamed with the app; any other name is the user's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl WebPin {
    /// What a pin of web app `aumid` keeps from `info` (None: not a web app).
    pub fn of(aumid: &str, info: Option<&WebApp>) -> Option<WebPin> {
        if !is_web_app(aumid) {
            return None;
        }
        let launch = info.and_then(|i| i.launch.clone()).unwrap_or_default();
        Some(WebPin { aumid: aumid.to_string(), exe: launch.exe, args: launch.args, icon: info.and_then(|i| i.icon.clone()), name: info.and_then(|i| i.name.clone()) })
    }

    fn launch(&self) -> Option<Launch> {
        (!self.exe.is_empty()).then(|| Launch { exe: self.exe.clone(), args: self.args.clone() })
    }
}

/// Pure: an icon resource string ("path,index", "@path,-id", quoted, with
/// %VARS%) as the file and index. None if it names no file.
pub fn parse_icon_resource(s: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<(String, i32)> {
    let s = s.trim();
    let s = s.strip_prefix('@').unwrap_or(s).trim();
    let (path, index) = match s.rsplit_once(',') {
        Some((p, i)) => match i.trim().parse::<i32>() {
            Ok(n) => (p, n),
            Err(_) => (s, 0),
        },
        None => (s, 0),
    };
    let path = expand_env(path.trim().trim_matches('"').trim(), env);
    (!path.is_empty()).then_some((path, index))
}

/// Pure: `%NAME%` replaced by `env(NAME)`; unknown names stay as written.
pub fn expand_env(s: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        match after.find('%') {
            Some(end) if end > 0 => match env(&after[..end]) {
                Some(v) => {
                    out.push_str(&v);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Pure: the dock's icon key of an icon resource: an .ico (one icon,
/// whatever the index) or another file's first icon is the file itself
/// (the shell reads it); any other icon of a file is "file,index" (an
/// index, or -id for a resource id: icons.rs reads that one).
pub fn icon_file(res: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let (path, index) = parse_icon_resource(res, env)?;
    let ico = path.to_ascii_lowercase().ends_with(".ico");
    Some(if ico || index == 0 { path } else { format!("{path},{index}") })
}

/// Pure: the browser and arguments of a RelaunchCommand: a quoted or
/// unquoted (up to the first word ending in ".exe") absolute local path of
/// an exe, then the rest as its arguments, as written. None for anything
/// else (a relative or network path, a non-exe, an empty command).
pub fn parse_command(cmd: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<Launch> {
    let s = cmd.trim();
    let (exe, rest) = match s.strip_prefix('"') {
        Some(q) => {
            let end = q.find('"')?;
            (&q[..end], &q[end + 1..])
        }
        None => {
            // (ASCII lowercase keeps every byte where it was.)
            let lower = s.to_ascii_lowercase();
            let ends_word = |e: usize| s[e..].chars().next().is_none_or(char::is_whitespace);
            let end = lower.match_indices(".exe").map(|(i, _)| i + 4).find(|&e| ends_word(e)).unwrap_or_else(|| s.find(char::is_whitespace).unwrap_or(s.len()));
            (&s[..end], &s[end..])
        }
    };
    let exe = expand_env(exe.trim(), env);
    let b = exe.as_bytes();
    let local = b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    let ok = local && exe.to_ascii_lowercase().ends_with(".exe") && !exe.contains(['"', '\0']);
    ok.then(|| Launch { exe, args: rest.trim().to_string() })
}

/// Pure: what a web app is shown as, from its shortcuts (if found: the
/// first is its link) and the window's relaunch icon, name and command (if
/// set). Name: the window's, else the shortcut's file name.
pub fn combine(links: Vec<String>, icon: Option<String>, name: Option<String>, launch: Option<Launch>) -> WebApp {
    let link = links.first().cloned();
    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).or_else(|| link.as_deref().and_then(stem));
    WebApp { link, links, icon, name, launch, ver: 0 }
}

/// A shortcut's name (its file name without ".lnk").
fn stem(path: &str) -> Option<String> {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty())
}

/// Pure: the icon key of a web app's item whose path is `path`: None = the
/// path's own icon (a shortcut draws the app's icon). An AppsFolder path
/// draws its shortcut when one was found since, else the window's own icon,
/// else its browser's; a browser exe (an old pin) the window's own icon.
pub fn icon_key(path: &str, info: &WebApp) -> Option<String> {
    if apps_folder_aumid(path).is_some_and(is_web_app) {
        return info.link.clone().or_else(|| info.icon.clone()).or_else(|| info.launch.as_ref().map(|l| l.exe.clone()));
    }
    if info.link.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(path)) || path.to_ascii_lowercase().ends_with(".lnk") {
        return None;
    }
    info.icon.clone()
}

/// How an item of a web app follows its details read now (`refresh`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Refresh {
    pub name: Option<String>,
    pub path: Option<String>,
}

/// Pure: how the item of web app `aumid` (shown as `name`, opening `path`;
/// `pinned`, with `auto` the name the dock gave the pin) follows `info`,
/// its details read now. Its name becomes the app's, unless a pin was given
/// another one. Its path: a running item's is the app's shortcut as found
/// now, else (none, or its shortcut gone or another app's now) its
/// AppsFolder path, never the browser's exe. A pin of a shortcut in a
/// Start menu read (`indexed`) that is not one of the app's any more takes
/// the app's shortcut (or its AppsFolder path); a pin elsewhere (the
/// taskbar's, the desktop's: never read) and a pin of the AppsFolder path
/// (opened through the shortcut found: `launch_for`) stay.
pub fn refresh(aumid: &str, name: &str, path: &str, pinned: bool, auto: Option<&str>, info: &WebApp, indexed: &dyn Fn(&str) -> bool) -> Refresh {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let wanted = info.link.clone().unwrap_or_else(|| apps_folder_path(aumid));
    let lnk = path.to_ascii_lowercase().ends_with(".lnk");
    let current = info.links.iter().any(|l| same(l, path));
    let path = if pinned {
        (lnk && !current && indexed(path)).then_some(wanted)
    } else {
        (!same(path, &wanted)).then_some(wanted)
    };
    let auto_name = auto.map(str::to_string);
    let named_by_dock = !pinned || auto_name.as_deref().is_some_and(|a| a == name);
    let name = info.name.clone().filter(|n| named_by_dock && n != name);
    Refresh { name, path }
}

/// How long an answer is kept: one with a shortcut for a while, one
/// without for less (the browser may make the shortcut a moment after the
/// app's first window). Reading again is cheap (the Start menus are only
/// checked: `shortcuts`); a change found then reaches the dock's items
/// (items.rs, `refresh`).
const KEEP_FOUND: Duration = Duration::from_secs(2 * 60);
const KEEP_MISSING: Duration = Duration::from_secs(30);

/// Pure: is an answer read `age` ago still good?
pub fn fresh(info: &WebApp, age: Duration) -> bool {
    age < if info.link.is_some() { KEEP_FOUND } else { KEEP_MISSING }
}

/// A pinned app not running is read again this often (its shortcuts only:
/// the Start menus checked, cheap).
const KEEP_PIN: Duration = Duration::from_secs(10);

/// Pure: is there no need to read again an answer read `age` ago (None:
/// never read, only a pin's), from a window (`windowed`), when asked
/// with a window (`window`) or for a pin without one? A window is read
/// unless one was, freshly; a pin's app only every KEEP_PIN.
fn keeps(info: &WebApp, age: Option<Duration>, windowed: bool, window: bool) -> bool {
    let Some(age) = age else { return false };
    let fresh_window = windowed && fresh(info, age);
    if window {
        fresh_window
    } else {
        fresh_window || age < KEEP_PIN
    }
}

/// Most apps kept (past it the cache starts over).
const MAX_APPS: usize = 128;

struct Entry {
    info: WebApp,
    /// When it was read; None: only what a pin kept (`seed`), read again
    /// for the first window.
    at: Option<Instant>,
    /// Read with a window (its Relaunch* properties), not only for a pin.
    windowed: bool,
}

fn cache() -> &'static Mutex<HashMap<String, Entry>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, Entry>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The answer for web app `aumid` if one was read or a pin kept one (no
/// disk access: UI thread). None for other apps.
pub fn cached(aumid: &str) -> Option<WebApp> {
    if !is_web_app(aumid) {
        return None;
    }
    cache().lock().ok()?.get(&aumid.to_lowercase()).map(|e| e.info.clone())
}

/// `cached`, only as read from the app's window (not what a pin kept):
/// what the dock's items follow (`refresh`).
pub fn read(aumid: &str) -> Option<WebApp> {
    if !is_web_app(aumid) {
        return None;
    }
    cache().lock().ok()?.get(&aumid.to_lowercase()).filter(|e| e.at.is_some()).map(|e| e.info.clone())
}

/// What a pin kept, until the app's window is read (a pin of an app not
/// running now: how it starts and looks). Never over a reading.
pub fn seed(p: &WebPin) {
    if !is_web_app(&p.aumid) {
        return;
    }
    let info = WebApp { icon: p.icon.clone(), name: p.name.clone(), launch: p.launch(), ..Default::default() };
    if let Ok(mut c) = cache().lock() {
        c.entry(p.aumid.to_lowercase()).or_insert(Entry { info, at: None, windowed: false });
    }
}

/// What to open for `file` (launch.rs): a web app's AppsFolder path opens
/// its shortcut as found, else its browser with its arguments (the
/// window's RelaunchCommand, or its pin's). None: `file` itself.
pub fn launch_for(file: &str) -> Option<Launch> {
    let info = cached(apps_folder_aumid(file)?)?;
    match info.link {
        Some(l) => Some(Launch { exe: l, args: String::new() }),
        None => info.launch,
    }
}

/// Reads what web app `aumid` (window `hwnd`, 0 = none at hand: a pin's
/// app not running) is shown as, unless a fresh answer is cached. Off the
/// UI thread, with COM.
pub fn warm(aumid: &str, hwnd: isize) {
    if !is_web_app(aumid) {
        return;
    }
    let key = aumid.to_lowercase();
    let before = match cache().lock() {
        Ok(c) => match c.get(&key) {
            Some(e) if keeps(&e.info, e.at.map(|at| at.elapsed()), e.windowed, hwnd != 0) => return,
            e => e.map(|e| e.info.clone()),
        },
        Err(_) => None,
    };
    let r = if hwnd != 0 { unsafe { relaunch_of(hwnd) } } else { Relaunch::default() };
    let env = |v: &str| std::env::var(v).ok();
    let icon = r.icon.and_then(|res| icon_file(&res, &env)).filter(|k| Path::new(crate::icons::icon_location(k).map_or(k.as_str(), |l| l.0)).is_file());
    let launch = r.command.and_then(|c| parse_command(&c, &env)).filter(|l| Path::new(&l.exe).is_file());
    let mut info = combine(shortcuts(aumid), icon, r.name, launch);
    // A later window that says less (or a reading without one) keeps what
    // an earlier one (or the pin) said.
    if let Some(b) = before {
        info.icon = info.icon.or(b.icon);
        info.launch = info.launch.or(b.launch);
        // Without a window (a pin's app not running) the name stays what
        // it was (a window's own name is not the shortcut's): the
        // shortcut's only if none was known.
        info.name = if hwnd == 0 { b.name.or(info.name) } else { info.name.or(b.name) };
    }
    info.ver = icon_ver(&info);
    if let Ok(mut c) = cache().lock() {
        if c.len() >= MAX_APPS {
            c.clear();
        }
        c.insert(key, Entry { info, at: Some(Instant::now()), windowed: hwnd != 0 });
    }
}

/// The stamps of the files a web app's icon is drawn from (its shortcut,
/// the window's icon file), as one number (WebApp::ver).
fn icon_ver(info: &WebApp) -> u64 {
    let icon = info.icon.as_deref().map(|k| crate::icons::icon_location(k).map_or(k, |l| l.0));
    let mut text = String::new();
    for f in [info.link.as_deref(), icon].into_iter().flatten() {
        let (t, len) = stamp(Path::new(f));
        text.push_str(&format!("{f}|{t:?}|{len};"));
    }
    crate::icons::fnv64(text.to_lowercase().as_bytes())
}

/// The pinned web apps of each dock (by its window), read without a
/// window while not running (`warm_pins`).
fn pins() -> &'static Mutex<HashMap<isize, Vec<String>>> {
    static PINS: std::sync::OnceLock<Mutex<HashMap<isize, Vec<String>>>> = std::sync::OnceLock::new();
    PINS.get_or_init(Default::default)
}

/// Dock `dock`'s pinned apps' AUMIDs now (UI thread, no disk access): the
/// web apps among them are read while not running (`warm_pins`).
pub fn watch_pins(dock: isize, aumids: Vec<String>) {
    let web: Vec<String> = aumids.into_iter().filter(|a| is_web_app(a)).collect();
    if let Ok(mut p) = pins().lock() {
        if web.is_empty() {
            p.remove(&dock);
        } else {
            p.insert(dock, web);
        }
    }
}

/// Pure: the pinned web apps to read without a window: each once, those
/// with a window (`running`, read with it) left out.
fn closed_pins(pinned: &HashMap<isize, Vec<String>>, running: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in pinned.values().flatten() {
        if !running.iter().any(|r| r.eq_ignore_ascii_case(a)) && !out.iter().any(|o| o.eq_ignore_ascii_case(a)) {
            out.push(a.clone());
        }
    }
    out
}

/// Reads the pinned web apps not running now (`running`: the AUMIDs of the
/// windows read): their shortcuts as found now, at most every KEEP_PIN.
/// The window tracker's thread, with COM.
pub fn warm_pins(running: &[&str]) {
    let todo = match pins().lock() {
        Ok(p) => closed_pins(&p, running),
        Err(_) => return,
    };
    for a in todo {
        warm(&a, 0);
    }
}

/// What to open for a pin of web app `aumid` at `path` when clicked: None
/// (its path, as usual) unless that is a shortcut gone since the dock last
/// read it (moved, deleted while the app was closed): then the app's
/// shortcut found since, else how it was started (pin_launch_with).
/// One file check, on a click.
pub fn pin_launch(path: &str, aumid: &str) -> Option<Launch> {
    if !is_web_app(aumid) || !path.to_ascii_lowercase().ends_with(".lnk") {
        return None;
    }
    pin_launch_with(path, cached(aumid).as_ref(), &|p| Path::new(p).is_file())
}

/// Pure: pin_launch with what is known of the app (`info`) and the file
/// check (`exists`). None: open the path itself (it is there, or nothing
/// else is known: its failure is told, with Unpin: launch.rs).
pub fn pin_launch_with(path: &str, info: Option<&WebApp>, exists: &dyn Fn(&str) -> bool) -> Option<Launch> {
    if exists(path) {
        return None;
    }
    let info = info?;
    match info.links.iter().find(|l| !l.eq_ignore_ascii_case(path) && exists(l)) {
        Some(l) => Some(Launch { exe: l.clone(), args: String::new() }),
        None => info.launch.clone(),
    }
}

/// `warm`, then the answer (may read the disk: not on the UI thread).
pub fn resolve(aumid: &str, hwnd: isize) -> Option<WebApp> {
    warm(aumid, hwnd);
    cached(aumid)
}

// ---- the Start menus' web-app shortcuts ---------------------------------------
//
// Every shortcut in the two Start menus, with its file's stamp and (web
// apps only) its AUMID. Reading a shortcut's AUMID loads it through COM;
// listing the folders does not. So the index is checked, not read again:
// its folders' times (a shortcut made, removed or renamed changes its
// folder's) and the stamp of each shortcut it answers with (one changed in
// place is read again, and leaves the AUMID it named). Read again it is
// only for what changed (`scan`), and at most MAX_AGE after the last time.
// A shortcut that could not be read (the browser still writing it, COM
// busy) is not "no web app": it is read again by the next scan, which
// comes RETRY after one failed (its stamp may never change again).

/// A file's time written and size: changes when the file does.
type Stamp = (Option<SystemTime>, u64);

fn stamp_of(m: &std::fs::Metadata) -> Stamp {
    (m.modified().ok(), m.len())
}

fn stamp(path: &Path) -> Stamp {
    std::fs::metadata(path).map(|m| stamp_of(&m)).unwrap_or((None, 0))
}

fn dir_time(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

/// One shortcut as last read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkRec {
    path: String,
    stamp: Stamp,
    /// Its AUMID if that is a web app's.
    aumid: Option<String>,
    /// It could not be read (`aumid` says nothing): read again.
    failed: bool,
}

/// What reading a shortcut found: its AUMID (None: it has none), or Err:
/// it could not be read.
type LinkRead = Result<Option<String>, ()>;

/// Pure: a reading as kept (a web app's AUMID, or none) and whether it
/// failed.
fn kept(r: LinkRead) -> (Option<String>, bool) {
    match r {
        Ok(a) => (a.filter(|a| is_web_app(a)), false),
        Err(()) => (None, true),
    }
}

#[derive(Debug)]
struct Index {
    at: Instant,
    /// Every folder read, and its time then.
    dirs: Vec<(PathBuf, Option<SystemTime>)>,
    links: Vec<LinkRec>,
}

impl Index {
    /// A folder read has changed since (or appeared, or gone).
    fn moved(&self) -> bool {
        self.dirs.iter().any(|(d, t)| dir_time(d) != *t)
    }

    /// A shortcut could not be read, and the last scan was RETRY ago.
    fn retry_due(&self) -> bool {
        self.at.elapsed() >= RETRY && self.links.iter().any(|r| r.failed)
    }

    /// The shortcuts with `aumid`, in search order, each checked first: one
    /// gone or changed is read again (`read`), and one that names another
    /// app now is not `aumid`'s any more.
    fn links_of(&mut self, aumid: &str, read: &mut dyn FnMut(&str) -> LinkRead) -> Vec<String> {
        let mut out = Vec::new();
        for rec in self.links.iter_mut().filter(|r| r.aumid.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(aumid))) {
            let now = stamp(Path::new(&rec.path));
            if now != rec.stamp {
                rec.stamp = now;
                (rec.aumid, rec.failed) = if now.0.is_some() { kept(read(&rec.path)) } else { (None, false) };
            }
            if rec.aumid.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(aumid)) {
                out.push(rec.path.clone());
            }
        }
        out
    }
}

static INDEX: Mutex<Option<Index>> = Mutex::new(None);

/// The index is read again (only what changed) at least this often.
const MAX_AGE: Duration = Duration::from_secs(30 * 60);

/// A shortcut that could not be read is read again this long after.
const RETRY: Duration = Duration::from_secs(30);

/// Folders this deep in a Start menu are read (Chrome's are one level).
const MAX_DEPTH: usize = 4;

/// The two Start menus' Programs folders: the user's, then everyone's.
fn start_menus() -> Vec<PathBuf> {
    ["APPDATA", "ProgramData"].iter().filter_map(std::env::var_os).map(|b| Path::new(&b).join(r"Microsoft\Windows\Start Menu\Programs")).collect()
}

/// Pure: is `path` in one of `roots` (the Start menus the index reads)?
pub fn under(path: &str, roots: &[PathBuf]) -> bool {
    let p = path.to_lowercase();
    roots.iter().any(|r| {
        let r = r.to_string_lossy().to_lowercase();
        let r = r.trim_end_matches('\\');
        p.len() > r.len() + 1 && p.starts_with(r) && p.as_bytes()[r.len()] == b'\\'
    })
}

/// Is `path` a shortcut in a Start menu the index reads, and read fine
/// there (refresh: one that could not be read may still be the app's)?
/// None: the index is being read now (the UI thread does not wait): ask
/// again later.
pub fn indexed(path: &str) -> Option<bool> {
    if !under(path, &start_menus()) {
        return Some(false);
    }
    match INDEX.try_lock() {
        Ok(g) => Some(g.as_ref().is_some_and(|ix| read_fine(ix, path))),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner().as_ref().is_some_and(|ix| read_fine(ix, path))),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// Pure: `ix` did not fail to read the shortcut at `path` (gone from it,
/// or read: its AUMID known).
fn read_fine(ix: &Index, path: &str) -> bool {
    !ix.links.iter().any(|r| r.failed && r.path.eq_ignore_ascii_case(path))
}

/// The Start-menu shortcuts with `aumid`, the index checked (and read
/// again where it changed) first. Off the UI thread, with COM.
fn shortcuts(aumid: &str) -> Vec<String> {
    let mut g = INDEX.lock().unwrap_or_else(|e| e.into_inner());
    let read = &mut |p: &str| unsafe { link_aumid(p) };
    if g.as_ref().is_none_or(|ix| ix.at.elapsed() >= MAX_AGE || ix.moved() || ix.retry_due()) {
        let next = scan(&start_menus(), g.as_ref(), read);
        *g = Some(next);
    }
    g.as_mut().map(|ix| ix.links_of(aumid, read)).unwrap_or_default()
}

/// Reads the shortcuts under `roots`: one whose path and stamp are as in
/// `prev` keeps its AUMID from there (unless it could not be read then),
/// the others are read (`read`).
fn scan(roots: &[PathBuf], prev: Option<&Index>, read: &mut dyn FnMut(&str) -> LinkRead) -> Index {
    let known: HashMap<&str, &LinkRec> = prev.map(|ix| ix.links.iter().map(|r| (r.path.as_str(), r)).collect()).unwrap_or_default();
    let mut ix = Index { at: Instant::now(), dirs: Vec::new(), links: Vec::new() };
    for r in roots {
        walk(r, MAX_DEPTH, &mut ix, &known, read);
    }
    ix
}

fn walk(dir: &Path, depth: usize, ix: &mut Index, known: &HashMap<&str, &LinkRec>, read: &mut dyn FnMut(&str) -> LinkRead) {
    ix.dirs.push((dir.to_path_buf(), dir_time(dir)));
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
    // (Search order: the same every time.)
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, ix, known, read);
            }
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
            let path = p.to_string_lossy().into_owned();
            let st = e.metadata().map(|m| stamp_of(&m)).unwrap_or((None, 0));
            let (aumid, failed) = match known.get(path.as_str()).filter(|r| r.stamp == st && !r.failed) {
                Some(r) => (r.aumid.clone(), false),
                None => kept(read(&path)),
            };
            ix.links.push(LinkRec { path, stamp: st, aumid, failed });
        }
    }
}

/// A shortcut's AUMID (System.AppUserModel.ID on the .lnk): Ok(None) if
/// it has none, Err if it could not be read.
unsafe fn link_aumid(path: &str) -> LinkRead {
    use windows::core::{Interface, HSTRING, PCWSTR};
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|_| ())?;
    link.cast::<IPersistFile>().map_err(|_| ())?.Load(PCWSTR(HSTRING::from(path).as_ptr()), STGM_READ).map_err(|_| ())?;
    let s = link.cast::<IPropertyStore>().map_err(|_| ())?.GetValue(&PKEY_AppUserModel_ID).map_err(|_| ())?.to_string();
    Ok((!s.is_empty()).then_some(s))
}

/// A window's Relaunch* properties.
#[derive(Default)]
struct Relaunch {
    icon: Option<String>,
    name: Option<String>,
    command: Option<String>,
}

/// The window's RelaunchIconResource, RelaunchDisplayNameResource (an
/// "@..." name read through SHLoadIndirectString) and RelaunchCommand.
unsafe fn relaunch_of(hwnd: isize) -> Relaunch {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Storage::EnhancedStorage::{PKEY_AppUserModel_RelaunchCommand, PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource};
    use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
    use windows::Win32::UI::Shell::SHLoadIndirectString;
    let Ok(store) = SHGetPropertyStoreForWindow::<IPropertyStore>(HWND(hwnd as *mut _)) else { return Relaunch::default() };
    let get = |k| store.GetValue(k).ok().map(|v| v.to_string()).filter(|s| !s.trim().is_empty());
    let icon = get(&PKEY_AppUserModel_RelaunchIconResource);
    let command = get(&PKEY_AppUserModel_RelaunchCommand);
    let name = get(&PKEY_AppUserModel_RelaunchDisplayNameResource).and_then(|n| {
        if !n.starts_with('@') {
            return Some(n);
        }
        let mut buf = [0u16; 512];
        SHLoadIndirectString(&HSTRING::from(n.as_str()), &mut buf, None).ok()?;
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end])).filter(|s| !s.trim().is_empty())
    });
    Relaunch { icon, name, command }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(v: &str) -> Option<String> {
        match v {
            "LOCALAPPDATA" => Some(r"C:\Users\me\AppData\Local".into()),
            "SystemRoot" => Some(r"C:\Windows".into()),
            _ => None,
        }
    }

    #[test]
    fn web_apps_by_aumid() {
        assert!(is_web_app("Chrome._crx_cmkncekebbplodngbpllndjkfo"));
        assert!(is_web_app("MSEdge._crx_kfbdpdaobnofkbopebjglnaadopfikhh"));
        assert!(is_web_app("Brave._CRX_abc.UserData.Profile1"));
        assert!(!is_web_app("Chrome"), "the browser itself");
        assert!(!is_web_app("Chrome.UserData.Profile1"), "a browser profile");
        assert!(!is_web_app("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"));
        assert!(!is_web_app("_crx_abc"), "no browser id");
        assert_eq!(apps_folder_aumid(r"shell:appsfolder\Chrome._crx_x"), Some("Chrome._crx_x"));
        assert_eq!(apps_folder_aumid(r"shell:AppsFolder\"), None);
        assert_eq!(apps_folder_aumid(r"C:\x.lnk"), None);
        assert_eq!(apps_folder_path("Chrome._crx_x"), r"shell:AppsFolder\Chrome._crx_x");
    }

    #[test]
    fn icon_resources_parse() {
        let e = &env;
        assert_eq!(
            parse_icon_resource(r"C:\Users\me\AppData\Local\Google\Chrome\User Data\Default\Google Profile.ico,0", e),
            Some((r"C:\Users\me\AppData\Local\Google\Chrome\User Data\Default\Google Profile.ico".into(), 0))
        );
        assert_eq!(parse_icon_resource(r"@%SystemRoot%\system32\shell32.dll,-21", e), Some((r"C:\Windows\system32\shell32.dll".into(), -21)));
        assert_eq!(parse_icon_resource(r#""%LOCALAPPDATA%\a b\x.ico""#, e), Some((r"C:\Users\me\AppData\Local\a b\x.ico".into(), 0)));
        assert_eq!(parse_icon_resource(r#""C:\a,b\x.exe",3"#, e), Some((r"C:\a,b\x.exe".into(), 3)), "a comma in the path");
        assert_eq!(parse_icon_resource(r"C:\a,b\x.ico", e), Some((r"C:\a,b\x.ico".into(), 0)), "a comma not before an index");
        assert_eq!(parse_icon_resource("  ", e), None);
        assert_eq!(parse_icon_resource(",0", e), None);
    }

    #[test]
    fn env_vars_expand() {
        let e = &env;
        assert_eq!(expand_env(r"%LOCALAPPDATA%\x", e), r"C:\Users\me\AppData\Local\x");
        assert_eq!(expand_env(r"%NOPE%\x", e), r"%NOPE%\x");
        assert_eq!(expand_env("100% sure", e), "100% sure");
        assert_eq!(expand_env("%%SystemRoot%", e), r"%C:\Windows");
    }

    #[test]
    fn icon_resources_keep_their_index() {
        let e = &env;
        assert_eq!(icon_file(r"C:\p\app.ico,0", e).as_deref(), Some(r"C:\p\app.ico"));
        assert_eq!(icon_file(r"C:\p\app.ICO,2", e).as_deref(), Some(r"C:\p\app.ICO"), "an .ico is one icon");
        assert_eq!(icon_file(r"C:\p\chrome.exe,0", e).as_deref(), Some(r"C:\p\chrome.exe"), "a first icon: the file");
        // Any other icon: the file and which one (positive: an index,
        // negative: a resource id), read as such (icons::icon_location).
        assert_eq!(icon_file(r"C:\App\app.exe,1", e).as_deref(), Some(r"C:\App\app.exe,1"));
        assert_eq!(icon_file(r#""C:\App\resources.dll",-42"#, e).as_deref(), Some(r"C:\App\resources.dll,-42"));
        assert_eq!(icon_file(r"@%SystemRoot%\system32\shell32.dll,-21", e).as_deref(), Some(r"C:\Windows\system32\shell32.dll,-21"));
        for (k, file, i) in [(r"C:\App\app.exe,1", r"C:\App\app.exe", 1), (r"C:\App\resources.dll,-42", r"C:\App\resources.dll", -42)] {
            assert_eq!(crate::icons::icon_location(&icon_file(k, e).unwrap()), Some((file, i)));
        }
        assert_eq!(crate::icons::icon_location(&icon_file(r"C:\p\chrome.exe,0", e).unwrap()), None);
    }

    #[test]
    fn relaunch_commands_split_into_program_and_arguments() {
        let e = &env;
        let chrome = r"C:\Program Files\Google\Chrome\Application\chrome_proxy.exe";
        let l = parse_command(&format!(r#""{chrome}"  --profile-directory=Default --app-id=cmkncekebbplodngbpllndjkfo"#), e).unwrap();
        assert_eq!(l, Launch { exe: chrome.into(), args: "--profile-directory=Default --app-id=cmkncekebbplodngbpllndjkfo".into() });
        // Unquoted, with spaces: up to the word that ends in .exe.
        let l = parse_command(r"C:\Program Files\Brave\brave.exe --app-id=x", e).unwrap();
        assert_eq!((l.exe.as_str(), l.args.as_str()), (r"C:\Program Files\Brave\brave.exe", "--app-id=x"));
        assert_eq!(parse_command(r"%LOCALAPPDATA%\B\b.exe", e).unwrap(), Launch { exe: r"C:\Users\me\AppData\Local\B\b.exe".into(), args: String::new() });
        // Arguments are kept as written (quotes, & and | are only text to
        // the program: nothing goes through a shell).
        assert_eq!(parse_command(r#""C:\b\b.exe" --x="a b" & calc"#, e).unwrap().args, r#"--x="a b" & calc"#);
        assert_eq!(parse_command(r"chrome.exe --app-id=x", e), None, "not an absolute path");
        assert_eq!(parse_command(r"\\server\share\b.exe", e), None, "not on this PC");
        assert_eq!(parse_command(r#""C:\b\b.cmd" x"#, e), None, "not a program");
        assert_eq!(parse_command(r#""C:\b\b.exe"#, e), None, "unclosed quote");
        assert_eq!(parse_command("  ", e), None);
    }

    #[test]
    fn shortcuts_match_whole_aumids_in_search_order() {
        let (user, all) = (temp("order-user"), temp("order-all"));
        let crd = "Chrome._crx_cmkncekebbplodngbpllndjkfo";
        let mine = user.0.join(r"Chrome 앱\Chrome 원격 데스크톱.lnk");
        let music = user.0.join(r"Chrome 앱\YouTube Music.lnk");
        let theirs = all.0.join("Other.lnk");
        std::fs::write(&mine, crd).unwrap();
        std::fs::write(&music, "Chrome._crx_cinhimbnkkghhklpknlkffjgod").unwrap();
        std::fs::write(&theirs, crd.to_lowercase()).unwrap();
        let reads = std::cell::RefCell::new(Vec::new());
        let mut ix = scan(&[user.0.clone(), all.0.clone()], None, &mut reader(&reads));
        let s = |p: &PathBuf| p.to_string_lossy().into_owned();
        assert_eq!(ix.links_of(&crd.to_uppercase(), &mut reader(&reads)), vec![s(&mine), s(&theirs)], "the user's first, any case");
        assert_eq!(ix.links_of("Chrome._crx_cinhimbnkkghhklpknlkffjgod", &mut reader(&reads)), vec![s(&music)]);
        assert!(ix.links_of("Chrome._crx_cmkncek", &mut reader(&reads)).is_empty(), "a whole AUMID, not a prefix");
        assert!(ix.links_of("Chrome", &mut reader(&reads)).is_empty());
    }

    #[test]
    fn names_and_icons_fall_back_in_order() {
        let link = r"C:\U\Programs\Chrome 앱\Chrome 원격 데스크톱.lnk".to_string();
        // The window's own name first, then the shortcut's.
        assert_eq!(combine(vec![link.clone()], None, Some(" Remote ".into()), None).name.as_deref(), Some("Remote"));
        assert_eq!(combine(vec![link.clone()], None, Some("  ".into()), None).name.as_deref(), Some("Chrome 원격 데스크톱"));
        assert_eq!(combine(Vec::new(), None, None, None), WebApp::default(), "nothing: the exe's name and icon (apps.rs)");

        // With a shortcut, the item is the shortcut and draws its icon.
        let both = combine(vec![link.clone()], Some(r"C:\a.ico".into()), None, None);
        assert_eq!(icon_key(&link, &both), None);
        // An old pin of the browser's exe: the window's icon.
        let no_link = combine(Vec::new(), Some(r"C:\a.ico".into()), None, None);
        assert_eq!(icon_key(r"c:\program files\google\chrome\application\chrome.exe", &no_link).as_deref(), Some(r"C:\a.ico"));
        assert_eq!(icon_key(r"c:\chrome.exe", &WebApp::default()), None);
        // A pin of a shortcut keeps the shortcut's icon.
        assert_eq!(icon_key(r"C:\pins\App.lnk", &no_link), None);
        // The AppsFolder path: the shortcut found since, else the window's
        // icon, else the browser's.
        let apps = apps_folder_path("Chrome._crx_x");
        assert_eq!(icon_key(&apps, &both).as_deref(), Some(link.as_str()));
        assert_eq!(icon_key(&apps, &no_link).as_deref(), Some(r"C:\a.ico"));
        let launch = WebApp { launch: Some(Launch { exe: r"C:\b\chrome_proxy.exe".into(), args: "--app-id=x".into() }), ..Default::default() };
        assert_eq!(icon_key(&apps, &launch).as_deref(), Some(r"C:\b\chrome_proxy.exe"));
        assert_eq!(icon_key(&apps, &WebApp::default()), None);
    }

    #[test]
    fn answers_keep_for_a_while() {
        let found = WebApp { link: Some("x.lnk".into()), ..Default::default() };
        assert!(fresh(&found, Duration::from_secs(60)));
        assert!(!fresh(&found, KEEP_FOUND));
        assert!(fresh(&WebApp::default(), Duration::from_secs(5)));
        assert!(!fresh(&WebApp::default(), KEEP_MISSING), "no shortcut yet: asked again soon");
    }

    const A: &str = "Chrome._crx_aaaa";

    fn info(links: &[&str], name: Option<&str>) -> WebApp {
        combine(links.iter().map(|s| s.to_string()).collect(), None, name.map(String::from), None)
    }

    #[test]
    fn running_items_follow_their_shortcut_and_name() {
        let menu = |p: &str| p.starts_with(r"C:\SM\");
        let apps = apps_folder_path(A);
        // The window came first: no shortcut, the browser's name.
        let r = refresh(A, "Google Chrome", &apps, false, None, &info(&[], None), &menu);
        assert_eq!(r, Refresh::default(), "nothing known yet: stays");
        // The shortcut was made later: the item becomes it, with its name.
        let r = refresh(A, "Google Chrome", &apps, false, None, &info(&[r"C:\SM\App.lnk"], None), &menu);
        assert_eq!(r, Refresh { name: Some("App".into()), path: Some(r"C:\SM\App.lnk".into()) });
        // Renamed: the new one.
        let r = refresh(A, "App", r"C:\SM\App.lnk", false, None, &info(&[r"C:\SM\New.lnk"], None), &menu);
        assert_eq!(r, Refresh { name: Some("New".into()), path: Some(r"C:\SM\New.lnk".into()) });
        // Deleted: never the deleted file, never the browser's exe.
        let r = refresh(A, "New", r"C:\SM\New.lnk", false, None, &info(&[], None), &menu);
        assert_eq!(r, Refresh { name: None, path: Some(apps.clone()) });
        // Up to date: nothing.
        assert_eq!(refresh(A, "New", r"c:\sm\new.lnk", false, None, &info(&[r"C:\SM\New.lnk"], None), &menu), Refresh::default());
    }

    #[test]
    fn pins_follow_the_app_but_keep_the_users_name() {
        let menu = |p: &str| p.starts_with(r"C:\SM\");
        // The pin's shortcut was renamed: the pin takes the new one, and the
        // name the dock gave it.
        let r = refresh(A, "App", r"C:\SM\App.lnk", true, Some("App"), &info(&[r"C:\SM\New.lnk"], Some("New")), &menu);
        assert_eq!(r, Refresh { name: Some("New".into()), path: Some(r"C:\SM\New.lnk".into()) });
        // The user named it: the name stays, the path follows.
        let r = refresh(A, "Work mail", r"C:\SM\App.lnk", true, Some("App"), &info(&[r"C:\SM\New.lnk"], Some("New")), &menu);
        assert_eq!(r, Refresh { name: None, path: Some(r"C:\SM\New.lnk".into()) });
        // Deleted: the AppsFolder path (opened as the window says).
        let r = refresh(A, "App", r"C:\SM\App.lnk", true, Some("App"), &info(&[], None), &menu);
        assert_eq!(r.path, Some(apps_folder_path(A)));
        // One of the app's shortcuts (everyone's, the user's comes first):
        // still the app's.
        assert_eq!(refresh(A, "App", r"C:\SM\All.lnk", true, Some("App"), &info(&[r"C:\SM\Mine.lnk", r"C:\SM\All.lnk"], Some("App")), &menu), Refresh::default());
        // A shortcut outside the Start menus (the taskbar's, never read)
        // and the AppsFolder path stay.
        assert_eq!(refresh(A, "App", r"C:\TB\App.lnk", true, Some("App"), &info(&[r"C:\SM\Other.lnk"], Some("App")), &menu).path, None);
        assert_eq!(refresh(A, "App", &apps_folder_path(A), true, Some("App"), &info(&[r"C:\SM\App.lnk"], Some("App")), &menu).path, None);
        // No name kept (an older pin): never renamed.
        assert_eq!(refresh(A, "App", &apps_folder_path(A), true, None, &info(&[], Some("New")), &menu).name, None);
    }

    #[test]
    fn pins_keep_what_starts_the_app() {
        let app = WebApp {
            link: None,
            links: Vec::new(),
            icon: Some(r"C:\App\res.dll,-42".into()),
            name: Some("App".into()),
            launch: Some(Launch { exe: r"C:\B\chrome_proxy.exe".into(), args: "--profile-directory=Default --app-id=aaaa".into() }),
            ver: 0,
        };
        let p = WebPin::of(A, Some(&app)).unwrap();
        assert_eq!(p.exe, r"C:\B\chrome_proxy.exe");
        assert_eq!(p.args, "--profile-directory=Default --app-id=aaaa");
        assert_eq!(WebPin::of("Chrome", Some(&app)), None, "not a web app");
        // Through the config file and back.
        let text = serde_json::to_string(&p).unwrap();
        let back: WebPin = serde_json::from_str(&text).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.launch(), app.launch);
        // Nothing known but who it is: only that is written.
        let bare = WebPin::of(A, None).unwrap();
        assert_eq!(serde_json::to_string(&bare).unwrap(), format!(r#"{{"aumid":"{A}"}}"#));
        assert_eq!(bare.launch(), None);
    }

    #[test]
    fn a_pin_starts_its_app_without_a_shortcut() {
        let aumid = "Chrome._crx_launchtest";
        let path = apps_folder_path(aumid);
        assert_eq!(launch_for(&path), None, "nothing known: the path itself");
        seed(&WebPin { aumid: aumid.into(), exe: r"C:\B\chrome_proxy.exe".into(), args: "--app-id=launchtest".into(), ..Default::default() });
        assert_eq!(launch_for(&path), Some(Launch { exe: r"C:\B\chrome_proxy.exe".into(), args: "--app-id=launchtest".into() }));
        assert_eq!(read(aumid), None, "a pin's details are not a reading");
        assert_eq!(launch_for(r"C:\x.lnk"), None);
        assert_eq!(launch_for(r"shell:AppsFolder\Microsoft.App_x!App"), None, "a packaged app: the shell's");
        // A shortcut found since: that.
        if let Ok(mut c) = cache().lock() {
            c.insert(aumid.to_lowercase(), Entry { info: info(&[r"C:\SM\L.lnk"], None), at: Some(Instant::now()), windowed: true });
        }
        assert_eq!(launch_for(&path).map(|l| l.exe).as_deref(), Some(r"C:\SM\L.lnk"));
        // A reading is never undone by a pin.
        seed(&WebPin { aumid: aumid.into(), ..Default::default() });
        assert_eq!(read(aumid).and_then(|i| i.link).as_deref(), Some(r"C:\SM\L.lnk"));
    }

    #[test]
    fn pinned_apps_not_running_are_read_now_and_then() {
        let found = info(&[r"C:\SM\App.lnk"], None);
        let s = Duration::from_secs;
        // Never read (only a pin's): read, with a window or without.
        assert!(!keeps(&found, None, false, true) && !keeps(&found, None, false, false));
        // A window read a minute ago: fresh for windows and pins.
        assert!(keeps(&found, Some(s(60)), true, true) && keeps(&found, Some(s(60)), true, false));
        // Read for a pin a moment ago: a window is read still (its own
        // name, icon and command), a pin not again yet...
        assert!(!keeps(&found, Some(s(1)), false, true));
        assert!(keeps(&found, Some(s(1)), false, false));
        // ...but every KEEP_PIN (a shortcut renamed while it is closed).
        assert!(!keeps(&found, Some(KEEP_PIN), false, false));

        let mut pinned = HashMap::new();
        pinned.insert(1, vec!["Chrome._crx_aaaa".to_string(), "Chrome._crx_bbbb".to_string()]);
        pinned.insert(2, vec!["chrome._crx_AAAA".to_string()]);
        // Each once (two docks pin it), those running left out.
        let mut todo = closed_pins(&pinned, &[]);
        todo.sort_by_key(|a| a.to_lowercase());
        assert_eq!(todo.len(), 2);
        assert_eq!(closed_pins(&pinned, &["CHROME._CRX_AAAA"]), vec!["Chrome._crx_bbbb".to_string()]);
    }

    #[test]
    fn a_pin_reads_its_name_without_a_window_but_keeps_it() {
        let aumid = "Chrome._crx_pinnamesxyz";
        seed(&WebPin { aumid: aumid.into(), name: Some("Remote".into()), exe: r"C:\B\chrome_proxy.exe".into(), ..Default::default() });
        // Read without a window: no shortcut in this test's Start menus,
        // so nothing but what the pin kept, now a reading.
        warm(aumid, 0);
        let r = read(aumid).expect("a reading");
        assert_eq!(r.name.as_deref(), Some("Remote"), "the pin's name stays");
        assert_eq!(r.launch.map(|l| l.exe).as_deref(), Some(r"C:\B\chrome_proxy.exe"), "how it starts stays");
    }

    #[test]
    fn a_click_on_a_pin_whose_shortcut_is_gone_opens_the_app() {
        let gone = r"C:\SM\App.lnk";
        let new = r"C:\SM\New.lnk";
        let launch = Launch { exe: r"C:\B\chrome_proxy.exe".into(), args: "--app-id=aaaa".into() };
        let known = WebApp { links: vec![gone.into(), new.into()], launch: Some(launch.clone()), ..Default::default() };
        let on_disk = |files: &'static [&'static str]| move |p: &str| files.iter().any(|f| f.eq_ignore_ascii_case(p));
        // There: itself.
        assert_eq!(pin_launch_with(gone, Some(&known), &on_disk(&[r"C:\SM\App.lnk"])), None);
        // Renamed: the shortcut there now.
        assert_eq!(pin_launch_with(gone, Some(&known), &on_disk(&[r"C:\SM\New.lnk"])).map(|l| l.exe).as_deref(), Some(new));
        // Deleted: as the window's RelaunchCommand (or the pin) said.
        assert_eq!(pin_launch_with(gone, Some(&known), &on_disk(&[])), Some(launch));
        // Nothing known: itself (and its failure says so, with Unpin).
        assert_eq!(pin_launch_with(gone, Some(&WebApp::default()), &on_disk(&[])), None);
        assert_eq!(pin_launch_with(gone, None, &on_disk(&[])), None);
        // Only a web app's shortcut pin.
        assert_eq!(pin_launch(r"C:\nowhere\x.lnk", "Vendor.App"), None);
        assert_eq!(pin_launch(&apps_folder_path(A), A), None);
    }

    #[test]
    fn a_pin_never_leaves_a_shortcut_that_could_not_be_read() {
        let p = r"C:\SM\App.lnk";
        let rec = |failed: bool| LinkRec { path: p.into(), stamp: (None, 0), aumid: None, failed };
        let ix = |links: Vec<LinkRec>| Index { at: Instant::now(), dirs: Vec::new(), links };
        assert!(!read_fine(&ix(vec![rec(true)]), &p.to_lowercase()), "unknown: it may be the app's still");
        assert!(read_fine(&ix(vec![rec(false)]), p), "read: another app's, or none");
        assert!(read_fine(&ix(Vec::new()), p), "gone");
        // So refresh keeps the pin's path while it cannot tell.
        let menu = |q: &str| q.starts_with(r"C:\SM\") && read_fine(&ix(vec![rec(true)]), q);
        assert_eq!(refresh(A, "App", p, true, Some("App"), &info(&[], None), &menu).path, None);
    }

    #[test]
    fn an_icon_changed_in_place_is_a_new_version() {
        let t = temp("ver");
        let ico = t.0.join("app.ico");
        let lnk = t.0.join(r"Chrome 앱\App.lnk");
        std::fs::write(&ico, "one").unwrap();
        std::fs::write(&lnk, "Chrome._crx_aaaa").unwrap();
        let s = |p: &PathBuf| p.to_string_lossy().into_owned();
        let app = WebApp { link: Some(s(&lnk)), links: vec![s(&lnk)], icon: Some(s(&ico)), ..Default::default() };
        let v1 = icon_ver(&app);
        assert_eq!(icon_ver(&app), v1, "nothing changed: the same");
        // The window's icon file replaced (same path, same AUMID, same name).
        std::fs::write(&ico, "another icon").unwrap();
        let v2 = icon_ver(&app);
        assert_ne!(v2, v1);
        // The shortcut changed in place (its icon location, say).
        std::fs::write(&lnk, "Chrome._crx_aaaa, another icon").unwrap();
        assert_ne!(icon_ver(&app), v2);
        // ...so the reading is another (items.rs follows it).
        assert_ne!(WebApp { ver: v1, ..app.clone() }, WebApp { ver: v2, ..app });
        // An icon "file,index": its file's stamp.
        let res = WebApp { icon: Some(format!("{},3", s(&ico))), ..Default::default() };
        let r1 = icon_ver(&res);
        std::fs::write(&ico, "a third icon, longer").unwrap();
        assert_ne!(icon_ver(&res), r1);
    }

    #[test]
    fn start_menu_paths() {
        let roots = [PathBuf::from(r"C:\U\Start Menu\Programs"), PathBuf::from(r"C:\All\Start Menu\Programs\")];
        assert!(under(r"c:\u\start menu\programs\Chrome 앱\A.lnk", &roots));
        assert!(under(r"C:\All\Start Menu\Programs\A.lnk", &roots));
        assert!(!under(r"C:\U\Start Menu\ProgramsX\A.lnk", &roots));
        assert!(!under(r"C:\U\Start Menu\Programs", &roots));
        assert!(!under(r"C:\Users\me\Desktop\A.lnk", &roots));
    }

    /// A folder of its own under the temp folder, gone after the test.
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn temp(name: &str) -> Temp {
        let d = std::env::temp_dir().join(format!("nopbar-webapps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Chrome 앱")).unwrap();
        Temp(d)
    }

    /// The fake shortcuts' AUMIDs are their contents; every read counted.
    fn reader(reads: &std::cell::RefCell<Vec<String>>) -> impl FnMut(&str) -> LinkRead + '_ {
        move |p: &str| {
            reads.borrow_mut().push(p.to_string());
            Ok(std::fs::read_to_string(p).ok().filter(|s| !s.is_empty()))
        }
    }

    #[test]
    fn a_shortcut_that_could_not_be_read_is_read_again() {
        let t = temp("busy");
        let p = t.0.join(r"Chrome 앱\App.lnk");
        std::fs::write(&p, "Chrome._crx_aaaa").unwrap();
        let ps = p.to_string_lossy().into_owned();
        let roots = [t.0.clone()];
        // The first reading fails (the browser has it open); the file is
        // not touched again after (same stamp).
        let mut first = true;
        let mut busy_once = |q: &str| -> LinkRead {
            if std::mem::take(&mut first) {
                return Err(());
            }
            Ok(std::fs::read_to_string(q).ok())
        };
        let mut ix = scan(&roots, None, &mut busy_once);
        assert!(ix.links_of("Chrome._crx_aaaa", &mut busy_once).is_empty(), "not known yet");
        assert!(ix.links.iter().any(|r| r.failed));
        assert!(!ix.moved(), "nothing on disk says so");
        assert!(!ix.retry_due(), "not at once");
        ix.at = Instant::now() - RETRY;
        assert!(ix.retry_due(), "a while later: scanned again");
        // The next scan (as the 30-minute one) reads it, stamp unchanged.
        let reads = std::cell::RefCell::new(Vec::new());
        let mut ix = scan(&roots, Some(&ix), &mut reader(&reads));
        assert_eq!(*reads.borrow(), vec![ps.clone()], "only the failed one");
        assert_eq!(ix.links_of("Chrome._crx_aaaa", &mut reader(&reads)), vec![ps.clone()]);
        assert!(!ix.retry_due());
        // A shortcut read fine without a web app's AUMID: not read again.
        std::fs::write(t.0.join("Plain.lnk"), "").unwrap();
        let ix = scan(&roots, Some(&ix), &mut reader(&reads));
        reads.borrow_mut().clear();
        let mut ix = scan(&roots, Some(&ix), &mut reader(&reads));
        assert!(reads.borrow().is_empty());
        assert!(!ix.links.iter().any(|r| r.failed));
        // Changed in place and failing to read then: read again too.
        std::fs::write(&p, "Chrome._crx_aaaa ").unwrap();
        assert!(ix.links_of("Chrome._crx_aaaa", &mut |_: &str| -> LinkRead { Err(()) }).is_empty());
        assert!(ix.links.iter().any(|r| r.failed && r.path == ps));
    }

    #[test]
    fn the_index_reads_only_what_changed() {
        let t = temp("scan");
        let a = t.0.join(r"Chrome 앱\A.lnk");
        let b = t.0.join(r"Chrome 앱\B.lnk");
        let other = t.0.join("Other.lnk");
        std::fs::write(&a, "Chrome._crx_aaaa").unwrap();
        std::fs::write(&b, "Chrome._crx_bbbb").unwrap();
        std::fs::write(&other, "Vendor.App").unwrap();
        let reads = std::cell::RefCell::new(Vec::new());
        let roots = [t.0.clone()];
        let mut ix = scan(&roots, None, &mut reader(&reads));
        assert_eq!(reads.borrow().len(), 3, "the first time, each one");
        assert!(!ix.moved());
        let a_s = a.to_string_lossy().into_owned();
        assert_eq!(ix.links_of("chrome._crx_AAAA", &mut reader(&reads)), vec![a_s.clone()]);
        assert_eq!(ix.links_of("Vendor.App", &mut reader(&reads)), Vec::<String>::new(), "not a web app: not kept");
        assert_eq!(reads.borrow().len(), 3, "unchanged: none read again");

        // Nothing changed: read again, nothing loaded.
        reads.borrow_mut().clear();
        let ix2 = scan(&roots, Some(&ix), &mut reader(&reads));
        assert!(reads.borrow().is_empty());
        assert_eq!(ix2.links, ix.links);

        // A folder's write time moves in system clock ticks (about 15.6 ms):
        // a fast machine (CI) could change it within the tick it was read in.
        let tick = || std::thread::sleep(std::time::Duration::from_millis(40));

        // A new shortcut: its folder changed; only it is read.
        tick();
        let c = t.0.join(r"Chrome 앱\C.lnk");
        std::fs::write(&c, "Chrome._crx_cccc").unwrap();
        assert!(ix.moved(), "a shortcut made shows on its folder");
        let ix3 = scan(&roots, Some(&ix), &mut reader(&reads));
        assert_eq!(*reads.borrow(), vec![c.to_string_lossy().into_owned()]);
        ix = ix3;

        // Renamed: the folder says so; the AUMID moves to the new path.
        tick();
        let a2 = t.0.join(r"Chrome 앱\A2.lnk");
        std::fs::rename(&a, &a2).unwrap();
        assert!(ix.moved());
        reads.borrow_mut().clear();
        ix = scan(&roots, Some(&ix), &mut reader(&reads));
        assert_eq!(ix.links_of("Chrome._crx_aaaa", &mut reader(&reads)), vec![a2.to_string_lossy().into_owned()]);
    }

    #[test]
    fn a_shortcut_replaced_in_place_leaves_the_app_it_named() {
        let t = temp("replace");
        let p = t.0.join(r"Chrome 앱\App.lnk");
        std::fs::write(&p, "Chrome._crx_aaaa").unwrap();
        let reads = std::cell::RefCell::new(Vec::new());
        let mut ix = scan(std::slice::from_ref(&t.0), None, &mut reader(&reads));
        let ps = p.to_string_lossy().into_owned();
        assert_eq!(ix.links_of("Chrome._crx_aaaa", &mut reader(&reads)), vec![ps.clone()]);
        // Same path, another app's shortcut now (another profile's).
        std::fs::write(&p, "Chrome._crx_bbbb.UserData.Profile1").unwrap();
        reads.borrow_mut().clear();
        assert_eq!(ix.links_of("Chrome._crx_aaaa", &mut reader(&reads)), Vec::<String>::new(), "A's shortcut is gone");
        assert_eq!(*reads.borrow(), vec![ps.clone()], "read again, once");
        assert_eq!(ix.links_of("Chrome._crx_bbbb.UserData.Profile1", &mut reader(&reads)), vec![ps.clone()]);
        // Deleted: nobody's.
        std::fs::remove_file(&p).unwrap();
        assert_eq!(ix.links_of("Chrome._crx_bbbb.UserData.Profile1", &mut reader(&reads)), Vec::<String>::new());
    }
}
