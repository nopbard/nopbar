//! Stacks: folders pinned to the dock. This part lists a folder (newest
//! first), prepares small icons and tells folders from files by their real
//! shell attributes. All of it runs on short-lived helper threads, never on
//! the UI thread, under a deadline: a folder or shell call that hangs (a
//! dead network share, a slow shell extension) is abandoned, never waited
//! for, and never holds up the next request. The popup lives in
//! dock/stack.rs.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Items shown in a stack.
pub const LIMIT: usize = 20;
/// Up to this many items show as a grid; more as a list.
pub const GRID_MAX: usize = 12;
/// Time a listing may take in all (enumeration + icons); the dock gives
/// up showing it a little after this.
pub const BUDGET: Duration = Duration::from_millis(2300);
/// Of that, the enumeration may take this long; the rest is for icons.
const LIST_SHARE: Duration = Duration::from_millis(1600);
/// Helper threads that may be alive at once (stuck ones included). Beyond
/// this, new work is refused at once instead of piling up threads.
const MAX_LIVE: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// What is shown (file name; shortcuts without ".lnk"/".url").
    pub name: String,
    pub path: String,
    /// Newest of created / modified (seconds since 1970): "date added".
    pub time: u64,
    pub dir: bool,
}

/// A listed folder with icons (premultiplied BGRA, `px` x `px`).
#[derive(Clone)]
pub struct Listing {
    pub seq: u64,
    pub folder: String,
    pub entries: Vec<Entry>,
    pub icons: Vec<Option<(u32, Vec<u8>)>>,
    /// Show as a grid (else a list).
    pub grid: bool,
    /// The folder could be read.
    pub ok: bool,
    /// The enumeration stopped at the deadline: these are the newest of
    /// what was seen, not necessarily of the whole folder.
    pub partial: bool,
}

impl Listing {
    /// Nothing could be shown (unreadable, too slow, too busy).
    pub fn failed(seq: u64, folder: &str) -> Self {
        Self { seq, folder: folder.to_string(), entries: Vec::new(), icons: Vec::new(), grid: false, ok: false, partial: false }
    }
}

// ---- pure helpers ---------------------------------------------------------------

/// A path that may name a folder on disk (drive or share root); shell
/// names and AppsFolder ids never do. Whether it really is one is asked
/// with `check_kinds` (never decided from the extension).
pub fn could_be_folder(path: &str) -> bool {
    let p = path.trim();
    let b = p.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\') || (p.starts_with("\\\\") && !p.starts_with("\\\\?\\") && !p.starts_with("\\\\.\\"))
}

/// Shown name of an entry.
pub fn display_name(file_name: &str) -> String {
    let l = file_name.to_lowercase();
    for ext in [".lnk", ".url"] {
        if l.ends_with(ext) && file_name.len() > ext.len() {
            return file_name[..file_name.len() - ext.len()].to_string();
        }
    }
    file_name.to_string()
}

/// Newest first (ties by name, ignoring case), at most `limit`.
pub fn newest_first(mut v: Vec<Entry>, limit: usize) -> Vec<Entry> {
    v.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    v.truncate(limit);
    v
}

/// Rank of an entry: bigger is "newer" (ties: the name first in order).
type Rank = (u64, Reverse<String>);

fn rank(e: &Entry) -> Rank {
    (e.time, Reverse(e.name.to_lowercase()))
}

/// The newest `limit` entries seen so far, in O(limit) memory however big
/// the folder is: a min-heap whose top is the oldest kept entry.
pub struct TopN {
    limit: usize,
    heap: BinaryHeap<Reverse<(Rank, usize)>>,
    store: Vec<Option<Entry>>,
}

impl TopN {
    pub fn new(limit: usize) -> Self {
        Self { limit, heap: BinaryHeap::new(), store: Vec::new() }
    }

    pub fn push(&mut self, e: Entry) {
        if self.limit == 0 {
            return;
        }
        let r = rank(&e);
        if self.heap.len() >= self.limit {
            match self.heap.peek() {
                Some(Reverse((worst, _))) if r > *worst => {}
                _ => return,
            }
            if let Some(Reverse((_, slot))) = self.heap.pop() {
                self.store[slot] = Some(e);
                self.heap.push(Reverse((r, slot)));
            }
            return;
        }
        self.store.push(Some(e));
        self.heap.push(Reverse((r, self.store.len() - 1)));
    }

    /// Newest first.
    pub fn into_sorted(self) -> Vec<Entry> {
        let limit = self.limit;
        newest_first(self.store.into_iter().flatten().collect(), limit)
    }
}

