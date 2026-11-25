//! Windows that live on the desktop (widgets): the part every desktop
//! widget shares, kept apart from what a widget shows (crate::notes is the
//! first one).
//!
//! - `layer`: the two places such a window can be (on the desktop, behind
//!   every app window; or summoned in front of everything) and what moves
//!   it between them (pure).
//! - `place`: where it is, remembered relative to its display so it comes
//!   back there, kept on screen when displays change (pure).
//! - `load`: what the widgets cost (measured) against the PC, and when
//!   they rest; `privacy`: kept out of screen sharing; `disk`: their
//!   files, written whole; `deskpaste`: Ctrl+V on the desktop makes a note
//!   or an image piece; `host`: the dock thread's hidden window for what
//!   the widgets there share (the foreground hook, Ctrl+V's posted work).
//! - `grid`: the desktop icons' grid shared with the widgets (pure):
//!   whole cells, snapping, free cells at the top right, icons pushed
//!   aside; `icons`: Explorer's icons read and moved on a thread of
//!   their own (IShellWindows / IFolderView).
//! - `win`: the Win32 side: finding the desktop shell window, putting a
//!   window on the desktop layer or in front, and holding it there.
//!
//! How a window stays on the desktop (Windows 11 24H2 and earlier): it is
//! an ordinary top-level window of ours, placed in the z-order just above
//! the window that hosts the desktop icons (Progman on 24H2, where
//! SHELLDLL_DefView and the wallpaper's WorkerW are Progman's children; on
//! older builds the WorkerW that holds SHELLDLL_DefView), so it is above
//! the icons and below every app window. Never owned by nor a child of
//! Explorer's window: both relations attach the two threads' input queues,
//! and a hung Explorer would then hang the widgets (review 2026-11-08 #6).
//! WM_WINDOWPOSCHANGING keeps it there (an activation's "to the top" is
//! turned into "just above the desktop", a hide from Show desktop is
//! refused), and a foreground WinEvent hook (no timer, no polling) puts it
//! back above the desktop when the desktop comes to the front (Show
//! desktop, Win+D) and down again when an app comes back. When Explorer
//! restarts (TaskbarCreated) the desktop window is found again.
//! The trade-off: Show desktop raises Explorer's window over ours first;
//! the widgets come back above it a moment later, from the hook.

pub mod deskpaste;
pub mod disk;
pub mod grid;
pub mod spacing;
pub mod grid_state;
pub mod host;
pub mod icons;
pub mod layer;
pub mod load;
pub mod place;
pub mod privacy;
pub mod win;
pub mod widget;
pub mod edit;
pub mod edit_view;
mod edit_surface;
pub mod backdrop;
pub mod live;
pub mod drag_hint;
mod drag_clock;
mod drag_metrics;
mod drag_input;
mod drag_visual;
pub mod grab;
pub mod field;
pub mod shade;
pub mod card;
pub mod surface;
pub mod hover;
#[cfg(test)]
mod snapshots;
#[cfg(test)]
pub(crate) mod graphite_snapshots;
#[cfg(test)]
mod handling_snapshots;
#[cfg(test)]
pub(crate) use handling_snapshots::render_readme_widgets;

/// Settings > widgets changed (or any config was applied: what did not
/// change costs nothing): which widgets are on the desktop, and whether
/// they stay out of captures. On the dock's thread.
pub fn apply_config(cfg: &crate::config::Config, theme: &crate::theme::Theme) {
    if privacy::hidden() != cfg.widgets.hide_from_capture {
        privacy::set_hidden(cfg.widgets.hide_from_capture);
    }
    edit::configure(theme);
    host::apply_config(cfg);
    crate::device_battery::runtime::apply(cfg, theme);
    crate::pieces::apply_config(cfg);
    crate::notes::set_per_desktop(cfg.widgets.notes_per_desktop);
    crate::notes::configure(&cfg.widgets);
    // The notes: turned off, they are saved and closed; on, loaded again.
    match (cfg.widgets.notes, crate::notes::running()) {
        (true, false) => crate::notes::init(theme),
        (false, true) => {
            // Every note saved first; one that can be kept nowhere keeps
            // them up (as the user's own quit does).
            if crate::notes::prepare_quit(crate::notes::Quit::User) {
                crate::notes::shutdown();
            } else {
                crate::log!("notes: turned off while a text cannot be saved; they stay");
            }
        }
        _ => {}
    }
}
pub(crate) mod shell;
