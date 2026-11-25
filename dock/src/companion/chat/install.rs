//! Explicit install/login action only. No action is run when the dialog opens.
use super::{
    cli,
    process::{self, LoginProcess, Process},
    state::Provider,
};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Confirm,
    Installing,
    Login,
    Verify,
    Done,
    Installed,
    Failed(&'static str),
    Cancelled,
}
#[derive(Clone)]
pub struct Snapshot {
    pub provider: Provider,
    pub mode: Mode,
    pub stage: Stage,
    pub details: bool,
    pub scroll: f32,
    pub output: String,
    pub generation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Install,
    Update,
    Login,
}
impl Mode {
    pub fn for_status(status: &cli::Status) -> Self {
        match status {
            cli::Status::Unsupported => Self::Update,
            cli::Status::Login => Self::Login,
            _ => Self::Install,
        }
    }
}
struct Dialog {
    state: Snapshot,
    scroll_max: f32,
    cancel: Arc<AtomicBool>,
}
static DIALOG: Mutex<Option<Dialog>> = Mutex::new(None);
static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
thread_local! { static TEST_SNAPSHOT: std::cell::RefCell<Option<Option<Snapshot>>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
pub struct SnapshotGuard(Option<Option<Snapshot>>);
#[cfg(test)]
impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        TEST_SNAPSHOT.with(|v| *v.borrow_mut() = self.0.take());
    }
}
#[cfg(test)]
pub fn test_snapshot(s: Option<Snapshot>) -> SnapshotGuard {
    SnapshotGuard(TEST_SNAPSHOT.with(|v| v.borrow_mut().replace(s)))
}
pub fn snapshot() -> Option<Snapshot> {
    #[cfg(test)]
    if let Some(s) = TEST_SNAPSHOT.with(|v| v.borrow().clone()) {
        return s;
    }
    DIALOG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|d| d.state.clone())
}
pub fn open(p: Provider) {
    open_mode(p, Mode::for_status(&cli::cached(p).status));
}
pub fn open_mode(p: Provider, mode: Mode) {
    close();
    let generation = SERIAL.fetch_add(1, Ordering::SeqCst) + 1;
    *DIALOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(Dialog {
        state: Snapshot {
            provider: p,
            mode,
            stage: Stage::Confirm,
            details: false,
            scroll: 0.,
            output: String::new(),
            generation,
        },
        cancel: Arc::new(AtomicBool::new(false)),
        scroll_max: 0.,
    });
}
pub fn close() {
    if let Some(d) = DIALOG.lock().unwrap_or_else(|e| e.into_inner()).take() {
        d.cancel.store(true, Ordering::SeqCst);
    }
}
pub fn details() {
    if let Some(d) = DIALOG.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        d.state.details = !d.state.details;
        d.state.scroll = 0.;
    }
}
pub fn scroll(delta: f32) {
    if let Some(d) = DIALOG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .filter(|d| d.state.details)
    {
        d.state.scroll = (d.state.scroll + delta).clamp(0., d.scroll_max);
    }
}
pub fn scroll_limit(generation: u64, max: f32) {
    if let Some(d) = DIALOG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .filter(|d| d.state.generation == generation)
    {
        d.scroll_max = max.max(0.);
        d.state.scroll = d.state.scroll.min(d.scroll_max);
    }
}
pub fn source(p: Provider) -> &'static str {
    match p {
        Provider::ClaudeCode => "https://code.claude.com/docs/en/setup",
        _ => "https://antigravity.google/docs/cli/overview",
    }
}
pub fn install_command(p: Provider) -> (PathBuf, Vec<String>) {
    if p == Provider::ClaudeCode {
        let exe =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        (
            exe,
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                "$ErrorActionPreference='Stop'; irm https://claude.ai/install.ps1 | iex".into(),
            ],
        )
    } else {
        let exe = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
            .join("Microsoft/WindowsApps/winget.exe");
        (
            exe,
            [
                "install",
                "--id",
                "Google.AntigravityCLI",
                "--exact",
                "--source",
                "winget",
                "--scope",
                "user",
                "--silent",
                "--disable-interactivity",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        )
    }
}
pub fn update_command(p: Provider) -> (PathBuf, Vec<String>) {
    if p == Provider::ClaudeCode {
        (
            cli::find(p).unwrap_or_else(|| "claude.exe".into()),
            vec!["update".into()],
        )
    } else {
        let (exe, mut args) = install_command(p);
        args[0] = "upgrade".into();
        (exe, args)
    }
}
/// Short display commands; execution always uses the structured commands above.
pub fn command_text(p: Provider, mode: Mode) -> String {
    let login = if p == Provider::ClaudeCode {
        "claude auth login"
    } else {
        "agy"
    };
    match (p, mode) {
        (_, Mode::Login) => login.into(),
        (Provider::ClaudeCode, Mode::Update) => "claude update".into(),
        (Provider::ClaudeCode, Mode::Install) => {
            format!("irm https://claude.ai/install.ps1 | iex\n{login}")
        }
        (_, Mode::Update) => "winget upgrade Google.AntigravityCLI --scope user".into(),
        (_, Mode::Install) => format!("winget install Google.AntigravityCLI --scope user\n{login}"),
    }
}
pub fn progressing() -> bool {
    snapshot().is_some_and(|d| matches!(d.stage, Stage::Installing | Stage::Login | Stage::Verify))
}

