//! "Always on top" for any window: the dock's window menu (a checked row),
//! the pin on each window preview card (preview.rs, showing the state) and
//! a configurable shortcut for the window in front (unset by default,
//! Settings > Shortcuts).
//!
//! It is SetWindowPos with HWND_TOPMOST / HWND_NOTOPMOST on that window,
//! posted (SWP_ASYNCWINDOWPOS: a window that stops answering cannot hold
//! the dock), then checked a moment later: a window that runs above our
//! rights (UIPI) or refused the change is logged, and the menu shows the
//! row as unavailable for such windows up front. Our own windows and the
//! shell are never changed.
//!
//! What we made topmost is kept in a set (pruned when windows close), so
//! a change can be told apart from the app's own and undone. On quit the
//! windows are LEFT AS THEY ARE, on purpose: the user asked for them to be
//! on top, Windows keeps that state with the window, and taking it away
//! behind their back when NopBar quits (or restarts after an update) would
//! be a surprise.

use super::*;
use crate::traywin::{facts, Facts, Refusal};

/// One-shot on the dock window: check the changes asked for just now.
pub(super) const VERIFY_TIMER: usize = 70;
const VERIFY_MS: u32 = 400;

thread_local! {
    /// Windows we made topmost: handle -> its process.
    static OURS: RefCell<HashMap<isize, u32>> = RefCell::new(HashMap::new());
    /// Changes asked for and not checked yet: (window, its pid, wanted).
    static PENDING: RefCell<Vec<(isize, u32, bool)>> = const { RefCell::new(Vec::new()) };
}

/// Pure: may "Always on top" change a window with `f` (None) or why not.
pub(super) fn refusal(f: &Facts) -> Option<Refusal> {
    if !f.alive {
        Some(Refusal::Gone)
    } else if f.ours {
        Some(Refusal::Own)
    } else if f.shell {
        Some(Refusal::Shell)
    } else if f.locked {
        Some(Refusal::Elevated)
    } else if f.hung {
        Some(Refusal::Hung)
    } else {
        None
    }
}

/// Is `h` always on top now (its own style bit; never waits on its app)?
pub(super) fn is_on_top(h: HWND) -> bool {
    unsafe { (GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0) != 0 }
}

/// Can `h` be changed now?
pub(super) fn available(h: HWND) -> std::result::Result<(), Refusal> {
    match refusal(&facts(h)) {
        Some(r) => Err(r),
        None => Ok(()),
    }
}

/// Turns "Always on top" over for `h` (or sets it to `want`). `dock` gets
/// the check a moment later. Ok(the state asked for).
pub(super) fn set(h: HWND, want: Option<bool>, dock: HWND) -> std::result::Result<bool, Refusal> {
    available(h)?;
    let on = want.unwrap_or(!is_on_top(h));
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(h, Some(&mut pid));
        // Another thread's window: posted to it instead of waited for.
        let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
        if let Err(e) = SetWindowPos(h, Some(after), 0, 0, 0, 0, flags) {
            crate::log!("ontop: {:#x} not changed ({e})", h.0 as isize);
            return Err(Refusal::Elevated);
        }
    }
    crate::log!("ontop: {:#x} (pid {pid}) -> {}", h.0 as isize, if on { "on top" } else { "normal" });
    PENDING.with(|p| {
        let mut p = p.borrow_mut();
        p.retain(|x| x.0 != h.0 as isize);
        p.push((h.0 as isize, pid, on));
    });
    unsafe {
        SetTimer(Some(dock), VERIFY_TIMER, VERIFY_MS, None);
    }
    Ok(on)
}

/// Pure: the set after a checked change of window `h` (pid `pid`): `want`
/// asked, `now` its state. A change that did not take leaves it as it was.
pub(super) fn after_check(set: &mut HashMap<isize, u32>, h: isize, pid: u32, want: bool, now: bool) -> bool {
    let took = want == now;
    if took {
        if now {
            set.insert(h, pid);
        } else {
            set.remove(&h);
        }
    }
    took
}

/// VERIFY_TIMER: did the changes take?
pub(super) fn verify() {
    let list = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for (h, pid, want) in list {
        let w = HWND(h as *mut _);
        if !unsafe { IsWindow(Some(w)).as_bool() } {
            OURS.with(|s| s.borrow_mut().remove(&h));
            continue;
        }
        let now = is_on_top(w);
        let took = OURS.with(|s| after_check(&mut s.borrow_mut(), h, pid, want, now));
        if !took {
            let why = if crate::apps::is_locked(pid) { "it runs with higher rights (UIPI)" } else { "the window refused it" };
            crate::log!("ontop: {h:#x} (pid {pid}) is still {} ({why})", if now { "on top" } else { "normal" });
        }
    }
}

