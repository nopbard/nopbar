//! Instant apply (like Windows' and macOS' settings): there is no Save or
//! Cancel. A change goes to the dock at once (a preview, so a slider moves
//! the dock live) and is saved a moment after the last change: one write
//! per gesture, not one per slider step. Leaving (closing the window,
//! "Quit NopBar", signing out) saves what is still waiting, synchronously;
//! nothing asks. A save that still fails is never dropped: a closed window
//! waits hidden and keeps trying, and what cannot be saved before the
//! process must end goes to a journal that the next settings window saves.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer, ShowWindow, SW_HIDE};

use super::App;
use crate::config::Config;

/// The save that follows a change (WM_TIMER).
pub(super) const SAVE_TIMER: usize = 0x5356;
/// How long after the last change it comes (ms): a slider dragged or keys
/// held write the file once, when they stop.
pub(super) const SAVE_MS: u32 = 400;
/// A save that failed is tried again this much later (ms).
pub(super) const RETRY_MS: u32 = 5000;
/// A window closed with a save that failed waits hidden and tries again
/// (WM_TIMER): this often (ms), this many times, then journals it.
pub(super) const CLOSE_TIMER: usize = 0x5357;
pub(super) const CLOSE_RETRY_MS: u32 = 1000;
pub(super) const CLOSE_TRIES: u32 = 120;
/// Tries made at once when leaving, and the pause between them (ms).
const FLUSH_TRIES: u32 = 3;
const FLUSH_PAUSE_MS: u64 = 150;

/// What could not be saved before the process had to end: the edited
/// settings (`keys` of `config`). An older build's journal may also hold
/// workspace shortcut edits (`ws`): read past, never taken in.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub(super) struct Journal {
    pub config: Config,
    pub keys: Vec<String>,
}

impl Journal {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// The journal's file (next to config.json).
pub(super) fn journal_file() -> PathBuf {
    crate::config::app_dir().join("settings-pending.json")
}

/// Writes `j` to `path` (a temporary file renamed over it: never half).
pub(super) fn write_journal_at(path: &Path, j: &Journal) -> std::io::Result<()> {
    let text = serde_json::to_string(j).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The journal at `path` (None: none, or unreadable).
pub(super) fn read_journal_at(path: &Path) -> Option<Journal> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Journal>(&text).ok().filter(|j| !j.is_empty())
}

/// Pure: `j`'s edits taken into the window's state (its keys over `cfg`,
/// added to `touched`), to be saved
/// like any change. A journal from an older build may hold edits under
/// keys whose rows are gone: they are taken in as the settings they are
/// now (old_keys).
pub(super) fn recover(mut j: Journal, cfg: &mut Config, touched: &mut Vec<String>) {
    let moved = old_keys(&mut j, cfg);
    super::model::apply_keys(&j.config, cfg, &j.keys);
    for k in j.keys.into_iter().chain(moved) {
        if !touched.contains(&k) {
            touched.push(k);
        }
    }
}

/// Pure: the edits of `j` under the keys of removed rows (the settings
/// structure pass), put in `cfg` as they are now; they leave `j.keys`, and
/// the keys that carry them now are returned. Only that one change moves
/// (never the journal's whole snapshot: `cfg` may be newer):
/// - Effects' magnification turned off: every dock's magnification Off
///   (the main dock's, each display's own), as config::settle_removed does
///   for a file;
/// - the safely-remove extras: by value (their rows are gone, the
///   settings stay: apply_keys copies them by key);
/// - the agenda's switch: dropped (it is always on now);
/// - "Reduce motion": Low-spec mode, on when the journal's small
///   animations were off (what that switch read); the animation items it
///   set are not copied (Low-spec mode does their job);
/// - "Lower automatically" (`effects.preset`): dropped (gone, on purpose).
fn old_keys(j: &mut Journal, cfg: &mut Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut take = |k: &str| match j.keys.iter().position(|x| x == k) {
        Some(i) => {
            j.keys.remove(i);
            true
        }
        None => false,
    };
    let magnify = take("effects.magnify");
    let hotplug = take("topbar.tray_hotplug");
    let dot = take("topbar.removable_dot");
    take("calendar.agenda");
    let reduce = take("effects.reduce_motion");
    take("effects.preset");
    if reduce {
        cfg.set_low_spec(!j.config.effects.small_animations);
        out.push(super::model::LOW_SPEC.into());
    }
    if magnify && !j.config.effects.magnify {
        cfg.dock.max_scale = crate::config::MAGNIFY_OFF;
        out.push("dock.max_scale".into());
        let mut own = false;
        for d in cfg.dock.per_display.values_mut().filter(|d| d.max_scale.is_some()) {
            d.max_scale = Some(crate::config::MAGNIFY_OFF);
            own = true;
        }
        if own {
            out.push("dock.per_display".into());
        }
    }
    if hotplug {
        cfg.topbar.tray_hotplug = j.config.topbar.tray_hotplug;
        out.push("topbar.tray_hotplug".into());
    }
    if dot {
        cfg.topbar.removable_dot = j.config.topbar.removable_dot;
        out.push("topbar.removable_dot".into());
    }
    out
}

impl App {
    /// A change was made: saved SAVE_MS after the last one (another change
    /// starts the wait again).
    pub(super) fn save_soon(&self) {
        if self.exit_tries.is_none() {
            unsafe {
                SetTimer(Some(self.hwnd), SAVE_TIMER, SAVE_MS, None);
            }
        }
    }

