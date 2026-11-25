//! Notes on disk, under `%APPDATA%\NopBar\notes\`, written on the notes'
//! own thread, at once (a note is a few KB; a whole-file replace takes
//! milliseconds): no writer thread, no queue, no answers arriving later.
//! What a call returns is what the disk now has.
//!
//! - `<id>.md`: a note's text (Markdown); its name never changes (other
//!   apps can open and sync it). Saved whole: a temporary file, flushed
//!   (FlushFileBuffers), then moved over it (MoveFileEx REPLACE_EXISTING
//!   | WRITE_THROUGH). Only that note's own save writes it.
//! - `notes.json`: places, sizes, order, the virtual desktop each belongs
//!   to (none: every desktop), the last one used. A text without an entry
//!   is a note (default place, every desktop); an entry without its text
//!   is left out (nothing is removed). Read again right before each write
//!   and merged field by field: what the file has now stays, but for the
//!   fields this thread changed since it last wrote (merge). A file that
//!   cannot be read is not written over (tried again later).
//! - `notes.json.broken-<ms>`: a notes.json that read but was not an index
//!   (broken), moved aside before a new one is written; kept for good
//!   (nothing removes it). If it cannot be moved aside, nothing is written
//!   over it (the layout is tried again; the user's quit waits).
//! - `recovery\<id>-<stamp>.md`: a text that could not be saved, as a new
//!   file (also in `%LOCALAPPDATA%\NopBar\notes-recovery\` when the notes
//!   folder itself fails). At start each one becomes a new note (a new
//!   file, never over a note), removed only once that is written; one
//!   whose text is its note's is just removed.
//! - `trash\<unix seconds>-<id>.md`: a deleted note, moved there whole (a
//!   rename; on failure the note stays). Restore moves it back when the
//!   name is free. Removed for good after 30 days.
//!
//! Nothing automatic writes over or removes a file holding text, but for
//! trash past 30 days and a recovery copy whose text is in a note.
//!
//! Files of an earlier form of this branch (`<id>.<seq>.md`, numbered
//! recovery and conflict copies) are read with the same rules: moved into
//! place where the name is free, otherwise imported as new notes.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::desklayer::place::Place;

/// Days a deleted note is kept.
pub const TRASH_DAYS: u64 = 30;
pub const INDEX: &str = "notes.json";
pub const RECOVERY: &str = "recovery";
/// The second recovery folder (%LOCALAPPDATA%), when the first fails.
pub const RESCUE: &str = "rescue";
pub const TRASH: &str = "trash";

/// One note in notes.json.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Entry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<crate::desklayer::widget::Size>,
    pub hidden:bool,
    pub id: String,
    pub place: Place,
    /// The virtual desktop it belongs to (its GUID text,
    /// crate::notes::desks); absent: every desktop (an older notes.json
    /// has none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop: Option<String>,
}

/// notes.json.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Index {
    /// Monotonic history: deleting the last note never resets first-run setup.
    pub seeded: bool,
    pub version: u32,
    /// In stacking order (the first is the bottom one).
    pub notes: Vec<Entry>,
    /// The note last typed in (summoning focuses it).
    pub last: String,
}

// ---------------------------------------------------------------- names (pure)

/// Pure: is `id` safe as a file name part (made by new_id)?
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Pure: a new note's id from the time (ms) and a counter.
pub fn new_id(now_ms: u128, n: u32) -> String {
    format!("{now_ms:x}{:02x}", n & 0xFF)
}

pub fn body_name(id: &str) -> String {
    format!("{id}.md")
}

/// Pure: the id of a note's text file (`<id>.md`).
pub fn parse_body(name: &str) -> Option<&str> {
    name.strip_suffix(".md").filter(|id| valid_id(id) && *id != "inbox")
}

/// Pure: (id, seq) of an earlier-form numbered text (`<id>.<seq>.md`).
pub fn parse_numbered(name: &str) -> Option<(&str, u64)> {
    let (id, s) = name.strip_suffix(".md")?.rsplit_once('.')?;
    let seq = s.parse().ok()?;
    valid_id(id).then_some((id, seq))
}

pub fn recovery_name(id: &str, stamp: u128) -> String {
    format!("{id}-{stamp}.md")
}

/// Pure: the note a recovery copy came from, whatever its form
/// (`<id>-<stamp>.md`, or the earlier `<id>.<seq>.md`,
/// `<id>.<seq>.deleted.md`, `conflict-<secs>-<id>-<seq>.md`).
pub fn recovery_origin(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".md")?;
    let stem = stem.strip_suffix(".deleted").unwrap_or(stem);
    if let Some(rest) = stem.strip_prefix("conflict-") {
        let (_, rest) = rest.split_once('-')?;
        let (id, _) = rest.rsplit_once('-')?;
        return valid_id(id).then_some(id);
    }
    let id = match stem.rsplit_once('-') {
        Some((id, s)) if s.chars().all(|c| c.is_ascii_digit()) => id,
        _ => stem.split('.').next()?,
    };
    valid_id(id).then_some(id)
}

pub fn trash_name(id: &str, secs: u64, n: u32) -> String {
    if n == 0 {
        format!("{secs}-{id}.md")
    } else {
        format!("{secs}-{id}~{n}.md")
    }
}

/// Pure: (deleted at, id) of a trash file's name (earlier forms too:
/// `<secs>-<id>-<seq>.md`).
pub fn parse_trash(name: &str) -> Option<(u64, &str)> {
    let stem = name.strip_suffix(".md")?;
    let (secs, rest) = stem.split_once('-')?;
    let secs = secs.parse().ok()?;
    let rest = rest.split('~').next()?;
    let id = match rest.rsplit_once('-') {
        Some((id, s)) if s.chars().all(|c| c.is_ascii_digit()) && valid_id(id) => id,
        _ => rest,
    };
    valid_id(id).then_some((secs, id))
}

/// Pure: has something kept since `at` been there `days` days at `now`?
pub fn expired(at: u64, now_secs: u64, days: u64) -> bool {
    now_secs.saturating_sub(at) >= days * 86_400
}

/// Pure: notes.json read leniently (a broken file is an empty index; bad
/// ids are dropped, duplicates keep the first).
#[cfg(test)]
pub fn parse_index(text: &str) -> Index {
    try_parse_index(text).unwrap_or_default()
}

/// Pure: notes.json, None when it is not an index at all (broken).
pub fn try_parse_index(text: &str) -> Option<Index> {
    let mut ix: Index = serde_json::from_str(text).ok()?;
    let mut seen = std::collections::HashSet::new();
    ix.notes.retain(|e| valid_id(&e.id) && seen.insert(e.id.clone()));
    Some(ix)
}

pub fn index_text(ix: &Index) -> String {
    let ix = Index { version: 1, ..ix.clone() };
    serde_json::to_string_pretty(&ix).unwrap_or_else(|_| "{}".into())
}

/// Pure: the index for the notes `present` on disk: the new layout's
/// entries, then the old layout's for notes the new one lacks, then the
/// rest with no place known (mon all zero). Entries for absent notes go.
pub fn compose(new: &Index, old: &Index, present: &[String]) -> Index {
    let mut notes: Vec<Entry> = Vec::new();
    for e in new.notes.iter().chain(old.notes.iter()) {
        if present.contains(&e.id) && !notes.iter().any(|n| n.id == e.id) {
            notes.push(e.clone());
        }
    }
    let mut rest: Vec<&String> = present.iter().filter(|id| !notes.iter().any(|n| n.id == **id)).collect();
    rest.sort();
    for id in rest {
        notes.push(Entry { id: id.clone(), place: Place { mon: [0; 4], ..Place::default() }, desktop: None, size:None, hidden:false });
    }
    Index { version: 1, notes, last: new.last.clone(), seeded: new.seeded || old.seeded || !old.notes.is_empty() || !present.is_empty() }
}

