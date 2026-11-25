//! Discovering app windows and identifying which app they belong to.

use windows::core::{Interface, BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, MAX_PATH};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Debug)]
pub struct AppWindow {
    pub hwnd: HWND,
    pub exe: Option<String>, // lowercase full path
    pub aumid: Option<String>,
    /// The window's process when the list was taken, and that process's
    /// creation time (FILETIME; 0 = unreadable): who the handle belonged
    /// to, so a handle reused by another process later is never acted on.
    pub pid: u32,
    pub started: u64,
}

#[derive(Clone, Debug, Default)]
pub struct AppId {
    pub exe: Option<String>,
    pub aumid: Option<String>,
}

impl AppId {
    /// Part of Windows (Explorer, Settings, Notepad, Edge, Store apps from
    /// Microsoft...): its icon is shown as Windows draws it, never boxed.
    pub fn is_system(&self) -> bool {
        // Apps that come with Windows (package family prefixes).
        const INBOX: [&str; 8] = [
            "microsoft.windows",
            "windows.",
            "microsoft.microsoftedge",
            "msedge",
            "microsoft.paint",
            "microsoft.screensketch",
            "microsoft.zunemusic",
            "microsoft.zunevideo",
        ];
        if let Some(a) = &self.aumid {
            let a = a.to_lowercase();
            if INBOX.iter().any(|p| a.starts_with(p)) {
                return true;
            }
        }
        if let Some(e) = &self.exe {
            let e = e.to_lowercase();
            if let Some(w) = windows_dir() {
                if e.starts_with(&format!(r"{}\", w.to_lowercase())) {
                    return true;
                }
            }
            for var in ["ProgramFiles(x86)", "ProgramFiles"] {
                if let Ok(pf) = std::env::var(var) {
                    if e == format!(r"{}\microsoft\edge\application\msedge.exe", pf.to_lowercase()) {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn matches(&self, w: &AppWindow) -> bool {
        match (&self.aumid, &w.aumid) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            // An installed web app runs the browser's exe but is an app of
            // its own: its windows are never the browser's (a pin of
            // chrome.exe), and a browser window without an AUMID (or one
            // not read this time) is never the web app's, whichever showed
            // first. Only the same AUMID joins it.
            (None, Some(a)) | (Some(a), None) if crate::webapps::is_web_app(a) => false,
            _ => matches!((&self.exe, &w.exe), (Some(a), Some(b)) if a == b),
        }
    }

    /// A browser's web app (by its AUMID).
    pub fn is_web_app(&self) -> bool {
        self.aumid.as_deref().is_some_and(crate::webapps::is_web_app)
    }
}

/// Which app a pin is: its path's (`identify`), or a web app's AUMID kept
/// with the pin (pin_identity).
pub fn identify_pin(path: &str, web: Option<&crate::webapps::WebPin>) -> AppId {
    identify_pin_checked(path, web).0
}

/// identify_pin, and whether the pin's shortcut is another app's now
/// (pin_identity).
pub fn identify_pin_checked(path: &str, web: Option<&crate::webapps::WebPin>) -> (AppId, bool) {
    pin_identity(path, identify(path), web)
}

/// Pure: which app a pin of `path` is, `found` what its path says now
/// (`identify`), `web` the web app kept with it; and whether its shortcut
/// is another app's now. A web app kept with the pin is who the pin is:
/// a shortcut at its path that names another AUMID (made over it while the
/// dock was closed: another app, another profile) or none (a shortcut to a
/// plain program) is not its shortcut any more, and never opened for it
/// (the pin opens the app by its AppsFolder path: webapps::launch_for).
/// A pin without one (older pins, other apps) is what its path says.
pub fn pin_identity(path: &str, found: AppId, web: Option<&crate::webapps::WebPin>) -> (AppId, bool) {
    let Some(kept) = web.map(|w| w.aumid.as_str()).filter(|a| crate::webapps::is_web_app(a)) else { return (found, false) };
    let lnk = path.to_ascii_lowercase().ends_with(".lnk");
    let other = match found.aumid.as_deref() {
        Some(a) => !a.eq_ignore_ascii_case(kept),
        // Read, without an AUMID; (not read at all: gone, kept as it was).
        None => found.exe.is_some(),
    };
    let stale = lnk && other;
    let exe = if stale { None } else { found.exe };
    (AppId { exe, aumid: Some(kept.to_string()) }, stale)
}

fn norm(path: &str) -> Option<String> {
    let p = path.trim().trim_matches('"');
    if p.is_empty() {
        None
    } else {
        Some(p.to_lowercase())
    }
}

/// When process `pid` was created (FILETIME as u64): tells it from a later
/// process that got the same id. None if it cannot be read.
pub fn process_started(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    if pid == 0 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok();
        let _ = CloseHandle(h);
        ok.then_some(((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
    }
}

/// The window's process id and that process's creation time (0 if unread).
unsafe fn owner_of_window(hwnd: HWND) -> (u32, u64) {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    (pid, process_started(pid).unwrap_or(0))
}

/// Pure: is a window now owned by `pid_now` (process created at
/// `started_now`) still the one recorded as (`pid`, `started`)? A recorded
/// time of 0 (unreadable then) checks the id only.
pub fn same_owner(pid: u32, started: u64, pid_now: u32, started_now: Option<u64>) -> bool {
    pid != 0 && pid_now == pid && (started == 0 || started_now == Some(started))
}

pub(crate) unsafe fn exe_of_window(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
    let _ = CloseHandle(h);
    if ok {
        let exe = norm(&String::from_utf16_lossy(&buf[..len as usize]))?;
        Some(helper_app(&exe).filter(|p| std::path::Path::new(p).is_file()).unwrap_or(exe))
    } else {
        None
    }
}

/// Helper processes that draw an app's windows for it, and the app's exe
/// relative to the helper's folder: their windows are that app's (Steam's
/// whole UI is steamwebhelper.exe windows, which showed in the Dock as a
/// second app named "Steam Client WebHelper").
const HELPERS: [(&str, &str); 1] = [("steamwebhelper.exe", r"..\..\..\steam.exe")];

/// The app exe a helper's windows belong to (lowercase, not checked to
/// exist), or None for any other exe.
pub fn helper_app(exe: &str) -> Option<String> {
    let lower = exe.to_lowercase();
    let (dir, name) = lower.rsplit_once('\\')?;
    let (_, rel) = HELPERS.iter().find(|(h, _)| *h == name)?;
    let mut parts: Vec<&str> = dir.split('\\').collect();
    for step in rel.split('\\') {
        if step == ".." {
            if parts.len() <= 1 {
                return None;
            }
            parts.pop();
        } else {
            parts.push(step);
        }
    }
    Some(parts.join("\\"))
}

unsafe fn store_aumid(store: &IPropertyStore) -> Option<String> {
    let v = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
    let s = v.to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// The window's own AppUserModelID, else its process's package app ID (a
/// Store app's process has one even when its windows set none: Slack's
/// window is "...\app\slack.exe" by path, the same app as the pinned
/// shell:AppsFolder\com.tinyspeck.slackdesktop_...!Slack, and showed twice).
unsafe fn aumid_of_window(hwnd: HWND) -> Option<String> {
    let own = SHGetPropertyStoreForWindow::<IPropertyStore>(hwnd).ok().and_then(|s| store_aumid(&s));
    own.or_else(|| {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        package_aumid(pid)
    })
}

/// The package app ID of process `pid` (None for unpackaged processes).
pub fn package_aumid(pid: u32) -> Option<String> {
    use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
    if pid == 0 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let r = GetApplicationUserModelId(h, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        let _ = CloseHandle(h);
        if r.0 != 0 || len == 0 {
            return None;
        }
        // len counts the terminating null.
        let n = (len as usize).saturating_sub(1).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        (!s.is_empty()).then_some(s)
    }
}

pub(crate) unsafe fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut _ as *mut _,
        std::mem::size_of::<u32>() as u32,
    )
    .is_ok()
        && cloaked != 0
}

/// Same rules the taskbar / Alt+Tab use, approximately.
pub unsafe fn is_app_window(hwnd: HWND) -> bool {
    if !IsWindowVisible(hwnd).as_bool() {
        return false;
    }
    let ex = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
    let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
    if ex.contains(WS_EX_APPWINDOW) {
        // explicitly wants a taskbar button
    } else if !owner.is_invalid() || ex.contains(WS_EX_TOOLWINDOW) || ex.contains(WS_EX_NOACTIVATE) {
        return false;
    }
    if GetWindowTextLengthW(hwnd) == 0 {
        return false;
    }
    if is_system_stub(hwnd) {
        return false;
    }
    !is_cloaked(hwnd)
}

/// System placeholder windows that look like app windows but are not apps:
/// the UAC prompt's stand-in on the normal desktop (consent.exe, title
/// "<app>이(가) 사용자 권한을 요청하고 있습니다.") would otherwise get an empty Dock slot.
unsafe fn is_system_stub(hwnd: HWND) -> bool {
    let mut buf = [0u16; 128];
    let n = GetClassNameW(hwnd, &mut buf).max(0) as usize;
    is_system_stub_class(&String::from_utf16_lossy(&buf[..n.min(buf.len())]))
}

fn is_system_stub_class(class: &str) -> bool {
    class.starts_with("$$$Secure UAP Dummy Window Class")
}

struct EnumCtx {
    skip: HWND,
    out: Vec<HWND>,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    let ctx = &mut *(lp.0 as *mut EnumCtx);
    if hwnd != ctx.skip && is_app_window(hwnd) {
        ctx.out.push(hwnd);
    }
    BOOL(1)
}

/// App windows in z-order (most recently active first).
pub fn enumerate(skip: HWND) -> Vec<AppWindow> {
    let mut ctx = EnumCtx { skip, out: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut _ as isize));
        ctx.out
            .into_iter()
            .map(|hwnd| {
                let (pid, started) = owner_of_window(hwnd);
                AppWindow { hwnd, exe: exe_of_window(hwnd), aumid: aumid_of_window(hwnd), pid, started }
            })
            .collect()
    }
}

/// Integrity level (RID) of a process token; None if it cannot be read.
unsafe fn integrity(process: windows::Win32::Foundation::HANDLE) -> Option<u32> {
    use windows::Win32::Security::{GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel, TOKEN_MANDATORY_LABEL, TOKEN_QUERY};
    use windows::Win32::System::Threading::OpenProcessToken;
    let mut token = windows::Win32::Foundation::HANDLE::default();
    OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
    let mut buf = [0u8; 128];
    let mut len = 0u32;
    let r = GetTokenInformation(token, TokenIntegrityLevel, Some(buf.as_mut_ptr() as *mut _), buf.len() as u32, &mut len);
    let _ = CloseHandle(token);
    r.ok()?;
    let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
    let sid = label.Label.Sid;
    let count = *GetSidSubAuthorityCount(sid);
    Some(*GetSidSubAuthority(sid, count as u32 - 1))
}

/// Process `pid` runs above our integrity level, so Windows (UIPI) drops
/// our input and posted messages to it. Unreadable counts as locked.
pub fn is_locked(pid: u32) -> bool {
    use windows::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let Some(ours) = integrity(GetCurrentProcess()) else { return false };
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return true };
        let theirs = integrity(h);
        let _ = CloseHandle(h);
        theirs.is_none_or(|t| t > ours)
    }
}

/// AppWindow for one window (any window, not only taskbar-worthy ones).
pub fn window_info(hwnd: HWND) -> Option<AppWindow> {
    if hwnd.is_invalid() {
        return None;
    }
    unsafe {
        let (pid, started) = owner_of_window(hwnd);
        Some(AppWindow { hwnd, exe: exe_of_window(hwnd), aumid: aumid_of_window(hwnd), pid, started })
    }
}

/// Works out which app a pinned path refers to.
pub fn identify(path: &str) -> AppId {
    let lower = path.to_lowercase();
    if let Some(rest) = lower.strip_prefix("shell:appsfolder\\") {
        return AppId { exe: None, aumid: Some(path[path.len() - rest.len()..].to_string()) };
    }
    if lower.ends_with(".exe") {
        return AppId { exe: norm(path), aumid: None };
    }
    if lower.ends_with(".lnk") {
        unsafe {
            if let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) {
                if let Ok(pf) = link.cast::<IPersistFile>() {
                    if pf.Load(PCWSTR(HSTRING::from(path).as_ptr()), STGM_READ).is_ok() {
                        let mut buf = [0u16; MAX_PATH as usize * 2];
                        let _ = link.GetPath(&mut buf, std::ptr::null_mut(), 0);
                        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                        let target = String::from_utf16_lossy(&buf[..end]);
                        let aumid = link.cast::<IPropertyStore>().ok().and_then(|s| store_aumid(&s));
                        let mut exe = norm(&target);
                        // File Explorer's pin points at a shell folder, but its
                        // windows belong to explorer.exe and carry no AUMID.
                        if exe.is_none()
                            && aumid.as_deref().is_some_and(|a| a.eq_ignore_ascii_case("Microsoft.Windows.Explorer"))
                        {
                            exe = windows_dir().map(|d| format!("{d}\\explorer.exe"));
                        }
                        return AppId { exe, aumid };
                    }
                }
            }
        }
    }
    AppId::default()
}

fn windows_dir() -> Option<String> {
    std::env::var("SystemRoot").ok().map(|s| s.to_lowercase())
}

/// Most exe names kept (audit A16: one-off and updated exe paths used to
/// stay for the life of the process). Past it the cache starts over; it
/// refills from the window tracker, off the UI thread.
const MAX_DESCRIPTIONS: usize = 512;

/// The exe's own product name ("FileDescription" in its version
/// resource), cached per path. None if it has none.
fn description_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, Option<String>>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Option<String>>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn file_description(exe: &str) -> Option<String> {
    let cache = description_cache();
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(exe).cloned()) {
        return hit;
    }
    let found = unsafe { read_description(exe) };
    if let Ok(mut c) = cache.lock() {
        if c.len() >= MAX_DESCRIPTIONS {
            c.clear();
        }
        c.insert(exe.to_string(), found.clone());
    }
    found
}