    /// A save failed: tried again a little later (closing tries at once;
    /// a window that is leaving has its own timer).
    pub(super) fn save_later(&self) {
        if self.exit_tries.is_none() {
            unsafe {
                SetTimer(Some(self.hwnd), SAVE_TIMER, RETRY_MS, None);
            }
        }
    }

    pub(super) fn save_timer_off(&self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), SAVE_TIMER);
        }
    }

    /// Saves now, a few times if it fails (leaving: synchronously). True
    /// when nothing is left.
    pub(super) fn flush_now(&mut self) -> bool {
        for k in 0..FLUSH_TRIES {
            if self.save() {
                return true;
            }
            if k + 1 < FLUSH_TRIES {
                std::thread::sleep(std::time::Duration::from_millis(FLUSH_PAUSE_MS));
            }
        }
        false
    }

    /// What is still waiting, as a journal.
    fn journal(&self) -> Journal {
        Journal { config: self.cfg.clone(), keys: self.touched.clone() }
    }

    /// The process must end with a save still failing: kept in the journal
    /// (the next settings window saves it).
    pub(super) fn write_journal(&mut self) {
        let j = self.journal();
        if j.is_empty() {
            return;
        }
        match write_journal_at(&journal_file(), &j) {
            Ok(()) => {
                self.journaled = true;
                crate::log!("settings: {} unsaved setting(s) kept for the next settings window", j.keys.len());
            }
            Err(e) => crate::log!("settings: could not keep the unsaved settings ({e})"),
        }
    }

    /// A journal from a window that could not save: its edits are taken
    /// in and saved like any change.
    pub(super) fn recover_journal(&mut self) {
        let Some(j) = read_journal_at(&journal_file()) else { return };
        crate::log!("settings: saving {} setting(s) left by the last window", j.keys.len());
        recover(j, &mut self.cfg, &mut self.touched);
        self.journaled = true;
        self.send_preview();
        self.save_soon();
    }

    /// Everything was saved: a journal is no longer needed.
    pub(super) fn journal_done(&mut self) {
        if std::mem::take(&mut self.journaled) {
            let _ = std::fs::remove_file(journal_file());
        }
    }

    /// The window was closed but its save failed: it disappears and keeps
    /// trying (CLOSE_TIMER) instead of dropping the changes.
    pub(super) fn leave_after_save(&mut self) {
        self.exit_tries = Some(0);
        self.save_timer_off();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            SetTimer(Some(self.hwnd), CLOSE_TIMER, CLOSE_RETRY_MS, None);
        }
    }

    /// CLOSE_TIMER: one more try. True when the window may go now (saved,
    /// or out of tries and journaled).
    pub(super) fn close_retry(&mut self) -> bool {
        let tries = self.exit_tries.unwrap_or(0) + 1;
        self.exit_tries = Some(tries);
        let done = self.save();
        if !done && tries < CLOSE_TRIES {
            return false;
        }
        if !done {
            self.write_journal();
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), CLOSE_TIMER);
        }
        true
    }

    /// Windows is signing out or shutting down (the process may not live
    /// past this): the first-run flow ends as skipped, everything waiting
    /// is saved now, and what still fails is journaled.
    pub(super) fn flush_for_exit(&mut self) {
        if self.welcome.is_some() {
            self.end_welcome(false, true);
        }
        if !self.flush_now() {
            self.write_journal();
        }
    }
}

