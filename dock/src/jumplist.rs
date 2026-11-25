//! Jump list tasks apps register for the taskbar (Chrome's "New window",
//! "New incognito window", recent pages; VS Code's "New window"...), read
//! from %APPDATA%\Microsoft\Windows\Recent\CustomDestinations\*.customDestinations-ms.
//!
//! The files are untrusted input:
//! - only shell link objects (CLSID_ShellLink) are ever created from them,
//! - a whole file must validate (header, every category footer) or nothing
//!   from it is used,
//! - size is capped, and parsing runs on a worker thread; the menu only
//!   reads the last finished snapshot (`tasks_for`), then asks for a fresh
//!   scan for next time (`refresh`).
//!
//! A file belongs to an app when its links launch that app's exe; only those
//! links are offered (anything else in the file is ignored). When several
//! files match, the most recently written one wins.
//!
//! Layout (per file): u32 version (2), u32 categories, u32 0; per category:
//! u32 kind (0 custom: u16 name length + UTF-16 name + u32 count + items;
//! 1 known: u32 id; 2 tasks: u32 count + items), then u32 0xBABFFBAB.
//! An item is a 16-byte CLSID followed by the object's IPersistStream data.

mod pinned;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use windows::core::{Interface, HSTRING};
use windows::Win32::Storage::EnhancedStorage::{PKEY_Link_Arguments, PKEY_Title};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistStream, IStream, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SHCreateMemStream, SHLoadIndirectString, ShellLink};

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    /// Packaged file activation (AppUserModelID, document path).
    pub packaged_file: Option<(String, String)>,
    pub title: String,
    pub path: String,
    pub args: String,
    /// The link's working directory ("" = none).
    pub dir: String,
    /// The category it is listed under ("" = the app's tasks).
    pub category: String,
}

const FOOTER: u32 = 0xBABF_FBAB;
/// CLSID_ShellLink {00021401-0000-0000-C000-000000000046}: the only class
/// we instantiate from these files.
const CLSID_SHELL_LINK: [u8; 16] = [0x01, 0x14, 0x02, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// Real files are a few KB to a few dozen KB.
const MAX_FILE: u64 = 512 * 1024;
const MAX_ARGS: usize = 8 * 1024;

/// Per file: (mtime, size) it was parsed at, and its tasks.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Known { Recent, Frequent }

#[derive(Clone, Debug, Default, PartialEq)]
struct CustomList { tasks: Vec<Task>, first_known: Option<Known> }

type FileCache = HashMap<PathBuf, ((SystemTime, u64), CustomList)>;

static SNAPSHOT: Mutex<Option<FileCache>> = Mutex::new(None);
static SCANNING: AtomicBool = AtomicBool::new(false);

fn dir() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|a| PathBuf::from(a).join("Microsoft\\Windows\\Recent\\CustomDestinations"))
}

/// Select a known AppID's own file even when it contains only a known
/// category marker. If that file is absent, keep the executable-based list.
fn custom_for(files: &FileCache, id: &crate::apps::AppId, native: bool) -> CustomList {
    let own = |t: &Task| id.exe.as_deref().is_some_and(|e| t.path.eq_ignore_ascii_case(e));
    let hash = id.aumid.as_deref().filter(|_| native).map(|a| format!("{:x}", pinned::app_hash(a)));
    let exact = hash.as_ref().and_then(|h| files.iter().find(|(file, _)| {
        file.file_stem().and_then(|f| f.to_str()).is_some_and(|f| f.eq_ignore_ascii_case(h))
    }));
    let selected = exact.or_else(|| files.iter().filter(|(_, (_, list))| list.tasks.iter().any(own))
        .max_by_key(|(_, ((mtime, _), _))| *mtime));
    let Some((_, (_, list))) = selected else { return CustomList::default() };
    CustomList { tasks: list.tasks.iter().filter(|t| own(t)).cloned().collect(), first_known: list.first_known }
}