/// Windows that closed (or changed hands) leave the set.
pub(super) fn prune() {
    OURS.with(|s| {
        s.borrow_mut().retain(|&h, &mut pid| unsafe {
            let w = HWND(h as *mut _);
            let mut now = 0u32;
            IsWindow(Some(w)).as_bool() && {
                GetWindowThreadProcessId(w, Some(&mut now));
                now == pid
            }
        })
    });
}

/// Made topmost by us (and still so).
#[cfg(test)]
pub(super) fn ours(h: HWND) -> bool {
    OURS.with(|s| s.borrow().contains_key(&(h.0 as isize)))
}

// ---- the shortcuts ---------------------------------------------------------------

/// WM_HOTKEY ids: "Always on top" and "Send to tray" for the window in front.
pub(super) const ONTOP_HOTKEY: i32 = 0x4F54;
pub(super) const TRAY_HOTKEY: i32 = 0x5457;

/// A registered global shortcut: (owner, spec, registered).
type Slot = std::sync::Mutex<Option<(isize, String, bool)>>;
static ONTOP_SLOT: Slot = std::sync::Mutex::new(None);
static TRAY_SLOT: Slot = std::sync::Mutex::new(None);

/// Registers `spec` on `owner` for `id`, replacing the one before, and
/// records the result for Settings (`key`: hotkeys::*).
fn apply(slot: &Slot, id: i32, key: &'static str, owner: HWND, spec: &str) {
    use crate::hotkeys::{record, Status};
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
    let Ok(mut cur) = slot.lock() else { return };
    let me = owner.0 as isize;
    if cur.as_ref().is_some_and(|(o, s, _)| *o == me && s == spec) {
        return;
    }
    if let Some((old, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), id);
        }
    }
    let ok = match crate::search::hotkey::parse(spec) {
        Ok(None) => {
            record(key, spec, Status::Off);
            false
        }
        Ok(Some((mods, vk))) => match unsafe { RegisterHotKey(Some(owner), id, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk) } {
            Ok(()) => {
                crate::log!("dock: hotkey {key} = {spec}");
                record(key, spec, Status::Ok);
                true
            }
            Err(e) => {
                crate::log!("dock: hotkey {key} = {spec} not registered ({e})");
                record(key, spec, Status::from_error(&e));
                false
            }
        },
        Err(e) => {
            crate::log!("dock: bad hotkey {key}: {e}");
            record(key, spec, Status::Invalid);
            false
        }
    };
    *cur = Some((me, spec.to_string(), ok));
}

fn release(slot: &Slot, id: i32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;
    let Ok(mut cur) = slot.lock() else { return };
    if let Some((owner, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(owner as *mut _)), id);
        }
    }
}

pub(super) fn apply_ontop_hotkey(owner: HWND, spec: &str) {
    apply(&ONTOP_SLOT, ONTOP_HOTKEY, crate::hotkeys::WINDOW_ON_TOP, owner, spec);
}

pub(super) fn apply_tray_hotkey(owner: HWND, spec: &str) {
    apply(&TRAY_SLOT, TRAY_HOTKEY, crate::hotkeys::WINDOW_TO_TRAY, owner, spec);
}

/// The dock window is going away.
pub(super) fn release_hotkeys() {
    release(&ONTOP_SLOT, ONTOP_HOTKEY);
    release(&TRAY_SLOT, TRAY_HOTKEY);
}

/// The window the shortcuts act on: the one in front (its top-level
/// window), not one of ours.
pub(super) fn front_window() -> Option<HWND> {
    let h = unsafe { GetAncestor(GetForegroundWindow(), GA_ROOT) };
    (!h.is_invalid()).then_some(h)
}

// ---- the dock's side (menus, cards, shortcuts) ----------------------------------

impl Dock {
    /// The window list's entry for `h` (its exe, AUMID and owner).
    pub(super) fn listed(&self, h: HWND) -> Option<apps::AppWindow> {
        self.last_wins.iter().find(|w| w.hwnd == h).cloned()
    }

    /// Where a notice about item `i`'s windows shows (next to its icon).
    fn notice_at(&self, h: HWND) -> Option<(POINT, crate::popup::Align)> {
        let i = self.items.iter().position(|it| it.windows.contains(&h))?;
        Some((self.menu_anchor(i), self.menu_align()))
    }

    /// Says why `r` stopped an action on `h` (title, why: text keys of
    /// that action's own wording).
    fn tell_refusal(&self, h: HWND, title: &str, why: &str, r: Refusal) {
        crate::log!("dock: {title} refused for {:#x}: {r:?}", h.0 as isize);
        crate::launch::tell(crate::i18n::t(title).into(), crate::i18n::t(why).into(), Vec::new(), self.notice_at(h));
    }