/// Reads the names of these windows' apps into the cache. Called by the
/// window tracker before it hands a list to the UI, so display_name on the
/// UI thread finds them there instead of reading version resources from
/// disk (audit A13).
pub fn warm_names(wins: &[AppWindow]) {
    for w in wins {
        if let Some(exe) = w.exe.as_deref() {
            let _ = file_description(exe);
            if is_explorer(Some(exe)) {
                let _ = explorer_display_name();
            }
        }
        // A browser's web app: its shortcut, icon and name (webapps.rs).
        if let Some(a) = w.aumid.as_deref() {
            crate::webapps::warm(a, w.hwnd.0 as isize);
        }
    }
    // Pinned web apps not running: their shortcuts as found now.
    let running: Vec<&str> = wins.iter().filter_map(|w| w.aumid.as_deref()).collect();
    crate::webapps::warm_pins(&running);
}

unsafe fn read_description(exe: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    let path = HSTRING::from(exe);
    let size = GetFileVersionInfoSizeW(&path, None);
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    GetFileVersionInfoW(&path, None, size, data.as_mut_ptr() as *mut _).ok()?;
    let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // First language/codepage pair.
    if !VerQueryValueW(data.as_ptr() as *const _, &HSTRING::from(r"\VarFileInfo\Translation"), &mut ptr, &mut len).as_bool() || len < 4 {
        return None;
    }
    let pair = std::slice::from_raw_parts(ptr as *const u16, 2);
    let key = format!(r"\StringFileInfo\{:04x}{:04x}\FileDescription", pair[0], pair[1]);
    if !VerQueryValueW(data.as_ptr() as *const _, &HSTRING::from(key), &mut ptr, &mut len).as_bool() || len == 0 {
        return None;
    }
    let text = std::slice::from_raw_parts(ptr as *const u16, len as usize);
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    let s = String::from_utf16_lossy(&text[..end]).trim().to_string();
    // Some exes describe themselves generically; keep those out.
    (!s.is_empty() && s.len() <= 40 && !s.contains(".exe")).then_some(s)
}

