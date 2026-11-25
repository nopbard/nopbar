//! Image pieces on disk: each picture is `%APPDATA%\NopBar\pins\<id>.png`,
//! and `pins\pieces.json` lists them with where they sit. Written whole
//! (desklayer::disk::Store) when a piece is made, moved, resized or deleted
//! by the user; nothing else writes or removes them. A list that does not
//! read (or is newer) is never written over: no piece is made or moved
//! until it reads. A listed picture that is not there is skipped (its line
//! stays); a picture not listed is left alone.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::desklayer::disk::Store;
use crate::desklayer::place::Place;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub place: Place,
    /// Cut from the screen: placed against its whole display, at its own
    /// px (place::to_px_screen), not pushed into the work area.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub screen: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Index {
    pub version: u32,
    pub pieces: Vec<Entry>,
}

impl Default for Index {
    fn default() -> Self {
        Index { version: 1, pieces: Vec::new() }
    }
}

impl Index {
    pub fn add(&mut self, e: Entry) {
        self.pieces.retain(|p| p.id != e.id);
        self.pieces.push(e);
    }

    /// Moves or resizes `id`. False when it is not listed.
    pub fn set_place(&mut self, id: &str, p: Place) -> bool {
        match self.pieces.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.place = p;
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.pieces.retain(|p| p.id != id);
    }
}

/// Pure: a new piece's id from the time (ms) and a counter.
pub fn new_id(now_ms: u128, n: u32) -> String {
    format!("p{now_ms}-{n}")
}

/// Pure: is `id` one of ours (no path tricks in a file name)? Lines with
/// other ids are left in the list but never opened.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn dir() -> PathBuf {
    crate::desklayer::disk::dir().join("pins")
}

pub fn picture(id: &str) -> PathBuf {
    dir().join(format!("{id}.png"))
}

/// The list, read (see the module notes).
pub fn open() -> Store<Index> {
    Store::open(dir().join("pieces.json"), |i: &Index| i.version, 1)
}

/// Writes a new piece's picture (never over another file).
pub fn write_picture(id: &str, png: &[u8]) -> bool {
    let p = picture(id);
    if !valid_id(id) || p.exists() {
        return false;
    }
    match crate::desklayer::disk::write_atomic(&p, png) {
        Ok(()) => true,
        Err(e) => {
            crate::log!("pieces: picture not saved ({e})");
            false
        }
    }
}

/// Writes new piece `id`: its picture first (a stray from before its
/// first byte), then its line in the list (read again right before and
/// changed on what it holds). True only when both were saved; else
/// nothing of it is left (the picture goes again, or stays a stray to try
/// again).
pub fn save_new(st: &mut Store<Index>, id: &str, png: &[u8], p: Place, screen: bool) -> bool {
    use crate::desklayer::disk;
    let pic = picture(id);
    disk::stray(&pic);
    if !write_picture(id, png) {
        // Not written (never over another file): nothing of ours there.
        disk::keep(&pic);
        return false;
    }
    if !st.update(|i| i.add(Entry { id: id.to_string(), place: p, screen })) {
        if disk::remove_gone(&pic) {
            disk::keep(&pic);
        }
        return false;
    }
    disk::keep(&pic);
    true
}

/// Pure: a piece's smallest size while resized (DIP) for its shape
/// `ratio` (w / h): `side` on its longer side, the shape kept (a thin
/// piece cut from the screen stays thin).
pub fn min_track(ratio: f32, side: f32) -> (f32, f32) {
    if !ratio.is_finite() || ratio <= 0.0 {
        return (side, side);
    }
    if ratio >= 1.0 {
        (side, (side / ratio).max(1.0))
    } else {
        ((side * ratio).max(1.0), side)
    }
}

/// Pure: a new piece's size (DIP) for a `w`x`h` px picture on a display
/// of `scale`: its own size, at most `max` DIP on its longer side, at
/// least `min` on its shorter, the shape kept.
pub fn first_size(w: u32, h: u32, scale: f32, max: f32, min: f32) -> (f32, f32) {
    if w == 0 || h == 0 {
        return (max, max);
    }
    let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let (dw, dh) = (w as f32 / s, h as f32 / s);
    let down = (max / dw.max(dh)).min(1.0);
    let up = (min / dw.min(dh)).max(1.0);
    let k = if down < 1.0 { down } else { up };
    ((dw * k).max(1.0), (dh * k).max(1.0))
}