/// `src` (w x h premultiplied BGRA) scaled to fit a `px` square, centered,
/// by area averaging (premultiplied, so edges stay clean).
pub fn fit_square(src: &[u8], w: u32, h: u32, px: u32) -> Vec<u8> {
    let mut out = vec![0u8; px as usize * px as usize * 4];
    if w == 0 || h == 0 || px == 0 || crate::icons::icon_bytes(w as i64, h as i64).is_none_or(|n| src.len() < n) {
        return out;
    }
    let s = (px as f32 / w as f32).min(px as f32 / h as f32);
    let (dw, dh) = (((w as f32 * s).round() as u32).clamp(1, px), ((h as f32 * s).round() as u32).clamp(1, px));
    let (ox, oy) = ((px - dw) / 2, (px - dh) / 2);
    for y in 0..dh {
        let sy0 = (y as f32 / dh as f32 * h as f32) as u32;
        let sy1 = (((y + 1) as f32 / dh as f32 * h as f32).ceil() as u32).clamp(sy0 + 1, h);
        for x in 0..dw {
            let sx0 = (x as f32 / dw as f32 * w as f32) as u32;
            let sx1 = (((x + 1) as f32 / dw as f32 * w as f32).ceil() as u32).clamp(sx0 + 1, w);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let i = ((sy * w + sx) * 4) as usize;
                    for k in 0..4 {
                        acc[k] += src[i + k] as u32;
                    }
                    n += 1;
                }
            }
            let o = (((y + oy) * px + x + ox) * 4) as usize;
            for k in 0..4 {
                out[o + k] = (acc[k] / n.max(1)) as u8;
            }
        }
    }
    out
}

// ---- dragging out of / dropping into a stack (pure) ---------------------------------

/// Drop effects (DROPEFFECT_COPY / _MOVE / _LINK).
pub const EFFECT_COPY: u32 = 1;
pub const EFFECT_MOVE: u32 = 2;
pub const EFFECT_LINK: u32 = 4;
/// Key state bits of a drag (MK_SHIFT, MK_CONTROL, MK_ALT).
pub const KEY_SHIFT: u32 = 0x4;
pub const KEY_CTRL: u32 = 0x8;
pub const KEY_ALT: u32 = 0x20;
/// The drag was made with the right button (MK_RBUTTON). The button is up
/// by the time of the drop, so targets remember it from DragEnter.
pub const KEY_RIGHT: u32 = 0x2;

/// The pointer moved far enough from the press (dx, dy px) to be a drag,
/// for the system drag box `cx` x `cy` px. Less is still a click.
pub fn past_drag_threshold(dx: i32, dy: i32, cx: i32, cy: i32) -> bool {
    dx.abs() >= cx.max(1) || dy.abs() >= cy.max(1)
}

/// A path compared ignoring case and trailing backslashes ("C:\" -> "c:").
fn norm(p: &str) -> String {
    p.trim().trim_end_matches('\\').to_lowercase()
}

/// The volume a path is on, from its text alone (no disk access): "c:" or
/// "\\server\share". None for anything else.
pub fn volume_of(path: &str) -> Option<String> {
    let n = norm(path);
    let b = n.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\') {
        return Some(n[..2].to_string());
    }
    let rest = n.strip_prefix("\\\\")?;
    if rest.starts_with("?\\") || rest.starts_with(".\\") {
        return None;
    }
    let mut parts = rest.split('\\');
    let server = parts.next().filter(|s| !s.is_empty())?;
    let share = parts.next().filter(|s| !s.is_empty())?;
    Some(format!("\\\\{server}\\{share}"))
}

/// The folder a path is in (normalized), None for a root.
fn parent_of(path: &str) -> Option<String> {
    let n = norm(path);
    let i = n.rfind('\\')?;
    let p = &n[..i];
    // "\\server\share" has no parent we could drop into.
    (!p.is_empty() && p != "\\").then(|| p.to_string())
}

/// Dropping `item` into `target` would put a folder into itself or into
/// one of its own subfolders.
pub fn into_itself(item: &str, target: &str) -> bool {
    let (i, t) = (norm(item), norm(target));
    !i.is_empty() && (t == i || t.starts_with(&format!("{i}\\")))
}