/// Display name for an unpinned app.
pub fn display_name(w: &AppWindow) -> String {
    let web = w.aumid.as_deref().and_then(|a| crate::webapps::resolve(a, w.hwnd.0 as isize));
    if let Some(name) = web.and_then(|i| i.name) {
        return name;
    }
    if is_explorer(w.exe.as_deref()) {
        if let Some(n) = explorer_display_name() {
            return n;
        }
    }
    name_from(w, w.exe.as_deref().and_then(file_description))
}

/// display_name without any disk access: the product name only if the
/// tracker already read it, else the exe's name (UI thread, audit A13).
pub fn display_name_cached(w: &AppWindow) -> String {
    if let Some(name) = web_app(w).and_then(|i| i.name) {
        return name;
    }
    if is_explorer(w.exe.as_deref()) {
        if let Some(Some(n)) = EXPLORER_NAME.get() {
            return n.clone();
        }
    }
    let desc = w.exe.as_deref().and_then(|e| description_cache().lock().ok().and_then(|c| c.get(e).cloned()).flatten());
    name_from(w, desc)
}

fn is_explorer(exe: Option<&str>) -> bool {
    exe.is_some_and(|e| e.rsplit(['\\', '/']).next().is_some_and(|n| n.eq_ignore_ascii_case("explorer.exe")))
}