/// Pure: the size of a piece being resized to `want` (DIP) keeping the
/// picture's shape `ratio` (w / h): the side moved most leads.
pub fn keep_shape(want: (f32, f32), ratio: f32, width_leads: bool) -> (f32, f32) {
    if !ratio.is_finite() || ratio <= 0.0 {
        return want;
    }
    if width_leads {
        (want.0, want.0 / ratio)
    } else {
        (want.1 * ratio, want.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desklayer::disk::ReadState;

    #[test]
    fn the_index_edits_and_ids() {
        let mut i = Index::default();
        i.add(Entry { id: new_id(1234, 1), place: Place::default(), screen: false });
        i.add(Entry { id: "p9-2".into(), place: Place { x: 5.0, ..Default::default() }, screen: false });
        assert!(i.set_place("p9-2", Place { x: 7.0, ..Default::default() }));
        assert!(!i.set_place("nope", Place::default()));
        assert_eq!(i.pieces[0].id, "p1234-1");
        assert_eq!(i.pieces[1].place.x, 7.0);
        i.remove("p9-2");
        assert_eq!(i.pieces.len(), 1);
        assert!(valid_id("p17-3") && !valid_id("") && !valid_id("a/b") && !valid_id("a.png") && !valid_id("..\\evil"));
    }

    #[test]
    fn pictures_are_written_beside_the_list_never_over_one() {
        let d = std::env::temp_dir().join(format!("nopbar-pieces-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        crate::desklayer::disk::set_test_dir(Some(d.clone()));
        assert!(write_picture("p1-1", b"png"));
        assert!(!write_picture("p1-1", b"other"), "never over a file");
        assert_eq!(std::fs::read(picture("p1-1")).unwrap(), b"png");
        assert!(picture("p1-1").starts_with(d.join("pins")));
        let mut s = open();
        assert_eq!(s.state, ReadState::Missing);
        assert!(s.update(|i| i.add(Entry { id: "p1-1".into(), place: Place::default(), screen: false })));
        assert_eq!(open().data, s.data);
        crate::desklayer::disk::set_test_dir(None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_list_that_does_not_read_is_never_written_over() {
        let d = std::env::temp_dir().join(format!("nopbar-pieces-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        crate::desklayer::disk::set_test_dir(Some(d.clone()));
        std::fs::create_dir_all(dir()).unwrap();
        std::fs::write(dir().join("pieces.json"), "{half").unwrap();
        let mut s = open();
        assert_eq!(s.state, ReadState::Unreadable);
        // A new piece's line is refused: the list stays as it was.
        assert!(!s.update(|i| i.add(Entry { id: "p1-1".into(), place: Place::default(), screen: false })));
        assert_eq!(std::fs::read_to_string(dir().join("pieces.json")).unwrap(), "{half");
        crate::desklayer::disk::set_test_dir(None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_piece_is_its_picture_then_its_line() {
        let d = std::env::temp_dir().join(format!("nopbar-pieces-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        crate::desklayer::disk::set_test_dir(Some(d.clone()));
        let mut s = open();
        let p = Place { mon: [-1920, 0, 0, 1080], x: 10.0, y: 20.0, w: 30.0, h: 40.0 };
        assert!(save_new(&mut s, "p5-1", b"png", p, true));
        assert_eq!(std::fs::read(picture("p5-1")).unwrap(), b"png");
        assert_eq!(open().data.pieces, vec![Entry { id: "p5-1".into(), place: p, screen: true }]);
        assert!(!crate::desklayer::disk::is_stray(&picture("p5-1")), "listed: kept");
        // A list that stopped reading takes no line: the picture goes again.
        std::fs::write(dir().join("pieces.json"), "{half").unwrap();
        assert!(!save_new(&mut s, "p5-2", b"png", p, false));
        assert!(!picture("p5-2").exists(), "nothing of it left");
        assert_eq!(std::fs::read_to_string(dir().join("pieces.json")).unwrap(), "{half");
        crate::desklayer::disk::set_test_dir(None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn older_lists_read_and_pasted_pieces_stay_as_they_were_written() {
        let old = r#"{"version":1,"pieces":[{"id":"p1-1","place":{"mon":[0,0,10,10],"x":1.0,"y":2.0,"w":3.0,"h":4.0}}]}"#;
        let i: Index = serde_json::from_str(old).unwrap();
        assert!(!i.pieces[0].screen, "a line without the flag: a pasted piece");
        let text = serde_json::to_string(&i).unwrap();
        assert!(!text.contains("screen"), "written as before for pasted pieces");
    }

    #[test]
    fn a_thin_piece_stays_thin_while_resized() {
        assert_eq!(min_track(4.0, 48.0), (48.0, 12.0));
        assert_eq!(min_track(0.5, 48.0), (24.0, 48.0));
        assert_eq!(min_track(1.0, 48.0), (48.0, 48.0));
        assert_eq!(min_track(1000.0, 48.0), (48.0, 1.0));
        assert_eq!(min_track(f32::NAN, 48.0), (48.0, 48.0));
    }

    #[test]
    fn a_new_piece_keeps_its_shape_within_bounds() {
        // A big screenshot: down to 320 on its long side.
        let (w, h) = first_size(1920, 1080, 1.0, 320.0, 80.0);
        assert!((w - 320.0).abs() < 0.01 && (h - 180.0).abs() < 0.01);
        // At 200 %: its pixels are half the DIP.
        assert_eq!(first_size(400, 200, 2.0, 320.0, 80.0), (200.0, 100.0));
        // A tiny one grows to the minimum on its short side.
        let (w, h) = first_size(40, 20, 1.0, 320.0, 80.0);
        assert!((h - 80.0).abs() < 0.01 && (w - 160.0).abs() < 0.01);
        assert_eq!(first_size(0, 5, 1.0, 320.0, 80.0), (320.0, 320.0));
        assert_eq!(keep_shape((300.0, 10.0), 2.0, true), (300.0, 150.0));
        assert_eq!(keep_shape((10.0, 100.0), 2.0, false), (200.0, 100.0));
        assert_eq!(keep_shape((10.0, 100.0), f32::NAN, false), (10.0, 100.0));
    }
}