/// Pure: the index to write for the notes `present` on disk (review 2,
/// notes per desktop): `disk` (the file as read right now) is the base;
/// of each note in `asked` (this thread's layout) only the fields that
/// changed since `mine` (the layout this thread last wrote) replace the
/// file's, and a note `mine` does not have yet is taken whole. Then the
/// file's other notes as they are, then the rest with no place known.
/// The order is `asked`'s; `last` only when this thread changed it.
pub fn merge(asked: &Index, mine: &Index, disk: &Index, present: &[String]) -> Index {
    let mut notes: Vec<Entry> = Vec::new();
    for e in &asked.notes {
        if !present.contains(&e.id) || notes.iter().any(|n| n.id == e.id) {
            continue;
        }
        let on_disk = disk.notes.iter().find(|d| d.id == e.id);
        let before = mine.notes.iter().find(|m| m.id == e.id);
        let entry = match (on_disk, before) {
            (Some(d), Some(m)) => Entry {
                id: e.id.clone(),
                size:if e.size!=m.size{e.size}else{d.size},
                hidden:if e.hidden!=m.hidden{e.hidden}else{d.hidden},
                place: if e.place != m.place { e.place } else { d.place },
                desktop: if e.desktop != m.desktop { e.desktop.clone() } else { d.desktop.clone() },
            },
            _ => e.clone(),
        };
        notes.push(entry);
    }
    for d in &disk.notes {
        if present.contains(&d.id) && !notes.iter().any(|n| n.id == d.id) {
            notes.push(d.clone());
        }
    }
    let mut rest: Vec<&String> = present.iter().filter(|id| !notes.iter().any(|n| n.id == **id)).collect();
    rest.sort();
    for id in rest {
        notes.push(Entry { id: id.clone(), place: Place { mon: [0; 4], ..Place::default() }, desktop: None, size:None, hidden:false });
    }
    let last = if asked.last != mine.last || disk.last.is_empty() { asked.last.clone() } else { disk.last.clone() };
    Index { version: 1, notes, last, seeded: asked.seeded || mine.seeded || disk.seeded || !present.is_empty() }
}

// ---------------------------------------------------------------- the disk

/// Where files go: the real folders, or memory in tests. `sub` is ""
/// (the notes folder), RECOVERY, RESCUE or TRASH.
pub trait Disk {
    /// The files there (Err: they could not be listed).
    fn list(&self, sub: &str) -> Result<Vec<String>, ()>;
    fn read(&self, sub: &str, name: &str) -> Option<String>;
    /// Some(there or not); None: cannot tell.
    fn exists(&self, sub: &str, name: &str) -> Option<bool>;
    /// Whole or not at all, over what is there (a note's own save, the
    /// index).
    fn replace(&mut self, sub: &str, name: &str, text: &str) -> bool;
    /// Whole or not at all, and only where nothing is.
    fn create(&mut self, sub: &str, name: &str, text: &str) -> bool;
    /// Moves a file, only where nothing is.
    fn rename(&mut self, sub: &str, name: &str, to_sub: &str, to: &str) -> bool;
    /// True when it is gone (or was never there).
    fn remove(&mut self, sub: &str, name: &str) -> bool;
}

pub struct RealDisk {
    pub base: PathBuf,
    pub rescue: PathBuf,
}

impl RealDisk {
    pub fn new() -> RealDisk {
        RealDisk { base: dir(), rescue: rescue_dir() }
    }

    fn at(&self, sub: &str) -> PathBuf {
        match sub {
            "" => self.base.clone(),
            RESCUE => self.rescue.clone(),
            s => self.base.join(s),
        }
    }
}

fn move_file(from: &Path, to: &Path, replace: bool) -> std::io::Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
    let flags = if replace { MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH } else { MOVEFILE_WRITE_THROUGH };
    unsafe { MoveFileExW(&HSTRING::from(from.as_os_str()), &HSTRING::from(to.as_os_str()), flags) }.map_err(|e| std::io::Error::other(e.to_string()))
}

/// Writes `bytes` to `path` whole: a temporary file next to it, flushed
/// (sync_all: FlushFileBuffers), then moved over it (`replace`) or only
/// where nothing is.
pub fn write_atomic(path: &Path, bytes: &[u8], replace: bool) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    // Its own temporary name (the writer and quitting may write at once).
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    name.push(format!(".{}.{}.tmp", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let tmp = path.with_file_name(name);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    move_file(&tmp, path, replace).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

impl Disk for RealDisk {
    fn list(&self, sub: &str) -> Result<Vec<String>, ()> {
        let rd = match std::fs::read_dir(self.at(sub)) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(()),
        };
        let mut out = Vec::new();
        for e in rd {
            let e = e.map_err(|_| ())?;
            if e.file_type().map_err(|_| ())?.is_file() {
                out.push(e.file_name().to_string_lossy().into_owned());
            }
        }
        Ok(out)
    }
    fn read(&self, sub: &str, name: &str) -> Option<String> {
        std::fs::read_to_string(self.at(sub).join(name)).ok()
    }
    fn exists(&self, sub: &str, name: &str) -> Option<bool> {
        match std::fs::metadata(self.at(sub).join(name)) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    }
    fn replace(&mut self, sub: &str, name: &str, text: &str) -> bool {
        write_atomic(&self.at(sub).join(name), text.as_bytes(), true).inspect_err(|e| crate::log!("notes: cannot write {sub}\\{name} ({e})")).is_ok()
    }
    fn create(&mut self, sub: &str, name: &str, text: &str) -> bool {
        write_atomic(&self.at(sub).join(name), text.as_bytes(), false).is_ok()
    }
    fn rename(&mut self, sub: &str, name: &str, to_sub: &str, to: &str) -> bool {
        let dest = self.at(to_sub);
        if std::fs::create_dir_all(&dest).is_err() {
            return false;
        }
        move_file(&self.at(sub).join(name), &dest.join(to), false).inspect_err(|e| crate::log!("notes: cannot move {sub}\\{name} ({e})")).is_ok()
    }
    fn remove(&mut self, sub: &str, name: &str) -> bool {
        match std::fs::remove_file(self.at(sub).join(name)) {
            Ok(()) => true,
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    }
}

// ---------------------------------------------------------------- saving (sync)

/// A new recovery copy of note `id`'s `text` (a free name) in `sub`.
pub fn write_recovery_to(disk: &mut impl Disk, sub: &str, id: &str, text: &str, now_ms: u128) -> bool {
    let id = if valid_id(id) { id } else { "note" };
    (0..64u128).any(|k| disk.create(sub, &recovery_name(id, now_ms + k), text))
}

/// Keeps a text that could not be saved: a new recovery copy in the notes'
/// folder, else in the local one (%LOCALAPPDATA%).
pub fn keep(disk: &mut impl Disk, id: &str, text: &str, now_ms: u128) -> bool {
    write_recovery_to(disk, RECOVERY, id, text, now_ms) || write_recovery_to(disk, RESCUE, id, text, now_ms)
}

/// Moves `<id>.md` to the trash under a free name.
fn to_trash(disk: &mut impl Disk, sub: &str, name: &str, id: &str, now_secs: u64) -> bool {
    (0..64u32).any(|n| disk.rename(sub, name, TRASH, &trash_name(id, now_secs, n)))
}

/// Where a note's text is after settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// In its file.
    Saved,
    /// In a recovery copy (its file could not be written).
    InRecovery,
    /// Neither could be written: only on screen.
    Lost,
}

/// A note as the hub holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Doc {
    pub id: String,
    /// The editor's text now.
    pub text: String,
    /// The text its file has (None: not known, or no file yet).
    pub saved: Option<String>,
    /// The text last kept in a recovery copy (one copy per text).
    pub kept: Option<String>,
}

impl Doc {
    /// The editor has text its file does not.
    pub fn dirty(&self) -> bool {
        self.saved.as_deref() != Some(self.text.as_str())
    }

    /// Saves the text now: its file replaced; failing that, a recovery
    /// copy (once per text).
    pub fn settle(&mut self, disk: &mut impl Disk, now_ms: u128) -> Kept {
        if !self.dirty() {
            return Kept::Saved;
        }
        if disk.replace("", &body_name(&self.id), &self.text) {
            self.saved = Some(self.text.clone());
            self.kept = None;
            return Kept::Saved;
        }
        if self.kept.as_deref() == Some(self.text.as_str()) {
            return Kept::InRecovery;
        }
        if keep(disk, &self.id, &self.text, now_ms) {
            self.kept = Some(self.text.clone());
            return Kept::InRecovery;
        }
        Kept::Lost
    }
}