/// File Explorer's name as Windows shows it on the taskbar and in Start
/// ("파일 탐색기"; the string its shortcuts' desktop.ini points at), not
/// explorer.exe's own description ("Windows 탐색기"). Read once.
static EXPLORER_NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
const EXPLORER_NAME_REF: &str = r"@%SystemRoot%\system32\shell32.dll,-22067";

pub fn explorer_display_name() -> Option<String> {
    EXPLORER_NAME.get_or_init(|| unsafe { load_indirect(&expand_env(EXPLORER_NAME_REF)) }).clone()
}

/// The name Windows shows for a shortcut it localizes: its folder's
/// desktop.ini says so ([LocalizedFileNames] "File Explorer.lnk=
/// @%SystemRoot%\system32\shell32.dll,-22067" -> "파일 탐색기"). None for
/// any other file. Read once per path.
pub fn localized_file_name(path: &str) -> Option<String> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Option<String>>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = path.to_lowercase();
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    let p = std::path::Path::new(path);
    let found = (|| {
        let file = p.file_name()?.to_str()?;
        let ini = read_ini(&p.parent()?.join("desktop.ini"))?;
        let value = localized_entry(&ini, file)?;
        unsafe { load_indirect(&expand_env(&value)) }
    })();
    if let Ok(mut c) = cache.lock() {
        if c.len() >= MAX_DESCRIPTIONS {
            c.clear();
        }
        c.insert(key, found.clone());
    }
    found
}

