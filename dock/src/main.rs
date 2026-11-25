#![windows_subsystem = "windows"]

mod a11y;
mod agenda;
mod agents;
mod apps;
mod audio;
mod autoupdate;
mod companion;
mod config;
mod corners;
mod desklayer;
mod device_battery;
mod displays;
mod dock;
mod dragout;
mod effects;
mod focus_timer;
mod fonts;
mod gfx;
mod glass;
mod glyphs;
mod guardian;
mod hogs;
mod hotkeys;
mod i18n;
mod icons;
mod idle;
mod ime;
mod input;
mod jumplist;
mod keys;
mod launch;
mod launchers;
mod launchpad;
mod log;
mod monitors;
mod motion;
mod notes;
mod notification_state;
#[cfg(test)]
mod native_review_tests;
mod pieces;
mod pinned;
mod pip;
mod placement;
mod popup;
mod release;
mod removable;
mod res;
mod search;
mod service;
mod settings;
mod shortcuts;
mod shot;
mod stacks;
mod syslook;
mod sysstat;
mod taskbar;
mod theme;
mod tile_art;
mod topbar;
mod tray;
mod traywin;
mod updates;
mod vdesk;
mod wallpaper;
mod wash;
mod webapps;
mod worker;

/// Process start, for startup timing in dock.log.
pub static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

