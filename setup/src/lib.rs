//! nopbar-setup: the file people download as `NopBar-<v>-setup.exe`.
//!
//! It carries the Inno Setup installer (packaging/inno/nopbar.iss, built
//! as `NopBar-<v>-inner.exe` and embedded at build time) and has two ways
//! to run (cli::Mode):
//! - double-clicked: a small Finder-like window; dragging the NopBar icon
//!   onto the Programs folder (or Enter) runs the installer silently, shows
//!   its progress, then closes, and NopBar runs;
//! - with any argument (the updater, winget, admins): no window; the inner
//!   installer gets the same command line and its exit code is returned.
//!
//! What gets installed (files, Apps & features entry, start at login,
//! per-user folder) is the inner installer's alone.

pub mod cli;
pub mod install;
pub mod launch;
pub mod payload;
pub mod progress;
pub mod strings;
pub mod ui;