/// desktop.ini as text: UTF-16 with a BOM (how Windows writes it), else
/// UTF-8 / ASCII.
fn read_ini(path: &std::path::Path) -> Option<String> {
    let b = std::fs::read(path).ok()?;
    Some(match b.strip_prefix(&[0xFF, 0xFE]) {
        Some(w) => String::from_utf16_lossy(&w.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()),
        None => String::from_utf8_lossy(&b).into_owned(),
    })
}

/// The [LocalizedFileNames] value for `file` in a desktop.ini's text.
fn localized_entry(ini: &str, file: &str) -> Option<String> {
    let file = file.to_lowercase();
    let mut inside = false;
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line.eq_ignore_ascii_case("[LocalizedFileNames]");
            continue;
        }
        let Some((k, v)) = line.split_once('=').filter(|_| inside) else { continue };
        if k.trim().to_lowercase() == file {
            let v = v.trim();
            return (!v.is_empty()).then(|| v.to_string());
        }
    }
    None
}

/// %VAR% from the environment; an unknown one stays as written.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find('%') {
        let Some(len) = rest[a + 1..].find('%') else { break };
        let name = &rest[a + 1..a + 1 + len];
        out.push_str(&rest[..a]);
        match std::env::var(name) {
            Ok(v) if !name.is_empty() => out.push_str(&v),
            _ => out.push_str(&rest[a..a + len + 2]),
        }
        rest = &rest[a + len + 2..];
    }
    out.push_str(rest);
    out
}

/// "@C:\x.dll,-123" -> the string in the UI language; plain text as is.
unsafe fn load_indirect(s: &str) -> Option<String> {
    if !s.starts_with('@') {
        return (!s.is_empty()).then(|| s.to_string());
    }
    let mut out = [0u16; 512];
    windows::Win32::UI::Shell::SHLoadIndirectString(&HSTRING::from(s), &mut out, None).ok()?;
    let end = out.iter().position(|&c| c == 0).unwrap_or(out.len());
    let r = String::from_utf16_lossy(&out[..end]).trim().to_string();
    (!r.is_empty()).then_some(r)
}

/// What the tracker read for `w` if it is a browser's web app (cache only).
fn web_app(w: &AppWindow) -> Option<crate::webapps::WebApp> {
    w.aumid.as_deref().and_then(crate::webapps::cached)
}

fn name_from(w: &AppWindow, desc: Option<String>) -> String {
    if let Some(desc) = desc {
        return desc;
    }
    if let Some(exe) = &w.exe {
        if let Some(stem) = std::path::Path::new(exe).file_stem() {
            let s = stem.to_string_lossy();
            let mut c = s.chars();
            if let Some(f) = c.next() {
                return f.to_uppercase().collect::<String>() + c.as_str();
            }
        }
    }
    w.aumid.clone().unwrap_or_else(|| "App".into())
}

/// Path that can launch or provide an icon for an unpinned app. A
/// browser's web app: its Start-menu shortcut (launches the app, draws its
/// icon, pins as the app), as the tracker found it, else its AppsFolder
/// path (webapps.rs: never the browser's exe).
pub fn launch_path(w: &AppWindow) -> String {
    launch_path_with(w, web_app(w).and_then(|i| i.link))
}

/// What a pin of `w` keeps besides its path: a web app's details
/// (webapps::WebPin), else None.
pub fn web_pin(w: &AppWindow) -> Option<crate::webapps::WebPin> {
    let a = w.aumid.as_deref()?;
    crate::webapps::WebPin::of(a, web_app(w).as_ref())
}

