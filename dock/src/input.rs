//! When the keyboard's focus shows (focus-visible): the one rule every
//! surface asks (2026-11-11, final/states-results.md 1).
//!
//! The ring (or a row's keyboard cursor) shows once the keyboard moves the
//! focus: Tab, Shift+Tab, the arrows, Home / End, Page Up / Down, Enter
//! and Space, none of Ctrl, Alt or the Windows key held (macOS's and the
//! web's `:focus-visible`). Any other key leaves it as it is: a media or
//! volume key, a letter, a shortcut (the owner's screenshot,
//! final/owner-volume-ring.png: the mute key put the Control Center's ring
//! round the sound slider). A press of the pointer hides it again. A
//! screen reader's SetFocus (UIA) moves the focus and shows the ring only
//! if it showed already or Windows is set to show keyboard cues always
//! (SPI_GETKEYBOARDCUES: "Underline access keys" / focus rectangles); the
//! reader draws its own highlight.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_RWIN, VK_SPACE, VK_TAB, VK_UP,
};

/// The modifiers held with a key (Shift is not one here: Shift+Tab moves
/// the focus back).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub win: bool,
}

impl Mods {
    /// What is held now (the thread's key state as the key message came).
    pub fn now() -> Mods {
        let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
        Mods { ctrl: down(VK_CONTROL.0), alt: down(VK_MENU.0), win: down(VK_LWIN.0) || down(VK_RWIN.0) }
    }

    fn any(self) -> bool {
        self.ctrl || self.alt || self.win
    }
}

/// The keys that move the keyboard's focus (and so show it).
pub const NAVIGATION: [u16; 11] = [VK_TAB.0, VK_LEFT.0, VK_UP.0, VK_RIGHT.0, VK_DOWN.0, VK_HOME.0, VK_END.0, VK_PRIOR.0, VK_NEXT.0, VK_RETURN.0, VK_SPACE.0];

/// Pure: key `vk` with `mods` held moves the keyboard's focus (and shows
/// it): a NAVIGATION key, no Ctrl, Alt or Windows key with it.
pub fn moves_focus(vk: u16, mods: Mods) -> bool {
    !mods.any() && NAVIGATION.contains(&vk)
}

/// Key `vk`, just pressed, moves the keyboard's focus: moves_focus with
/// the modifiers held now. Every surface's "show the ring" asks this.
pub fn navigates(vk: u16) -> bool {
    moves_focus(vk, Mods::now())
}