    /// "Always on top" turned over for window `h`.
    pub(super) fn toggle_on_top(&self, h: HWND) {
        if let Err(r) = set(h, None, self.hwnd) {
            self.tell_refusal(h, "ontop.failed", r.ontop_key(), r);
        }
    }

    /// "Send to tray" for window `h` (one the window list knows: a
    /// taskbar-worthy app window), with its app's dock icon: minimized and
    /// tucked (crate::traywin).
    pub(super) fn send_to_tray(&mut self, h: HWND) {
        let Some(w) = self.listed(h) else {
            self.tell_refusal(h, "traywin.failed", Refusal::Gone.why_key(), Refusal::Gone);
            return;
        };
        let icon = self.items.iter().find(|it| it.windows.contains(&h)).and_then(|it| it.large_brush());
        match crate::traywin::tuck(&w, icon) {
            Ok(()) => {
                if preview::windows().contains(&h) {
                    preview::hide();
                }
            }
            Err(r) => self.tell_refusal(h, "traywin.failed", r.why_key(), r),
        }
    }

    /// The "Always on top" shortcut: the window in front.
    pub(super) fn on_top_front(&mut self) {
        match front_window() {
            Some(h) => self.toggle_on_top(h),
            None => crate::log!("ontop: shortcut: no window in front"),
        }
    }

    /// The "Send to tray" shortcut: the window in front.
    pub(super) fn tray_front(&mut self) {
        match front_window() {
            Some(h) => self.send_to_tray(h),
            None => crate::log!("traywin: shortcut: no window in front"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Facts {
        Facts { alive: true, visible: true, ..Default::default() }
    }

    #[test]
    fn our_windows_the_shell_and_elevated_ones_are_not_changed() {
        assert_eq!(refusal(&ok()), None);
        // Hidden or minimized windows can still be pinned.
        assert_eq!(refusal(&Facts { visible: false, ..ok() }), None);
        assert_eq!(refusal(&Facts { cloaked: true, ..ok() }), None, "cloaked frames take topmost fine");
        assert_eq!(refusal(&Facts { ours: true, ..ok() }), Some(Refusal::Own));
        assert_eq!(refusal(&Facts { shell: true, ..ok() }), Some(Refusal::Shell));
        assert_eq!(refusal(&Facts { locked: true, ..ok() }), Some(Refusal::Elevated));
        assert_eq!(refusal(&Facts { hung: true, ..ok() }), Some(Refusal::Hung));
        assert_eq!(refusal(&Facts::default()), Some(Refusal::Gone));
    }

    #[test]
    fn the_set_follows_changes_that_took() {
        let mut s = HashMap::new();
        assert!(after_check(&mut s, 1, 10, true, true));
        assert_eq!(s.get(&1), Some(&10));
        // Did not take (UIPI): left as it was.
        assert!(!after_check(&mut s, 2, 20, true, false));
        assert!(!s.contains_key(&2));
        assert!(after_check(&mut s, 1, 10, false, false));
        assert!(s.is_empty());
        assert!(!ours(HWND(std::ptr::null_mut())));
    }

    #[test]
    fn a_middle_click_closes_only_when_asked_and_running() {
        use input::{middle_click, Middle};
        assert_eq!(middle_click(false, true, true, false), Middle::Launch, "the default: a new window");
        assert_eq!(middle_click(true, true, true, false), Middle::Close);
        assert_eq!(middle_click(true, true, true, true), Middle::Close, "its own windows (the tray's stay)");
        assert_eq!(middle_click(true, true, false, true), Middle::Nothing, "only tray windows: never closed from the dock");
        assert_eq!(middle_click(true, true, false, false), Middle::Launch, "nothing to close: start it");
        assert_eq!(middle_click(true, false, true, false), Middle::Launch, "stacks, the trash: as before");
        assert_eq!(middle_click(false, true, false, true), Middle::Launch, "the default ignores the tray");
        assert!(!crate::config::DockCfg::default().middle_click_closes());
        let mut d = crate::config::DockCfg::default();
        d.middle_click = "CLOSE".into();
        assert!(d.middle_click_closes());
        d.middle_click = "elsewhere".into();
        assert!(!d.middle_click_closes(), "unknown text: a new window");
    }

    #[test]
    fn hotkey_ids_are_distinct() {
        assert_ne!(ONTOP_HOTKEY, TRAY_HOTKEY);
        assert!(!crate::shortcuts::DIGIT_IDS.contains(&ONTOP_HOTKEY) && !crate::shortcuts::DIGIT_IDS.contains(&TRAY_HOTKEY));
    }
}
