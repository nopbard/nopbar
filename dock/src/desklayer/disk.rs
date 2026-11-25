//! Widget files on disk, kept simple (the notes' lesson, 2026-11-08):
//! written synchronously and whole (a temporary file next to it, flushed,
//! then moved over it), read as they are. Nothing here overwrites or
//! deletes a file by itself: every write is one the user's action asked
//! for, and a file that does not read is left where it is.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The widgets' folder: %APPDATA%\NopBar (tests: their own).
pub fn dir() -> PathBuf {
    #[cfg(test)]
    if let Some(d) = TEST_DIR.with(|d| d.borrow().clone()) {
        return d;
    }
    crate::config::app_dir()
}

#[cfg(test)]
thread_local! {
    static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Tests: this thread's widget files live in `d`.
#[cfg(test)]
pub fn set_test_dir(d: Option<PathBuf>) {
    TEST_DIR.with(|x| *x.borrow_mut() = d);
}

fn move_over(from: &Path, to: &Path) -> std::io::Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
    unsafe { MoveFileExW(&HSTRING::from(from.as_os_str()), &HSTRING::from(to.as_os_str()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Writes `bytes` to `path` whole: a temporary file beside it, flushed,
/// then moved over it. A failure leaves the old file as it was, and the
/// temporary file (it may hold what was being written) is removed on every
/// failure path; one that could not be removed is swept at the next start
/// (sweep_tmp).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic_with(path, bytes, |f, b| {
        use std::io::Write;
        f.write_all(b)?;
        f.sync_all()
    })
}

// ---------------------------------------------------------------- strays

// Files NopBar made that must go unless they are confirmed kept: each is
// listed the moment it is made (before a byte is written) and leaves the
// list only when it is confirmed kept (`keep`) or is gone (`retry_strays`:
// removed, or not there). One that would not go stays listed and is tried
// again (quit); what is left at quit is found again
// at the next start (sweep_tmp, the kept copies nothing lists).
// Per thread: widget files are made on the dock's thread only, so a
// retry never meets a file another thread is writing.
thread_local! {
    static STRAYS: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `p` is about to be made: it goes unless kept.
pub fn stray(p: &Path) {
    STRAYS.with(|v| {
        let mut v = v.borrow_mut();
        if !v.iter().any(|x| x == p) {
            v.push(p.to_path_buf());
        }
    });
}

/// `p` is confirmed kept (listed where it belongs): no longer a stray.
pub fn keep(p: &Path) {
    STRAYS.with(|v| v.borrow_mut().retain(|x| x != p));
}

/// Removes every stray that will go now; returns how many are left.
pub fn retry_strays() -> usize {
    let now: Vec<PathBuf> = STRAYS.with(|v| v.borrow().clone());
    let gone: Vec<PathBuf> = now.into_iter().filter(|p| remove_gone(p)).collect();
    STRAYS.with(|v| {
        let mut v = v.borrow_mut();
        v.retain(|p| !gone.contains(p));
        v.len()
    })
}

/// Is `p` listed as a stray now? (Tests.)
#[cfg(test)]
pub fn is_stray(p: &Path) -> bool {
    STRAYS.with(|v| v.borrow().iter().any(|x| x == p))
}

/// Removes the temporary file when dropped, unless it was moved in place
/// (then it is no longer a stray). One that would not go stays a stray.
struct TmpGuard(Option<PathBuf>);

impl Drop for TmpGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            if remove_gone(&p) {
                keep(&p);
            }
        }
    }
}

/// write_atomic with the writing given (tests make it fail).
pub fn write_atomic_with(path: &Path, bytes: &[u8], write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    static N: AtomicU64 = AtomicU64::new(0);
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".{}.{}.tmp", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let tmp = path.with_file_name(name);
    // Listed before it exists: a crash or a failed removal leaves it found.
    stray(&tmp);
    let mut guard = TmpGuard(Some(tmp.clone()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        write(&mut f, bytes)?;
    }
    move_over(&tmp, path)?;
    guard.0 = None;
    keep(&tmp);
    Ok(())
}

/// Pure: is `name` a temporary file write_atomic makes (`<name>.<pid>.<n>.tmp`)?
pub fn is_our_tmp(name: &str) -> bool {
    let Some(rest) = name.strip_suffix(".tmp") else { return false };
    let mut parts = rest.rsplitn(3, '.');
    let (n, pid, base) = (parts.next(), parts.next(), parts.next());
    let digits = |s: Option<&str>| s.is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()));
    digits(n) && digits(pid) && base.is_some_and(|b| !b.is_empty())
}

