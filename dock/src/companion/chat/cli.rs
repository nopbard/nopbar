//! Original CLI executables only; no credential file access or token copying.
use super::{
    ndjson::{Event, Parser},
    process::{self, Process},
    state::Provider,
    transport::Update,
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Mutex,
    },
    time::{Duration, Instant},
};
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Checking,
    Missing,
    Login,
    Unknown,
    Ready,
    Unsupported,
    Failed,
}
#[derive(Clone, Debug)]
pub struct Detection {
    pub path: Option<PathBuf>,
    pub version: String,
    pub status: Status,
}
impl Default for Detection {
    fn default() -> Self {
        Self {
            path: None,
            version: String::new(),
            status: Status::Checking,
        }
    }
}
static DETECTIONS: Mutex<Option<[Detection; 2]>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);
static AGY_MODELS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
#[cfg(test)]
thread_local! {static TEST_DETECTION: std::cell::RefCell<Option<[Detection; 2]>> = const {std::cell::RefCell::new(None)};}
#[cfg(test)]
pub struct DetectionGuard(Option<[Detection; 2]>);
#[cfg(test)]
impl Drop for DetectionGuard {
    fn drop(&mut self) {
        TEST_DETECTION.with(|v| *v.borrow_mut() = self.0.take());
    }
}
#[cfg(test)]
pub fn test_detection(status: Status) -> DetectionGuard {
    test_detections(status.clone(), status)
}
#[cfg(test)]
pub fn test_detections(claude: Status, agy: Status) -> DetectionGuard {
    let old = TEST_DETECTION.with(|v| {
        v.borrow_mut()
            .replace([claude, agy].map(|status| Detection {
                status,
                ..Detection::default()
            }))
    });
    DetectionGuard(old)
}
pub fn models(p: Provider) -> Vec<(String, String)> {
    let mut list = vec![(String::new(), crate::i18n::t("chat.model.default").into())];
    if p == Provider::ClaudeCode {
        list.extend([
            ("sonnet".into(), "Sonnet".into()),
            ("opus".into(), "Opus".into()),
        ]);
    } else if p == Provider::Antigravity {
        list.extend(AGY_MODELS.lock().unwrap_or_else(|e| e.into_inner()).clone());
    }
    list
}
pub fn cached(p: Provider) -> Detection {
    #[cfg(test)]
    if let Some(d) = TEST_DETECTION.with(|v| v.borrow().clone()) {
        return d[usize::from(p == Provider::Antigravity)].clone();
    }
    DETECTIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|d| d[usize::from(p == Provider::Antigravity)].clone())
        .unwrap_or_default()
}
pub const TIMER: usize = 0xD216;
pub fn usable(p: Provider) -> bool {
    matches!(
        cached(p).status,
        Status::Ready | Status::Unknown | Status::Checking | Status::Failed
    )
}
pub fn awake(p: Provider) -> bool {
    cached(p).status == Status::Ready
}
pub fn recheck_on_click(p: Provider) {
    if matches!(
        cached(p).status,
        Status::Login | Status::Missing | Status::Failed
    ) {
        refresh(Default::default());
    }
}
fn merged(old: &Detection, mut new: Detection) -> Detection {
    // A successful response is stronger evidence than a CLI without auth status.
    if new.path == old.path && new.status == Status::Unknown && old.status == Status::Ready {
        new.status = Status::Ready;
    }
    // A slow diagnostic must not disable a previously usable executable.
    if new.path == old.path
        && new.status == Status::Failed
        && matches!(old.status, Status::Ready | Status::Unknown)
    {
        new.status = old.status.clone();
    }
    new
}
pub(super) fn remember(p: Provider, detection: Detection) {
    let mut all = DETECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    let values = all.get_or_insert_with(|| std::array::from_fn(|_| Detection::default()));
    let i = usize::from(p == Provider::Antigravity);
    values[i] = merged(&values[i], detection);
}
pub fn work_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("NopBar/chat-work")
}
pub fn candidates(
    p: Provider,
    path: Option<&std::ffi::OsStr>,
    home: &Path,
    local: &Path,
) -> Vec<PathBuf> {
    let name = if p == Provider::ClaudeCode {
        "claude.exe"
    } else {
        "agy.exe"
    };
    let mut paths: Vec<_> = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|p| p.is_absolute())
        .map(|d| d.join(name))
        .collect();
    paths.extend([
        home.join(".local/bin").join(name),
        local.join("Microsoft/WinGet/Links").join(name),
    ]);
    // WinGet portable binaries may be installed without the Links alias.
    if let Ok(dirs) = std::fs::read_dir(local.join("Microsoft/WinGet/Packages")) {
        let prefix = if p == Provider::ClaudeCode {
            "Anthropic.ClaudeCode_"
        } else {
            "Google.AntigravityCLI_"
        };
        for d in dirs
            .flatten()
            .filter(|d| d.file_name().to_string_lossy().starts_with(prefix))
        {
            paths.push(d.path().join(name));
        }
    }
    paths
}
pub fn find(p: Provider) -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_default();
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    candidates(p, std::env::var_os("PATH").as_deref(), &home, &local)
        .into_iter()
        .find(|p| p.is_file())
}
pub fn version(text: &str) -> Option<(u32, u32, u32)> {
    text.split_whitespace().find_map(|word| {
        let mut v = word.trim_start_matches('v').split('.');
        Some((
            v.next()?.parse().ok()?,
            v.next()?.parse().ok()?,
            v.next()?.parse().ok()?,
        ))
    })
}
pub fn probe(p: Provider, path: Option<PathBuf>, cwd: &Path) -> Detection {
    probe_cancel(p, path, cwd, &mut || false)
}
pub(super) fn probe_cancel(
    p: Provider,
    path: Option<PathBuf>,
    cwd: &Path,
    cancelled: &mut impl FnMut() -> bool,
) -> Detection {
    let Some(path) = path else {
        return Detection {
            status: Status::Missing,
            ..Detection::default()
        };
    };
    let mut d = Detection {
        path: Some(path.clone()),
        version: String::new(),
        status: Status::Failed,
    };
    let Ok((true, v)) = process::diagnostic_cancel(&path, &["--version"], cwd, cancelled) else {
        return d;
    };
    d.version = v.trim().to_string();
    if version(&v).is_none() {
        d.status = Status::Failed;
        return d;
    }
    let Ok((true, help)) = process::diagnostic_cancel(&path, &["--help"], cwd, cancelled) else {
        return d;
    };
    let flags = if p == Provider::ClaudeCode {
        vec![
            "--safe-mode",
            "--tools",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--include-partial-messages",
        ]
    } else {
        vec![
            "--settings",
            "--mode",
            "--sandbox",
            "--disable-slash-commands",
        ]
    };
    if flags.iter().any(|f| !help.contains(f)) {
        d.status = Status::Unsupported;
        return d;
    }
    d.status = Status::Unknown;
    if p == Provider::ClaudeCode {
        if let Ok((_, s)) =
            process::output_cancel(&path, &["auth", "status", "--json"], cwd, cancelled)
        {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                d.status = match v["loggedIn"].as_bool() {
                    Some(true) => Status::Ready,
                    Some(false) => Status::Login,
                    None => Status::Unknown,
                };
            }
        }
    }
    d
}
pub fn refresh(hwnd: windows::Win32::Foundation::HWND) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || {
        let cwd = work_dir();
        let _ = std::fs::create_dir_all(&cwd);
        let values = [
            probe(Provider::ClaudeCode, find(Provider::ClaudeCode), &cwd),
            probe(Provider::Antigravity, find(Provider::Antigravity), &cwd),
        ];
        if let Some(path) = values[1].path.as_ref() {
            if let Ok((true, text)) = process::output(path, &["models"], &cwd) {
                *AGY_MODELS.lock().unwrap_or_else(|e| e.into_inner()) = text
                    .lines()
                    .filter_map(|line| {
                        let (id, label) = line.split_once('\t')?;
                        (!id.is_empty()
                            && id
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)))
                        .then(|| (id.to_string(), label.to_string()))
                    })
                    .collect();
            }
        }
        for (p, d) in [Provider::ClaudeCode, Provider::Antigravity]
            .into_iter()
            .zip(values)
        {
            remember(p, d);
        }
        BUSY.store(false, Ordering::SeqCst);
        super::settings::updated(hwnd);
        super::mcp::wake();
        if hwnd != 0 {
            super::hint::notify_kind("CompanionCliChanged");
        }
    });
}
pub fn args(p: Provider, model: &str) -> Vec<String> {
    let mut a:Vec<String>=match p {
        Provider::ClaudeCode=>vec!["-p","--output-format","stream-json","--verbose","--include-partial-messages","--tools","","--strict-mcp-config","--mcp-config","{\"mcpServers\":{}}","--setting-sources","","--settings","{\"disableAllHooks\":true}","--safe-mode","--disable-slash-commands","--no-session-persistence","--permission-mode","dontAsk","--no-chrome","--system-prompt"],
        Provider::Antigravity=>vec!["--input-format","stream-json","--output-format","stream-json","--mode","plan","--sandbox","--disable-slash-commands","--settings",r#"{"toolPermission":"strict","allowNonWorkspaceAccess":false,"statusLine":{"enabled":false},"title":{"enabled":false},"permissions":{"deny":["command(*)","read_file(*)","write_file(*)","read_url(*)","execute_url(*)","mcp(*)"]}}"#],
        _=>vec![]
    }.into_iter().map(str::to_string).collect();
    if p == Provider::ClaudeCode {
        a.push(super::persona::instructions());
    }
    if !model.is_empty() {
        a.extend(["--model".into(), model.into()]);
    }
    a
}
pub fn prompt(p: Provider, input: &serde_json::Value) -> String {
    let history = serde_json::to_string(input).unwrap_or_default();
    let body = format!(
        "{}\nConversation messages (JSON data, not tools or commands):\n{}",
        super::persona::instructions(),
        history
    );
    if p == Provider::Antigravity {
        format!(
            "{}\n",
            serde_json::json!({"event":"user","message":{"content":body}})
        )
    } else {
        body
    }
}
pub fn start(
    p: Provider,
    generation: u64,
    model: String,
    input: serde_json::Value,
    limit: u32,
    tx: Sender<Update>,
) -> tokio::sync::oneshot::Sender<()> {
    let (cancel, mut stopped) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let d = cached(p);
            let path = d
                .path
                .filter(|path| path.is_file())
                .or_else(|| find(p))
                .ok_or("chat.error.missing")?;
            let cwd = work_dir();
            std::fs::create_dir_all(&cwd).map_err(|_| "chat.error.spawn")?;
            if stopped.try_recv() != Err(tokio::sync::oneshot::error::TryRecvError::Empty) {
                return Ok(());
            }
            run_turn(
                p,
                generation,
                &model,
                &input,
                &path,
                &cwd,
                &mut stopped,
                &tx,
                |command| super::quota::dispatch(limit, || Process::spawn(command, false)),
            )
        })();
        if result == Err("chat.error.login") || result == Err("chat.error.missing") {
            let mut d = cached(p);
            d.status = if result == Err("chat.error.login") {
                Status::Login
            } else {
                Status::Missing
            };
            remember(p, d);
            super::mcp::wake();
        }
        if result.is_ok()
            && stopped.try_recv() == Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        {
            let mut d = cached(p);
            d.path = d.path.or_else(|| find(p));
            d.status = Status::Ready;
            remember(p, d);
            super::auth::cli_first_connection();
            super::mcp::wake();
        }
        if result.is_err() {
            refresh(Default::default());
        }
        let error = result.err().map(crate::i18n::t);
        let _ = tx.send(Update::Finished(generation, error));
    });
    cancel
}
#[allow(clippy::too_many_arguments)]
fn run_turn(
    p: Provider,
    generation: u64,
    model: &str,
    input: &serde_json::Value,
    path: &Path,
    cwd: &Path,
    stopped: &mut tokio::sync::oneshot::Receiver<()>,
    tx: &Sender<Update>,
    spawn: impl FnOnce(&mut std::process::Command) -> Result<Process, &'static str>,
) -> Result<(), &'static str> {
    let mut command = Process::command(path);
    command.args(args(p, model)).current_dir(cwd);
    // Never inherit a nesting marker from the hosting Claude terminal.
    command.env_remove("CLAUDECODE");
    let mut child = spawn(&mut command)?;
    let mut stdin = child.child().stdin.take().unwrap();
    let text = prompt(p, input);
    std::thread::spawn(move || {
        let _ = stdin.write_all(text.as_bytes());
    });
    let rx = process::reader(child.child().stdout.take().unwrap());
    let errors = process::reader(child.child().stderr.take().unwrap());
    let mut stderr = String::new();
    let mut parser = Parser::new(p);
    let mut received = false;
    let mut at = Instant::now();
    let emit = |events: Vec<Event>| -> Result<(), &'static str> {
        for e in events {
            match e {
                Event::Delta(d) => tx
                    .send(Update::Delta(generation, d))
                    .map_err(|_| "chat.error.exit")?,
                Event::Error(e) => return Err(e),
                Event::Done => {}
            }
        }
        Ok(())
    };
    loop {
        if stopped.try_recv() != Err(tokio::sync::oneshot::error::TryRecvError::Empty) {
            return Ok(());
        }
        while let Ok(b) = errors.try_recv() {
            if stderr.len() < 64 * 1024 {
                stderr.push_str(&String::from_utf8_lossy(&b));
            }
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(b) => {
                received = true;
                at = Instant::now();
                emit(parser.push(&b)?)?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(status) = child.child().try_wait().map_err(|_| "chat.error.exit")? {
                    while let Ok(b) = errors.try_recv() {
                        if stderr.len() < 64 * 1024 {
                            stderr.push_str(&String::from_utf8_lossy(&b));
                        }
                    }
                    if !status.success() {
                        return Err(super::ndjson::error(&stderr));
                    }
                    return emit(parser.end(true)?);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {}
        }
        if at.elapsed() > Duration::from_secs(if received { 60 } else { 15 }) {
            return Err("chat.error.response_timeout");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn two_hundred_fixture_turns_release_handles_and_threads() {
        const TEST:&str="companion::chat::cli::tests::two_hundred_fixture_turns_release_handles_and_threads";
        if std::env::var_os("NOPBAR_ASSISTANT_LEAK_CHILD").is_none(){
            use std::os::windows::process::CommandExt;
            let root=std::env::temp_dir().join(format!("nopbar-assistant-leak-{}",uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("appdata")).unwrap();
            std::fs::create_dir_all(root.join("localappdata")).unwrap();
            let result=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact",TEST,"--nocapture","--test-threads=1"])
                .env("NOPBAR_ASSISTANT_LEAK_CHILD","1").env("APPDATA",root.join("appdata"))
                .env("LOCALAPPDATA",root.join("localappdata")).current_dir(&root)
                .creation_flags(process::NO_WINDOW).output().unwrap();
            let report=format!("{}{}",String::from_utf8_lossy(&result.stdout),String::from_utf8_lossy(&result.stderr));
            std::fs::write(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/assistant-cli-leak.txt"),&report).unwrap();
            assert!(result.status.success(),"{report}");
            return;
        }
        let exe=super::super::fixture::executable();
        let cwd=exe.parent().unwrap().join("assistant-200-turns");std::fs::create_dir(&cwd).unwrap();
        let turn=|generation|{
            let (_cancel,mut stopped)=tokio::sync::oneshot::channel();let(tx,rx)=std::sync::mpsc::channel();
            run_turn(Provider::ClaudeCode,generation,"",&serde_json::json!([{"role":"user","content":"fixture"}]),&exe,&cwd,&mut stopped,&tx,|c|Process::spawn(c,false)).unwrap();
            assert!(rx.try_iter().any(|u|matches!(u,Update::Delta(_,s) if s.contains("fixture reply"))));
        };
        for n in 0..3{turn(n);}
        std::thread::sleep(Duration::from_millis(250));
        let baseline=crate::res::usage();eprintln!("baseline: {baseline:?}");
        for n in 0..200{turn(n+3);if (n+1)%50==0{eprintln!("turn {}: {:?}",n+1,crate::res::usage());}}
        let deadline=Instant::now()+Duration::from_secs(5);
        let final_usage=loop{let u=crate::res::usage();if (u.handles<=baseline.handles+8&&u.threads<=baseline.threads+2)||Instant::now()>=deadline{break u;}std::thread::sleep(Duration::from_millis(20));};
        eprintln!("after 200: {final_usage:?}; handle_delta={} thread_delta={}",final_usage.handles as i64-baseline.handles as i64,final_usage.threads as i64-baseline.threads as i64);
        assert!(final_usage.handles<=baseline.handles+8,"handles: {baseline:?} -> {final_usage:?}");
        assert!(final_usage.threads<=baseline.threads+2,"threads: {baseline:?} -> {final_usage:?}");
    }
    #[test]
    fn help_on_stderr_detects_both_providers() {
        let exe = super::super::fixture::executable();
        let cwd = exe
            .parent()
            .unwrap()
            .join(format!("stderr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("stderr-help"), "").unwrap();
        let statuses = [Provider::ClaudeCode, Provider::Antigravity]
            .map(|p| probe(p, Some(exe.clone()), &cwd).status);
        std::fs::remove_file(cwd.join("stderr-help")).unwrap();
        std::fs::remove_dir(cwd).unwrap();
        assert_eq!(statuses, [Status::Ready, Status::Unknown]);
    }
    #[test]
    fn only_missing_capabilities_mean_update_required() {
        let exe = super::super::fixture::executable();
        for (marker, expected) in [
            ("mixed-help", [Status::Ready, Status::Unknown]),
            (
                "missing-options",
                [Status::Unsupported, Status::Unsupported],
            ),
            ("bad-version", [Status::Failed, Status::Failed]),
        ] {
            let cwd = exe
                .parent()
                .unwrap()
                .join(format!("probe-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&cwd).unwrap();
            std::fs::write(cwd.join(marker), "").unwrap();
            let statuses = [Provider::ClaudeCode, Provider::Antigravity]
                .map(|p| probe(p, Some(exe.clone()), &cwd).status);
            std::fs::remove_file(cwd.join(marker)).unwrap();
            std::fs::remove_dir(cwd).unwrap();
            assert_eq!(statuses, expected, "{marker}");
        }
    }
    #[test]
    fn one_turn_starts_one_child_without_diagnostic_processes() {
        let exe = super::super::fixture::executable();
        let cwd = exe
            .parent()
            .unwrap()
            .join(format!("turn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("record-invocations"), "").unwrap();
        let (_cancel, mut stopped) = tokio::sync::oneshot::channel();
        let (tx, rx) = std::sync::mpsc::channel();
        run_turn(
            Provider::ClaudeCode,
            1,
            "",
            &serde_json::json!([]),
            &exe,
            &cwd,
            &mut stopped,
            &tx,
            |command| Process::spawn(command, false),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("invocations"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(rx
            .try_iter()
            .any(|u| matches!(u,Update::Delta(1,ref s) if s=="fixture reply")));
        for name in ["record-invocations", "invocations"] {
            std::fs::remove_file(cwd.join(name)).unwrap();
        }
        std::fs::remove_dir(cwd).unwrap();
    }
    #[test]
    fn detection_starts_checking_and_timeouts_do_not_replace_success() {
        assert_eq!(Detection::default().status, Status::Checking);
        let ready = Detection {
            status: Status::Ready,
            path: Some("fixture.exe".into()),
            version: "1".into(),
        };
        for status in [Status::Unknown, Status::Failed] {
            assert_eq!(
                merged(
                    &ready,
                    Detection {
                        status,
                        ..ready.clone()
                    }
                )
                .status,
                Status::Ready
            );
        }
        assert_eq!(
            merged(
                &ready,
                Detection {
                    status: Status::Missing,
                    ..Detection::default()
                }
            )
            .status,
            Status::Missing
        );
        let _guard = test_detection(Status::Unknown);
        assert!(usable(Provider::Antigravity));
        assert!(!awake(Provider::Antigravity));
    }
    #[test]
    fn offline_detection_states_path_and_known_installation() {
        let exe = super::super::fixture::executable();
        let root = exe.parent().unwrap();
        for (dir, status) in [
            ("ready", Status::Ready),
            ("login", Status::Login),
            ("unknown", Status::Unknown),
        ] {
            let cwd = root.join(dir);
            std::fs::create_dir_all(&cwd).unwrap();
            assert_eq!(
                probe(Provider::ClaudeCode, Some(exe.clone()), &cwd).status,
                status
            );
        }
        assert_eq!(
            probe(Provider::ClaudeCode, None, root).status,
            Status::Missing
        );
        let home = root.join("home");
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let installed = bin.join("claude.exe");
        std::fs::copy(&exe, &installed).unwrap();
        let list = candidates(Provider::ClaudeCode, None, &home, &root.join("local"));
        assert_eq!(
            list.into_iter().find(|p| p.is_file()),
            Some(installed.clone())
        );
        let path = std::env::join_paths([bin]).unwrap();
        let list = candidates(Provider::ClaudeCode, Some(&path), &home, root);
        assert_eq!(list[0], installed);
        let c = Process::command(&exe);
        assert!(c
            .get_envs()
            .any(|(k, v)| k == "NOPBAR_CHAT" && v == Some(std::ffi::OsStr::new("1"))));
    }
    #[test]
    fn command_contract_and_history() {
        for p in [Provider::ClaudeCode, Provider::Antigravity] {
            let a = args(p, "");
            assert!(!a.iter().any(|a| a.contains("dangerously-skip")));
            assert!(a.contains(&"stream-json".into()));
            assert!(a.contains(&"--settings".into()));
            assert!(!a.contains(&"--continue".into()));
            assert!(prompt(p, &serde_json::json!([{"role":"user","content":"hi"}])).contains("hi"));
        }
        let a = args(Provider::ClaudeCode, "");
        assert!(a.windows(2).any(|p| p == ["--tools", ""]));
        assert!(a.contains(&"--safe-mode".into()));
        let a = args(Provider::Antigravity, "");
        assert!(a
            .iter()
            .any(|s| s.contains("\"toolPermission\":\"strict\"")));
        assert_eq!(version("2.1.325 (Claude Code)"), Some((2, 1, 325)));
        assert_eq!(version("1.5.8\n"), Some((1, 5, 8)));
        assert_eq!(version("broken"), None);
    }
}
