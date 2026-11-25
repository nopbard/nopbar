//! The Shortcuts page's key recorder: click a shortcut's field, press the
//! keys. Esc cancels, Backspace (or Delete) alone turns it off, a plain key
//! without a modifier is refused. A chord another NopBar shortcut uses, or
//! one Windows or another app holds (tried with RegisterHotKey on this
//! window and given back at once), is not taken: the row says so and offers
//! free keys instead. What is taken is previewed on the dock like any edit.

use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};

use super::model::Ctl;
use super::{App, Hit};
use crate::i18n::{t, tf};
use crate::shortcuts::{self, Chord, Kind, Rec};

/// The id the test registrations use (never kept).
const TEST_HOTKEY: i32 = 0xBFF0;

/// The modifiers held now (as seen by this window's messages).
pub(super) fn held_mods() -> u32 {
    let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
    let mut m = 0;
    if down(VK_CONTROL.0) {
        m |= shortcuts::CTRL;
    }
    if down(VK_MENU.0) {
        m |= shortcuts::ALT;
    }
    if down(VK_SHIFT.0) {
        m |= shortcuts::SHIFT;
    }
    if down(VK_LWIN.0) || down(VK_RWIN.0) {
        m |= shortcuts::WIN;
    }
    m
}

impl App {
    /// The shortcut id of row `ri` on the page shown.
    fn shortcut_id(&self, ri: usize) -> Option<&'static str> {
        match self.rows().get(ri).map(|r| &r.ctl) {
            Some(Ctl::Shortcut { id }) => Some(id),
            _ => None,
        }
    }

    /// Is `c` held by Windows or another app right now? Tried for real:
    /// registered on this window and given back at once.
    pub(super) fn chord_taken(&self, c: Chord) -> bool {
        unsafe {
            match RegisterHotKey(Some(self.hwnd), TEST_HOTKEY, HOT_KEY_MODIFIERS(c.mods) | MOD_NOREPEAT, c.vk) {
                Ok(()) => {
                    let _ = UnregisterHotKey(Some(self.hwnd), TEST_HOTKEY);
                    false
                }
                Err(_) => true,
            }
        }
    }

    /// Is `c` not free for a shortcut (held by Windows or another app)?
    pub(super) fn chord_in_use(&self, c: Chord) -> bool {
        self.chord_taken(c)
    }

    /// The field of row `ri` was clicked: record (a chord), flip (a switch).
    pub(super) fn click_shortcut(&mut self, ri: usize) {
        let Some(id) = self.shortcut_id(ri) else { return };
        let Some(s) = shortcuts::find(id) else { return };
        self.rec_note = None;
        match s.kind {
            Kind::Chord => {
                if self.recording.map(|(r, _)| r) == Some(ri) {
                    self.stop_recording();
                } else {
                    self.stop_recording();
                    self.recording = Some((ri, 0));
                    // The dock's own shortcuts stand aside meanwhile, so the
                    // keys pressed reach this window.
                    self.client.suspend_keys(true);
                }
                self.redraw();
            }
            Kind::Switch(on) => {
                let v = if shortcuts::is_off(&self.cfg.shortcut(id)) { on } else { "none" };
                self.cfg.set_shortcut(id, v);
                self.changed(ri);
            }
            Kind::Digits | Kind::Fixed => {}
        }
    }

    /// Row `ri` back to its default.
    pub(super) fn reset_shortcut(&mut self, ri: usize) {
        let Some(id) = self.shortcut_id(ri) else { return };
        let Some(s) = shortcuts::find(id) else { return };
        self.stop_recording();
        self.rec_note = None;
        // The default may be another shortcut's keys now: said, not taken.
        if let Ok(Some(c)) = shortcuts::parse(s.default) {
            if let Some(other) = shortcuts::clash(&self.cfg, id, c) {
                self.rec_note = Some((id, tf("shortcuts.note.duplicate", &[("name", &other)])));
                self.redraw();
                return;
            }
        }
        self.cfg.set_shortcut(id, s.default);
        self.changed(ri);
    }

    /// Recording ends (the old value stays unless a chord was taken).
    pub(super) fn stop_recording(&mut self) {
        if self.recording.take().is_some() {
            // The modifiers still held come up next: not the window's menu.
            self.rec_tail = held_mods() != 0;
            // The dock's shortcuts come back (a closed window resumes them
            // too: its disconnect is a Revert).
            self.client.suspend_keys(false);
        }
    }

    /// A key went down while recording. Always taken (nothing else sees
    /// it: not Alt's menu, not the page's keys).
    pub(super) fn record_key(&mut self, vk: u16) {
        let Some((ri, _)) = self.recording else { return };
        let Some(id) = self.shortcut_id(ri) else {
            self.stop_recording();
            return;
        };
        let mods = held_mods();
        match shortcuts::record(vk as u32, mods) {
            Rec::Pending(m) => {
                self.recording = Some((ri, m));
                self.rec_note = None;
            }
            Rec::Cancel => {
                self.rec_note = None;
                self.stop_recording();
            }
            Rec::NeedsModifier => self.rec_note = Some((id, t("shortcuts.note.needs_modifier").to_string())),
            Rec::Clear => {
                self.rec_note = None;
                self.stop_recording();
                self.cfg.set_shortcut(id, "none");
                self.changed(ri);
                return;
            }
            Rec::Done(c) => {
                self.stop_recording();
                self.rec_note = None;
                let current = shortcuts::parse(&self.cfg.shortcut(id)).ok().flatten();
                if current == Some(c) {
                    // The same keys again: nothing changes.
                } else if let Some(other) = shortcuts::clash(&self.cfg, id, c) {
                    // Another shortcut's.
                    self.rec_note = Some((id, tf("shortcuts.note.duplicate", &[("name", &other)])));
                } else if !(matches!(id, shortcuts::FOCUS_DOCK | "taskbar_tray") && c.mods == shortcuts::WIN && matches!(c.vk, 0x54 | 0x42)) && self.chord_taken(c) {
                    let note = super::status::in_use_note(c, &self.suggestions(id, c));
                    self.rec_note = Some((id, note));
                } else {
                    self.cfg.set_shortcut(id, &c.spec());
                    self.changed(ri);
                    return;
                }
            }
        }
        self.redraw();
    }

    /// A key came up: the tail of a recording ends once no modifier is
    /// held. True while it is swallowed.
    pub(super) fn record_tail(&mut self) -> bool {
        if self.recording.is_some() {
            return true;
        }
        if self.rec_tail {
            if held_mods() & (shortcuts::CTRL | shortcuts::ALT | shortcuts::SHIFT | shortcuts::WIN) == 0 {
                self.rec_tail = false;
            }
            return true;
        }
        false
    }

    /// Was `h` a click on the recorder itself (keeps recording)?
    pub(super) fn keeps_recording(&self, h: Hit) -> bool {
        matches!((h, self.recording), (Hit::Shortcut(a), Some((b, _))) if a == b)
    }
}