use windows::core::{w, Result};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::{CreateMutexW, SetProcessShutdownParameters};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, DQTAT_COM_STA,
    DQTYPE_THREAD_CURRENT, RO_INIT_SINGLETHREADED,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--native-launch") { std::process::exit(launch::helper(&args)); }
    if args.get(1).map(String::as_str) == Some("--note-reminder") {
        if args.get(2).is_none_or(|url| notes::toast::cli(url)) { return Ok(()); }
    }
    if release::parse_cli(&args)==release::Cli::WidgetsAdd {
        desklayer::shell::forward_existing();
        std::process::exit(0);
    }
    if std::env::args().nth(1).as_deref() == Some("--mcp") {
        std::process::exit(companion::chat::mcp::run());
    }
    START.get_or_init(std::time::Instant::now);
    // Release builds abort on panic, which leaves no trace anywhere. Write the
    // message, location and thread to dock.log first so crashes can be read.
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        crate::log!("PANIC in thread '{thread}' at {loc}: {msg}");
    }));
    // An agent's hook (agents/hook.rs): one line to the running dock and
    // gone, before anything else of NopBar's starts or writes.
    if args.get(1).map(String::as_str) == Some("--agent") {
        std::process::exit(agents::hook::run(&args));
    }
    // A Claude Code too old for a hook's args runs this exe bare from its
    // hook: never start a dock for that (agents::hook::bare_hook).
    if args.len() == 1 && agents::hook::bare_hook() {
        std::process::exit(0);
    }
    // Before anything that writes: the installer runs `--quit` from a copy
    // in its temp folder.
    if release::parse_cli(&args) == release::Cli::Quit {
        std::process::exit(release::quit_running());
    }
    // Automatic updates (docs/updates.md): the updater (a copy of the new
    // exe in the staged folder), "update now" and the toast's Install now.
    match args.get(1).map(|a| a.to_ascii_lowercase()).as_deref() {
        Some("--apply-update") => std::process::exit(autoupdate::apply::run(&args)),
        Some("--apply-setup") => std::process::exit(autoupdate::apply::run_setup(&args)),
        Some("--recover-update") => std::process::exit(autoupdate::apply::recover()),
        Some("--update-now") => std::process::exit(autoupdate::cli(true)),
        Some("--install-update") => std::process::exit(autoupdate::cli(false)),
        _ => {}
    }
    // Renamed from NopDock (2026-11): move its data and autostart over once.
    if args.get(1).map(String::as_str) != Some("--guardian") {
        release::migrate_from_nopdock();
    }
    if args.get(1).map(String::as_str) == Some("--guardian") {
        if let Some(pid) = args.get(2).and_then(|p| p.parse().ok()) {
            guardian::run(pid);
        }
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--debug-apps") {
        unsafe {
            RoInitialize(RO_INIT_SINGLETHREADED)?;
        }
        let mut out = String::new();
        for p in config::Config::load().dock.pins {
            out += &format!("PIN {} => {:?}
", p.name, apps::identify(&p.path));
        }
        for w in apps::enumerate(Default::default()) {
            out += &format!("WIN {:?} exe={:?} aumid={:?}
", w.hwnd, w.exe, w.aumid);
        }
        let _ = std::fs::write(config::app_dir().join("debug-apps.txt"), out);
        return Ok(());
    }
    if let release::Cli::Uninstall { remove_data, quiet: _ } = release::parse_cli(&args) {
        std::process::exit(release::uninstall(remove_data));
    }
    if args.get(1).map(String::as_str) == Some("--settings") {
        let page = args.iter().position(|a| a == "--page").and_then(|i| args.get(i + 1)).map(String::as_str);
        let result = settings::run(args.iter().any(|a| a == "--welcome"), page);
        companion::chat::protected::shutdown();
        return result;
    }
    if args.get(1).map(String::as_str) == Some("--restore-taskbar") {
        taskbar::restore_before_exit(std::time::Duration::from_secs(60));
        return Ok(());
    }

    unsafe {
        // One dock at a time. Held for the whole run, closed through an
        // owner (audit A18), also on the early return below.
        let _mutex = OwnedHandle(CreateMutexW(None, true, w!("Local\\NopBar.Single"))?);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Be among the first to hear about logoff/shutdown, while explorer is
        // still alive to take the taskbar state back (0x3FF = earliest app level).
        let _ = SetProcessShutdownParameters(0x3FF, 0);
        RoInitialize(RO_INIT_SINGLETHREADED)?;

        // WinRT factory caches and the audio listener outlive idle MTA
        // workers. Keep the apartment alive until process teardown.
        let _mta_usage = windows::Win32::System::Com::CoIncrementMTAUsage()?;

        // Windows.UI.Composition needs a DispatcherQueue on the UI thread.
        let _queue = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?;

        // Arm the guardian. A crashed run's leftovers are handled by the dock
        // itself: hiding keeps the saved original state, and with hiding off
        // apply_taskbar_mode restores it. (Restoring here and hiding again
        // would cost two slow, synchronous round trips to explorer.)
        taskbar::spawn_guardian();
        // Update checks (direct-download build; sleeps when switched off).
        updates::start();
        notes::toast::init();
        notification_state::start();
        // Debug builds / NOPBAR_DEBUG=1: memory, threads and handles in
        // dock.log every 10 minutes (leaks show as a slope over days).
        res::start_log();
        // Startup touched far more than an idle dock needs: give the freed
        // heap and the cold pages back once it has settled.
        service::after("startup trim", std::time::Duration::from_secs(60), || res::trim("startup"));

        let r = dock::run();
        dock::finish_full_quit();
        companion::chat::protected::shutdown();
        // Desktop notes: saved and closed (their own thread). Whether the
        // quit may go on was asked before the dock went (notes::prepare_quit).
        notes::shutdown();
        // An update check in flight must not hold the exit up.
        updates::cancel();
        agenda::stop();
        // Keep Awake's power request (Windows drops it with the process too).
        topbar::control::release_awake();
        agents::release();
        crate::log!("exit: run returned {:?}", r.as_ref().err());
        // Every way out (menu / tray Quit, --quit, logoff, settings, the
        // installer) comes through here: the taskbar is given back and
        // checked (state and windows, tried twice) before the process goes.
        // If explorer does not take it in time, the marker stays and the
        // guardian restores it after we are gone.
        if taskbar::restore_before_exit(std::time::Duration::from_secs(5)) {
            crate::log!("exit: taskbar back");
        } else {
            crate::log!("exit: taskbar restore not confirmed; the guardian takes over");
        }
        // The GPU could not be brought back: exit with a crash code so the
        // guardian starts a fresh dock (within its restart budget).
        let code = dock::requested_exit_code();
        if code != 0 {
            crate::log!("exit: code {code:#x} for the guardian");
            std::process::exit(code as i32);
        }
        if let Err(e) = &r {
            crate::log!("exit: dock failed: {e}");
            return r;
        }
        // A clean quit ends with exit code 0, whatever would run while the
        // process unwinds (clean quits used to reach the guardian as 0x87a,
        // a code nothing of ours sets: it came from the teardown after main
        // returned). All that matters is done: settings saved (WM_DESTROY
        // waited for them), the taskbar restored above.
        exit_clean()
    }
}

/// Ends this process now with exit code 0 (TerminateProcess on ourselves:
/// no DLL teardown or thread-local destructors can change the code).
fn exit_clean() -> ! {
    use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
    unsafe {
        let _ = TerminateProcess(GetCurrentProcess(), 0);
    }
    std::process::exit(0)
}

/// A kernel handle closed when dropped.
struct OwnedHandle(windows::Win32::Foundation::HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Font spike: the same surfaces in Segoe UI Variable and in Pretendard
/// (tests only). Declared last: the tests that read a file's code read it
/// up to its first `#[cfg(test)]`.
#[cfg(test)]
mod font_compare;