/// What a drop into a folder does, like File Explorer: Ctrl+Shift or Alt
/// makes shortcuts, Ctrl copies, Shift moves; with no keys, the same volume
/// moves and another volume copies. A right-button drag (Explorer would
/// ask) always copies unless a shortcut is asked for: nothing is ever
/// moved without the user choosing it. Narrowed to what the source allows
/// (a key the source does not allow means nothing happens; the default
/// falls back to copy, then move, then a shortcut).
pub fn choose_effect(keys: u32, same_volume: bool, source: u32) -> u32 {
    let (ctrl, shift, alt) = (keys & KEY_CTRL != 0, keys & KEY_SHIFT != 0, keys & KEY_ALT != 0);
    let forced = if (ctrl && shift) || alt {
        Some(EFFECT_LINK)
    } else if ctrl || keys & KEY_RIGHT != 0 {
        Some(EFFECT_COPY)
    } else if shift {
        Some(EFFECT_MOVE)
    } else {
        None
    };
    if let Some(f) = forced {
        return if source & f != 0 { f } else { 0 };
    }
    let first = if same_volume { EFFECT_MOVE } else { EFFECT_COPY };
    [first, EFFECT_COPY, EFFECT_MOVE, EFFECT_LINK].into_iter().find(|e| source & e != 0).unwrap_or(0)
}

/// The effect of dropping `files` into the stack folder `target` (0: not
/// allowed). Pure: decided from the paths' text, never the disk, so a slow
/// share cannot stall the drag.
pub fn drop_into(keys: u32, files: &[String], target: &str, source: u32) -> u32 {
    if files.is_empty() || norm(target).is_empty() {
        return 0;
    }
    // A folder into itself or its own child: never.
    if files.iter().any(|f| into_itself(f, target)) {
        return 0;
    }
    let tv = volume_of(target);
    let same = tv.is_some() && files.iter().all(|f| volume_of(f) == tv);
    let e = choose_effect(keys, same, source);
    // Moving things to where they already are does nothing.
    let t = norm(target);
    if e == EFFECT_MOVE && files.iter().all(|f| parent_of(f).as_deref() == Some(t.as_str())) {
        return 0;
    }
    e
}

// ---- moving files into a stack (own thread) ----------------------------------------

/// Moves, copies or links `files` into `folder` with Windows' own copy
/// engine (IFileOperation: conflict questions, progress, undo and
/// elevation are all Windows'), on a thread of its own (STA). `done` gets
/// whether everything went through (false: failed, refused or cancelled).
pub fn spawn_transfer(files: Vec<String>, folder: String, effect: u32, done: impl FnOnce(bool) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("stack-drop".into()).spawn(move || {
        let ok = unsafe {
            let com = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            let r = transfer(&files, &folder, effect);
            if com.is_ok() {
                windows::Win32::System::Com::CoUninitialize();
            }
            r
        };
        let ok = match ok {
            Ok(ok) => ok,
            Err(e) => {
                crate::log!("stack drop: {} item(s) into {folder:?} failed: {e}", files.len());
                false
            }
        };
        crate::log!("stack drop: effect {effect}, {} item(s) into {folder:?}, ok {ok}", files.len());
        done(ok);
    });
    if spawned.is_err() {
        crate::log!("stack drop: no thread; nothing done");
    }
}

unsafe fn transfer(files: &[String], folder: &str, effect: u32) -> windows::core::Result<bool> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    use windows::Win32::UI::Shell::{
        FileOperation, IFileOperation, IFileOperationProgressSink, IShellItem, SHCreateItemFromParsingName, FILEOPERATION_FLAGS, FOFX_ADDUNDORECORD,
        FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR,
    };
    let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
    op.SetOperationFlags(FILEOPERATION_FLAGS(FOF_ALLOWUNDO.0 | FOF_NOCONFIRMMKDIR.0 | FOFX_ADDUNDORECORD.0))?;
    let dest: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(folder), None)?;
    let no_sink = None::<&IFileOperationProgressSink>;
    let mut temp: Option<std::path::PathBuf> = None;
    let mut names: Vec<String> = Vec::new();
    let mut queued = 0;
    for f in files {
        // Gone since the drag began: skip it, do the rest.
        let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(f.as_str()), None) else {
            crate::log!("stack drop: {f:?} is gone; skipped");
            continue;
        };
        let r = match effect {
            EFFECT_MOVE => op.MoveItem(&item, &dest, PCWSTR::null(), no_sink),
            EFFECT_COPY => op.CopyItem(&item, &dest, PCWSTR::null(), no_sink),
            _ => queue_link(&op, &item, f, folder, &dest, &mut temp, &mut names),
        };
        match r {
            Ok(()) => queued += 1,
            Err(e) => crate::log!("stack drop: cannot queue {f:?}: {e}"),
        }
    }
    let r = if queued > 0 { op.PerformOperations() } else { Ok(()) };
    let aborted = queued == 0 || op.GetAnyOperationsAborted().map(|b| b.as_bool()).unwrap_or(true);
    // Our own scratch folder for new shortcuts (never the user's files).
    if let Some(t) = temp {
        let _ = std::fs::remove_dir_all(t);
    }
    r?;
    Ok(!aborted && queued == files.len())
}