fn update(generation: u64, stage: Stage, hwnd: isize) {
    if let Some(d) = DIALOG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .filter(|d| d.state.generation == generation)
    {
        d.state.stage = stage;
    }
    super::settings::updated(hwnd);
}
fn log(generation: u64, bytes: &[u8]) {
    if let Some(d) = DIALOG
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .filter(|d| d.state.generation == generation)
    {
        // Only installer output. Login output can contain authorization URLs/codes
        // and is never captured, retained or displayed in NopBar.
        if d.state.output.len() < 16 * 1024 {
            d.state.output.push_str(&String::from_utf8_lossy(bytes));
        }
    }
}
pub fn begin(hwnd: windows::Win32::Foundation::HWND) {
    let (state, cancel) = {
        let mut lock = DIALOG.lock().unwrap_or_else(|e| e.into_inner());
        let Some(d) = lock.as_mut() else { return };
        if !matches!(
            d.state.stage,
            Stage::Confirm | Stage::Failed(_) | Stage::Cancelled
        ) {
            return;
        }
        d.cancel = Arc::new(AtomicBool::new(false));
        d.state.stage = if d.state.mode == Mode::Login {
            Stage::Login
        } else {
            Stage::Installing
        };
        d.state.output.clear();
        (d.state.clone(), d.cancel.clone())
    };
    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || {
        let g = state.generation;
        let p = state.provider;
        let result = (|| {
            let cwd = cli::work_dir();
            std::fs::create_dir_all(&cwd).map_err(|_| "chat.error.spawn")?;
            if state.mode == Mode::Update || (state.mode == Mode::Install && cli::find(p).is_none())
            {
                let (exe, args) = if state.mode == Mode::Update {
                    update_command(p)
                } else {
                    install_command(p)
                };
                let mut c = Process::command(&exe);
                c.args(args).current_dir(&cwd).stdin(Stdio::null());
                let mut process = Process::spawn(&mut c, false)?;
                let out = process::reader(process.child().stdout.take().unwrap());
                let err = process::reader(process.child().stderr.take().unwrap());
                let started = Instant::now();
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        return Err("chat.install.cancelled");
                    }
                    for rx in [&out, &err] {
                        while let Ok(b) = rx.try_recv() {
                            log(g, &b);
                            super::settings::updated(hwnd);
                        }
                    }
                    if let Some(status) =
                        process.child().try_wait().map_err(|_| "chat.error.spawn")?
                    {
                        if !status.success() {
                            return Err("chat.install.failed");
                        }
                        break;
                    }
                    if started.elapsed() > Duration::from_secs(600) {
                        return Err("chat.error.install_timeout");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if cancel.load(Ordering::SeqCst) {
                return Err("chat.install.cancelled");
            }
            let exe = cli::find(p).ok_or("chat.error.missing")?;
            if state.mode == Mode::Update {
                update(g, Stage::Verify, hwnd);
                let detected =
                    cli::probe_cancel(p, Some(exe), &cwd, &mut || cancel.load(Ordering::SeqCst));
                if cancel.load(Ordering::SeqCst) {
                    return Err("chat.install.cancelled");
                }
                let status = detected.status.clone();
                cli::remember(p, detected);
                return match status {
                    cli::Status::Unsupported => Err("chat.error.version"),
                    cli::Status::Failed | cli::Status::Missing | cli::Status::Checking => {
                        Err("chat.error.spawn")
                    }
                    _ => Ok(Stage::Done),
                };
            }
            update(g, Stage::Login, hwnd);
            // Both programs may require interactive browser-code fallback.
            // The owner explicitly requested a visible terminal when needed.
            let mut c = Command::new(exe);
            c.current_dir(&cwd)
                .env("NOPBAR_CHAT", "1")
                .env_remove("CLAUDECODE");
            if p == Provider::ClaudeCode {
                c.args(["auth", "login"]);
            }
            let mut login = LoginProcess::spawn(&mut c)?;
            let at = Instant::now();
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return Err("chat.install.cancelled");
                }
                if login
                    .child()
                    .try_wait()
                    .map_err(|_| "chat.error.spawn")?
                    .is_some()
                {
                    // A user may close the interactive terminal after browser
                    // sign-in. The read-only status check is authoritative.
                    break;
                }
                if at.elapsed() > Duration::from_secs(900) {
                    return Err("chat.error.login_timeout");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            update(g, Stage::Verify, hwnd);
            let detected =
                cli::probe_cancel(p, cli::find(p), &cwd, &mut || cancel.load(Ordering::SeqCst));
            if cancel.load(Ordering::SeqCst) {
                return Err("chat.install.cancelled");
            }
            cli::remember(p, detected.clone());
            completion(detected.status)
        })();
        update(
            g,
            match result {
                Ok(stage) => stage,
                Err("chat.install.cancelled") => Stage::Cancelled,
                Err(e) => Stage::Failed(e),
            },
            hwnd,
        );
        cli::refresh(windows::Win32::Foundation::HWND(hwnd as *mut _));
    });
}
fn completion(status: cli::Status) -> Result<Stage, &'static str> {
    match status {
        cli::Status::Ready => Ok(Stage::Done),
        cli::Status::Unknown => Ok(Stage::Installed),
        _ => Err("chat.error.login"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_commands_are_short_and_do_not_change_execution_flags() {
        assert_eq!(
            command_text(Provider::Antigravity, Mode::Install),
            "winget install Google.AntigravityCLI --scope user\nagy"
        );
        assert_eq!(
            command_text(Provider::Antigravity, Mode::Update),
            "winget upgrade Google.AntigravityCLI --scope user"
        );
        assert_eq!(
            command_text(Provider::ClaudeCode, Mode::Login),
            "claude auth login"
        );
        for p in [Provider::ClaudeCode, Provider::Antigravity] {
            for mode in [Mode::Install, Mode::Update, Mode::Login] {
                let text = command_text(p, mode);
                assert!(!text.contains(['&', '\'', '"', '\\']));
            }
        }
        let (_, args) = install_command(Provider::Antigravity);
        assert!(args.iter().any(|a| a == "--disable-interactivity"));
        assert!(args.iter().any(|a| a == "--accept-package-agreements"));
    }
    #[test]
    fn update_and_login_modes_never_include_the_other_operation() {
        assert_eq!(Mode::for_status(&cli::Status::Unsupported), Mode::Update);
        assert_eq!(Mode::for_status(&cli::Status::Login), Mode::Login);
        assert_eq!(Mode::for_status(&cli::Status::Missing), Mode::Install);
        let (_, args) = update_command(Provider::Antigravity);
        assert_eq!(args[0], "upgrade");
        assert!(args.windows(2).any(|a| a == ["--scope", "user"]));
        let (_, args) = update_command(Provider::ClaudeCode);
        assert_eq!(args, ["update"]);
        for provider in [Provider::ClaudeCode, Provider::Antigravity] {
            let update = command_text(provider, Mode::Update);
            assert!(!update.contains("auth login") && !update.contains("interactive sign-in"));
            let login = command_text(provider, Mode::Login);
            assert!(
                !login.contains("install.ps1")
                    && !login.contains("winget")
                    && !login.contains("update")
            );
        }
    }
    #[test]
    fn explicit_confirmation_retry_and_cancellation() {
        assert_eq!(completion(cli::Status::Unknown), Ok(Stage::Installed));
        assert_eq!(completion(cli::Status::Ready), Ok(Stage::Done));
        assert_eq!(completion(cli::Status::Login), Err("chat.error.login"));
        open(Provider::ClaudeCode);
        assert_eq!(snapshot().unwrap().stage, Stage::Confirm);
        details();
        assert!(snapshot().unwrap().details);
        let g = snapshot().unwrap().generation;
        scroll_limit(g, 32.);
        scroll(500.);
        assert_eq!(snapshot().unwrap().scroll, 32.);
        scroll(-16.);
        assert_eq!(snapshot().unwrap().scroll, 16.);
        scroll_limit(g, 0.);
        assert_eq!(snapshot().unwrap().scroll, 0.);
        for stage in [
            Stage::Installing,
            Stage::Login,
            Stage::Verify,
            Stage::Done,
            Stage::Cancelled,
            Stage::Confirm,
        ] {
            update(g, stage.clone(), 0);
            assert_eq!(snapshot().unwrap().stage, stage);
        }
        let cancelled = DIALOG.lock().unwrap().as_ref().unwrap().cancel.clone();
        update(g, Stage::Failed("test"), 0);
        assert!(matches!(snapshot().unwrap().stage, Stage::Failed(_)));
        open(Provider::Antigravity);
        assert!(cancelled.load(Ordering::SeqCst));
        update(g, Stage::Done, 0);
        assert_eq!(snapshot().unwrap().stage, Stage::Confirm);
        let retry = snapshot().unwrap().generation;
        update(retry, Stage::Installing, 0);
        log(retry, b"download 50%\ninstalled\n");
        assert!(snapshot().unwrap().output.contains("download 50%"));
        update(retry, Stage::Done, 0);
        assert_eq!(snapshot().unwrap().stage, Stage::Done);
        close();
        assert!(snapshot().is_none());
    }
    #[test]
    fn only_official_installers_user_scope_no_prompt() {
        let (_, a) = install_command(Provider::ClaudeCode);
        assert!(a.last().unwrap().contains("https://claude.ai/install.ps1"));
        let (_, a) = install_command(Provider::Antigravity);
        assert!(a.windows(2).any(|a| a == ["--id", "Google.AntigravityCLI"]));
        assert!(a.windows(2).any(|a| a == ["--scope", "user"]));
        assert!(!a.iter().any(|a| a == "--force" || a == "--override"));
    }
}