// One write per gesture, soon enough that closing right after a change is
// rare (and closing saves anyway); a failing disk is not hammered; a closed
// window keeps trying for two minutes at most.
const _: () = assert!(SAVE_MS >= 200 && SAVE_MS <= 1000 && RETRY_MS > SAVE_MS * 4 && CLOSE_RETRY_MS * CLOSE_TRIES <= 120_000);

#[cfg(test)]
mod tests {
    use super::*;

    /// The body of `name` in `src` (up to the first line that closes a
    /// method at its indent).
    fn body<'a>(src: &'a str, name: &str) -> &'a str {
        let at = src.find(name).unwrap_or_else(|| panic!("{name} not found"));
        let rest = &src[at..];
        &rest[..rest.find("\n    }\n").or_else(|| rest.find("\n    }\r\n")).unwrap_or(rest.len())]
    }

    /// Closing (X, Alt+F4, Esc, signing out) saves what is waiting and asks
    /// nothing: there is no Save / Cancel and no "Save your changes?".
    #[test]
    fn closing_saves_without_a_question() {
        let src = include_str!("mod.rs");
        assert!(body(src, "fn close(&mut self)").contains("self.flush_now()"));
        let wm_close = &src[src.find("WM_CLOSE => {").unwrap()..];
        let wm_close = &wm_close[..wm_close.find("LRESULT(0)").unwrap()];
        assert!(wm_close.contains("close_now(hwnd)") && !wm_close.contains("if "), "{wm_close}");
        for gone in ["Hit::Save", "Hit::Cancel", "request_close", "settings.leave.", "settings.note.previewing", "client.revert"] {
            assert!(!src.contains(gone), "{gone}");
        }
    }

    /// A save that fails on the way out is not lost: a closed window waits
    /// hidden and keeps trying (the dock keeps its preview: the link stays),
    /// and signing out, a quit or running out of tries journals it.
    #[test]
    fn a_failed_save_on_leaving_is_kept() {
        let src = include_str!("mod.rs");
        let at = src.find("unsafe fn close_now(hwnd: HWND)").unwrap();
        let close_now = &src[at..at + 700];
        assert!(close_now.contains("leave_after_save") && close_now.contains("if !saved"), "{close_now}");
        for msg in ["WM_QUERYENDSESSION", "WM_ENDSESSION"] {
            let at = src.find(&format!("{msg} => {{")).unwrap_or_else(|| panic!("{msg}"));
            assert!(src[at..at + 300].contains("flush_for_exit"), "{msg}");
        }
        assert!(src.contains("apply::CLOSE_TIMER") && src.contains("close_retry"));
        let quit = body(src, "fn quit_and_restore(&mut self)");
        assert!(quit.contains("flush_now") && quit.contains("write_journal"), "{quit}");
        assert!(body(include_str!("apply.rs"), "pub(super) fn close_retry").contains("write_journal"));
    }

    /// Every change is saved soon after it is made: touch() (every edit
    /// goes through it) starts the save.
    #[test]
    fn every_change_starts_the_save() {
        let src = include_str!("mod.rs");
        assert!(body(src, "fn touch(&mut self, key: &str)").contains("self.save_soon();"));
        assert!(body(src, "fn save(&mut self) -> bool").contains("self.client.save("));
    }

    /// The journal keeps the settings edited; taken in, only its keys
    /// change the newer config, and they are saved like any change. An
    /// older journal's workspace shortcut edits are read past.
    #[test]
    fn the_journal_round_trips_and_recovers_only_its_keys() {
        let dir = std::env::temp_dir().join(format!("nopbar-journal-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings-pending.json");
        let mut edited = Config::default();
        edited.dock.autohide = !edited.dock.autohide;
        edited.tray_icon = !edited.tray_icon;
        let j = Journal { config: edited.clone(), keys: vec!["dock.autohide".into()] };
        write_journal_at(&path, &j).unwrap();
        let back = read_journal_at(&path).expect("read back");
        assert_eq!(back, j);
        let mut cfg = Config::default();
        let mut touched = vec!["language".to_string()];
        recover(back, &mut cfg, &mut touched);
        assert_eq!(cfg.dock.autohide, edited.dock.autohide);
        assert_eq!(cfg.tray_icon, Config::default().tray_icon, "not in the journal's keys");
        assert_eq!(touched, ["language", "dock.autohide"]);
        // An older build's journal with workspace edits: still read.
        std::fs::write(&path, r#"{"config":{"version":2},"keys":["dock.autohide"],"ws":[["w1","ctrl+alt+1"]]}"#).unwrap();
        assert_eq!(read_journal_at(&path).map(|j| j.keys), Some(vec!["dock.autohide".to_string()]));
        std::fs::write(&path, r#"{"config":{"version":2},"keys":[],"ws":[["w1","ctrl+alt+1"]]}"#).unwrap();
        assert!(read_journal_at(&path).is_none(), "only workspace edits: nothing to take in");
        // Nothing to keep: no journal.
        write_journal_at(&path, &Journal::default()).unwrap();
        assert!(read_journal_at(&path).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A journal from a build before the settings structure pass (review
    /// P2): Effects' magnification turned off there is every dock's
    /// magnification Off here, saved under the keys of now; the rest of
    /// the newer config stays as it is.
    #[test]
    fn an_older_journal_s_removed_rows_are_recovered_as_they_are_now() {
        use crate::config::{DisplayDock, MAGNIFY_OFF};
        // The older build: magnification off, Safely Remove's icon on,
        // the dot off, the agenda off, the dock hidden; the save failed.
        let old = r#"{"version":2,"effects":{"magnify":false},"topbar":{"tray_hotplug":true,"removable_dot":false},"calendar":{"agenda":false},"dock":{"autohide":true}}"#;
        let edited: Config = serde_json::from_str(old).unwrap();
        let keys = ["effects.magnify", "topbar.tray_hotplug", "topbar.removable_dot", "calendar.agenda", "dock.autohide"].map(String::from).to_vec();
        let text = serde_json::to_string(&Journal { config: edited, keys }).unwrap();
        let back: Journal = serde_json::from_str(&text).unwrap();
        // Now: the file on disk (magnification Small, a display with its
        // own, one without; the language changed since).
        let mut disk = Config::default();
        disk.dock.max_scale = 1.4;
        disk.dock.per_display.insert("DEL40F2".into(), DisplayDock { name: "DELL".into(), max_scale: Some(1.6), ..Default::default() });
        disk.dock.per_display.insert("OTHER".into(), DisplayDock { name: "LG".into(), ..Default::default() });
        disk.language = "ko".into();
        let mut cfg = disk.clone();
        let mut touched = Vec::new();
        recover(back, &mut cfg, &mut touched);
        assert_eq!(touched, ["dock.autohide", "dock.max_scale", "dock.per_display", "topbar.tray_hotplug", "topbar.removable_dot"], "the keys of now");
        assert!(cfg.effects.magnify, "the switch is gone: on");
        assert_eq!((cfg.dock.max_scale, cfg.dock.per_display["DEL40F2"].max_scale, cfg.dock.per_display["OTHER"].max_scale), (MAGNIFY_OFF, Some(MAGNIFY_OFF), None));
        assert!(cfg.dock.autohide && cfg.topbar.tray_hotplug && !cfg.topbar.removable_dot);
        assert!(cfg.calendar.agenda && cfg.language == "ko", "the rest stays");
        // Saved (the window's keys over the file), then read again: it holds.
        super::super::model::apply_keys(&cfg, &mut disk, &touched);
        let again = crate::config::from_json(&serde_json::to_string(&disk).unwrap()).unwrap();
        assert_eq!((again.dock.max_scale, again.dock.per_display["DEL40F2"].max_scale), (MAGNIFY_OFF, Some(MAGNIFY_OFF)));
        assert!(again.topbar.tray_hotplug && !again.topbar.removable_dot && again.dock.autohide && again.language == "ko");
        // Magnification turned on there: nothing to move.
        let on = Journal { config: Config::default(), keys: vec!["effects.magnify".into()] };
        let mut cfg = disk.clone();
        cfg.dock.max_scale = 2.0;
        let mut touched = Vec::new();
        recover(on, &mut cfg, &mut touched);
        assert!(touched.is_empty() && cfg.dock.max_scale == 2.0);
    }

    /// An older build's unsaved "Reduce motion" (with "Lower automatically"
    /// beside it) comes back as Low-spec mode; the Auto switch is dropped.
    #[test]
    fn an_older_journal_s_reduce_motion_is_low_spec_mode() {
        use super::super::model::{apply_keys, LOW_SPEC};
        use crate::config::Minimize;
        // The snapshots as the older build wrote them: Reduce motion on
        // (every animation item off, no low_spec yet) or off (back at a
        // new install's), Auto turned off ("custom").
        let on = r#"{"version":2,"effects":{"preset":"custom","small_animations":false,"spring":false,"label_fade":false,"launchpad_motion":false},"animation":{"bounce":false,"minimize":"none"}}"#;
        let off = r#"{"version":2,"effects":{"preset":"custom","small_animations":true}}"#;
        let keys = || ["effects.reduce_motion", "effects.preset"].map(String::from).to_vec();
        let journal = |snap: &str| -> Journal {
            let j = Journal { config: serde_json::from_str(snap).unwrap(), keys: keys() };
            serde_json::from_str(&serde_json::to_string(&j).unwrap()).unwrap()
        };
        // On: the disk (a new install's) gets Low-spec mode, and keeps its
        // own animation items and preset.
        let disk = Config::default();
        let mut cfg = disk.clone();
        let mut touched = Vec::new();
        recover(journal(on), &mut cfg, &mut touched);
        assert_eq!(touched, [LOW_SPEC]);
        assert!(cfg.effects.low_spec);
        assert_eq!((cfg.effects.preset.as_str(), cfg.animation.minimize, cfg.effects.small_animations), ("high", Minimize::Shrink, true));
        let mut saved = disk.clone();
        apply_keys(&cfg, &mut saved, &touched);
        let again = crate::config::from_json(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert!(again.effects.low_spec, "saved and read again");
        // Off: Low-spec mode off over a disk that had it on.
        let mut disk = Config::default();
        disk.set_low_spec(true);
        let mut cfg = disk.clone();
        let mut touched = Vec::new();
        recover(journal(off), &mut cfg, &mut touched);
        assert_eq!(touched, [LOW_SPEC]);
        assert!(!cfg.effects.low_spec);
        apply_keys(&cfg, &mut disk, &touched);
        assert!(!disk.effects.low_spec);
        // Auto's switch alone: nothing moves.
        let mut cfg = Config::default();
        let mut touched = Vec::new();
        recover(Journal { config: Config::default(), keys: vec!["effects.preset".into()] }, &mut cfg, &mut touched);
        assert!(touched.is_empty() && cfg == Config::default());
    }
}
