//! Shell events supply candidates. Only those files get finite stability checks.
use super::card::{Button, Card, Emphasis, Kind, Line, Route};
use crate::desklayer::card::Mark;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
#[path = "downloads_watch.rs"]
mod watch;
pub use watch::WM_CHANGED;
pub unsafe fn changed(
    wp: windows::Win32::Foundation::WPARAM,
    lp: windows::Win32::Foundation::LPARAM,
) {
    use windows::Win32::{Foundation::HANDLE, UI::Shell::*};
    let mut pidls = std::ptr::null_mut();
    let mut event = 0;
    let lock = SHChangeNotification_Lock(
        HANDLE(wp.0 as *mut _),
        lp.0 as u32,
        Some(&mut pidls),
        Some(&mut event),
    );
    if !lock.is_invalid() {
        shell_event(event, pidls);
        let _ = SHChangeNotification_Unlock(lock);
    }
}
fn normalized(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}
fn temporary(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_ascii_lowercase().as_str(),
            "crdownload" | "part" | "partial" | "tmp" | "temp" | "download" | "opdownload"
        )
    })
}
fn ignored_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    name.starts_with("~$") || matches!(name.as_str(), "desktop.ini" | "thumbs.db")
}
// Local scripts and extracted files are never downloads without Mark of the Web.
fn download_origin(attributes: u32, zone: bool) -> bool {
    attributes & (2 | 4) == 0 && zone
}
struct Candidate {
    size: Option<u64>,
    stable: u64,
    created: u64,
    retry_at: u64,
}
#[derive(Default)]
pub struct Tracker {
    root: Option<PathBuf>,
    pending: BTreeMap<PathBuf, Candidate>,
    completed: BTreeMap<PathBuf, u64>,
    batch: Vec<PathBuf>,
    batch_at: u64,
}
impl Tracker {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            ..Default::default()
        }
    }
    fn accepts(&self, path: &Path) -> bool {
        !temporary(path)
            && !ignored_name(path)
            && self.root.as_ref().is_some_and(|r| {
                path.parent()
                    .is_some_and(|p| normalized(p) == normalized(r))
            })
    }
    fn rename_arrives(&self, previous: &Path) -> bool {
        self.root.as_ref().is_some_and(|root| {
            previous
                .parent()
                .is_some_and(|p| normalized(p) == normalized(root))
        }) && (temporary(previous) || self.pending.contains_key(previous))
    }
    pub fn event(&mut self, path: PathBuf, new: bool, now: u64, size: Option<u64>) {
        if !self.accepts(&path) {
            return;
        }
        self.completed
            .retain(|_, at| now.saturating_sub(*at) < 600_000);
        if self.completed.contains_key(&path) {
            return;
        }
        if let Some(c) = self.pending.get_mut(&path) {
            if c.size != size {
                c.size = size;
                c.stable = now;
            }
        } else if new && self.pending.len() < 256 {
            self.pending.insert(
                path,
                Candidate {
                    size,
                    stable: now,
                    created: now,
                    retry_at: now,
                },
            );
        }
    }
    pub fn tick(&mut self, now: u64, mut read: impl FnMut(&Path) -> Option<u64>) -> Vec<PathBuf> {
        self.pending.retain(|p, c| {
            if now.saturating_sub(c.created) > 60_000 { return false; }
            if now < c.retry_at { return true; }
            let Some(size) = read(p) else {
                // Chromium can publish the final name before Defender writes MOTW.
                // UPDATEITEM keeps the same candidate; it never resets this deadline.
                c.retry_at = now.saturating_add(2000);
                return now.saturating_sub(c.created) < 60_000;
            };
            if c.size != Some(size) {
                c.size = Some(size);
                c.stable = now;
                return true;
            }
            if size == 0 || now.saturating_sub(c.stable) < 2000 {
                return true;
            }
            if self.batch.is_empty() {
                self.batch_at = now;
            }
            self.batch.push(p.clone());
            if self.completed.len() >= 1024 {
                self.completed.pop_first();
            }
            self.completed.insert(p.clone(), now);
            false
        });
        if self.pending.is_empty() || now.saturating_sub(self.batch_at) >= 2000 {
            std::mem::take(&mut self.batch)
        } else {
            vec![]
        }
    }
    fn waiting(&self) -> bool {
        !self.pending.is_empty() || !self.batch.is_empty()
    }
}
#[derive(Clone, Debug)]
pub struct Download {
    pub path: PathBuf,
    pub bytes: Option<u64>,
}
fn size_label(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024. * 1024. * 1024.))
    }
}
fn file_glyph(path: &Path) -> Mark {
    match path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => Mark::Image,
        "zip" | "7z" | "rar" | "gz" | "tar" => Mark::Archive,
        _ => Mark::File,
    }
}
pub fn card(files: &[Download]) -> Option<Card> {
    let first = files.first()?;
    let folder = first.path.parent()?.to_path_buf();
    let name = first.path.file_name()?.to_string_lossy().into_owned();
    let open = if files.len() == 1 {
        first.path.clone()
    } else {
        folder.clone()
    };
    let folder_name = folder.file_name().unwrap_or_default().to_string_lossy();
    let detail = first.bytes.map_or_else(
        || folder_name.to_string(),
        |b| format!("{} · {folder_name}", size_label(b)),
    );
    Some(Card {
        kind: Kind::Download,
        title: crate::i18n::t("assistant.downloads").into(),
        emphasis: (files.len() > 1).then(|| Emphasis {
            value: crate::i18n::t("assistant.files").replace("{n}", &files.len().to_string()),
            title: String::new(),
            route: Route::OpenFile(folder.clone()),
        }),
        lines: vec![Line {
            glyph: file_glyph(&first.path),
            label: name,
            value: String::new(),
            detail,
            route: Route::OpenFile(first.path.clone()),
        }],
        buttons: vec![
            Button {
                label: crate::i18n::t("assistant.open").into(),
                route: Route::OpenFile(open),
                primary: true,
            },
            Button {
                label: crate::i18n::t("assistant.folder").into(),
                route: Route::OpenFile(folder),
                primary: false,
            },
        ],
    })
}
pub fn in_front(root: &Path, files: &[PathBuf], folders: &[PathBuf], title: &str) -> bool {
    folders.iter().any(|p| normalized(p) == normalized(root))
        || files.iter().any(|p| {
            let title = title.to_lowercase();
            let file = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            !file.is_empty()
                && (title == file
                    || title.starts_with(&format!("{file} - "))
                    || title.starts_with(&format!("{file} — "))
                    || title.contains(&normalized(p)))
        })
}
static ENABLED: AtomicBool = AtomicBool::new(false);
static TIMER: AtomicBool = AtomicBool::new(false);
const MAX_NOTICE_AGE: Duration = Duration::from_secs(10 * 60);
struct Queued<T> {
    item: T,
    completed: Instant,
}
impl<T> Queued<T> {
    fn fresh(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.completed) < MAX_NOTICE_AGE
    }
}
static READY: Mutex<Vec<Queued<PathBuf>>> = Mutex::new(Vec::new());
static APPROVED: Mutex<Vec<Queued<Download>>> = Mutex::new(Vec::new());
static VERIFY: AtomicBool = AtomicBool::new(false);
enum Job {
    Configure(Option<PathBuf>),
    Event(PathBuf, bool, Option<PathBuf>),
    Tick,
    Present(Vec<Queued<PathBuf>>),
}
fn queue() -> &'static crate::idle::IdleQueue<Job> {
    static QUEUE: OnceLock<crate::idle::IdleQueue<Job>> = OnceLock::new();
    static MODEL: Mutex<Option<Tracker>> = Mutex::new(None);
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    QUEUE.get_or_init(|| {
        crate::idle::IdleQueue::new("download completion", Duration::from_secs(5), 512, |jobs| {
            while let Some(job) = jobs.next() {
                if !ENABLED.load(Ordering::SeqCst) && !matches!(job, Job::Configure(_)) {
                    if matches!(job, Job::Present(_)) {
                        VERIFY.store(false, Ordering::SeqCst);
                    }
                    continue;
                }
                let now = EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64;
                let mut model = MODEL.lock().unwrap_or_else(|e| e.into_inner());
                let t = model.get_or_insert_with(Tracker::default);
                let mut ready = vec![];
                let mut presentation = None;
                match job {
                    Job::Configure(root) => {
                        if t.root != root {
                            *t = Tracker::new(root);
                        }
                    }
                    Job::Event(path, new, previous) => {
                        if t.accepts(&path) {
                            let new = previous.as_ref().map_or(new, |old| t.rename_arrives(old));
                            let size = file_size(&path);
                            t.event(path, new, now, size);
                        }
                    }
                    Job::Tick => ready = t.tick(now, file_size),
                    Job::Present(files) => presentation = Some(files),
                }
                let root = t.root.clone();
                let waiting = t.waiting();
                drop(model);
                let now = Instant::now();
                if let Some(files) = presentation {
                    if let Some(root) = root {
                        let files: Vec<_> = native_unseen(&root, files, now).into_iter().filter_map(|file| {
                            // Revalidate metadata on the worker at presentation.
                            let bytes = file_size(&file.item)?;
                            Some(Queued {
                                item: Download { bytes: Some(bytes), path: file.item },
                                completed: file.completed,
                            })
                        }).collect();
                        let mut out = APPROVED.lock().unwrap_or_else(|e| e.into_inner());
                        if ENABLED.load(Ordering::SeqCst) {
                            out.retain(|file| file.fresh(now));
                            for file in files {
                                if out.len() < 256 && !out.iter().any(|f| f.item.path == file.item.path) {
                                    out.push(file);
                                }
                            }
                        }
                    }
                    VERIFY.store(false, Ordering::SeqCst);
                    super::refresh();
                } else if !ready.is_empty() {
                    let mut out = READY.lock().unwrap_or_else(|e| e.into_inner());
                    if ENABLED.load(Ordering::SeqCst) {
                        out.retain(|file| file.fresh(now));
                        for path in ready {
                            if out.len() < 256 && !out.iter().any(|f| f.item == path) {
                                out.push(Queued { item: path, completed: now });
                            }
                        }
                    }
                    super::refresh();
                }
                if waiting
                    && !TIMER.swap(true, Ordering::SeqCst)
                    && crate::service::after("download stability", Duration::from_secs(2), || {
                        TIMER.store(false, Ordering::SeqCst);
                        let _ = queue().send(Job::Tick);
                    })
                    .is_none()
                {
                    TIMER.store(false, Ordering::SeqCst);
                }
            }
        })
    })
}
fn file_size(path: &Path) -> Option<u64> {
    use std::os::windows::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || ignored_name(path) {
        return None;
    }
    let mut ads = path.as_os_str().to_os_string();
    ads.push(":Zone.Identifier");
    let zone = std::fs::File::open(Path::new(&ads)).is_ok();
    download_origin(m.file_attributes(), zone).then_some(m.len())
}