pub fn tasks_for(id: &crate::apps::AppId, max_destinations: u32) -> Vec<Task> {
    let native = crate::taskbar::hidden_by_us();
    let list = SNAPSHOT.lock().ok().and_then(|g| g.as_ref().map(|files| custom_for(files, id, native))).unwrap_or_default();
    refresh();
    if !native { return list.tasks.into_iter().take(12).collect(); }
    with_documents(list.tasks, id, list.first_known, max_destinations)
}

pub fn max_destinations() -> u32 {
    use windows::core::w;
    let key = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");
    crate::syslook::dword(key, w!("JumpListItems_Maximum")).unwrap_or(12).min(100)
}

/// Keep tasks even when Windows suppresses document history.
fn limit_categories(tasks: &mut Vec<Task>, limit: u32) {
    let mut used = 0;
    tasks.retain(|t| {
        if t.category.is_empty() { return true; }
        used += 1;
        used <= limit
    });
}

fn with_documents(mut tasks: Vec<Task>, id: &crate::apps::AppId, known: Option<Known>, max_destinations: u32) -> Vec<Task> {
    use windows::core::w;
    let key = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");
    let limit = if crate::syslook::dword(key, w!("Start_TrackDocs")) == Some(0) { 0 } else { max_destinations };
    limit_categories(&mut tasks, limit);
    let Some(appid) = id.aumid.as_ref().filter(|s| !s.is_empty()) else { return tasks };
    let packaged = appid.contains('!');
    let custom = tasks.iter().any(|t| !t.category.is_empty());
    // A second opening can use a slow shell reply. Never reuse a reply
    // for a different app, language, privacy setting or category choice.
    #[derive(Clone, PartialEq)]
    struct Query { appid: String, exe: Option<String>, packaged: bool, limit: u32, max_destinations: u32, custom: bool, known: Option<Known>, language: &'static str }
    static LAST: Mutex<Option<(Query, std::time::Instant, Vec<Task>)>> = Mutex::new(None);
    let query = Query { appid: appid.clone(), exe: id.exe.clone(), packaged, limit, max_destinations, custom,
        known, language: crate::i18n::current() };
    let merge = |tasks: &mut Vec<Task>, docs: Vec<Task>| {
        tasks.retain(|t| t.category.is_empty() || !docs.iter().any(|d| same_destination(t, d)));
        tasks.extend(docs);
    };
    if let Some(docs) = LAST.lock().ok().and_then(|last| last.as_ref().filter(|(q, at, _)| *q == query && at.elapsed().as_secs() < 5)
        .map(|(_, _, d)| d.clone())) {
        merge(&mut tasks, docs);
        return tasks;
    }
    // A stalled shell extension must not stall the UI or spawn more workers.
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::AcqRel) { return tasks; }
    let (appid, exe) = (appid.clone(), id.exe.clone());
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if std::thread::Builder::new().name("jump-documents".into()).spawn(move || unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let docs = if initialized {
            let pinned_label = crate::i18n::t("dock.menu.pinned_group");
            let mut docs: Vec<_> = pinned::paths(&appid, max_destinations as usize).unwrap_or_default().iter()
                .filter_map(|path| file_task(&appid, exe.as_deref(), packaged, path, pinned_label)).collect();
            if !custom && limit > 0 {
                for task in read_documents(&appid, exe.as_deref(), packaged, limit, known) {
                    if !docs.iter().any(|t| same_destination(t, &task)) { docs.push(task); }
                }
            }
            windows::Win32::System::Com::CoUninitialize();
            docs
        } else { Vec::new() };
        if let Ok(mut last) = LAST.lock() { *last = Some((query, std::time::Instant::now(), docs.clone())); }
        BUSY.store(false, Ordering::Release);
        let _ = tx.send(docs);
    }).is_err() { BUSY.store(false, Ordering::Release); return tasks; }
    if let Ok(docs) = rx.recv_timeout(std::time::Duration::from_millis(180)) { merge(&mut tasks, docs); }
    tasks
}

fn same_destination(a: &Task, b: &Task) -> bool {
    a.path.eq_ignore_ascii_case(&b.path) && a.args.eq_ignore_ascii_case(&b.args)
}