/// Windows shows keyboard cues (focus rectangles, access key underlines)
/// always, not only after a key (SPI_GETKEYBOARDCUES).
pub fn keyboard_cues() -> bool {
    use windows::core::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETKEYBOARDCUES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    let mut on = BOOL(0);
    unsafe { SystemParametersInfoW(SPI_GETKEYBOARDCUES, 0, Some(&mut on as *mut BOOL as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) }.is_ok() && on.as_bool()
}

/// Pure: the ring after a screen reader's SetFocus, `was` its state
/// before and `cues` keyboard_cues(): the focus moves either way; the ring
/// shows only as it did, or with Windows' cues always on.
pub fn ring_on_reader_focus(was: bool, cues: bool) -> bool {
    was || cues
}

/// ring_on_reader_focus with Windows' setting as it is now.
pub fn reader_focus(was: bool) -> bool {
    ring_on_reader_focus(was, keyboard_cues())
}

/// The keys the surfaces' sequence tests press (tests only): a media and
/// a volume key, letters and digits, shortcuts, and the navigation keys.
#[cfg(test)]
pub mod sample {
    use super::Mods;

    /// Thread-local key state only: no input is sent to another window.
    pub struct Keys([u8; 256]);
    impl Keys {
        pub fn new() -> Self {
            let mut old = [0; 256];
            unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardState(&mut old).unwrap(); }
            Self(old)
        }
        pub fn set(&self, modifier: Option<u16>) {
            let mut keys = [0; 256];
            if let Some(vk) = modifier { keys[vk as usize] = 0x80; }
            unsafe { windows::Win32::UI::Input::KeyboardAndMouse::SetKeyboardState(&keys).unwrap(); }
        }
    }
    impl Drop for Keys {
        fn drop(&mut self) {
            unsafe { let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetKeyboardState(&self.0); }
        }
    }

    pub const MUTE: u16 = 0xAD;
    pub const VOLUME_DOWN: u16 = 0xAE;
    pub const VOLUME_UP: u16 = 0xAF;
    pub const PLAY_PAUSE: u16 = 0xB3;
    pub const NEXT_TRACK: u16 = 0xB0;
    pub const BRIGHTNESS: u16 = 0xFF;

    /// Keys that never show the ring (with no modifier).
    pub const QUIET: [u16; 12] = [MUTE, VOLUME_DOWN, VOLUME_UP, PLAY_PAUSE, NEXT_TRACK, BRIGHTNESS, b'A' as u16, b'Z' as u16, b'5' as u16, 0x70 /* F1 */, 0x14 /* Caps Lock */, 0x1B /* Esc */];
    pub const CTRL: Mods = Mods { ctrl: true, alt: false, win: false };
    pub const ALT: Mods = Mods { ctrl: false, alt: true, win: false };
    pub const WIN: Mods = Mods { ctrl: false, alt: false, win: true };

    /// QUIET without Esc (it closes most surfaces): what a surface's
    /// sequence test presses while it stays up.
    pub const QUIET_OPEN: [u16; 11] = [MUTE, VOLUME_DOWN, VOLUME_UP, PLAY_PAUSE, NEXT_TRACK, BRIGHTNESS, b'A' as u16, b'Z' as u16, b'5' as u16, 0x70, 0x14];

    /// The sequence every surface keeps (2026-11-11, final/
    /// owner-volume-ring.png): from no ring, media, volume and letter keys
    /// leave it off; `first` (a navigation key) shows it; a media key after
    /// it leaves it on; a press of the pointer (`press`) hides it; `again`
    /// (another navigation key) shows it again, and the pointer hides it.
    /// `ring` reads the surface's ring, `key` presses a key on it.
    pub fn sequence<S>(what: &str, s: &mut S, first: u16, again: u16, ring: impl Fn(&S) -> bool, mut key: impl FnMut(&mut S, u16), mut press: impl FnMut(&mut S)) {
        assert!(!ring(s), "{what}: no ring at first");
        for vk in QUIET_OPEN {
            key(s, vk);
            assert!(!ring(s), "{what}: key {vk:#x} showed the ring");
        }
        key(s, first);
        assert!(ring(s), "{what}: {first:#x} moves the focus: the ring shows");
        key(s, MUTE);
        assert!(ring(s), "{what}: a media key after it leaves it");
        press(s);
        assert!(!ring(s), "{what}: a press of the pointer hides it");
        key(s, PLAY_PAUSE);
        assert!(!ring(s), "{what}: a media key after the press");
        key(s, again);
        assert!(ring(s), "{what}: {again:#x} shows it again");
        press(s);
        assert!(!ring(s), "{what}: the pointer again");
    }
}

#[cfg(test)]
mod tests {
    use super::sample::*;
    use super::*;

    #[test]
    fn only_the_navigation_keys_show_the_focus() {
        for vk in NAVIGATION {
            assert!(moves_focus(vk, Mods::default()), "{vk:#x}");
            for m in [CTRL, ALT, WIN] {
                assert!(!moves_focus(vk, m), "{vk:#x} with {m:?}: a shortcut");
            }
        }
        for vk in QUIET {
            assert!(!moves_focus(vk, Mods::default()), "{vk:#x}");
        }
        // Every key there is but the navigation ones.
        for vk in 0u16..=0xFE {
            assert_eq!(moves_focus(vk, Mods::default()), NAVIGATION.contains(&vk), "{vk:#x}");
        }
    }

    /// The ring after key `vk` with `mods`: it shows once the keyboard
    /// moves the focus, and stays as it was on any other key (what every
    /// surface does with navigates()).
    fn ring_after_key(was: bool, vk: u16, mods: Mods) -> bool {
        was || moves_focus(vk, mods)
    }

    #[test]
    fn the_ring_shows_after_a_navigation_key_and_stays_on_others() {
        // Off: a media key, a letter, a shortcut leave it off.
        let mut ring = false;
        for vk in QUIET {
            ring = ring_after_key(ring, vk, Mods::default());
        }
        ring = ring_after_key(ring, VK_TAB.0, CTRL);
        ring = ring_after_key(ring, VK_LEFT.0, ALT);
        assert!(!ring);
        // Tab shows it; a media key after it does not hide it.
        ring = ring_after_key(ring, VK_TAB.0, Mods::default());
        assert!(ring);
        assert!(ring_after_key(ring, MUTE, Mods::default()));
    }

    #[test]
    fn a_screen_readers_focus_shows_the_ring_only_as_it_was_or_with_cues() {
        assert!(!ring_on_reader_focus(false, false));
        assert!(ring_on_reader_focus(false, true));
        assert!(ring_on_reader_focus(true, false));
        // The setting reads (whatever it is on this PC).
        let _ = keyboard_cues();
    }
}