/// Deletes a note now: its text settled first (in its file or a recovery
/// copy), then its file moved to the trash. False: nothing changed (the
/// note stays as it was).
pub fn delete(disk: &mut impl Disk, doc: &mut Doc, now_ms: u128) -> bool {
    if doc.settle(disk, now_ms) == Kept::Lost {
        return false;
    }
    match disk.exists("", &body_name(&doc.id)) {
        Some(false) => true,
        Some(true) => to_trash(disk, "", &body_name(&doc.id), &doc.id, (now_ms / 1000) as u64),
        None => false,
    }
}

/// Brings a deleted note back with `text` now: its newest trash copy
/// moved back when the name is free, then its text saved. None: it could
/// be neither moved back nor written (still deleted).
pub fn restore(disk: &mut impl Disk, id: &str, text: &str, now_ms: u128) -> Option<Doc> {
    let mut moved = false;
    if disk.exists("", &body_name(id)) == Some(false) {
        if let Ok(names) = disk.list(TRASH) {
            let newest = names.into_iter().filter(|n| parse_trash(n).is_some_and(|(_, i)| i == id)).max_by_key(|n| parse_trash(n).map(|(s, _)| s).unwrap_or(0));
            if let Some(n) = newest {
                moved = disk.rename(TRASH, &n, "", &body_name(id));
            }
        }
    }
    let saved = if moved { disk.read("", &body_name(id)) } else { None };
    let mut d = Doc { id: id.to_string(), text: text.to_string(), saved, kept: None };
    match d.settle(disk, now_ms) {
        Kept::Saved => Some(d),
        _ if moved => Some(d),
        // Not back as a file: still deleted (its text is in the trash, and
        // now perhaps in a recovery copy too).
        _ => None,
    }
}

/// The text a note is saved with: what its editor has (`read`, which
/// becomes the last text known), or, when the editor cannot be read (its
/// window gone or going), the last text known (captured when its window
/// was destroyed). Never an empty text standing in for an unreadable
/// editor.
pub fn text_now(read: Option<String>, last: &mut String) -> String {
    if let Some(t) = read {
        *last = t;
    }
    last.clone()
}

/// Pure: does the index written hold every note of the layout asked for?
/// (A note whose file is not there yet is left out by write_index: the
/// layout is then not done and is written again later.)
pub fn layout_done(asked: &Index, written: &Index) -> bool {
    (!asked.seeded || written.seeded) && asked.notes.iter().all(|e| written.notes.iter().any(|w| w.id == e.id))
}

/// The id quitting reports for notes.json when it could not be written.
pub const LAYOUT: &str = "notes.json";

/// Settles every note now (quitting, logging off, updating): the ids
/// whose text could be neither saved nor kept.
pub fn settle_all(disk: &mut impl Disk, docs: &mut [Doc], now_ms: u128) -> Vec<String> {
    docs.iter_mut().filter_map(|d| (d.settle(disk, now_ms) == Kept::Lost).then(|| d.id.clone())).collect()
}

/// What settling everything did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settled {
    /// The ids whose text could be kept nowhere, and LAYOUT when the
    /// layout is not written whole (failed, or written without a note):
    /// what quitting decides on.
    pub lost: Vec<String>,
    /// notes.json as written now (None: not written now).
    pub index: Option<Index>,
    /// The layout asked for is on disk whole (or none was asked).
    pub layout_done: bool,
}

/// Settles every note and then the layout (`layout`: the layout asked for
/// and the one written before; None: nothing new to write). The one
/// place that decides what is "lost": the hub and quitting use it as is.
pub fn settle_with_layout(disk: &mut impl Disk, docs: &mut [Doc], layout: Option<(&Index, &Index)>, now_ms: u128) -> Settled {
    let mut lost = settle_all(disk, docs, now_ms);
    let (index, layout_done) = match layout {
        None => (None, true),
        Some((asked, old)) => match write_index_at(disk, asked, old, now_ms) {
            Some(ix) => {
                let done = layout_done(asked, &ix);
                (Some(ix), done)
            }
            None => (None, false),
        },
    };
    if !layout_done {
        lost.push(LAYOUT.to_string());
    }
    Settled { lost, index, layout_done }
}

/// The name a broken notes.json is kept under.
pub fn broken_name(now_ms: u128, n: u32) -> String {
    if n == 0 {
        format!("{INDEX}.broken-{now_ms}")
    } else {
        format!("{INDEX}.broken-{now_ms}~{n}")
    }
}

/// Moves a broken notes.json aside (under a free name, kept for good).
/// False: it could not be, and nothing may be written over it.
pub fn set_aside_broken(disk: &mut impl Disk, now_ms: u128) -> bool {
    (0..64u32).any(|n| disk.rename("", INDEX, "", &broken_name(now_ms, n)))
}

/// write_index_at, now (tests).
#[cfg(test)]
pub fn write_index(disk: &mut impl Disk, layout: &Index, mine: &Index) -> Option<Index> {
    write_index_at(disk, layout, mine, now_ms())
}

/// Writes `layout` to notes.json for the notes there are, merged into the
/// file as read right now (see merge; `mine`: the layout this thread
/// last wrote, or loaded). None: not written (the folder could not be
/// listed, notes.json is there but could not be read, a broken one could
/// not be moved aside, or the write failed): tried again later. A file
/// that reads but is not an index at all (broken) is first moved aside
/// (set_aside_broken), then a new one is written from this thread's
/// layout.
pub fn write_index_at(disk: &mut impl Disk, layout: &Index, mine: &Index, now_ms: u128) -> Option<Index> {
    let names = disk.list("").ok()?;
    let present: Vec<String> = names.iter().filter_map(|n| parse_body(n).map(String::from)).collect();
    let now = match disk.read("", INDEX) {
        Some(t) => match try_parse_index(&t) {
            Some(ix) => ix,
            None if set_aside_broken(disk, now_ms) => mine.clone(),
            None => return None,
        },
        None if disk.exists("", INDEX) == Some(false) => Index::default(),
        None => return None,
    };
    let ix = merge(layout, mine, &now, &present);
    disk.replace("", INDEX, &index_text(&ix)).then_some(ix)
}

/// What is on disk.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub index: Index,
    /// The notes, with their text: the index's in its order, then the
    /// others (mon all zero: no place known).
    pub notes: Vec<(Entry, String)>,
}

/// A new note's file for `text` under a fresh id (a free name).
fn import(disk: &mut impl Disk, text: &str, now_ms: u128, made: &mut u32) -> Option<String> {
    for _ in 0..64 {
        *made = made.wrapping_add(1);
        let id = new_id(now_ms, *made);
        if disk.create("", &body_name(&id), text) {
            return Some(id);
        }
    }
    None
}

/// Seed only after load/recovery, and only when the disk can be read safely.
/// The empty body is created first: a crash before the index write leaves a
/// discoverable note, so the next load records its history without duplicating it.
pub fn seed_first(disk: &mut impl Disk, place: Place, now_ms: u128) -> bool {
    let Some(mut ix) = disk.read("", INDEX).and_then(|s| try_parse_index(&s)) else { return false };
    if ix.seeded { return false; }
    let Ok(names) = disk.list("") else { return false };
    let Ok(trash) = disk.list(TRASH) else { return false };
    if !ix.notes.is_empty() || names.iter().any(|n| parse_body(n).is_some()) || trash.iter().any(|n| parse_trash(n).is_some()) {
        ix.seeded = true;
        return disk.replace("", INDEX, &index_text(&ix));
    }
    let Some(id) = import(disk, "", now_ms, &mut 0) else { return false };
    ix.seeded = true;
    ix.last = id.clone();
    ix.notes.push(Entry { id, place, desktop: None, size:None, hidden:false });
    let _ = disk.replace("", INDEX, &index_text(&ix));
    true
}