fn file_task(appid: &str, exe: Option<&str>, packaged: bool, file: &str, category: &str) -> Option<Task> {
    let path = std::path::Path::new(file);
    if !path.is_absolute() || crate::webapps::is_web_app(appid) || (!packaged && exe.is_none()) { return None; }
    Some(Task { packaged_file: packaged.then(|| (appid.into(), file.into())), title: path.file_name()?.to_str()?.into(),
        path: exe.unwrap_or_default().into(), args: crate::launchers::quote_arg(file), dir: String::new(), category: category.into() })
}

unsafe fn read_documents(appid: &str, exe: Option<&str>, packaged: bool, limit: u32, known: Option<Known>) -> Vec<Task> {
    use windows::Win32::UI::Shell::{ApplicationDocumentLists, IApplicationDocumentLists, Common::IObjectArray,
        ADLT_FREQUENT, ADLT_RECENT, IShellItem, SIGDN_FILESYSPATH};
    let mut docs = Vec::new();
    let Ok(lists): windows::core::Result<IApplicationDocumentLists> = CoCreateInstance(&ApplicationDocumentLists, None, CLSCTX_INPROC_SERVER) else { return docs };
    if lists.SetAppID(&HSTRING::from(appid)).is_err() { return docs; }
    for (kind, label) in [(ADLT_RECENT, "search.group.recent_files"), (ADLT_FREQUENT, "dock.menu.frequent")] {
        if matches!(known, Some(Known::Frequent)) && kind != ADLT_FREQUENT { continue; }
        if matches!(known, Some(Known::Recent)) && kind != ADLT_RECENT { continue; }
        let Ok(array): windows::core::Result<IObjectArray> = lists.GetList(kind, limit) else { continue };
        let category = crate::i18n::t(label);
        for i in 0..array.GetCount().unwrap_or(0).min(limit) {
            let file = if let Ok(item) = array.GetAt::<IShellItem>(i) {
                let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) else { continue };
                let path = name.to_string().ok();
                windows::Win32::System::Com::CoTaskMemFree(Some(name.0.cast()));
                let Some(path) = path else { continue };
                path
            } else if let Ok(link) = array.GetAt::<IShellLinkW>(i) {
                if let Some(t) = task_of(&link, category).filter(|t| exe.is_some_and(|e| e.eq_ignore_ascii_case(&t.path))) {
                    docs.push(t);
                    continue;
                }
                let mut buf = [0u16; 1024];
                if link.GetPath(&mut buf, std::ptr::null_mut(), 0).is_err() { continue; }
                wstr(&buf)
            } else { continue };
            docs.extend(file_task(appid, exe, packaged, &file, category));
        }
        if !docs.is_empty() { break; } // Recent first, Frequent when Recent is empty.
    }
    docs
}

pub unsafe fn open_packaged_file(appid: &str, path: &str) -> windows::core::Result<()> {
    use windows::Win32::UI::Shell::{ApplicationActivationManager, IApplicationActivationManager, IShellItem, IShellItemArray,
        SHCreateItemFromParsingName, SHCreateShellItemArrayFromShellItem};
    let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None)?;
    let items: IShellItemArray = SHCreateShellItemArrayFromShellItem(&item)?;
    let manager: IApplicationActivationManager = CoCreateInstance(&ApplicationActivationManager, None, windows::Win32::System::Com::CLSCTX_LOCAL_SERVER)?;
    let _ = windows::Win32::System::Com::CoAllowSetForegroundWindow(&manager, None);
    let pid = manager.ActivateForFile(&HSTRING::from(appid), &items, windows::core::w!("open"))?;
    if pid != 0 {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
    }
    Ok(())
}

/// Rescans in the background (one scan at a time). Unchanged files reuse
/// their last result; failures are not remembered (retried next time).
pub fn refresh() {
    if SCANNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new().name("jumplist".into()).spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let old = SNAPSHOT.lock().ok().and_then(|g| g.clone()).unwrap_or_default();
        let mut fresh: FileCache = HashMap::new();
        if let Some(d) = dir() {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) != Some("customDestinations-ms") {
                    continue;
                }
                let Ok(meta) = e.metadata() else { continue };
                let key = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
                if key.1 > MAX_FILE {
                    continue;
                }
                if let Some((k, tasks)) = old.get(&path).filter(|(k, _)| *k == key) {
                    fresh.insert(path, (*k, tasks.clone()));
                    continue;
                }
                if let Some(bytes) = read_capped(&path) {
                    if let Ok(tasks) = unsafe { parse(&bytes) } {
                        fresh.insert(path, (key, tasks));
                    }
                }
            }
        }
        if let Ok(mut g) = SNAPSHOT.lock() {
            *g = Some(fresh);
        }
        SCANNING.store(false, Ordering::SeqCst);
    });
}