fn native_unseen(root: &Path, files: Vec<Queued<PathBuf>>, now: Instant) -> Vec<Queued<PathBuf>> {
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let h = GetForegroundWindow();
        let mut title = [0u16; 1024];
        let n = GetWindowTextW(h, &mut title);
        let folders = crate::dock::companion_foreground_folders(h)
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let title = String::from_utf16_lossy(&title[..n.max(0) as usize]);
        files
            .into_iter()
            .filter(|file| file.fresh(now) && !in_front(root, std::slice::from_ref(&file.item), &folders, &title))
            .collect()
    }
}
pub fn configure(enabled: bool) {
    if ENABLED.swap(enabled, Ordering::SeqCst) == enabled {
        return;
    }
    // Test executables can never resolve or use the owner's Downloads location.
    #[cfg(not(test))]
    let root = enabled
        .then(crate::icons::downloads_folder)
        .flatten()
        .map(PathBuf::from);
    #[cfg(test)]
    let root = None;
    if !enabled {
        READY.lock().unwrap_or_else(|e| e.into_inner()).clear();
        APPROVED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
    watch::configure(root.as_deref());
    let _ = queue().send(Job::Configure(root));
}
/// Called while the existing SHChangeNotification lock owns these PIDLs.
pub unsafe fn shell_event(
    event: i32,
    pidls: *mut *mut windows::Win32::UI::Shell::Common::ITEMIDLIST,
) {
    use windows::Win32::UI::Shell::*;
    if !ENABLED.load(Ordering::SeqCst) || pidls.is_null() {
        return;
    }
    let rename = event & SHCNE_RENAMEITEM.0 as i32 != 0;
    let create = event & SHCNE_CREATE.0 as i32 != 0;
    if !rename && !create && event & SHCNE_UPDATEITEM.0 as i32 == 0 {
        return;
    }
    // PIDL comparison only: unrelated events never decode a path or enter the queue.
    let inside = [
        watch::contains(*pidls),
        if rename {
            watch::contains(*pidls.add(1))
        } else {
            false
        },
    ];
    let Some(index) = event_target(true, rename, inside) else {
        return;
    };
    let pidl = *pidls.add(index);
    let path_of = |pidl| {
        let mut path = [0u16; 32768];
        if !SHGetPathFromIDListEx(pidl, &mut path, GPFIDL_DEFAULT).as_bool() {
            return None;
        }
        let n = path.iter().position(|c| *c == 0).unwrap_or(path.len());
        Some(PathBuf::from(String::from_utf16_lossy(&path[..n])))
    };
    if let Some(path) = path_of(pidl) {
        let previous = if rename {
            if (*pidls).is_null() {
                return;
            }
            let Some(old) = path_of(*pidls) else {
                return;
            };
            Some(old)
        } else {
            None
        };
        let _ = queue().send(Job::Event(path, rename || create, previous));
    }
}
pub fn poll() {
    if !ENABLED.load(Ordering::SeqCst) || super::card::active() || super::chat::busy() {
        return;
    }
    if READY.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
        && APPROVED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    {
        return;
    }
    if super::remind::delivery() != crate::notes::reminders::Delivery::Bubble {
        return;
    }
    let now = Instant::now();
    let files: Vec<_> = std::mem::take(&mut *APPROVED.lock().unwrap_or_else(|e| e.into_inner()))
        .into_iter()
        .filter(|file| file.fresh(now))
        .map(|file| file.item)
        .collect();
    if files.is_empty() {
        if VERIFY.load(Ordering::SeqCst) {
            return;
        }
        let mut pending = READY.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|file| file.fresh(now));
        let pending = std::mem::take(&mut *pending);
        if !pending.is_empty() {
            VERIFY.store(true, Ordering::SeqCst);
            if queue().send(Job::Present(pending)).is_err() {
                VERIFY.store(false, Ordering::SeqCst);
            }
        }
        return;
    }
    let Some(card) = card(&files) else {
        return;
    };
    super::APP.with(|a| {
        let mut a = a.borrow_mut();
        if let Some(a) = a.as_mut() {
            if super::card::show(&a.gfx, card) {
                a.model.landed(Instant::now());
                a.purpose.notice(false);
                if a.peek_home().is_some() {
                    a.peek.notify(a.start.elapsed().as_secs_f64());
                }
            }
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_notice_expires_at_ten_minutes() {
        let now = Instant::now();
        let file = Queued { item: (), completed: now };
        assert!(file.fresh(now + MAX_NOTICE_AGE - Duration::from_millis(1)));
        assert!(!file.fresh(now + MAX_NOTICE_AGE));
    }
    #[test]
    fn review_ads_retry_is_bounded_and_update_does_not_restart_deadline() {
        let mut t = Tracker::new(Some("X:/fixture".into()));
        let path = PathBuf::from("X:/fixture/large.zip");
        t.event(path.clone(), true, 0, None);
        let mut reads = 0;
        for now in 0..=60_000 {
            if now == 59_000 { t.event(path.clone(), false, now, None); }
            assert!(t.tick(now, |_| { reads += 1; None }).is_empty());
        }
        assert_eq!(reads, 31);
        assert!(!t.waiting());
    }
    #[test]
    fn origin_requires_motw_and_rejects_hidden_and_system_files() {
        assert!(!download_origin(2, true));
        assert!(!download_origin(4, true));
        assert!(!download_origin(0, false));
        assert!(download_origin(0, true));
    }
    #[test]
    fn review_4_zero_byte_growth_and_ten_minute_dedup() {
        let mut t = Tracker::new(Some("X:/fixture".into()));
        let p = PathBuf::from("X:/fixture/a.pdf");
        t.event(p.clone(), true, 0, Some(0));
        assert!(t.tick(2000, |_| Some(0)).is_empty());
        assert!(t.tick(3000, |_| Some(5)).is_empty());
        assert!(t.tick(4999, |_| Some(5)).is_empty());
        assert_eq!(t.tick(5000, |_| Some(5)), vec![p.clone()]);
        t.event(p.clone(), true, 65000, Some(5));
        assert!(t.tick(67000, |_| Some(5)).is_empty());
        t.event(p.clone(), true, 605000, Some(5));
        assert_eq!(t.tick(607000, |_| Some(5)), vec![p]);
    }
    #[test]
    fn review_4_system_names_and_moved_files_are_not_arrivals() {
        let t = Tracker::new(Some("X:/fixture".into()));
        for n in ["~$report.docx", "desktop.ini", "Thumbs.db"] {
            assert!(!t.accepts(&Path::new("X:/fixture").join(n)));
        }
        assert!(!t.rename_arrives(Path::new("X:/other/moved.pdf")));
    }
    #[test]
    fn renaming_an_old_download_is_not_a_new_arrival() {
        let root = PathBuf::from("C:/fixture/downloads");
        let mut t = Tracker::new(Some(root.clone()));
        let old = root.join("old.zip");
        assert!(!t.rename_arrives(&old));
        assert!(t.rename_arrives(&root.join("new.zip.crdownload")));
        assert!(!t.rename_arrives(Path::new("C:/fixture/staging/new.zip")));
        t.event(old.clone(), true, 0, Some(10));
        assert!(t.rename_arrives(&old));
        assert_eq!(t.tick(2000, |_| Some(10)), vec![old.clone()]);
        assert!(!t.rename_arrives(&old));
    }
    #[test]
    fn temporary_extensions_updates_and_other_folders_never_start_candidates() {
        let mut t = Tracker::new(Some(PathBuf::from("X:/fixture")));
        for p in [
            "X:/fixture/a.CRDOWNLOAD",
            "X:/fixture/a.part",
            "X:/fixture/a.tmp",
            "X:/other/a.pdf",
        ] {
            t.event(p.into(), true, 0, Some(3));
        }
        t.event("X:/fixture/old.pdf".into(), false, 0, Some(3));
        assert!(!t.waiting());
    }
    #[test]
    fn stable_size_two_seconds_rename_and_grouping() {
        let mut t = Tracker::new(Some("X:/fixture".into()));
        for p in ["a.pdf", "b.zip"] {
            t.event(Path::new("X:/fixture").join(p), true, 0, Some(10));
        }
        assert!(t.tick(1999, |_| Some(10)).is_empty());
        assert!(t.tick(2000, |_| Some(20)).is_empty());
        assert_eq!(t.tick(4000, |_| Some(20)).len(), 2);
        assert!(t.tick(6000, |_| Some(20)).is_empty());
    }
    #[test]
    fn deleted_candidates_are_removed_and_duplicates_do_not_realert() {
        let mut t = Tracker::new(Some("X:/fixture".into()));
        let p = PathBuf::from("X:/fixture/a.pdf");
        t.event(p.clone(), true, 0, Some(10));
        assert_eq!(t.tick(2000, |_| Some(10)).len(), 1);
        t.event(p.clone(), true, 3000, Some(10));
        assert!(!t.waiting());
        t.event(p, true, 70000, Some(10));
        assert!(t.tick(72000, |_| None).is_empty());
        assert!(!t.waiting());
    }
    #[test]
    fn foreground_folder_and_file_suppress_without_prefix_false_matches() {
        let root = Path::new("X:/fixture");
        let files = vec![root.join("report.pdf")];
        assert!(in_front(root, &files, &["x:\\fixture".into()], ""));
        assert!(in_front(root, &files, &[], "report.pdf - Viewer"));
        assert!(!in_front(root, &files, &[], "report.pdf2 - Viewer"));
    }
    #[test]
    fn actual_size_checks_use_only_a_temporary_fixture() {
        let root =
            std::env::temp_dir().join(format!("nopbar-download-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("test.pdf");
        std::fs::write(&path, b"fixture").unwrap();
        let mut t = Tracker::new(Some(root.clone()));
        t.event(path.clone(), true, 0, file_size(&path));
        assert!(t.tick(2000, file_size).is_empty());
        let mut ads = path.as_os_str().to_os_string();
        ads.push(":Zone.Identifier");
        std::fs::write(Path::new(&ads), b"[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
        t.event(path.clone(), false, 4000, file_size(&path));
        assert_eq!(t.tick(6000, file_size), vec![path.clone()]);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn grouped_count_only_includes_actual_motw_files() {
        let root=std::env::temp_dir().join(format!("nopbar-motw-batch-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let mut tracker=Tracker::new(Some(root.clone()));
        let mut paths=Vec::new();
        for (name,zone) in [("_qa_a4.py",false),("report.pdf",true),("archive.zip",true)] {
            let path=root.join(name);std::fs::write(&path,b"fixture").unwrap();
            if zone {let mut ads=path.as_os_str().to_os_string();ads.push(":Zone.Identifier");std::fs::write(Path::new(&ads),b"[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();}
            tracker.event(path.clone(),true,0,file_size(&path));paths.push(path);
        }
        assert!(tracker.tick(2000,file_size).is_empty());
        let ready=tracker.tick(4000,file_size);
        assert_eq!(ready.len(),2);assert!(!ready.contains(&paths[0]));
        let files:Vec<_>=ready.into_iter().map(|path|Download{bytes:file_size(&path),path}).collect();
        let _lang=crate::i18n::test_lock("ko");
        assert_eq!(card(&files).unwrap().emphasis.unwrap().value,crate::i18n::t("assistant.files").replace("{n}","2"));
        for path in paths {std::fs::remove_file(path).unwrap();}std::fs::remove_dir(root).unwrap();
    }
    #[test]
    #[ignore = "offscreen single and grouped download cards"]
    fn harmony_assistant_downloads() {
        let _lang = crate::i18n::test_lock("ko");
        let stage = crate::gfx::promo_kit::stage();
        for (name, files) in [
            ("download", vec![PathBuf::from("X:/fixture/제품 검토.pdf")]),
            (
                "downloads",
                vec![
                    PathBuf::from("X:/fixture/a.pdf"),
                    PathBuf::from("X:/fixture/b.zip"),
                ],
            ),
        ] {
            let files: Vec<_> = files
                .into_iter()
                .map(|path| Download {
                    path,
                    bytes: Some(2457600),
                })
                .collect();
            super::super::card::tests::snapshot(&stage.gfx, name, &card(&files).unwrap());
        }
    }
}

fn event_target(enabled: bool, rename: bool, inside: [bool; 2]) -> Option<usize> {
    (enabled && inside[usize::from(rename)] && (!rename || inside[0]))
        .then_some(usize::from(rename))
}
#[cfg(test)]
mod review_shell_tests {
    use super::*;
    #[test]
    fn review_5_unrelated_bursts_never_reach_path_decode_or_queue() {
        let mut decoded = 0;
        for _ in 0..10000 {
            if event_target(true, false, [false, false]).is_some() {
                decoded += 1;
            }
        }
        assert_eq!(decoded, 0);
        assert_eq!(event_target(false, false, [true, false]), None);
        assert_eq!(event_target(true, false, [true, false]), Some(0));
        assert_eq!(event_target(true, true, [true, true]), Some(1));
        assert_eq!(event_target(true, true, [true, false]), None);
    }
}