/// A shortcut to `file` in `folder`, named the way Explorer would ("X -
/// Shortcut"): written to our scratch folder, then moved in by the copy
/// engine (so a read-only or protected folder is reported, or elevated, by
/// Windows like any other drop).
unsafe fn queue_link(
    op: &windows::Win32::UI::Shell::IFileOperation,
    item: &windows::Win32::UI::Shell::IShellItem,
    file: &str,
    folder: &str,
    dest: &windows::Win32::UI::Shell::IShellItem,
    temp: &mut Option<std::path::PathBuf>,
    names: &mut Vec<String>,
) -> windows::core::Result<()> {
    use windows::core::{Interface, BOOL, HSTRING, PCWSTR};
    use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{IFileOperationProgressSink, IShellItem, IShellLinkW, SHCreateItemFromParsingName, SHGetNewLinkInfoW, ShellLink};
    static N: AtomicU64 = AtomicU64::new(0);
    let no_sink = None::<&IFileOperationProgressSink>;
    let mut name = [0u16; 260];
    let mut must_copy = BOOL(0);
    if !SHGetNewLinkInfoW(&HSTRING::from(file), &HSTRING::from(folder), &mut name, &mut must_copy, 0).as_bool() {
        return Err(windows::Win32::Foundation::E_FAIL.into());
    }
    // A shortcut to a shortcut is a copy of it.
    if must_copy.as_bool() {
        return op.CopyItem(item, dest, PCWSTR::null(), no_sink);
    }
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let full = String::from_utf16_lossy(&name[..end]);
    let Some(proposed) = std::path::Path::new(&full).file_name().map(|s| s.to_string_lossy().to_string()) else {
        return Err(windows::Win32::Foundation::E_FAIL.into());
    };
    // The shell only knows what is in the folder already, not the other
    // shortcuts of this drop: two "report.txt" from different folders must
    // not get one name.
    let file_name = unique_name(&proposed, names);
    names.push(file_name.to_lowercase());
    // Each shortcut in its own scratch folder: nothing is ever overwritten
    // there; a clash in the target is the copy engine's to ask about.
    let root = temp
        .get_or_insert_with(|| std::env::temp_dir().join(format!("nopbar-links-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst))))
        .clone();
    let dir = root.join(names.len().to_string());
    std::fs::create_dir_all(&dir).map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
    let lnk = dir.join(file_name);
    let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
    sl.SetPath(&HSTRING::from(file))?;
    sl.cast::<IPersistFile>()?.Save(&HSTRING::from(lnk.as_os_str()), true)?;
    let li: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(lnk.as_os_str()), None)?;
    op.MoveItem(&li, dest, PCWSTR::null(), no_sink)
}

/// `name` ("X - Shortcut.lnk"), or "X - Shortcut (2).lnk" and up if this
/// drop already uses it (`used`: lowercase names).
pub fn unique_name(name: &str, used: &[String]) -> String {
    if !used.contains(&name.to_lowercase()) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|c| !used.contains(&c.to_lowercase()))
        .unwrap_or_else(|| name.to_string())
}

// ---- listing (helper threads) -----------------------------------------------------

/// Lists `folder` until `deadline` or until `stop()` says so: the newest
/// LIMIT visible entries of everything enumerated, and whether it stopped
/// early. None if the folder cannot be read (or it was stopped).
pub fn list(folder: &str, deadline: Instant, stop: &dyn Fn() -> bool) -> Option<(Vec<Entry>, bool)> {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    let rd = std::fs::read_dir(folder).ok()?;
    let mut top = TopN::new(LIMIT);
    let mut partial = false;
    for e in rd {
        if stop() {
            return None;
        }
        if Instant::now() >= deadline {
            partial = true;
            break;
        }
        let Ok(e) = e else { continue };
        // From the directory listing itself (no extra file opens).
        let Ok(md) = e.metadata() else { continue };
        if md.file_attributes() & (HIDDEN | SYSTEM) != 0 {
            continue;
        }
        let file = e.file_name().to_string_lossy().to_string();
        if file.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let secs = |t: std::io::Result<std::time::SystemTime>| t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        let time = secs(md.created()).max(secs(md.modified()));
        top.push(Entry { name: display_name(&file), path: e.path().to_string_lossy().to_string(), time, dir: md.is_dir() });
    }
    Some((top.into_sorted(), partial))
}

/// The listing request that counts (newer ones make older ones stop).
static CURRENT: AtomicU64 = AtomicU64::new(0);
/// Helper threads alive (listings and attribute checks).
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// Runs `f` on a new helper thread with STA COM, unless MAX_LIVE are
/// alive already (some may be stuck in a call that never returns; they are
/// abandoned, not waited for). False if it was refused.
fn helper(name: &str, f: impl FnOnce() + Send + 'static) -> bool {
    if LIVE.fetch_add(1, Ordering::SeqCst) >= MAX_LIVE {
        LIVE.fetch_sub(1, Ordering::SeqCst);
        crate::log!("stacks: {MAX_LIVE} helpers still busy; {name} refused");
        return false;
    }
    let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        f();
        LIVE.fetch_sub(1, Ordering::SeqCst);
    });
    if spawned.is_err() {
        LIVE.fetch_sub(1, Ordering::SeqCst);
        return false;
    }
    true
}