/// Reads the notes. Earlier-form texts are moved into place where the
/// name is free, else imported; recovery copies are imported as new notes
/// (or dropped when their note has the same text); trash past 30 days
/// goes. Anything that cannot be listed or read is left as it is.
pub fn load(disk: &mut impl Disk, now_ms: u128) -> Loaded {
    let now_secs = (now_ms / 1000) as u64;
    // notes.json: written again below only if it read as an index, was
    // not there, or (broken) was moved aside first.
    let (mut layout, index_ok) = match disk.read("", INDEX) {
        Some(t) => match try_parse_index(&t) {
            Some(ix) => (ix, true),
            None => (Index::default(), set_aside_broken(disk, now_ms)),
        },
        None => (Index::default(), disk.exists("", INDEX) == Some(false)),
    };
    // Preserve historical evidence before stale entries/trash are cleaned up.
    layout.seeded |= !layout.notes.is_empty() || disk.list(TRASH).is_ok_and(|ns| ns.iter().any(|n| parse_trash(n).is_some()));
    let mut made = 0u32;
    let mut imported: Vec<String> = Vec::new();
    // Earlier form: numbered texts.
    if let Ok(names) = disk.list("") {
        let mut numbered: Vec<(&str, u64, &String)> = names.iter().filter_map(|n| parse_numbered(n).map(|(id, s)| (id, s, n))).collect();
        numbered.sort_by(|a, b| a.0.cmp(b.0).then(b.1.cmp(&a.1)));
        for (id, _, n) in numbered {
            if disk.exists("", &body_name(id)) == Some(false) && disk.rename("", n, "", &body_name(id)) {
                continue;
            }
            // The name is taken: imported like a recovery copy.
            if let Some(text) = disk.read("", n) {
                if disk.read("", &body_name(id)).as_deref() == Some(text.as_str()) || import(disk, &text, now_ms, &mut made).is_some() {
                    disk.remove("", n);
                }
            }
        }
    }
    // Recovery copies (and the earlier rescue folder).
    for sub in [RECOVERY, RESCUE] {
        let Ok(names) = disk.list(sub) else { continue };
        for n in names.into_iter().filter(|n| n.ends_with(".md")) {
            if n.ends_with(".deleted.md") {
                // An earlier form's deleted note: to the trash.
                let id = recovery_origin(&n).unwrap_or("note").to_string();
                to_trash(disk, sub, &n, &id, now_secs);
                continue;
            }
            let Some(text) = disk.read(sub, &n) else { continue };
            let same_as_note = recovery_origin(&n).is_some_and(|id| disk.read("", &body_name(id)).as_deref() == Some(text.as_str()));
            if same_as_note || imported.contains(&text) {
                disk.remove(sub, &n);
                continue;
            }
            if import(disk, &text, now_ms, &mut made).is_some() {
                imported.push(text);
                disk.remove(sub, &n);
            }
        }
    }
    if let Ok(names) = disk.list(TRASH) {
        for n in names {
            if parse_trash(&n).is_some_and(|(at, _)| expired(at, now_secs, TRASH_DAYS)) {
                disk.remove(TRASH, &n);
            }
        }
    }
    let Ok(names) = disk.list("") else {
        // Not listed: nothing shown, notes.json left alone.
        return Loaded { index: layout, notes: Vec::new() };
    };
    let present: Vec<String> = names.iter().filter_map(|n| parse_body(n).map(String::from)).collect();
    let index = compose(&layout, &layout, &present);
    if index_ok {
        let _ = disk.replace("", INDEX, &index_text(&index));
    }
    let mut notes: Vec<(Entry, String)> = Vec::new();
    for e in &index.notes {
        if let Some(t) = disk.read("", &body_name(&e.id)) {
            notes.push((e.clone(), t));
        }
    }
    Loaded { index, notes }
}

// ---------------------------------------------------------------- folders

static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The notes folder.
pub fn dir() -> PathBuf {
    if let Some(d) = DIR.lock().ok().and_then(|d| d.clone()) {
        return d;
    }
    crate::config::app_dir().join("notes")
}

/// The second recovery folder: local app data (another folder than the
/// roaming one the notes live in, often another drive when redirected).
pub fn rescue_dir() -> PathBuf {
    if let Some(d) = DIR.lock().ok().and_then(|d| d.clone()) {
        return d.join("rescue-local");
    }
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("NopBar").join("notes-recovery")
}