/// Pure: launch_path, given the web app's shortcut if `w` is one.
pub fn launch_path_with(w: &AppWindow, web_link: Option<String>) -> String {
    if let Some(link) = web_link {
        return link;
    }
    // A web app without a shortcut (none, or the Start menus not read
    // yet) is still its own app, not the browser.
    if let Some(a) = w.aumid.as_deref().filter(|a| crate::webapps::is_web_app(a)) {
        return crate::webapps::apps_folder_path(a);
    }
    let packaged = w
        .exe
        .as_deref()
        .map(|e| e.contains("\\windowsapps\\") || e.ends_with("applicationframehost.exe"))
        .unwrap_or(false);
    match (&w.aumid, &w.exe) {
        (Some(a), _) if packaged || w.exe.is_none() => format!("shell:AppsFolder\\{a}"),
        (_, Some(e)) => e.clone(),
        (Some(a), None) => format!("shell:AppsFolder\\{a}"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod system_tests {
    use super::*;

    #[test]
    fn windows_apps_are_system() {
        let w = windows_dir().unwrap_or_else(|| r"c:\windows".into());
        assert!(AppId { exe: Some(format!(r"{w}\explorer.exe")), aumid: None }.is_system());
        assert!(AppId { exe: None, aumid: Some("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into()) }.is_system());
        assert!(AppId { exe: None, aumid: Some("windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel".into()) }.is_system());
        assert!(AppId { exe: Some(r"c:\program files (x86)\microsoft\edge\application\msedge.exe".into()), aumid: None }.is_system());
        assert!(!AppId { exe: Some(r"c:\program files\google\chrome\application\chrome.exe".into()), aumid: None }.is_system());
        assert!(!AppId { exe: None, aumid: Some("Chrome".into()) }.is_system());
    }

    #[test]
    fn steams_helper_windows_are_steams() {
        assert_eq!(helper_app(r"C:\Program Files (x86)\Steam\bin\cef\cef.win64\steamwebhelper.exe").as_deref(), Some(r"c:\program files (x86)\steam\steam.exe"));
        assert_eq!(helper_app(r"c:\program files\google\chrome\application\chrome.exe"), None);
        assert_eq!(helper_app(r"steamwebhelper.exe"), None);
        assert_eq!(helper_app(r"c:\steamwebhelper.exe"), None, "no folder to climb");
    }
}

#[cfg(test)]
mod localized_name_tests {
    use super::*;

    #[test]
    fn desktop_ini_names_the_shortcut() {
        let ini = "\u{feff}[.ShellClassInfo]\r\nLocalizedResourceName=@x.dll,-1\r\n\r\n[LocalizedFileNames]\r\nFile Explorer.lnk=@%SystemRoot%\\system32\\shell32.dll,-22067\r\n크롬.lnk = 크롬 \r\n";
        assert_eq!(localized_entry(ini, "File Explorer.lnk").as_deref(), Some(r"@%SystemRoot%\system32\shell32.dll,-22067"));
        assert_eq!(localized_entry(ini, "file explorer.LNK").as_deref(), Some(r"@%SystemRoot%\system32\shell32.dll,-22067"), "case as Windows ignores it");
        assert_eq!(localized_entry(ini, "크롬.lnk").as_deref(), Some("크롬"));
        assert_eq!(localized_entry(ini, "Google Chrome.lnk"), None);
        assert_eq!(localized_entry("[.ShellClassInfo]\nFile Explorer.lnk=x\n", "File Explorer.lnk"), None, "only its own section");
    }

    #[test]
    fn environment_names_expand() {
        std::env::set_var("NOPBAR_TEST_ROOT", r"C:\Win");
        assert_eq!(expand_env(r"@%NOPBAR_TEST_ROOT%\system32\shell32.dll,-22067"), r"@C:\Win\system32\shell32.dll,-22067");
        assert_eq!(expand_env("%NOPBAR_NO_SUCH_VAR%\\x"), "%NOPBAR_NO_SUCH_VAR%\\x");
        assert_eq!(expand_env("100% sure"), "100% sure");
        assert_eq!(expand_env("plain"), "plain");
    }

    #[test]
    fn file_explorer_is_named_like_windows_names_it() {
        assert!(is_explorer(Some(r"c:\windows\explorer.exe")));
        assert!(is_explorer(Some(r"C:\Windows\Explorer.EXE")));
        assert!(!is_explorer(Some(r"c:\program files\x\iexplorer.exe")));
        assert!(!is_explorer(None));
        // Read from Windows itself: whatever the language, a name, not empty.
        assert!(explorer_display_name().is_some_and(|n| !n.is_empty() && !n.contains('@')));
    }
}

#[cfg(test)]
mod web_app_tests {
    use super::*;

    fn win(exe: &str, aumid: Option<&str>) -> AppWindow {
        AppWindow { hwnd: HWND::default(), exe: Some(exe.into()), aumid: aumid.map(String::from), pid: 0, started: 0 }
    }

    const CHROME: &str = r"c:\program files\google\chrome\application\chrome.exe";
    const CRD: &str = "Chrome._crx_cmkncekebbplodngbpllndjkfo";

    #[test]
    fn web_apps_are_their_own_groups() {
        let browser = win(CHROME, Some("Chrome"));
        let app = win(CHROME, Some(CRD));
        let plain = win(CHROME, None);
        // Chrome pinned by its exe: its windows, not its web apps'.
        let by_exe = AppId { exe: Some(CHROME.into()), aumid: None };
        assert!(by_exe.matches(&browser) && by_exe.matches(&plain));
        assert!(!by_exe.matches(&app), "a web app is not Chrome's pin");
        // Chrome pinned by its shortcut (AUMID "Chrome").
        let by_link = AppId { exe: Some(CHROME.into()), aumid: Some("Chrome".into()) };
        assert!(by_link.matches(&browser) && !by_link.matches(&app));
        // The web app pinned by its shortcut (chrome_proxy.exe, its AUMID).
        let pinned = AppId { exe: Some(r"c:\program files\google\chrome\application\chrome_proxy.exe".into()), aumid: Some(CRD.into()) };
        assert!(pinned.matches(&app) && !pinned.matches(&browser) && !pinned.matches(&plain));
        // Two web apps of one browser stay apart.
        assert!(!pinned.matches(&win(CHROME, Some("Chrome._crx_cinhimbnkkghhklpknlkffjgod"))));
        // Other apps by exe as before.
        let other = AppId { exe: Some(r"c:\apps\a.exe".into()), aumid: None };
        assert!(other.matches(&win(r"c:\apps\a.exe", Some("Vendor.App"))));
        // ...also a pinned shortcut's AUMID against a window without one.
        let lnk = AppId { exe: Some(r"c:\apps\a.exe".into()), aumid: Some("Vendor.App".into()) };
        assert!(lnk.matches(&win(r"c:\apps\a.exe", None)));
    }

    /// The running group a web app's first window makes: the window's own
    /// exe (chrome.exe, not the shortcut's chrome_proxy.exe) and AUMID.
    fn group_of(w: &AppWindow) -> AppId {
        AppId { exe: w.exe.clone(), aumid: w.aumid.clone() }
    }

    #[test]
    fn a_web_apps_group_and_a_plain_browser_window_stay_apart_both_ways() {
        let app = win(CHROME, Some(CRD));
        let plain = win(CHROME, None);
        // The web app came first: a browser window without an AUMID (or
        // whose AUMID was not read this time) does not join it...
        assert!(!group_of(&app).matches(&plain));
        // ...and the other way round.
        assert!(!group_of(&plain).matches(&app));
        // The web app's own windows do.
        assert!(group_of(&app).matches(&win(CHROME, Some(&CRD.to_uppercase()))));
        // Two web apps and a profile of the one browser: four groups.
        let other = win(CHROME, Some("Chrome._crx_cinhimbnkkghhklpknlkffjgod"));
        let profile = win(CHROME, Some("Chrome.UserData.Profile1"));
        assert!(!group_of(&app).matches(&other) && !group_of(&other).matches(&app));
        assert!(!group_of(&profile).matches(&app) && !group_of(&app).matches(&profile));
        // Plain windows still join each other by exe.
        assert!(group_of(&plain).matches(&win(CHROME, None)));
    }

    /// The groups windows make in this order (as items.rs's
    /// sync_windows_inner: the first matching group, else a new one).
    fn grouped(wins: &[AppWindow]) -> Vec<(Option<String>, usize)> {
        let mut groups: Vec<(AppId, usize)> = Vec::new();
        for w in wins {
            match groups.iter_mut().find(|(id, _)| id.matches(w)) {
                Some((_, n)) => *n += 1,
                None => groups.push((group_of(w), 1)),
            }
        }
        let mut out: Vec<(Option<String>, usize)> = groups.into_iter().map(|(id, n)| (id.aumid.map(|a| a.to_lowercase()), n)).collect();
        out.sort();
        out
    }

    #[test]
    fn grouping_does_not_depend_on_which_window_came_first() {
        let wins = [
            win(CHROME, Some(CRD)),
            win(CHROME, None),
            win(CHROME, Some("Chrome._crx_cinhimbnkkghhklpknlkffjgod")),
            win(CHROME, Some(CRD)),
            win(CHROME, None),
        ];
        let forward = grouped(&wins);
        let mut rev = wins.to_vec();
        rev.reverse();
        assert_eq!(forward, grouped(&rev));
        let crd = Some(CRD.to_lowercase());
        assert!(forward.contains(&(crd, 2)) && forward.contains(&(None, 2)), "{forward:?}");
        assert_eq!(forward.len(), 3);
        // (A browser profile's window and a plain one still share a group by
        // exe, as before: no web app among them.)
    }

    #[test]
    fn a_pin_whose_shortcut_is_gone_is_still_its_web_app() {
        let web = crate::webapps::WebPin { aumid: CRD.into(), ..Default::default() };
        // (A path that does not exist: identify reads nothing from it.)
        let id = identify_pin(r"C:\nowhere\gone.lnk", Some(&web));
        assert_eq!(id.aumid.as_deref(), Some(CRD));
        assert!(id.matches(&win(CHROME, Some(CRD))) && !id.matches(&win(CHROME, None)));
        let id = identify_pin(&crate::webapps::apps_folder_path(CRD), None);
        assert!(id.matches(&win(CHROME, Some(CRD))) && id.is_web_app());
        assert_eq!(identify_pin(r"C:\nowhere\gone.lnk", None).aumid, None);
        let not_web = crate::webapps::WebPin { aumid: "Vendor.App".into(), ..Default::default() };
        assert_eq!(identify_pin(r"C:\nowhere\gone.lnk", Some(&not_web)).aumid, None);
    }

    #[test]
    fn a_pin_is_the_web_app_it_kept_not_a_shortcut_made_over_it() {
        let web = crate::webapps::WebPin { aumid: CRD.into(), ..Default::default() };
        let lnk = r"C:\SM\Chrome 앱\App.lnk";
        let read = |aumid: Option<&str>| AppId { exe: Some(CHROME.into()), aumid: aumid.map(String::from) };
        // Its own shortcut (any case): itself, fine.
        let (id, stale) = pin_identity(lnk, read(Some(&CRD.to_uppercase())), Some(&web));
        assert!(!stale && id.aumid.as_deref() == Some(CRD) && id.exe.as_deref() == Some(CHROME));
        // Another app's (another profile's) shortcut at that path now: still
        // the kept app, its windows only, and the shortcut is not its own.
        let b = "Chrome._crx_bbbb.UserData.Profile1";
        let (id, stale) = pin_identity(lnk, read(Some(b)), Some(&web));
        assert!(stale);
        assert!(id.matches(&win(CHROME, Some(CRD))) && !id.matches(&win(CHROME, Some(b))) && !id.matches(&win(CHROME, None)));
        // A plain program's shortcut there: not its own either.
        assert!(pin_identity(lnk, read(None), Some(&web)).1);
        // The shortcut gone (nothing read): who it kept, the path as it was.
        let (id, stale) = pin_identity(lnk, AppId::default(), Some(&web));
        assert!(!stale && id.aumid.as_deref() == Some(CRD));
        // Its AppsFolder path: never a shortcut to replace.
        assert!(!pin_identity(&crate::webapps::apps_folder_path(CRD), read(Some(CRD)), Some(&web)).1);
        // An older pin (nothing kept): what its path says, as before.
        let (id, stale) = pin_identity(lnk, read(Some(b)), None);
        assert!(!stale && id.aumid.as_deref() == Some(b));
    }

    #[test]
    fn web_apps_launch_from_their_shortcut() {
        let link = r"C:\U\Start Menu\Programs\Chrome 앱\Chrome 원격 데스크톱.lnk";
        assert_eq!(launch_path_with(&win(CHROME, Some(CRD)), Some(link.into())), link);
        // No shortcut found (yet): the app's AppsFolder path, never the
        // browser's exe.
        assert_eq!(launch_path_with(&win(CHROME, Some(CRD)), None), format!(r"shell:AppsFolder\{CRD}"));
        assert_eq!(launch_path_with(&win(CHROME, Some("Chrome")), None), CHROME);
        assert_eq!(launch_path_with(&win(CHROME, None), None), CHROME);
        let pkg = win(r"c:\program files\windowsapps\x\x.exe", Some("X_1!App"));
        assert_eq!(launch_path_with(&pkg, None), r"shell:AppsFolder\X_1!App");
    }

    /// Read-only: what browsers set on their windows, and what the dock
    /// makes of it.
    /// cargo test -p dock probe_browser_windows -- --ignored --nocapture
    #[test]
    #[ignore]
    fn probe_browser_windows() {
        use windows::Win32::Storage::EnhancedStorage::{
            PKEY_AppUserModel_RelaunchCommand, PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
        };
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let wins = enumerate(HWND::default());
        warm_names(&wins);
        for w in wins {
            let exe = w.exe.clone().unwrap_or_default();
            if !["chrome.exe", "msedge.exe", "brave.exe"].iter().any(|b| exe.ends_with(b)) {
                continue;
            }
            let s = unsafe { SHGetPropertyStoreForWindow::<IPropertyStore>(w.hwnd) };
            let get = |k| s.as_ref().ok().and_then(|s| unsafe { s.GetValue(k) }.ok()).map(|v| v.to_string()).unwrap_or_default();
            println!(
                "{exe}\n  aumid={:?}\n  relaunch icon={} name={} cmd={}\n  dock: name={} path={} web={:?}",
                w.aumid,
                get(&PKEY_AppUserModel_RelaunchIconResource),
                get(&PKEY_AppUserModel_RelaunchDisplayNameResource),
                get(&PKEY_AppUserModel_RelaunchCommand),
                display_name_cached(&w),
                launch_path(&w),
                w.aumid.as_deref().and_then(crate::webapps::cached),
            );
        }
    }
}

#[cfg(test)]
mod system_stub_tests {
    #[test]
    fn uac_interim_dialog_is_not_an_app() {
        assert!(super::is_system_stub_class("$$$Secure UAP Dummy Window Class For Interim Dialog"));
        assert!(!super::is_system_stub_class("GHOST_WindowClass"));
        assert!(!super::is_system_stub_class("UnityContainerWndClass"));
    }
}