/// The file's bytes, refusing anything over the cap (also if it grew after
/// the size check).
fn read_capped(path: &PathBuf) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_FILE + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= MAX_FILE).then_some(buf)
}

#[derive(Debug, PartialEq)]
pub struct Bad;

/// Parses one file; any inconsistency rejects the whole file.
unsafe fn parse(bytes: &[u8]) -> Result<CustomList, Bad> {
    let stream = SHCreateMemStream(Some(bytes)).ok_or(Bad)?;
    let read_exact = |s: &IStream, n: usize| -> Result<Vec<u8>, Bad> {
        let mut b = vec![0u8; n];
        let mut got = 0u32;
        s.Read(b.as_mut_ptr() as *mut _, n as u32, Some(&mut got)).ok().map_err(|_| Bad)?;
        if got as usize == n { Ok(b) } else { Err(Bad) }
    };
    let read_u32 = |s: &IStream| -> Result<u32, Bad> {
        let b = read_exact(s, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if read_u32(&stream)? != 2 {
        return Err(Bad);
    }
    let cats = read_u32(&stream)?;
    if read_u32(&stream)? != 0 || cats > 16 {
        return Err(Bad);
    }
    let mut out = CustomList::default();
    for _ in 0..cats {
        let kind = read_u32(&stream)?;
        let (category, count) = match kind {
            0 => {
                let lb = read_exact(&stream, 2)?;
                let len = u16::from_le_bytes([lb[0], lb[1]]) as usize;
                if len > 260 {
                    return Err(Bad);
                }
                let nb = read_exact(&stream, len * 2)?;
                let units: Vec<u16> = nb.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                (String::from_utf16_lossy(&units), read_u32(&stream)?)
            }
            // A known category (recent / frequent): no items here.
            1 => {
                let known = match read_u32(&stream)? { 1 => Known::Frequent, 2 => Known::Recent, _ => return Err(Bad) };
                out.first_known.get_or_insert(known);
                (String::new(), 0)
            }
            2 => (String::new(), read_u32(&stream)?),
            _ => return Err(Bad),
        };
        if count > 64 {
            return Err(Bad);
        }
        let mut cat = Vec::new();
        for _ in 0..count {
            if read_exact(&stream, 16)? != CLSID_SHELL_LINK {
                // Only shell links are ever instantiated.
                return Err(Bad);
            }
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|_| Bad)?;
            link.cast::<IPersistStream>().map_err(|_| Bad)?.Load(&stream).map_err(|_| Bad)?;
            if let Some(t) = task_of(&link, &category) {
                cat.push(t);
            }
        }
        // Only a category that ends correctly counts.
        if read_u32(&stream)? != FOOTER {
            return Err(Bad);
        }
        out.tasks.extend(cat);
    }
    Ok(out)
}

fn wstr(buf: &[u16]) -> String {
    String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())])
}

/// "@C:\x.dll,-123" -> the localized string; plain text as is.
unsafe fn resolve(s: &str) -> Option<String> {
    if !s.starts_with('@') {
        return (!s.is_empty()).then(|| s.to_string());
    }
    let mut out = [0u16; 512];
    SHLoadIndirectString(&HSTRING::from(s), &mut out, None).ok()?;
    let r = wstr(&out);
    (!r.is_empty()).then_some(r)
}

