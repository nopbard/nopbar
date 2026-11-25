//! Screenshots in the Live Island (opt-in: Settings > Top bar, off by
//! default). When a new screenshot lands on the clipboard (Win+Shift+S,
//! Print Screen, Snipping Tool or another capture tool), its thumbnail
//! shows in the island for a few seconds: drag it out as a PNG file, save
//! it to Pictures\Screenshots, copy it again, or open it in Windows' image
//! editor (Paint unless the user chose another).
//!
//! Privacy: nothing runs unless the setting is on. Then a message window
//! is told by Windows when the clipboard changes (AddClipboardFormatListener:
//! event-driven, no polling); only a picture put there by a capture tool
//! (or by Print Screen itself) is read, on a worker thread, and kept in
//! memory while the island shows it. Nothing is uploaded or logged (the
//! log says "a screenshot", never what is in it). A file is written only
//! when asked: dragged, saved or opened in the editor; the temporary ones
//! (%TEMP%\NopBar\Screenshots) are deleted after 24 hours. NopBar's own
//! clipboard writes ("Copy") are never taken for a new screenshot.
//!
//! This file: the pure part (the debounce and the own-write rule, which
//! apps count as capture tools, file names, the cleanup policy, the BMP
//! wrapper for clipboard DIBs), tested. run.rs: the window, the worker and
//! the actions; image.rs: the clipboard and WIC.

pub(crate) mod image;
mod run;

use serde::{Deserialize, Serialize};

pub use run::{act, busy, drag_out, set_enabled, shutdown, state, thumb_for};

/// Wait this long after the last clipboard change of a burst before
/// looking (a capture tool writes several formats one after the other).
pub const DEBOUNCE_MS: u64 = 350;
/// How long the island shows a screenshot (longer while its card is out).
pub const SHOW_MS: u32 = 6000;
/// Temporary screenshot files older than this are deleted.
pub const KEEP_SECS: u64 = 24 * 60 * 60;
/// Clipboard pictures bigger than this are not read.
pub const MAX_BYTES: usize = 256 << 20;
/// The thumbnail box (px).
pub const THUMB_W: u32 = 192;
pub const THUMB_H: u32 = 120;

/// What the clipboard holds when the burst is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Look {
    /// A picture (PNG, DIB).
    pub image: bool,
    /// Put there by a capture tool (or by Windows' Print Screen).
    pub capture: bool,
}

/// Decides when a clipboard change is a new screenshot. Pure: the caller
/// gives the times, the sequence numbers and what the clipboard holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detector {
    /// When the burst of changes counts as over (ms).
    due: Option<u64>,
    /// The sequence number last decided on (each is decided once).
    last: Option<u32>,
    /// The sequence number of our own last write.
    own: Option<u32>,
    /// We are writing the clipboard right now.
    writing: bool,
}

impl Detector {
    /// The clipboard changed at `now` (ms). Returns how long to wait.
    pub fn changed(&mut self, now: u64) -> u64 {
        self.due = Some(now + DEBOUNCE_MS);
        DEBOUNCE_MS
    }

    /// The wait is over at `now`? Err(ms): a later change pushed it back.
    pub fn ready(&mut self, now: u64) -> Result<(), u64> {
        match self.due {
            Some(d) if d > now => Err(d - now),
            Some(_) => {
                self.due = None;
                Ok(())
            }
            None => Err(DEBOUNCE_MS),
        }
    }

    /// NopBar starts / ended writing the clipboard (`seq`: the sequence
    /// number once done).
    pub fn writing(&mut self, on: bool, seq: Option<u32>) {
        self.writing = on;
        if let Some(s) = seq {
            self.own = Some(s);
        }
    }

    /// Is the clipboard as it is now (`seq`, `look`) a new screenshot?
    pub fn decide(&mut self, seq: u32, look: Look) -> bool {
        if self.writing {
            // Ours, half written: decided again after our own change note.
            return false;
        }
        if self.last == Some(seq) {
            return false;
        }
        self.last = Some(seq);
        if self.own == Some(seq) {
            return false;
        }
        look.image && look.capture
    }
}