/// Tests: the notes live in `d` instead.
#[cfg(test)]
pub fn set_dir(d: PathBuf) {
    if let Ok(mut x) = DIR.lock() {
        *x = Some(d);
    }
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

#[cfg(test)]
pub mod mem {
    //! A disk in memory with faults on demand: writes, renames and
    //! removals that fail at random, listings that fail, the power going
    //! after a number of changes. It also checks the one promise of the
    //! design: nothing automatic writes over or removes a file holding
    //! text (but the note's own save, the index, trash past 30 days, and
    //! a recovery copy whose text is in a note).
    use super::Disk;
    use std::collections::BTreeMap;

    #[derive(Default, Clone, Debug)]
    pub struct MemDisk {
        pub files: BTreeMap<(String, String), String>,
        pub fail_writes: bool,
        /// Only replacing a file fails (a note's save).
        pub fail_replaces: bool,
        pub fail_renames: bool,
        pub fail_removes: bool,
        pub fail_lists: bool,
        /// Every write into this folder fails.
        pub fail_sub: Option<String>,
        /// Only notes.json fails.
        pub fail_index: bool,
        /// Some(n): n more changes, then the power is off.
        pub power: Option<usize>,
        /// Each change fails with this chance (%), on its own; listings
        /// with half of it.
        pub chaos: u64,
        pub rng: std::cell::Cell<u64>,
        /// For the trash rule.
        pub now_secs: u64,
        /// Promises broken.
        pub broken: Vec<String>,
    }

    impl MemDisk {
        fn roll(&self, pct: u64) -> bool {
            if pct == 0 {
                return false;
            }
            let r = self.rng.get().wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.rng.set(r);
            (r >> 33) % 100 < pct
        }

        fn ok(&mut self) -> bool {
            if self.roll(self.chaos) {
                return false;
            }
            match &mut self.power {
                None => true,
                Some(0) => false,
                Some(n) => {
                    *n -= 1;
                    true
                }
            }
        }

        fn has(&self, sub: &str, name: &str) -> bool {
            self.files.contains_key(&(sub.to_string(), name.to_string()))
        }

        /// Every text anywhere.
        pub fn all_texts(&self) -> Vec<String> {
            self.files.iter().filter(|((_, n), _)| n.ends_with(".md")).map(|(_, t)| t.clone()).collect()
        }
    }

    impl Disk for MemDisk {
        fn list(&self, sub: &str) -> Result<Vec<String>, ()> {
            if self.fail_lists || self.roll(self.chaos / 2) {
                return Err(());
            }
            Ok(self.files.keys().filter(|(s, _)| s == sub).map(|(_, n)| n.clone()).collect())
        }
        fn read(&self, sub: &str, name: &str) -> Option<String> {
            self.files.get(&(sub.to_string(), name.to_string())).cloned()
        }
        fn exists(&self, sub: &str, name: &str) -> Option<bool> {
            Some(self.has(sub, name))
        }
        fn replace(&mut self, sub: &str, name: &str, text: &str) -> bool {
            if !sub.is_empty() && self.has(sub, name) {
                self.broken.push(format!("wrote over {sub}\\{name}"));
            }
            if self.fail_writes || self.fail_replaces || self.fail_sub.as_deref() == Some(sub) || (self.fail_index && name == super::INDEX) || !self.ok() {
                return false;
            }
            self.files.insert((sub.to_string(), name.to_string()), text.to_string());
            true
        }
        fn create(&mut self, sub: &str, name: &str, text: &str) -> bool {
            if self.has(sub, name) || self.fail_writes || self.fail_sub.as_deref() == Some(sub) || !self.ok() {
                return false;
            }
            self.files.insert((sub.to_string(), name.to_string()), text.to_string());
            true
        }
        fn rename(&mut self, sub: &str, name: &str, to_sub: &str, to: &str) -> bool {
            if !self.has(sub, name) || self.has(to_sub, to) || self.fail_renames || !self.ok() {
                return false;
            }
            let t = self.files.remove(&(sub.to_string(), name.to_string())).unwrap_or_default();
            self.files.insert((to_sub.to_string(), to.to_string()), t);
            true
        }
        fn remove(&mut self, sub: &str, name: &str) -> bool {
            if let Some(text) = self.files.get(&(sub.to_string(), name.to_string())).cloned() {
                let allowed = match sub {
                    super::TRASH => super::parse_trash(name).is_some_and(|(at, _)| super::expired(at, self.now_secs, super::TRASH_DAYS)),
                    // A copy whose text is in a note.
                    _ => self.files.iter().any(|((s, n), t)| s.is_empty() && n != name && n.ends_with(".md") && *t == text),
                };
                if !allowed {
                    self.broken.push(format!("removed {sub}\\{name} ({text:?})"));
                }
            }
            if self.fail_removes || !self.ok() {
                return false;
            }
            self.files.remove(&(sub.to_string(), name.to_string()));
            true
        }
    }
}

#[cfg(test)]
mod entry_tests {
    use super::*;
    use super::mem::MemDisk;

    #[test]
    fn first_note_is_seeded_once_and_deletion_never_reseeds() {
        let mut disk = MemDisk::default();
        assert!(!load(&mut disk, 1000).index.seeded);
        assert!(seed_first(&mut disk, Place::default(), 1000));
        let first = load(&mut disk, 1001);
        assert!(first.index.seeded);
        assert_eq!(first.notes.len(), 1);
        assert!(first.notes[0].1.is_empty());
        assert!(!seed_first(&mut disk, Place::default(), 1002));
        let id = first.notes[0].0.id.clone();
        let mut doc = Doc { id, text: String::new(), saved: Some(String::new()), kept: None };
        assert!(delete(&mut disk, &mut doc, 2000));
        let after = load(&mut disk, 2001);
        assert!(after.index.seeded && after.notes.is_empty());
        assert!(!seed_first(&mut disk, Place::default(), 2002));
        assert!(load(&mut disk, 2003).notes.is_empty());
    }

    #[test]
    fn legacy_notes_and_old_trash_count_as_history() {
        for old in [r#"{"version":1,"notes":[{"id":"gone"}]}"#, r#"{"version":1,"notes":[]}"#] {
            let mut disk = MemDisk::default();
            assert!(disk.replace("", INDEX, old));
            assert!(disk.create(TRASH, "1-gone.md", "old"));
            let got = load(&mut disk, (TRASH_DAYS as u128 + 1) * 86_400_000);
            assert!(got.index.seeded);
            assert!(!seed_first(&mut disk, Place::default(), 1000));
        }
        let mut disk = MemDisk::default();
        assert!(disk.create("", "existing.md", ""));
        assert!(load(&mut disk, 1000).index.seeded);
        assert!(!seed_first(&mut disk, Place::default(), 1001));
    }

    #[test]
    fn interrupted_seed_and_failed_disk_never_duplicate_the_first_card() {
        let mut disk = MemDisk::default();
        load(&mut disk, 1000);
        disk.fail_writes = true;
        assert!(!seed_first(&mut disk, Place::default(), 1000));
        disk.fail_writes = false;
        disk.fail_index = true;
        assert!(seed_first(&mut disk, Place::default(), 1001));
        disk.fail_index = false;
        assert!(load(&mut disk, 1002).index.seeded);
        assert!(!seed_first(&mut disk, Place::default(), 1003));
        assert_eq!(load(&mut disk, 1004).notes.len(), 1);
        let yes = Index { seeded: true, ..Default::default() };
        assert!(merge(&Index::default(), &Index::default(), &yes, &[]).seeded);
        assert!(merge(&Index::default(), &yes, &Index::default(), &[]).seeded);
        assert!(!layout_done(&yes, &Index::default()));
    }
}

#[cfg(test)]
mod tests {
    use super::mem::MemDisk;
    use super::*;

    fn texts(l: &Loaded) -> Vec<String> {
        let mut v: Vec<String> = l.notes.iter().map(|(_, t)| t.clone()).collect();
        v.sort();
        v
    }

    fn body(d: &MemDisk, id: &str) -> Option<String> {
        d.read("", &body_name(id))
    }

    fn doc(id: &str, text: &str) -> Doc {
        Doc { id: id.into(), text: text.into(), ..Default::default() }
    }

    #[test]
    fn names_round_trip() {
        assert_eq!(parse_body("ab.md"), Some("ab"));
        assert_eq!(parse_body("ab.7.md"), None);
        assert_eq!(parse_body("../x.md"), None);
        assert_eq!(parse_numbered("ab.7.md"), Some(("ab", 7)));
        assert_eq!(recovery_origin(&recovery_name("ab", 123)), Some("ab"));
        assert_eq!(recovery_origin("ab.9.md"), Some("ab"));
        assert_eq!(recovery_origin("ab.9.deleted.md"), Some("ab"));
        assert_eq!(recovery_origin("conflict-5-ab-9.md"), Some("ab"));
        assert_eq!(parse_trash(&trash_name("ab", 100, 0)), Some((100, "ab")));
        assert_eq!(parse_trash(&trash_name("ab", 100, 3)), Some((100, "ab")));
        assert_eq!(parse_trash("100-ab-7.md"), Some((100, "ab")));
        assert!(expired(0, TRASH_DAYS * 86_400, TRASH_DAYS));
    }

    #[test]
    fn a_save_replaces_the_text_under_its_one_name() {
        let mut d = MemDisk::default();
        let mut n = doc("a", "one");
        assert_eq!(n.settle(&mut d, 0), Kept::Saved);
        n.text = "two".into();
        assert!(n.dirty());
        assert_eq!(n.settle(&mut d, 0), Kept::Saved);
        assert!(!n.dirty());
        assert_eq!(body(&d, "a").as_deref(), Some("two"));
        assert_eq!(d.list("").unwrap(), vec!["a.md".to_string()]);
    }

    /// S1: going back to the text the file has needs nothing (no save on
    /// its way that could land later: there is none).
    /// T1: an unreadable editor is never an empty text: the last text
    /// known (captured when its window went) is what is saved.
    #[test]
    fn an_unreadable_editor_keeps_the_last_text() {
        let mut last = String::from("A");
        assert_eq!(text_now(Some("B".into()), &mut last), "B");
        assert_eq!(last, "B");
        assert_eq!(text_now(None, &mut last), "B");
        let mut d = MemDisk::default();
        let mut n = doc("a", "A");
        n.settle(&mut d, 0);
        let mut last = String::from("A typed");
        n.text = text_now(None, &mut last);
        n.settle(&mut d, 1);
        assert_eq!(body(&d, "a").as_deref(), Some("A typed"));
    }

    /// T2: a layout is done only when every note asked for is in what was
    /// written.
    #[test]
    fn a_layout_is_done_only_with_every_note() {
        let e = |id: &str| Entry { id: id.into(), ..Default::default() };
        let asked = Index { version: 1, seeded: false, notes: vec![e("a"), e("b")], last: String::new() };
        assert!(layout_done(&asked, &asked));
        assert!(!layout_done(&asked, &Index { version: 1, seeded: false, notes: vec![e("a")], last: String::new() }));
        // Written for real: a note without its file yet is left out.
        let mut d = MemDisk::default();
        doc("a", "x").settle(&mut d, 0);
        let ix = write_index(&mut d, &asked, &Index::default()).unwrap();
        assert!(!layout_done(&asked, &ix));
    }

    /// 7th review: a layout written without one of its notes (its file
    /// could not be written; its text is in recovery) is not done and
    /// counts for quitting, like a layout that could not be written.
    #[test]
    fn a_partly_written_layout_counts_as_lost() {
        let e = |id: &str| Entry { id: id.into(), ..Default::default() };
        let asked = Index { version: 1, seeded: false, notes: vec![e("a"), e("b")], last: String::new() };
        let mut d = MemDisk::default();
        let mut docs = vec![doc("a", "A")];
        docs[0].settle(&mut d, 0);
        // b's file cannot be written: kept in recovery, left out of notes.json.
        d.fail_sub = None;
        let mut b = doc("b", "B");
        d.fail_replaces = true;
        assert_eq!(b.settle(&mut d, 1), Kept::InRecovery);
        d.fail_replaces = false;
        let s = settle_with_layout(&mut d, &mut docs, Some((&asked, &Index::default())), 2);
        assert!(s.index.is_some() && !s.layout_done);
        assert_eq!(s.lost, vec![LAYOUT.to_string()]);
        // Not written at all: the same.
        d.fail_index = true;
        let s = settle_with_layout(&mut d, &mut docs, Some((&asked, &Index::default())), 3);
        assert_eq!((s.index, s.lost), (None, vec![LAYOUT.to_string()]));
        // Nothing asked: nothing lost.
        assert!(settle_with_layout(&mut d, &mut docs, None, 4).lost.is_empty());
    }

    #[test]
    fn going_back_to_the_saved_text_is_clean() {
        let mut d = MemDisk::default();
        let mut n = doc("a", "A");
        n.settle(&mut d, 0);
        n.text = "B".into();
        n.text = "A".into();
        assert!(!n.dirty());
        assert_eq!(n.settle(&mut d, 0), Kept::Saved);
        assert_eq!(body(&d, "a").as_deref(), Some("A"));
    }

    #[test]
    fn a_failed_save_stays_dirty_and_is_kept_once() {
        let mut d = MemDisk::default();
        let mut n = doc("a", "old");
        n.settle(&mut d, 0);
        n.text = "new".into();
        d.fail_replaces = true;
        assert_eq!(n.settle(&mut d, 5), Kept::InRecovery);
        assert_eq!(n.settle(&mut d, 6), Kept::InRecovery);
        assert!(n.dirty(), "still to be saved");
        assert_eq!(body(&d, "a").as_deref(), Some("old"));
        assert_eq!(d.list(RECOVERY).unwrap().len(), 1, "one copy per text");
        d.fail_replaces = false;
        assert_eq!(n.settle(&mut d, 7), Kept::Saved);
        assert!(d.broken.is_empty());
    }

    /// The second recovery folder, then nothing: Lost (only on screen).
    #[test]
    fn keeping_falls_back_to_the_local_folder() {
        let mut d = MemDisk { fail_replaces: true, fail_sub: Some(RECOVERY.into()), ..Default::default() };
        let mut n = doc("a", "x");
        assert_eq!(n.settle(&mut d, 1), Kept::InRecovery);
        assert_eq!(d.list(RESCUE).unwrap().len(), 1);
        let mut m = doc("b", "y");
        d.fail_writes = true;
        assert_eq!(m.settle(&mut d, 2), Kept::Lost);
        assert_eq!(settle_all(&mut d, &mut [m], 3), vec!["b".to_string()]);
    }

    #[test]
    fn a_recovery_copy_never_goes_over_a_note() {
        let mut d = MemDisk::default();
        let mut n = doc("a", "A");
        n.settle(&mut d, 0);
        write_recovery_to(&mut d, RECOVERY, "a", "B", 1);
        let l = load(&mut d, 3);
        assert_eq!(texts(&l), vec!["A".to_string(), "B".to_string()]);
        assert_eq!(body(&d, "a").as_deref(), Some("A"));
        assert!(d.list(RECOVERY).unwrap().is_empty());
        assert!(d.broken.is_empty(), "{:?}", d.broken);
    }

    #[test]
    fn a_recovery_copy_like_its_note_is_just_removed() {
        let mut d = MemDisk::default();
        doc("a", "same").settle(&mut d, 0);
        write_recovery_to(&mut d, RESCUE, "a", "same", 1);
        let l = load(&mut d, 3);
        assert_eq!(texts(&l), vec!["same".to_string()]);
        assert!(d.list(RESCUE).unwrap().is_empty());
        assert!(d.broken.is_empty());
    }

    #[test]
    fn a_failed_listing_changes_nothing() {
        let mut d = MemDisk::default();
        doc("a", "C").settle(&mut d, 0);
        write_recovery_to(&mut d, RECOVERY, "a", "B", 1);
        let before = d.files.clone();
        d.fail_lists = true;
        assert!(load(&mut d, 2).notes.is_empty());
        assert!(write_index(&mut d, &Index::default(), &Index::default()).is_none());
        assert_eq!(d.files, before, "nothing touched");
        d.fail_lists = false;
        assert_eq!(texts(&load(&mut d, 3)), vec!["B".to_string(), "C".to_string()]);
        assert!(d.broken.is_empty());
    }

    #[test]
    fn a_failed_import_keeps_the_copy() {
        let mut d = MemDisk::default();
        let mut n = doc("a", "C");
        n.settle(&mut d, 0);
        write_recovery_to(&mut d, RECOVERY, "a", "B", 1);
        d.fail_writes = true;
        assert_eq!(texts(&load(&mut d, 2)), vec!["C".to_string()]);
        assert_eq!(d.list(RECOVERY).unwrap().len(), 1);
        d.fail_writes = false;
        n.text = "D".into();
        n.settle(&mut d, 3);
        assert_eq!(d.list(RECOVERY).unwrap().len(), 1, "a save never touches recovery");
        assert_eq!(texts(&load(&mut d, 4)), vec!["B".to_string(), "D".to_string()]);
        assert!(d.broken.is_empty());
    }

    /// S2: delete and restore happen now; nothing arrives later to delete
    /// the note again.
    #[test]
    fn delete_and_restore_happen_at_once() {
        let mut d = MemDisk::default();
        let mut n = doc("a", "text");
        n.settle(&mut d, 0);
        n.text = "latest".into();
        assert!(delete(&mut d, &mut n, 50_000), "saved first, then moved");
        assert!(body(&d, "a").is_none());
        assert_eq!(d.read(TRASH, &trash_name("a", 50, 0)).as_deref(), Some("latest"));
        let back = restore(&mut d, "a", "latest", 51_000).expect("back");
        assert!(!back.dirty());
        assert_eq!(body(&d, "a").as_deref(), Some("latest"));
        assert!(d.list(TRASH).unwrap().is_empty());
        assert_eq!(texts(&load(&mut d, 52_000)), vec!["latest".to_string()]);
        assert!(d.broken.is_empty());
    }

    #[test]
    fn a_failed_delete_or_restore_changes_nothing() {
        let mut d = MemDisk::default();
        let mut n = doc("a", "text");
        n.settle(&mut d, 0);
        d.fail_renames = true;
        assert!(!delete(&mut d, &mut n, 50_000));
        assert_eq!(body(&d, "a").as_deref(), Some("text"));
        d.fail_renames = false;
        assert!(delete(&mut d, &mut n, 50_000));
        // Neither the move back nor a write works: still deleted.
        d.fail_renames = true;
        d.fail_writes = true;
        assert!(restore(&mut d, "a", "text", 51_000).is_none());
        assert_eq!(d.list(TRASH).unwrap().len(), 1);
        // The move back fails but the text can be written: back.
        d.fail_writes = false;
        assert!(restore(&mut d, "a", "text", 52_000).is_some());
        assert_eq!(body(&d, "a").as_deref(), Some("text"));
        assert!(d.broken.is_empty());
    }

    /// A delete whose text cannot be kept anywhere does not happen.
    #[test]
    fn a_delete_never_loses_the_latest_text() {
        let mut d = MemDisk::default();
        let mut n = doc("a", "saved");
        n.settle(&mut d, 0);
        n.text = "typed".into();
        d.fail_writes = true;
        d.fail_replaces = true;
        assert!(!delete(&mut d, &mut n, 1));
        assert_eq!(body(&d, "a").as_deref(), Some("saved"));
        assert!(d.list(TRASH).unwrap().is_empty());
    }

    #[test]
    fn the_trash_expires_after_thirty_days_only() {
        let mut d = MemDisk::default();
        let mut n = doc("a", "bye");
        n.settle(&mut d, 0);
        delete(&mut d, &mut n, 1_000_000);
        d.now_secs = 1000 + 29 * 86_400;
        let now = d.now_secs as u128 * 1000;
        load(&mut d, now);
        assert_eq!(d.list(TRASH).unwrap().len(), 1);
        d.now_secs = 1000 + 30 * 86_400;
        let now = d.now_secs as u128 * 1000;
        load(&mut d, now);
        assert!(d.list(TRASH).unwrap().is_empty());
        assert!(d.broken.is_empty());
    }

    #[test]
    fn texts_and_entries_settle_without_removing_anything() {
        let mut d = MemDisk::default();
        d.files.insert((String::new(), "a.md".into()), "found".into());
        let ix = Index { version: 1, seeded: false, notes: vec![Entry { id: "gone".into(), ..Default::default() }, Entry { id: "a".into(), place: Place { x: 7.0, ..Place::default() }, desktop: None, size:None, hidden:false }], last: String::new() };
        d.files.insert((String::new(), INDEX.into()), index_text(&ix));
        let l = load(&mut d, 0);
        assert_eq!(l.notes.len(), 1);
        assert_eq!(l.notes[0].0.place.x, 7.0);
        assert_eq!(l.index.notes.len(), 1, "an entry without its text is left out");
        d.files.insert((String::new(), "b.md".into()), "new".into());
        let ix = write_index(&mut d, &Index::default(), &l.index).expect("written");
        let listed: Vec<String> = ix.notes.into_iter().map(|e| e.id).collect();
        assert_eq!(listed, vec!["a".to_string(), "b".to_string()]);
        assert!(d.broken.is_empty());
    }

    #[test]
    fn the_earlier_form_moves_in_without_writing_over() {
        let mut d = MemDisk::default();
        d.files.insert((String::new(), "a.3.md".into()), "three".into());
        d.files.insert((String::new(), "a.2.md".into()), "two".into());
        d.files.insert((String::new(), "b.md".into()), "b".into());
        d.files.insert((String::new(), "b.4.md".into()), "b".into());
        d.files.insert((RECOVERY.into(), "c.5.md".into()), "c".into());
        d.files.insert((RECOVERY.into(), "c.6.deleted.md".into()), "gone".into());
        d.files.insert((RECOVERY.into(), "conflict-1-a-1.md".into()), "kept".into());
        d.files.insert((RESCUE.into(), "a.99.md".into()), "rescued".into());
        let l = load(&mut d, 7_000);
        assert_eq!(texts(&l), vec!["b".to_string(), "c".to_string(), "kept".to_string(), "rescued".to_string(), "three".to_string(), "two".to_string()]);
        assert_eq!(body(&d, "a").as_deref(), Some("three"));
        assert!(d.list(RECOVERY).unwrap().is_empty() && d.list(RESCUE).unwrap().is_empty());
        assert_eq!(d.list(TRASH).unwrap().len(), 1);
        assert!(d.broken.is_empty(), "{:?}", d.broken);
    }

    #[test]
    fn the_real_disk_replaces_in_place() {
        let dir = std::env::temp_dir().join(format!("nopbar-notes-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut d = RealDisk { base: dir.clone(), rescue: dir.join("rescue-local") };
        let mut n = doc("n", "- [ ] 우유");
        assert_eq!(n.settle(&mut d, 0), Kept::Saved);
        n.text = "빵".into();
        assert_eq!(n.settle(&mut d, 0), Kept::Saved);
        assert!(!d.create("", "n.md", "never"), "create never goes over a file");
        assert!(keep(&mut d, "n", "최신", 5));
        let mut names = d.list("").unwrap();
        names.sort();
        assert_eq!(names, vec!["n.md".to_string()], "no temporary file left");
        assert_eq!(texts(&load(&mut d, 10)), vec!["빵".to_string(), "최신".to_string()]);
        assert!(delete(&mut d, &mut n, 20_000));
        assert!(!dir.join("n.md").exists());
        assert!(restore(&mut d, "n", "빵", 21_000).is_some());
        assert_eq!(std::fs::read_to_string(dir.join("n.md")).unwrap(), "빵");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each note's desktop is kept in notes.json; an older file without it
    /// means every desktop; a note the layout does not know keeps what the
    /// file says (read again before the write).
    #[test]
    fn the_desktop_of_each_note_is_kept() {
        let old = r#"{"version":1,"notes":[{"id":"a","place":{"x":3.0}}],"last":"a"}"#;
        let ix = parse_index(old);
        assert_eq!(ix.notes[0].desktop, None, "an older notes.json: every desktop");
        assert_eq!(ix.notes[0].place.x, 3.0);
        let g = "{01010101-0101-0101-0101-010101010101}".to_string();
        let mut d = MemDisk::default();
        doc("a", "A").settle(&mut d, 0);
        doc("b", "B").settle(&mut d, 0);
        let e = |id: &str, desk: Option<&String>| Entry { id: id.into(), desktop: desk.cloned(), ..Default::default() };
        let asked = Index { version: 1, seeded: false, notes: vec![e("a", Some(&g)), e("b", None)], last: String::new() };
        let ix = write_index(&mut d, &asked, &Index::default()).unwrap();
        assert_eq!(ix, Index { version: 1, seeded: true, ..asked.clone() });
        let text = d.read("", INDEX).unwrap();
        assert!(text.contains(&g));
        assert_eq!(text.matches("desktop").count(), 1, "nothing written for every desktop: {text}");
        assert_eq!(parse_index(&text).notes, asked.notes);
        // A layout that lacks a (and an old one that lacks it too): the file's
        // entry, read again, keeps its desktop and place.
        let ix = write_index(&mut d, &Index { version: 1, seeded: false, notes: vec![e("b", None)], last: String::new() }, &Index::default()).unwrap();
        assert_eq!(ix.notes.iter().find(|n| n.id == "a").and_then(|n| n.desktop.clone()), Some(g.clone()));
        // The layout's own word wins over the file's.
        let ix = write_index(&mut d, &Index { version: 1, seeded: false, notes: vec![e("a", None)], last: String::new() }, &Index::default()).unwrap();
        assert_eq!(ix.notes.iter().find(|n| n.id == "a").unwrap().desktop, None);
        // Loaded with it.
        write_index(&mut d, &asked, &Index::default()).unwrap();
        let l = load(&mut d, 5);
        assert_eq!(l.notes.iter().map(|(e, _)| e.desktop.clone()).collect::<Vec<_>>(), vec![Some(g), None]);
        assert!(d.broken.is_empty());
    }

    /// Review 2: notes.json changed by someone else between two writes
    /// (a sync): what this thread did not change since its last write
    /// stays as the file has it, for its own notes and for the others.
    #[test]
    fn a_write_keeps_what_others_changed_in_notes_json() {
        let (ga, gb) = ("{0A}".to_string(), "{0B}".to_string());
        let mut d = MemDisk::default();
        for id in ["a", "x"] {
            doc(id, id).settle(&mut d, 0);
        }
        let e = |id: &str, x: f32, desk: Option<&String>| Entry { id: id.into(), place: Place { x, ..Place::default() }, desktop: desk.cloned(), size:None, hidden:false };
        let ix = |notes: Vec<Entry>, last: &str| Index { version: 1, seeded: false, notes, last: last.into() };
        // The file has x (a note this thread does not show) on A.
        d.files.insert((String::new(), INDEX.into()), index_text(&ix(vec![e("x", 5.0, Some(&ga))], "x")));
        let mut mine = Index::default();
        let asked = ix(vec![e("a", 1.0, None)], "a");
        let w = write_index(&mut d, &asked, &mine).unwrap();
        mine = asked.clone();
        assert_eq!(w.notes, vec![e("a", 1.0, None), e("x", 5.0, Some(&ga))]);
        // Someone moves x to B and a to another place and desktop.
        d.files.insert((String::new(), INDEX.into()), index_text(&ix(vec![e("a", 9.0, Some(&gb)), e("x", 7.0, Some(&gb))], "x")));
        // This thread changes only a's desktop (its menu).
        let asked = ix(vec![e("a", 1.0, Some(&ga))], "a");
        let w = write_index(&mut d, &asked, &mine).unwrap();
        assert_eq!(w.notes, vec![e("a", 9.0, Some(&ga)), e("x", 7.0, Some(&gb))], "only the field changed here is written over");
        assert_eq!(w.last, "x", "nor the last note used, unchanged here");
        assert_eq!(parse_index(&d.read("", INDEX).unwrap()), w);
        // Nothing changed here at all: the file stays as it is.
        let before = d.read("", INDEX).unwrap();
        mine = asked.clone();
        let w = write_index(&mut d, &asked, &mine).unwrap();
        assert_eq!(index_text(&w), before);
        assert!(d.broken.is_empty());
    }

    /// Review 2: a notes.json that cannot be read is not written over (the
    /// layout is tried again); a missing one is written; a broken one is
    /// written over from this thread's own layout.
    #[test]
    fn notes_json_unreadable_is_not_written_over() {
        struct Unreadable(MemDisk);
        impl Disk for Unreadable {
            fn list(&self, sub: &str) -> Result<Vec<String>, ()> {
                self.0.list(sub)
            }
            fn read(&self, sub: &str, name: &str) -> Option<String> {
                (name != INDEX).then(|| self.0.read(sub, name)).flatten()
            }
            fn exists(&self, sub: &str, name: &str) -> Option<bool> {
                if name == INDEX {
                    None
                } else {
                    self.0.exists(sub, name)
                }
            }
            fn replace(&mut self, sub: &str, name: &str, text: &str) -> bool {
                self.0.replace(sub, name, text)
            }
            fn create(&mut self, sub: &str, name: &str, text: &str) -> bool {
                self.0.create(sub, name, text)
            }
            fn rename(&mut self, sub: &str, name: &str, to_sub: &str, to: &str) -> bool {
                self.0.rename(sub, name, to_sub, to)
            }
            fn remove(&mut self, sub: &str, name: &str) -> bool {
                self.0.remove(sub, name)
            }
        }
        let mut m = MemDisk::default();
        doc("a", "A").settle(&mut m, 0);
        m.files.insert((String::new(), INDEX.into()), "{\"version\":1,\"notes\":[{\"id\":\"a\",\"desktop\":\"{0A}\"}]}".into());
        let asked = Index { version: 1, seeded: false, notes: vec![Entry { id: "a".into(), ..Default::default() }], last: String::new() };
        let mut u = Unreadable(m);
        let before = u.0.files.clone();
        assert!(write_index(&mut u, &asked, &Index::default()).is_none());
        assert_eq!(u.0.files, before, "not written over");
        let s = settle_with_layout(&mut u, &mut [], Some((&asked, &Index::default())), 1);
        assert_eq!(s.lost, vec![LAYOUT.to_string()], "the layout counts as not written (retried, quitting waits)");
        // Missing: written.
        let mut m = u.0;
        m.files.remove(&(String::new(), INDEX.to_string()));
        assert!(write_index(&mut m, &asked, &Index::default()).is_some());
        // Broken (2nd review): moved aside, kept, and only then a new one
        // from this thread's layout.
        let broken = "{\"notes\": [{\"id\": \"x\", \"desktop\": \"{0B}\"}, 7".to_string();
        m.files.insert((String::new(), INDEX.into()), broken.clone());
        let mine = Index { version: 1, seeded: false, notes: vec![Entry { id: "a".into(), desktop: Some("{0A}".into()), ..Default::default() }], last: String::new() };
        let w = write_index_at(&mut m, &mine, &mine, 77).unwrap();
        assert_eq!(w.notes[0].desktop.as_deref(), Some("{0A}"));
        assert_eq!(m.read("", &broken_name(77, 0)).as_deref(), Some(broken.as_str()), "the broken file is kept whole");
        // It cannot be moved aside: nothing is written over it.
        m.files.insert((String::new(), INDEX.into()), broken.clone());
        m.fail_renames = true;
        assert!(write_index_at(&mut m, &mine, &mine, 78).is_none());
        assert_eq!(m.read("", INDEX).as_deref(), Some(broken.as_str()));
        let s = settle_with_layout(&mut m, &mut [], Some((&mine, &mine)), 79);
        assert_eq!(s.lost, vec![LAYOUT.to_string()], "retried; the user's quit waits");
        assert_eq!(m.read("", INDEX).as_deref(), Some(broken.as_str()));
        // Starting with it: the same (kept aside, or left alone).
        assert!(load(&mut m, 80).notes.len() == 1);
        assert_eq!(m.read("", INDEX).as_deref(), Some(broken.as_str()), "a start does not write over it either");
        m.fail_renames = false;
        let l = load(&mut m, 81);
        assert_eq!(l.notes.len(), 1);
        assert_eq!(m.read("", &broken_name(81, 0)).as_deref(), Some(broken.as_str()));
        assert!(try_parse_index(&m.read("", INDEX).unwrap()).is_some(), "a new one written");
        // Two broken copies at once never take each other's name.
        assert_eq!(broken_name(81, 1), format!("{INDEX}.broken-81~1"));
        assert!(m.broken.is_empty(), "{:?}", m.broken);
    }

    /// A notes.json there but unreadable is not written over at start.
    #[test]
    fn a_start_never_writes_over_an_unreadable_notes_json() {
        let mut d = MemDisk::default();
        doc("a", "A").settle(&mut d, 0);
        d.files.insert((String::new(), INDEX.into()), "{\"version\":1,\"notes\":[{\"id\":\"a\",\"desktop\":\"{0A}\"}]}".into());
        let before = d.read("", INDEX);
        struct NoIndex<'a>(&'a mut MemDisk);
        impl Disk for NoIndex<'_> {
            fn list(&self, sub: &str) -> Result<Vec<String>, ()> {
                self.0.list(sub)
            }
            fn read(&self, sub: &str, name: &str) -> Option<String> {
                (name != INDEX).then(|| self.0.read(sub, name)).flatten()
            }
            fn exists(&self, sub: &str, name: &str) -> Option<bool> {
                if name == INDEX {
                    None
                } else {
                    self.0.exists(sub, name)
                }
            }
            fn replace(&mut self, sub: &str, name: &str, text: &str) -> bool {
                self.0.replace(sub, name, text)
            }
            fn create(&mut self, sub: &str, name: &str, text: &str) -> bool {
                self.0.create(sub, name, text)
            }
            fn rename(&mut self, sub: &str, name: &str, to_sub: &str, to: &str) -> bool {
                self.0.rename(sub, name, to_sub, to)
            }
            fn remove(&mut self, sub: &str, name: &str) -> bool {
                self.0.remove(sub, name)
            }
        }
        let l = load(&mut NoIndex(&mut d), 5);
        assert_eq!(l.notes.len(), 1);
        assert_eq!(d.read("", INDEX), before);
    }

    #[test]
    fn ids_are_file_safe_and_distinct() {
        let a = new_id(1_700_000_000_000, 0);
        let b = new_id(1_700_000_000_000, 1);
        assert!(valid_id(&a) && valid_id(&b) && a != b);
        assert!(!valid_id("a/b") && !valid_id("..") && !valid_id("a.b") && !valid_id("a-b") && !valid_id(""));
    }
}

#[cfg(test)] mod widget_hide_tests {
    use super::*;
    #[test] fn hidden_memo_round_trip_and_field_merge_preserve_text_identity(){
        let old:Index=serde_json::from_str(r#"{"notes":[{"id":"kept"}]}"#).unwrap();assert!(!old.notes[0].hidden);
        let mut asked=old.clone();asked.notes[0].hidden=true;
        let mut disk=old.clone();disk.notes[0].place.x=17.;
        let merged=merge(&asked,&old,&disk,&["kept".to_string()]);
        assert!(merged.notes[0].hidden);assert_eq!(merged.notes[0].place.x,17.);assert_eq!(merged.notes[0].id,"kept");
        let round:Index=serde_json::from_str(&serde_json::to_string(&merged).unwrap()).unwrap();assert_eq!(merged,round);
        let mut restored=merged.clone();restored.notes[0].hidden=false;
        assert!(!merge(&restored,&merged,&merged,&["kept".to_string()]).notes[0].hidden);
    }
}

#[cfg(test)] mod widget_review_tests {
 use super::*;
 use crate::desklayer::widget::Size;
 #[test] fn review_02_size_roundtrip_and_fieldwise_merge_preserve_large(){
  let legacy:Entry=serde_json::from_str(r#"{"id":"a"}"#).unwrap();assert_eq!(legacy.size,None);
  let e=Entry{size:Some(Size::LARGE),..legacy};
  assert_eq!(serde_json::from_str::<Entry>(&serde_json::to_string(&e).unwrap()).unwrap().size,Some(Size::LARGE));
  let mine=Index{notes:vec![e],..Default::default()};let mut asked=mine.clone();asked.notes[0].hidden=true;
  let mut disk=mine.clone();disk.notes[0].size=Some(Size::MEDIUM);
  assert_eq!(merge(&asked,&mine,&disk,&["a".into()]).notes[0].size,Some(Size::MEDIUM));
  asked.notes[0].size=Some(Size::SMALL);
  assert_eq!(merge(&asked,&mine,&disk,&["a".into()]).notes[0].size,Some(Size::SMALL));
 }
}