/// Lists `folder` with icons on its own thread and hands the result to
/// `done` (unless a newer request came first: then the work stops early
/// and nothing is sent). Refused (too many stuck helpers): `done` gets a
/// failed listing at once.
pub fn spawn_listing(seq: u64, folder: String, grid_px: u32, list_px: u32, done: impl FnOnce(Listing) + Send + 'static) {
    CURRENT.store(seq, Ordering::SeqCst);
    let deadline = Instant::now() + BUDGET;
    // `done` is needed on both paths: share it through an Option.
    let done = std::sync::Arc::new(Mutex::new(Some(done)));
    let d2 = done.clone();
    let f2 = folder.clone();
    let ok = helper("stack-list", move || {
        let stop = || CURRENT.load(Ordering::SeqCst) != seq;
        let l = build(seq, &f2, grid_px, list_px, deadline, &stop);
        if !stop() {
            if let Some(d) = d2.lock().ok().and_then(|mut g| g.take()) {
                d(l);
            }
        }
    });
    if !ok {
        if let Some(d) = done.lock().ok().and_then(|mut g| g.take()) {
            d(Listing::failed(seq, &folder));
        }
    }
}

/// Lists `folder` and loads icons, all before `deadline` (icons not loaded
/// by then are left out). The caller's thread must have COM.
pub fn build(seq: u64, folder: &str, grid_px: u32, list_px: u32, deadline: Instant, stop: &dyn Fn() -> bool) -> Listing {
    let list_deadline = deadline.checked_sub(BUDGET - LIST_SHARE).unwrap_or(deadline).min(deadline);
    let Some((entries, partial)) = list(folder, list_deadline, stop) else {
        return Listing::failed(seq, folder);
    };
    let grid = entries.len() <= GRID_MAX;
    let px = if grid { grid_px } else { list_px }.clamp(8, 256);
    let icons = entries
        .iter()
        .map(|e| {
            if stop() || Instant::now() >= deadline {
                return None;
            }
            crate::icons::load(&e.path, px as i32).map(|i| (px, fit_square(&i.pixels, i.width, i.height, px)))
        })
        .collect();
    Listing { seq, folder: folder.to_string(), entries, icons, grid, ok: true, partial }
}

// ---- folder or not (real attributes, cached) --------------------------------------

/// path (lowercase) -> (is a folder, shell display name).
fn kinds() -> &'static Mutex<HashMap<String, (bool, String)>> {
    static K: OnceLock<Mutex<HashMap<String, (bool, String)>>> = OnceLock::new();
    K.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What a check found for `path`: Some(true) for a folder, None if not
/// checked (yet).
pub fn known_kind(path: &str) -> Option<bool> {
    kinds().lock().ok()?.get(&path.to_lowercase()).map(|k| k.0)
}

/// The shell's name for a checked folder ("Downloads" shows as "다운로드"
/// on a Korean system).
pub fn known_name(path: &str) -> Option<String> {
    kinds().lock().ok()?.get(&path.to_lowercase()).map(|k| k.1.clone()).filter(|n| !n.is_empty())
}

/// Record a result found elsewhere (e.g. a folder an Explorer window shows).
pub fn remember(path: &str, folder: bool, name: &str) {
    if let Ok(mut k) = kinds().lock() {
        if k.len() > 512 {
            k.clear();
        }
        k.insert(path.to_lowercase(), (folder, name.to_string()));
    }
}

/// Last path part, for when the shell has no name.
pub fn last_part(path: &str) -> String {
    let t = path.trim_end_matches('\\');
    t.rsplit('\\').next().filter(|s| !s.is_empty()).unwrap_or(t).to_string()
}