/// Capture tools whose clipboard pictures count as screenshots (file
/// names, lowercase). No owner at all is Windows' own Print Screen.
pub const CAPTURE_TOOLS: [&str; 10] = [
    "screenclippinghost.exe",
    "snippingtool.exe",
    "screensketch.exe",
    "explorer.exe",
    "sharex.exe",
    "greenshot.exe",
    "lightshot.exe",
    "picpick.exe",
    "snagit32.exe",
    "flameshot.exe",
];

/// Does a picture from the app at `exe` (None: no owner) count?
pub fn is_capture_tool(exe: Option<&str>) -> bool {
    match exe {
        None => true,
        Some(p) => {
            let name = p.rsplit(['\\', '/']).next().unwrap_or(p).to_ascii_lowercase();
            CAPTURE_TOOLS.contains(&name.as_str())
        }
    }
}

/// "Screenshot-2026-11-02-143015.png" for a local time.
pub fn file_name(y: u16, mo: u16, d: u16, h: u16, mi: u16, s: u16) -> String {
    format!("Screenshot-{y:04}-{mo:02}-{d:02}-{h:02}{mi:02}{s:02}.png")
}

/// Try `k` for a fresh file name: `name` itself, then "name (2).png",
/// "name (3).png"... (each tried with create_new, never by looking first).
pub fn nth_name(name: &str, k: usize) -> String {
    if k == 0 {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    format!("{stem} ({}){ext}", k + 1)
}

/// A plain file name a screenshot of ours may have (no folders, no drive,
/// our pattern).
pub fn is_our_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.starts_with("screenshot-") && l.ends_with(".png") && !name.contains(['\\', '/', ':']) && !name.contains("..")
}

/// One temporary file NopBar made: its exact name in our folder, when,
/// and its size and modification time (ms) right after we wrote it. Only
/// files in this record are ever deleted, and only while unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Made {
    pub name: String,
    pub created: u64,
    pub size: u64,
    pub modified_ms: u64,
}

/// Reads the record (entries whose name is not plainly ours are dropped;
/// anything unreadable reads as empty).
pub fn parse_made(text: &str) -> Vec<Made> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        made: Vec<serde_json::Value>,
    }
    let Ok(f) = serde_json::from_str::<File>(text.trim_start_matches('\u{feff}')) else { return Vec::new() };
    f.made.into_iter().filter_map(|v| serde_json::from_value::<Made>(v).ok()).filter(|m| is_our_name(&m.name)).collect()
}

pub fn made_text(list: &[Made]) -> String {
    serde_json::to_string_pretty(&serde_json::json!({ "version": 1, "made": list })).unwrap_or_else(|_| r#"{"version":1,"made":[]}"#.into())
}

/// What cleanup does with a recorded file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    Keep,
    Delete,
    /// Gone, or changed since we wrote it (an edit saved over it: the
    /// user's now): the record goes, the file is never deleted.
    Forget,
}

/// The cleanup policy for record `m` at `now` (s): `file` is the file's
/// size and modification time (ms) now, None when it is not there.
pub fn fate(m: &Made, now: u64, file: Option<(u64, u64)>) -> Fate {
    match file {
        None => Fate::Forget,
        Some((size, t)) if size != m.size || t != m.modified_ms => Fate::Forget,
        Some(_) if now >= m.created.saturating_add(KEEP_SECS) => Fate::Delete,
        Some(_) => Fate::Keep,
    }
}