unsafe fn task_of(link: &IShellLinkW, category: &str) -> Option<Task> {
    let mut buf = [0u16; 1024];
    link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
    let path = wstr(&buf);
    if path.is_empty() {
        return None;
    }
    let props = link.cast::<IPropertyStore>().ok();
    // Full arguments from the property store (GetArguments may truncate);
    // anything unreadable or too long is not offered.
    let args = match props.as_ref().and_then(|p| p.GetValue(&PKEY_Link_Arguments).ok()) {
        Some(v) => v.to_string(),
        None => {
            let mut a = vec![0u16; MAX_ARGS + 1];
            link.GetArguments(&mut a).ok()?;
            let s = wstr(&a);
            if s.encode_utf16().count() >= MAX_ARGS {
                return None;
            }
            s
        }
    };
    if args.len() > MAX_ARGS {
        return None;
    }
    let mut d = [0u16; 1024];
    let _ = link.GetWorkingDirectory(&mut d);
    let dir = wstr(&d);
    // Tasks keep their visible name in System.Title (maybe "@res,-id").
    let title = props
        .and_then(|p| p.GetValue(&PKEY_Title).ok())
        .and_then(|v| resolve(&v.to_string()))
        .or_else(|| {
            let mut desc = [0u16; 512];
            link.GetDescription(&mut desc).ok()?;
            resolve(&wstr(&desc))
        })?;
    Some(Task { packaged_file: None, title, path, args, dir, category: category.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn junk_and_empty_files_are_rejected() {
        unsafe {
            assert_eq!(parse(&[]), Err(Bad));
            assert_eq!(parse(&[1, 2, 3]), Err(Bad));
            // Reserved header field must be 0.
            assert_eq!(parse(&le(&[2, 1, 7, 1, 1, FOOTER])), Err(Bad));
            // Missing footer.
            assert_eq!(parse(&le(&[2, 1, 0, 1, 1, 0x1234])), Err(Bad));
            // A good file with one known category: no tasks, but valid.
            assert_eq!(parse(&le(&[2, 1, 0, 1, 1, FOOTER])), Ok(CustomList { tasks: Vec::new(), first_known: Some(Known::Frequent) }));
        }
    }

    #[test]
    fn missing_appid_file_keeps_the_newest_matching_exe_list() {
        let id = crate::apps::AppId { exe: Some("app.exe".into()), aumid: Some("Vendor.App".into()) };
        let mut files = FileCache::new();
        for (file, exe, title, age) in [("old", "APP.exe", "Old", 1), ("other", "other.exe", "Other", 3), ("new", "app.exe", "New", 2)] {
            let task = Task { title: title.into(), category: String::new(), path: exe.into(), args: String::new(),
                dir: String::new(), packaged_file: None };
            files.insert(format!("{file}.customDestinations-ms").into(),
                ((SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(age), 0), CustomList { tasks: vec![task], first_known: None }));
        }
        let visible = custom_for(&files, &id, false);
        assert_eq!(visible.tasks[0].title, "New");
        assert_eq!(custom_for(&files, &id, true), visible);
        let exact = CustomList { tasks: Vec::new(), first_known: Some(Known::Frequent) };
        files.insert(format!("{:x}.customDestinations-ms", pinned::app_hash("Vendor.App")).into(),
            ((SystemTime::UNIX_EPOCH, 0), exact.clone()));
        assert_eq!(custom_for(&files, &id, true), exact);
    }

    #[test]
    fn document_limit_keeps_tasks_and_respects_privacy() {
        let row = |title: &str, category: &str| Task { title: title.into(), category: category.into(), path: "app.exe".into(), args: String::new(), dir: String::new(), packaged_file: None };
        let all = vec![row("A", "Recent"), row("B", "Recent"), row("Open", "")];
        let mut one = all.clone();
        limit_categories(&mut one, 1);
        assert_eq!(one, vec![all[0].clone(), all[2].clone()]);
        limit_categories(&mut one, 0);
        assert_eq!(one, vec![all[2].clone()]);
    }

    #[test]
    fn only_shell_links_are_created() {
        unsafe {
            // One task whose CLSID is not CLSID_ShellLink: rejected before
            // anything is instantiated.
            let mut b = le(&[2, 1, 0, 2, 1]);
            b.extend([0xAAu8; 16]);
            assert_eq!(parse(&b), Err(Bad));
        }
    }
}
