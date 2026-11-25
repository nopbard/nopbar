//! `NopBar-<v>-setup.exe` (see lib.rs).
#![windows_subsystem = "windows"]

use nopbar_setup::{cli, payload, ui};

/// The Inno Setup installer (build.rs: NOPBAR_SETUP_INNER; empty in dev
/// builds).
static INNER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/inner.bin"));
/// The app version (dock/Cargo.toml).
const VERSION: &str = env!("NOPBAR_APP_VERSION");

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let code = match cli::mode(&args) {
        cli::Mode::Passthrough => payload::passthrough(INNER, VERSION, &cli::own_tail(), cli::log_path(&args).as_deref(), &std::env::temp_dir()),
        cli::Mode::Window => ui::window::run(INNER, VERSION),
    };
    std::process::exit(code);
}