/// The thumbnail size for a `w`x`h` picture: fits THUMB_W x THUMB_H,
/// never larger, at least 1x1. None for an empty picture.
pub fn thumb_size(w: u32, h: u32) -> Option<(u32, u32)> {
    if w == 0 || h == 0 {
        return None;
    }
    let k = (THUMB_W as f64 / w as f64).min(THUMB_H as f64 / h as f64).min(1.0);
    Some((((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1)))
}

/// A clipboard DIB (CF_DIB / CF_DIBV5: a header, maybe masks and a color
/// table, then the pixels) as a .bmp file's bytes, for the image decoder.
/// None when the header does not hold together.
pub fn bmp_file(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |o: usize| dib.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u16_at = |o: usize| dib.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let head = u32_at(0)? as usize;
    if !(head == 40 || head == 52 || head == 56 || head == 108 || head == 124) || dib.len() < head {
        return None;
    }
    let bits = u16_at(14)? as u32;
    let compression = u32_at(16)?;
    let used = u32_at(32)?;
    // BI_BITFIELDS / BI_ALPHABITFIELDS masks follow a plain 40-byte header.
    let masks = match (head, compression) {
        (40, 3) => 12,
        (40, 6) => 16,
        _ => 0,
    };
    let colors = if used > 0 { used.min(256) } else if bits <= 8 { 1 << bits } else { 0 };
    let off = 14 + head + masks + colors as usize * 4;
    if off - 14 > dib.len() {
        return None;
    }
    let size = 14 + dib.len();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(off as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

/// A CF_DIB for `w`x`h` straight BGRA pixels (top-down): a 40-byte header,
/// 32 bits, rows bottom-up as most apps expect.
pub fn dib_of(w: u32, h: u32, bgra: &[u8]) -> Option<Vec<u8>> {
    let row = (w as usize).checked_mul(4)?;
    if bgra.len() != row.checked_mul(h as usize)? || w == 0 || h == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(40 + bgra.len());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(bgra.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for y in (0..h as usize).rev() {
        out.extend_from_slice(&bgra[y * row..(y + 1) * row]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOT: Look = Look { image: true, capture: true };

    #[test]
    fn a_burst_is_decided_once_after_it_settles() {
        let mut d = Detector::default();
        assert_eq!(d.ready(0), Err(DEBOUNCE_MS), "nothing changed");
        // Three notes in a burst: the wait restarts each time.
        d.changed(1000);
        d.changed(1100);
        d.changed(1200);
        assert_eq!(d.ready(1000 + DEBOUNCE_MS), Err(200), "pushed back by the later notes");
        assert_eq!(d.ready(1200 + DEBOUNCE_MS), Ok(()));
        assert!(d.decide(41, SHOT));
        // The same clipboard decided again (a stray note): not twice.
        d.changed(2000);
        assert_eq!(d.ready(2000 + DEBOUNCE_MS), Ok(()));
        assert!(!d.decide(41, SHOT));
        // A new one.
        assert!(d.decide(42, SHOT));
    }

    #[test]
    fn our_own_writes_are_never_a_new_screenshot() {
        let mut d = Detector::default();
        assert!(d.decide(10, SHOT));
        // "Copy": while writing, nothing is decided (and not marked done).
        d.writing(true, None);
        assert!(!d.decide(11, SHOT));
        d.writing(false, Some(12));
        assert!(!d.decide(12, SHOT), "our write");
        // Another app's screenshot after ours is new.
        assert!(d.decide(13, SHOT));
        // Seq 11 was never marked done, but it is ours-in-progress only:
        // later notes carry newer numbers anyway.
        assert!(!d.decide(13, SHOT));
    }

    #[test]
    fn only_capture_pictures_count() {
        let mut d = Detector::default();
        assert!(!d.decide(1, Look { image: false, capture: true }), "text from a capture tool");
        assert!(!d.decide(2, Look { image: true, capture: false }), "a picture copied in a browser");
        assert!(d.decide(3, SHOT));
        assert!(is_capture_tool(None), "Print Screen: no owner");
        assert!(is_capture_tool(Some("C:\\Program Files\\WindowsApps\\Microsoft.ScreenSketch_11\\SnippingTool\\SnippingTool.exe")));
        assert!(is_capture_tool(Some("C:\\Windows\\SystemApps\\X\\ScreenClippingHost.exe")));
        assert!(is_capture_tool(Some("C:\\Program Files\\ShareX\\ShareX.exe")));
        assert!(!is_capture_tool(Some("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe")));
        assert!(!is_capture_tool(Some("C:\\x\\mspaint.exe")));
    }

    #[test]
    fn file_names_are_tried_one_after_another() {
        let n = file_name(2026, 11, 2, 9, 5, 7);
        assert_eq!(n, "Screenshot-2026-11-02-090507.png");
        assert_eq!(nth_name(&n, 0), n);
        assert_eq!(nth_name(&n, 1), "Screenshot-2026-11-02-090507 (2).png");
        assert_eq!(nth_name(&n, 2), "Screenshot-2026-11-02-090507 (3).png");
        assert!(is_our_name(&n) && is_our_name(&nth_name(&n, 5)));
        for bad in ["notes.png", "Screenshot-1.jpg", "..\\Screenshot-1.png", "Screenshot-..png", "C:Screenshot-1.png", "sub/Screenshot-1.png"] {
            assert!(!is_our_name(bad), "{bad}");
        }
    }

    #[test]
    fn only_recorded_unchanged_files_are_ever_deleted() {
        let m = Made { name: "Screenshot-2026-11-01-120000.png".into(), created: 1000, size: 50, modified_ms: 7 };
        let same = Some((50, 7));
        assert_eq!(fate(&m, 1000 + KEEP_SECS - 1, same), Fate::Keep, "less than a day old");
        assert_eq!(fate(&m, 1000 + KEEP_SECS, same), Fate::Delete);
        assert_eq!(fate(&m, 1000 + KEEP_SECS * 9, None), Fate::Forget, "gone already");
        assert_eq!(fate(&m, 1000 + KEEP_SECS * 9, Some((51, 7))), Fate::Forget, "edited: the user's now");
        assert_eq!(fate(&m, 1000 + KEEP_SECS * 9, Some((50, 8))), Fate::Forget, "saved over");
        assert_eq!(fate(&Made { created: u64::MAX, ..m.clone() }, u64::MAX - 1, same), Fate::Keep, "no overflow");
        // The record round-trips; entries that are not plainly ours drop.
        assert_eq!(parse_made(&made_text(std::slice::from_ref(&m))), vec![m.clone()]);
        let bad = r#"{"version":1,"made":[{"name":"..\\x\\Screenshot-1.png","created":1,"size":1,"modified_ms":1},{"name":"family.png","created":1,"size":1,"modified_ms":1},{"name":"Screenshot-2.png","created":1,"size":2,"modified_ms":3},{"oops":1}]}"#;
        assert_eq!(parse_made(bad).iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["Screenshot-2.png"]);
        assert!(parse_made("garbage").is_empty() && parse_made("").is_empty());
    }

    #[test]
    fn dibs_become_bmp_files_and_back() {
        // 2x1, 32 bits, plain header.
        let px = [1u8, 2, 3, 255, 4, 5, 6, 255];
        let dib = dib_of(2, 1, &px).unwrap();
        assert_eq!(dib.len(), 40 + 8);
        let bmp = bmp_file(&dib).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize, bmp.len());
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 14 + 40, "pixels right after the header");
        // Bit fields: three masks after a 40-byte header.
        let mut bf = dib.clone();
        bf[16..20].copy_from_slice(&3u32.to_le_bytes());
        bf.splice(40..40, [0u8; 12]);
        assert_eq!(u32::from_le_bytes(bmp_file(&bf).unwrap()[10..14].try_into().unwrap()), 14 + 40 + 12);
        // 8 bits: a 256-color table.
        let mut pal = vec![0u8; 40 + 1024 + 4];
        pal[0..4].copy_from_slice(&40u32.to_le_bytes());
        pal[14..16].copy_from_slice(&8u16.to_le_bytes());
        assert_eq!(u32::from_le_bytes(bmp_file(&pal).unwrap()[10..14].try_into().unwrap()), 14 + 40 + 1024);
        // Nonsense.
        assert!(bmp_file(&[1, 2, 3]).is_none());
        assert!(bmp_file(&[7u8; 64]).is_none());
        // Rows go bottom-up.
        let two = dib_of(1, 2, &[1, 1, 1, 1, 2, 2, 2, 2]).unwrap();
        assert_eq!(&two[40..], &[2, 2, 2, 2, 1, 1, 1, 1]);
        assert!(dib_of(2, 2, &px).is_none(), "wrong length");
        assert_eq!(thumb_size(3840, 2160), Some((192, 108)));
        assert_eq!(thumb_size(100, 50), Some((100, 50)), "never larger");
        assert_eq!(thumb_size(0, 5), None);
    }
}
