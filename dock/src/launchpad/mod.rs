//! Launchpad: our own replacement for the Start menu (which always opens
//! at the bottom, where the hidden taskbar is). A full-screen overlay on
//! one monitor with a search field at the top, the installed apps in a
//! paged grid and page dots (power is in the top bar's system menu).
//!
//! Opened from the top bar's logo (a plain click; right-click opens the
//! system menu), the "Launchpad" pin in the dock, an optional global hotkey
//! (shortcut "launchpad") and, optionally, a lone tap of the Windows key
//! (shortcut "launchpad_win_tap"). Esc, a click on empty space, launching an app or
//! another window taking the focus closes it.
//!
//! Its pages are the user's Home (only what they put there) and, last, the
//! App Library (library.rs): every app in categories, new ones with a dot,
//! the hidden ones in Hidden. Uninstallers, web links and documents start
//! in the Library, not on the first Home made (cleanup.rs).
//!
//! It reuses search: the app list (shell:AppsFolder, Store apps included),
//! its icon loader and disk cache, its matcher (Korean initials too), its
//! text field and the way it launches apps. Like search it is one window
//! drawn into Windows.UI.Composition visuals, modal in a nested message
//! loop on the UI thread. Nothing runs while it is closed except the
//! optional hotkey and Windows key hook; its visuals and GPU surfaces go
//! with the window.

mod cleanup;
mod edit;
mod folders;
pub mod glyph;
mod grid;
pub mod hotkey;
mod layout_store;
mod library;
mod list;
mod loader;
pub(crate) mod motion;
mod picks;
mod tidy;
mod usage;
mod view;
pub(crate) mod wintap;

use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::gfx::Gfx;
use crate::theme::Theme;

/// The dock pin that opens Launchpad (a pseudo path: no file behind it).
pub const PIN: &str = "nopbar:launchpad";
/// Name stored with a new config's pin (the dock shows the localized one).
pub const PIN_NAME: &str = "Launchpad";

/// Posted to the dock window: open or close Launchpad outside any borrow.
/// wParam OPEN_TOGGLE (hotkey, Windows key: the foreground window's
/// monitor) or OPEN_ON (lParam: the HMONITOR to open on).
pub const WM_LAUNCHPAD: u32 = WM_APP + 80;
pub const OPEN_TOGGLE: usize = 0;
pub const OPEN_ON: usize = 1;

/// The dock window (gets WM_LAUNCHPAD and the hotkey).
static OWNER: AtomicIsize = AtomicIsize::new(0);

/// Is `path` the Launchpad pin?
pub fn is_pin(path: &str) -> bool {
    path.eq_ignore_ascii_case(PIN)
}

/// Shortcut "launchpad" (crate::shortcuts): its global hotkey. `owner`:
/// the dock window (on the UI thread).
pub fn apply_hotkey(owner: HWND, spec: &str) {
    OWNER.store(owner.0 as isize, Ordering::SeqCst);
    hotkey::apply(owner, spec);
    warm_up();
}

/// Shortcut "launchpad_win_tap": a lone Windows key opens Launchpad ("win")
/// or not ("none"). The keyboard hook is shared with Win+digit.
pub fn apply_win_tap(owner: HWND, v: &str) {
    OWNER.store(owner.0 as isize, Ordering::SeqCst);
    wintap::set_tap(owner, !crate::shortcuts::is_off(v));
}

/// Win+1..9, 0 switch to dock apps (shortcut "dock_apps" set to "win"):
/// caught by the same keyboard hook, posted to `owner` as WM_DOCK_DIGIT.
pub fn set_win_digits(owner: HWND, on: bool, msg: u32) {
    wintap::set_digits(owner, on, msg);
}

/// Once, a while after startup: read the app list in the background so the
/// first open has it (one shell:AppsFolder read; nothing after that until
/// Launchpad or search opens).
fn warm_up() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = std::thread::Builder::new().name("launchpad-warm".into()).spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(8));
            if crate::search::app_list().is_empty() {
                crate::search::refresh_apps();
            }
        });
    });
}

/// The dock window is going away: hotkey and hook given back.
pub fn shutdown() {
    view::cancel();
    hotkey::release();
    wintap::off();
}