/// A real file-system folder (not a .zip "folder"), by its shell
/// attributes, with the shell's display name. Blocks: helper threads only.
unsafe fn read_kind(path: &str) -> Option<(bool, String)> {
    use windows::core::HSTRING;
    use windows::Win32::System::SystemServices::{SFGAO_FILESYSTEM, SFGAO_FOLDER, SFGAO_STREAM};
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY};
    let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
    let a = item.GetAttributes(SFGAO_FOLDER | SFGAO_FILESYSTEM | SFGAO_STREAM).ok()?;
    let folder = a & SFGAO_FOLDER == SFGAO_FOLDER && a & SFGAO_FILESYSTEM == SFGAO_FILESYSTEM && a & SFGAO_STREAM != SFGAO_STREAM;
    let name = item
        .GetDisplayName(SIGDN_NORMALDISPLAY)
        .ok()
        .map(|p| {
            let s = p.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            s
        })
        .unwrap_or_default();
    Some((folder, name))
}

/// Checks which of `paths` are folders on a helper thread, remembers the
/// answers and hands them to `done` (path, is folder). Paths that cannot be
/// read count as "not a folder". False if refused (too many busy helpers).
pub fn check_kinds(paths: Vec<String>, done: impl FnOnce(Vec<(String, bool)>) + Send + 'static) -> bool {
    if paths.is_empty() {
        return true;
    }
    helper("stack-kinds", move || {
        let mut out = Vec::new();
        for p in paths.into_iter().take(64) {
            let (folder, name) = unsafe { read_kind(&p) }.unwrap_or((false, String::new()));
            remember(&p, folder, &name);
            out.push((p, folder));
        }
        done(out);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, time: u64) -> Entry {
        Entry { name: name.into(), path: format!("C:\\d\\{name}"), time, dir: false }
    }

    #[test]
    fn stacks_sort_newest_first() {
        let v = vec![e("old.txt", 10), e("new.zip", 30), e("b.pdf", 20), e("A.pdf", 20)];
        let names: Vec<String> = newest_first(v, 20).into_iter().map(|x| x.name).collect();
        // Same time: by name, ignoring case.
        assert_eq!(names, vec!["new.zip", "A.pdf", "b.pdf", "old.txt"]);
    }

    #[test]
    fn stacks_keep_the_newest_twenty() {
        let v: Vec<Entry> = (0..50).map(|i| e(&format!("f{i:02}"), i)).collect();
        let out = newest_first(v, LIMIT);
        assert_eq!(out.len(), 20);
        assert_eq!(out[0].name, "f49");
        assert_eq!(out[19].name, "f30");
    }

    #[test]
    fn top_twenty_of_a_huge_folder_includes_late_entries() {
        // 100,000 entries in "disk order"; the newest come last and in the
        // middle. The bounded heap must find exactly the overall newest 20.
        let mut top = TopN::new(LIMIT);
        let mut all = Vec::new();
        for i in 0..100_000u64 {
            let t = (i * 7919) % 100_003; // scrambled times
            let x = e(&format!("n{i}"), t);
            all.push(x.clone());
            top.push(x);
        }
        top.push(e("latest", 1_000_000));
        all.push(e("latest", 1_000_000));
        let got = top.into_sorted();
        assert_eq!(got, newest_first(all, LIMIT));
        assert_eq!(got[0].name, "latest");
    }

    #[test]
    fn top_n_breaks_ties_like_the_sort() {
        let mut top = TopN::new(2);
        for n in ["c", "B", "a", "d"] {
            top.push(e(n, 5));
        }
        let names: Vec<String> = top.into_sorted().into_iter().map(|x| x.name).collect();
        assert_eq!(names, vec!["a", "B"]);
        assert!(TopN::new(0).into_sorted().is_empty());
    }

    #[test]
    fn only_rooted_paths_may_be_folders() {
        assert!(could_be_folder("C:\\Users\\me\\Downloads"));
        assert!(could_be_folder("D:\\"));
        assert!(could_be_folder("\\\\nas\\share\\docs"));
        // Not decided by extension: these are checked like any other path.
        assert!(could_be_folder("C:\\Archive\\tools.exe"));
        assert!(could_be_folder("C:\\Docs\\report.pdf"));
        assert!(!could_be_folder("shell:AppsFolder\\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"));
        assert!(!could_be_folder("::{645FF040-5081-101B-9F08-00AA002F954E}"));
        assert!(!could_be_folder("\\\\?\\C:\\x"));
        assert!(!could_be_folder("relative\\dir"));
        assert!(!could_be_folder(""));
    }

    #[test]
    fn unknown_paths_are_not_folders_until_checked() {
        let p = "Z:\\never\\checked\\in\\tests";
        assert_eq!(known_kind(p), None);
        remember(p, true, "Checked");
        assert_eq!(known_kind(&p.to_uppercase()), Some(true));
        assert_eq!(known_name(p).as_deref(), Some("Checked"));
    }

    #[test]
    fn a_listing_stops_at_its_deadline() {
        let dir = std::env::temp_dir().join(format!("nopbar-stack-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        for n in ["a.txt", "b.txt"] {
            let _ = std::fs::write(dir.join(n), b"x");
        }
        let d = dir.to_string_lossy().to_string();
        // In time: everything, complete.
        let (v, partial) = list(&d, Instant::now() + BUDGET, &|| false).expect("readable");
        assert_eq!((v.len(), partial), (2, false));
        // A deadline already past: stops before taking anything, partial.
        let (v, partial) = list(&d, Instant::now(), &|| false).expect("readable");
        assert!(v.is_empty() && partial);
        // Asked to stop: no result at all.
        assert!(list(&d, Instant::now() + BUDGET, &|| true).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    const ALL: u32 = EFFECT_COPY | EFFECT_MOVE | EFFECT_LINK;

    #[test]
    fn drop_effect_follows_keys_and_volume_like_explorer() {
        // No keys: same volume moves, another volume copies.
        assert_eq!(choose_effect(0, true, ALL), EFFECT_MOVE);
        assert_eq!(choose_effect(0, false, ALL), EFFECT_COPY);
        // Ctrl copies, Shift moves, whatever the volume.
        assert_eq!(choose_effect(KEY_CTRL, true, ALL), EFFECT_COPY);
        assert_eq!(choose_effect(KEY_SHIFT, false, ALL), EFFECT_MOVE);
        // Alt, or Ctrl+Shift: shortcuts.
        assert_eq!(choose_effect(KEY_ALT, true, ALL), EFFECT_LINK);
        assert_eq!(choose_effect(KEY_CTRL | KEY_SHIFT, false, ALL), EFFECT_LINK);
        // Mouse buttons in the key state change nothing.
        assert_eq!(choose_effect(0x1, true, ALL), EFFECT_MOVE);
    }

    #[test]
    fn drop_effect_respects_what_the_source_allows() {
        // Default move not allowed: copy instead; default copy not allowed: move.
        assert_eq!(choose_effect(0, true, EFFECT_COPY), EFFECT_COPY);
        assert_eq!(choose_effect(0, false, EFFECT_MOVE), EFFECT_MOVE);
        assert_eq!(choose_effect(0, false, EFFECT_LINK), EFFECT_LINK);
        // A key asking for what the source refuses: nothing.
        assert_eq!(choose_effect(KEY_SHIFT, true, EFFECT_COPY), 0);
        assert_eq!(choose_effect(KEY_ALT, true, EFFECT_COPY | EFFECT_MOVE), 0);
        assert_eq!(choose_effect(0, true, 0), 0);
    }

    #[test]
    fn volumes_come_from_the_path_text() {
        assert_eq!(volume_of("C:\\Users\\me\\a.txt").as_deref(), Some("c:"));
        assert_eq!(volume_of("d:\\").as_deref(), Some("d:"));
        assert_eq!(volume_of("\\\\NAS\\Share\\docs\\x").as_deref(), Some("\\\\nas\\share"));
        assert_eq!(volume_of("\\\\nas"), None);
        assert_eq!(volume_of("\\\\?\\C:\\x"), None);
        assert_eq!(volume_of("relative\\x"), None);
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Same drive moves, another drive or share copies.
        assert_eq!(drop_into(0, &f(&["C:\\a\\x.txt"]), "c:\\Stack", ALL), EFFECT_MOVE);
        assert_eq!(drop_into(0, &f(&["D:\\a\\x.txt"]), "C:\\Stack", ALL), EFFECT_COPY);
        assert_eq!(drop_into(0, &f(&["\\\\nas\\s\\x"]), "\\\\nas\\other\\Stack", ALL), EFFECT_COPY);
        assert_eq!(drop_into(0, &f(&["\\\\nas\\s\\x"]), "\\\\NAS\\S\\Stack", ALL), EFFECT_MOVE);
        // Mixed volumes: copy (nothing leaves a drive by surprise).
        assert_eq!(drop_into(0, &f(&["C:\\a\\x", "D:\\y"]), "C:\\Stack", ALL), EFFECT_COPY);
    }

    #[test]
    fn folders_never_drop_into_themselves() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(into_itself("C:\\Work", "c:\\work"));
        assert!(into_itself("C:\\Work\\", "C:\\Work\\Sub\\Deep"));
        assert!(into_itself("D:\\", "D:\\Stack"));
        assert!(!into_itself("C:\\Work", "C:\\Workshop"), "a sibling with a longer name");
        assert!(!into_itself("C:\\Work\\Sub", "C:\\Work"));
        assert_eq!(drop_into(0, &f(&["C:\\Work"]), "C:\\Work\\Sub", ALL), 0);
        assert_eq!(drop_into(KEY_CTRL, &f(&["C:\\a.txt", "C:\\Stack"]), "C:\\Stack", ALL), 0, "one bad item refuses the drop");
        // Already there: a move does nothing; a copy (Ctrl) duplicates, like Explorer.
        assert_eq!(drop_into(0, &f(&["C:\\Stack\\a.txt"]), "C:\\Stack\\", ALL), 0);
        assert_eq!(drop_into(KEY_CTRL, &f(&["C:\\Stack\\a.txt"]), "C:\\Stack", ALL), EFFECT_COPY);
        assert_eq!(drop_into(0, &[], "C:\\Stack", ALL), 0);
    }

    #[test]
    fn right_button_drops_copy_and_never_move() {
        // Same volume would move with the left button; the right one copies.
        assert_eq!(choose_effect(KEY_RIGHT, true, ALL), EFFECT_COPY);
        assert_eq!(choose_effect(KEY_RIGHT | KEY_SHIFT, true, ALL), EFFECT_COPY, "Shift does not turn it into a move");
        assert_eq!(choose_effect(KEY_RIGHT | KEY_ALT, true, ALL), EFFECT_LINK);
        // Copy not allowed by the source: nothing (never a silent move).
        assert_eq!(choose_effect(KEY_RIGHT, true, EFFECT_MOVE), 0);
        let f = vec!["C:\\a\\x.txt".to_string()];
        assert_eq!(drop_into(KEY_RIGHT, &f, "C:\\Stack", ALL), EFFECT_COPY);
    }

    #[test]
    fn shortcuts_of_one_drop_get_distinct_names() {
        let mut used = Vec::new();
        let a = unique_name("report.txt - Shortcut.lnk", &used);
        used.push(a.to_lowercase());
        let b = unique_name("report.txt - Shortcut.lnk", &used);
        used.push(b.to_lowercase());
        let c = unique_name("REPORT.TXT - Shortcut.lnk", &used);
        assert_eq!(a, "report.txt - Shortcut.lnk");
        assert_eq!(b, "report.txt - Shortcut (2).lnk");
        assert_eq!(c, "REPORT.TXT - Shortcut (3).lnk");
        assert_eq!(unique_name("noext", &["noext".into()]), "noext (2)");
    }

    #[test]
    fn a_drag_starts_only_past_the_system_box() {
        // SM_CXDRAG = SM_CYDRAG = 4.
        assert!(!past_drag_threshold(0, 0, 4, 4));
        assert!(!past_drag_threshold(3, -3, 4, 4), "a shaky click is still a click");
        assert!(past_drag_threshold(4, 0, 4, 4));
        assert!(past_drag_threshold(0, -5, 4, 4));
        // Odd metrics never make every press a drag.
        assert!(!past_drag_threshold(0, 0, 0, 0));
    }

    #[test]
    fn shortcut_names_lose_their_extension() {
        assert_eq!(display_name("Report.lnk"), "Report");
        assert_eq!(display_name("site.URL"), "site");
        assert_eq!(display_name("photo.jpg"), "photo.jpg");
        assert_eq!(display_name(".lnk"), ".lnk");
    }

    #[test]
    fn icons_fit_a_square() {
        // 4x2 opaque white -> 2x2: one row of white, centered vertically.
        let src = vec![255u8; 4 * 2 * 4];
        let out = fit_square(&src, 4, 2, 2);
        assert_eq!(out.len(), 16);
        let alpha: Vec<u8> = out.chunks(4).map(|p| p[3]).collect();
        assert_eq!(alpha.iter().filter(|&&a| a == 255).count(), 2);
        // Same size: unchanged.
        let sq: Vec<u8> = (0..16u8).collect();
        assert_eq!(fit_square(&sq, 2, 2, 2), sq);
        // Bad input: transparent, no panic.
        assert_eq!(fit_square(&[], 3, 3, 4), vec![0u8; 64]);
    }
}