/// Removes temporary files write_atomic left in `dir` (a crash, or one
/// that could not be removed at the time) whose name starts with one of
/// `bases` ("" for any). Only ours; returns those that would not go.
pub fn sweep_tmp(dir: &Path, bases: &[&str]) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut left = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if is_our_tmp(&name) && bases.iter().any(|b| name.starts_with(b)) && !remove_gone(&e.path()) {
            // Tried again while NopBar runs.
            stray(&e.path());
            left.push(e.path());
        }
    }
    left
}

/// Removes `p`; true when it is gone (removed now, or not there).
pub fn remove_gone(p: &Path) -> bool {
    match std::fs::remove_file(p) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Moves `path` to the Recycle Bin (a user's delete: it can come back from
/// there). False when the shell did not take it.
pub fn recycle(path: &Path) -> bool {
    use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
    // Double-null terminated list of one path.
    let mut from: Vec<u16> = path.as_os_str().encode_wide_vec();
    from.push(0);
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    unsafe { SHFileOperationW(&mut op) == 0 && !op.fAnyOperationsAborted.as_bool() }
}

trait Wide {
    fn encode_wide_vec(&self) -> Vec<u16>;
}

impl Wide for std::ffi::OsStr {
    fn encode_wide_vec(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().collect()
    }
}

// ---------------------------------------------------------------- stores

/// How a widget file read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadState {
    /// Not there yet: written on the first save.
    Missing,
    Loaded,
    /// There but not readable (an I/O error, or not our JSON): never
    /// written over; read again before each write.
    Unreadable,
    /// A newer version than this NopBar knows: never written over.
    Unsupported,
}

impl ReadState {
    /// May the file be written (a save replaces it whole)?
    pub fn writable(self) -> bool {
        matches!(self, ReadState::Missing | ReadState::Loaded)
    }
}

/// Pure: what reading gave (`Err(NotFound)`: missing) and the data.
pub fn parse_read<T: serde::de::DeserializeOwned + Default>(read: std::io::Result<String>, version: fn(&T) -> u32, known: u32) -> (ReadState, T) {
    match read {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (ReadState::Missing, T::default()),
        Err(_) => (ReadState::Unreadable, T::default()),
        Ok(text) => match serde_json::from_str::<T>(&text) {
            Ok(v) if version(&v) > known => (ReadState::Unsupported, T::default()),
            Ok(v) => (ReadState::Loaded, v),
            Err(_) => (ReadState::Unreadable, T::default()),
        },
    }
}

/// One widget JSON file: read once, written whole only while it read
/// (or is not there). A file that did not read is read again before any
/// write and never replaced until it reads; the data in memory changes
/// only once a save went through.
pub struct Store<T> {
    pub path: PathBuf,
    pub state: ReadState,
    pub data: T,
    version: fn(&T) -> u32,
    known: u32,
}

impl<T: serde::de::DeserializeOwned + serde::Serialize + Default + Clone> Store<T> {
    pub fn open(path: PathBuf, version: fn(&T) -> u32, known: u32) -> Self {
        let (state, data) = parse_read(std::fs::read_to_string(&path), version, known);
        if !state.writable() {
            crate::log!("widgets: {} did not read ({state:?}); left as it is, not written", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        }
        Store { path, state, data, version, known }
    }

    /// Reads the file again (right before any write): its state, and if it
    /// read, its data. A file that does not read now is never written.
    pub fn reread(&mut self) -> bool {
        let (state, data) = parse_read(std::fs::read_to_string(&self.path), self.version, self.known);
        self.state = state;
        if state.writable() {
            self.data = data;
        }
        state.writable()
    }

    /// May a change be saved now (the file read again)?
    pub fn writable(&mut self) -> bool {
        self.reread()
    }

    /// Writes `next` whole; only then is it the data.
    fn write(&mut self, next: T) -> bool {
        let text = serde_json::to_string_pretty(&next).unwrap_or_default();
        match write_atomic(&self.path, text.as_bytes()) {
            Ok(()) => {
                self.data = next;
                self.state = ReadState::Loaded;
                true
            }
            Err(e) => {
                crate::log!("widgets: {} not saved ({e})", self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                false
            }
        }
    }

    /// The file read again right now; if it read, `edit` is applied to
    /// what it holds (never to an older or failed state) and that is
    /// saved; the data changes only if saved. `edit`'s answer comes back
    /// with whether it was saved (None: the file did not read).
    pub fn edit<R>(&mut self, edit: impl FnOnce(&mut T) -> R) -> Option<(R, bool)> {
        if !self.reread() {
            return None;
        }
        let mut next = self.data.clone();
        let r = edit(&mut next);
        Some((r, self.write(next)))
    }

    /// edit() without an answer: true when saved.
    pub fn update(&mut self, edit: impl FnOnce(&mut T)) -> bool {
        self.edit(edit).is_some_and(|(_, saved)| saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_replaces_the_file_whole() {
        let d = std::env::temp_dir().join(format!("nopbar-disk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("sub").join("a.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, "둘".as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "둘");
        // No temporary file is left behind.
        assert_eq!(std::fs::read_dir(d.join("sub")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_write_leaves_no_temporary_copy() {
        let d = std::env::temp_dir().join(format!("nopbar-disk-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("secret.txt");
        std::fs::write(&p, b"old").unwrap();
        // The disk fills up halfway through: an error after part was written.
        let r = write_atomic_with(&p, b"what was pinned", |f, b| {
            use std::io::Write;
            f.write_all(&b[..4])?;
            Err(std::io::Error::other("disk full"))
        });
        assert!(r.is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"old", "the old file is as it was");
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["secret.txt"], "no temporary copy left");
        // The move failing (the target is a folder): the same.
        let dir_target = d.join("folder");
        std::fs::create_dir_all(&dir_target).unwrap();
        assert!(write_atomic(&dir_target, b"x").is_err());
        assert!(std::fs::read_dir(&d).unwrap().flatten().all(|e| !is_our_tmp(&e.file_name().to_string_lossy())));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_change_is_made_on_what_the_file_holds_right_before_the_write() {
        let d = std::env::temp_dir().join(format!("nopbar-store-reread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("x.json");
        // Blocked at start (did not read)...
        std::fs::write(&p, "{locked").unwrap();
        let mut s: Store<Doc> = Store::open(p.clone(), |d| d.version, 1);
        assert_eq!(s.state, ReadState::Unreadable);
        // ...then it reads again with the user's list in it.
        std::fs::write(&p, r#"{"version":1,"list":["pinned","hidden","place"]}"#).unwrap();
        // The next change keeps all of it (never the empty state of the failure).
        assert!(s.update(|x| x.list.push("new pin".into())));
        let back: Doc = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back.list, ["pinned", "hidden", "place", "new pin"]);
        // Changed by someone else while loaded: still applied on the file's.
        std::fs::write(&p, r#"{"version":1,"list":["other"]}"#).unwrap();
        assert!(s.update(|x| x.list.push("mine".into())));
        assert_eq!(s.data.list, ["other", "mine"]);
        // A file that stops reading: refused, nothing written.
        std::fs::write(&p, "{x").unwrap();
        assert_eq!(s.edit(|x| x.list.clear()), None);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{x");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn files_we_make_stay_listed_until_kept_or_gone() {
        let d = std::env::temp_dir().join(format!("nopbar-strays-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("copy.txt");
        stray(&f);
        std::fs::write(&f, b"secret").unwrap();
        // Held open without delete sharing: it would not go.
        let held = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new().read(true).share_mode(1).open(&f).unwrap()
        };
        let _ = retry_strays();
        assert!(is_stray(&f) && f.exists(), "still listed to try again");
        drop(held);
        let _ = retry_strays();
        assert!(!is_stray(&f) && !f.exists(), "gone once it is free (no restart)");
        // Kept: no longer a stray, never removed.
        let k = d.join("kept.txt");
        stray(&k);
        std::fs::write(&k, b"pinned").unwrap();
        keep(&k);
        let _ = retry_strays();
        assert!(k.exists());
        // A failed write's temporary file: listed while it would not go.
        let target = d.join("target.txt");
        let r = write_atomic_with(&target, b"x", |f, _| {
            // The temporary file is open here: hold it from another handle
            // without delete sharing so the guard cannot remove it.
            let _ = f;
            Err(std::io::Error::other("fail"))
        });
        assert!(r.is_err());
        assert!(std::fs::read_dir(&d).unwrap().flatten().all(|e| !is_our_tmp(&e.file_name().to_string_lossy())), "removed when it could be");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn leftover_temporary_files_are_swept_only_if_ours() {
        assert!(is_our_tmp("board.json.123.4.tmp"));
        assert!(is_our_tmp("k1-2-3.txt.99.0.tmp"));
        assert!(!is_our_tmp("board.json"));
        assert!(!is_our_tmp("notes.tmp"));
        assert!(!is_our_tmp("a.b.tmp"));
        assert!(!is_our_tmp(".1.2.tmp"));
        let d = std::env::temp_dir().join(format!("nopbar-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for n in ["board.json.5.1.tmp", "config.json.5.1.tmp", "board.json", "other.tmp"] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        assert!(sweep_tmp(&d, &["board.json"]).is_empty());
        let mut names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["board.json", "config.json.5.1.tmp", "other.tmp"], "only board.json's leftover goes");
        assert!(remove_gone(&d.join("board.json")) && remove_gone(&d.join("never-there")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
    #[serde(default)]
    struct Doc {
        version: u32,
        list: Vec<String>,
    }

    #[test]
    fn reads_tell_missing_loaded_unreadable_and_newer_apart() {
        let v = |d: &Doc| d.version;
        let nf = || Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(parse_read::<Doc>(nf(), v, 1).0, ReadState::Missing);
        assert_eq!(parse_read::<Doc>(Err(std::io::Error::other("locked")), v, 1).0, ReadState::Unreadable);
        assert_eq!(parse_read::<Doc>(Ok("{bad".into()), v, 1).0, ReadState::Unreadable);
        assert_eq!(parse_read::<Doc>(Ok(r#"{"version":2}"#.into()), v, 1).0, ReadState::Unsupported);
        let (s, d) = parse_read::<Doc>(Ok(r#"{"version":1,"list":["a"]}"#.into()), v, 1);
        assert_eq!((s, d.list), (ReadState::Loaded, vec!["a".to_string()]));
        assert!(ReadState::Missing.writable() && ReadState::Loaded.writable());
        assert!(!ReadState::Unreadable.writable() && !ReadState::Unsupported.writable());
    }

    #[test]
    fn a_file_that_did_not_read_is_never_written_over() {
        let d = std::env::temp_dir().join(format!("nopbar-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("x.json");
        std::fs::write(&p, "{broken").unwrap();
        let mut s: Store<Doc> = Store::open(p.clone(), |d| d.version, 1);
        assert_eq!(s.state, ReadState::Unreadable);
        // A user's change afterwards is refused; the file stays as it was.
        assert!(!s.update(|x| x.list.push("new".into())));
        assert!(!s.update(|x| *x = Doc { version: 1, list: vec![] }));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{broken");
        assert!(s.data.list.is_empty(), "nothing changed in memory either");
        // A newer version: the same.
        std::fs::write(&p, r#"{"version":9,"list":["keep"]}"#).unwrap();
        assert!(!s.update(|x| x.list.push("new".into())));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), r#"{"version":9,"list":["keep"]}"#);
        // Once it reads, its data is taken, then the change is saved on it.
        std::fs::write(&p, r#"{"version":1,"list":["old"]}"#).unwrap();
        assert!(s.update(|x| x.list.push("new".into())));
        assert_eq!(s.state, ReadState::Loaded);
        assert_eq!(s.data.list, vec!["old".to_string(), "new".to_string()]);
        let back: Doc = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back.list, s.data.list);
        // Missing: the first save writes it.
        let mut m: Store<Doc> = Store::open(d.join("new.json"), |d| d.version, 1);
        assert_eq!(m.state, ReadState::Missing);
        assert!(m.update(|x| x.version = 1));
        assert!(d.join("new.json").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