/// Asks the dock window to open Launchpad on monitor `hmon` (the dock's
/// pin: the click is handled inside the dock's borrow, Launchpad runs its
/// own loop, so it opens a moment later from the message).
pub fn post_open(hmon: Option<isize>) {
    let owner = OWNER.load(Ordering::SeqCst);
    if owner == 0 {
        return;
    }
    let (wp, lp) = match hmon {
        Some(m) => (OPEN_ON, m),
        None => (OPEN_TOGGLE, 0),
    };
    unsafe {
        let _ = PostMessageW(Some(HWND(owner as *mut _)), WM_LAUNCHPAD, WPARAM(wp), LPARAM(lp));
    }
}

/// WM_LAUNCHPAD reached the dock window (outside its borrow).
pub fn on_message(wp: WPARAM, lp: LPARAM) {
    if view::is_closing() {
        // Asked again while it animates away: gone at once, then this
        // request again (it opens fresh once the old window is gone).
        view::cancel();
        repost(wp, lp);
        return;
    }
    match wp.0 {
        // (The dock's pin again while it is open: close.)
        OPEN_ON if view::is_open() => view::close(),
        OPEN_ON => open(Some(lp.0)),
        _ => {
            if lp.0 == 1 {
                // (Logged here, not in the hook.) UIPI: an elevated app in front.
                crate::log!("launchpad: Windows key mask not sent; Start may open too");
            }
            toggle()
        }
    }
}

/// The Launchpad hotkey was pressed.
pub fn on_hotkey() {
    toggle();
}

/// Sends a WM_LAUNCHPAD request to the dock window again.
fn repost(wp: WPARAM, lp: LPARAM) {
    let owner = OWNER.load(Ordering::SeqCst);
    if owner != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(owner as *mut _)), WM_LAUNCHPAD, wp, lp);
        }
    }
}

fn toggle() {
    if view::is_closing() {
        view::cancel();
        repost(WPARAM(OPEN_TOGGLE), LPARAM(0));
    } else if view::is_open() {
        view::close();
    } else {
        open(None);
    }
}

/// Opens on monitor `hmon` (None: the foreground window's) with the dock's
/// look. Must not be called inside the dock's borrow.
fn open(hmon: Option<isize>) {
    if view::is_open() {
        return;
    }
    if let Some((gfx, theme)) = crate::dock::look() {
        show(&gfx, &theme, hmon);
    }
}

/// Shows Launchpad until it closes (the top bar's logo calls this with its
/// own look and monitor). Must not be called while the caller holds a
/// RefCell borrow the nested loop could need.
pub fn show(gfx: &Rc<Gfx>, theme: &Theme, hmon: Option<isize>) {
    view::show(gfx, theme, hmon);
}

/// Launchpad is on screen (other panels do not open over it).
pub fn is_open() -> bool {
    view::is_open()
}

/// Closes Launchpad at once if it is open (the GPU device was replaced),
/// without the closing animation.
pub fn cancel() {
    view::cancel();
}

/// The theme was re-resolved (light/dark, accent, high contrast...).
pub fn retheme(theme: &Theme) {
    view::retheme(theme);
}

/// Search's app list was read again (called from its reader thread).
pub fn apps_refreshed() {
    view::apps_refreshed();
}

#[cfg(test)]
pub(crate) use view::font_compare;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin() {
        assert!(is_pin(PIN));
        assert!(is_pin("NopBar:Launchpad"));
        assert!(!is_pin("shell:AppsFolder\\Launchpad"));
        assert!(!is_pin(""));
    }

    #[test]
    fn strings_exist() {
        for tag in ["en", "ko"] {
            let _l = crate::i18n::test_lock(tag);
            for k in [
                "launchpad.title",
                "launchpad.placeholder",
                "launchpad.loading",
                "launchpad.no_results",
                "launchpad.menu.open",
                "launchpad.menu.keep_in_dock",
                "launchpad.menu.remove_from_dock",
                "launchpad.page",
                // Edit mode and folders (arrange.rs, access.rs).
                "launchpad.folder.default",
                "launchpad.folder.a11y",
                "launchpad.folder.rename",
                "launchpad.folder.close",
                "launchpad.menu.arrange",
                "launchpad.edit.on",
                "launchpad.edit.off",
                "launchpad.edit.done",
                "launchpad.edit.hint",
                "launchpad.edit.item_hint",
                "launchpad.edit.lifted",
                "launchpad.edit.dropped",
                "launchpad.edit.moved",
                "launchpad.edit.into_folder",
                "launchpad.edit.move_back",
                "launchpad.edit.move_on",
                "settings.launchpad.reset",
                "settings.launchpad.reset.hint",
                "settings.launchpad.reset.button",
                "settings.launchpad.reset.confirm",
                "settings.launchpad.reset.done",
                // The App Library (applib.rs).
                "launchpad.library.title",
                "launchpad.library.a11y",
                "launchpad.library.new",
                "launchpad.library.hidden_app",
                "launchpad.menu.add_home",
                "launchpad.menu.remove_home",
                "launchpad.menu.hide",
                "launchpad.menu.unhide",
                "launchpad.menu.move_to",
                "launchpad.done.added",
                "launchpad.done.removed",
                "launchpad.done.hidden",
                "launchpad.done.unhidden",
                "launchpad.done.moved",
                "settings.launchpad.library",
                "settings.launchpad.library.hint",
                // Tidy Home and several at once (organize.rs).
                "launchpad.menu.tidy",
                "launchpad.menu.group",
                "launchpad.done.tidied",
                "launchpad.done.untidied",
                "launchpad.done.grouped",
                "launchpad.done.removed_n",
                "launchpad.done.hidden_n",
                "launchpad.done.moved_n",
                "launchpad.edit.picked",
                "launchpad.edit.unpicked",
                "launchpad.edit.picked_n",
                "launchpad.edit.picks_cleared",
                "launchpad.edit.lifted_n",
                // Reused from the dock.
                "dock.menu.run_admin",
                "dock.menu.open_location",
            ] {
                assert!(crate::i18n::get(k).is_some(), "{tag}: {k}");
            }
            let page = crate::i18n::tf("launchpad.page", &[("n", "2"), ("count", "4")]);
            assert!(page.contains('2') && page.contains('4'), "{tag}: {page}");
            let moved = crate::i18n::tf("launchpad.edit.moved", &[("name", "Calc"), ("page", "3"), ("n", "7")]);
            assert!(moved.contains("Calc") && moved.contains('3') && moved.contains('7'), "{tag}: {moved}");
            let folder = crate::i18n::tf("launchpad.folder.a11y", &[("name", "Games"), ("count", "5")]);
            assert!(folder.contains("Games") && folder.contains('5'), "{tag}: {folder}");
        }
    }

    /// The new strings fill their placeholders in every language, and the
    /// menu rows stay short (Apple-like: no explanations in menus).
    #[test]
    fn organize_strings_in_every_language() {
        for (tag, _) in crate::i18n::languages() {
            let _l = crate::i18n::test_lock(tag);
            for (k, args) in [
                ("launchpad.edit.picked", &[("name", "Calc")][..]),
                ("launchpad.edit.unpicked", &[("name", "Calc")][..]),
                ("launchpad.edit.picked_n", &[("count", "7")][..]),
                ("launchpad.edit.lifted_n", &[("count", "7")][..]),
                ("launchpad.done.removed_n", &[("count", "7")][..]),
                ("launchpad.done.hidden_n", &[("count", "7")][..]),
                ("launchpad.done.moved_n", &[("count", "7"), ("category", "Dev")][..]),
                ("launchpad.done.grouped", &[("count", "7"), ("name", "Work")][..]),
            ] {
                let s = crate::i18n::tf(k, args);
                for (_, v) in args {
                    assert!(s.contains(v), "{tag}: {k}: {s}");
                }
                assert!(!s.contains('{'), "{tag}: {k}: {s}");
            }
            for k in ["launchpad.menu.tidy", "launchpad.menu.group"] {
                let s = crate::i18n::t(k);
                assert!(!s.is_empty() && s.chars().count() <= 28 && !s.contains(['.', '。']), "{tag}: {k}: {s}");
            }
        }
        let _l = crate::i18n::test_lock("ko");
        assert_eq!(crate::i18n::t("launchpad.menu.tidy"), "고정 항목 정렬");
        assert_eq!(crate::i18n::t("launchpad.menu.group"), "폴더로 묶기");
    }

    /// Edit mode says one short line: drag to move, overlap for a folder.
    #[test]
    fn the_edit_hint_is_one_short_line() {
        for (tag, _) in crate::i18n::languages() {
            let _l = crate::i18n::test_lock(tag);
            let h = crate::i18n::t("launchpad.edit.hint");
            assert_eq!(h.matches(" · ").count(), 1, "{tag}: {h}");
            assert!(h.chars().count() <= 52 && !h.contains(['.', '。']), "{tag}: {h}");
        }
        let _l = crate::i18n::test_lock("ko");
        assert_eq!(crate::i18n::t("launchpad.edit.hint"), "끌어서 옮기기 · 겹치면 폴더");
    }
}
