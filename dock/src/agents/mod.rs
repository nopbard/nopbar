//! The AI agents' board (docs/superpowers/briefs/2026-11-12-agent-board.md):
//! which Claude Code and Codex sessions are working, waiting for their
//! person, or done, signalled by the companion.
//!
//! - hook.rs: the other side, `nopbar.exe --agent <claude|codex>`, run by
//!   the agents' hooks: one line to the dock's pipe, then gone;
//! - connect.rs: Settings > Agents' connect and disconnect (NopBar's
//!   entries in the agents' own settings, nothing else of theirs touched);
//! - awake.rs: no sleep while a session works.
//!
//! What arrives is data only: a state, a session's folder name, the
//! processes it runs under and the host window.
//! Nothing of it is run, opened or written anywhere: another process of
//! the same user (the pipe admits no one else) that sends a made-up line
//! can change the reported companion state and keep-awake status.
//!
//! The board itself (Board) is pure; the dock's pipe thread feeds it
//! (ingest), the companion reads its signal, and awake follows its work.

pub mod connect;
pub mod hook;
mod awake;
mod pipe;

/// The hooks' pipe starts listening (once).
pub fn listen() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(pipe::listen);
}

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A session's state: working, waiting for its person, or done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum St {
    Work,
    Wait,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn parse(s: &str) -> Option<Agent> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    /// The name a session goes by when its folder has none.
    pub fn title(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }
}

/// What a hook's line means for its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev {
    To(St),
    Failure,
    /// The session ended (Claude's SessionEnd).
    End,
    /// Claude's "still idle" reminder: a working session that never said
    /// Stop (stopped with Esc, or an error) is done; else nothing.
    Idle,
    /// Nothing the board shows.
    Ignore,
}

/// Pure: `agent`'s event `name` (Claude's hook_event_name, Codex's notify
/// type) with Claude's notification type `ntype`.
/// - Claude: a prompt sent, or a tool done (after a permission was given
///   the turn goes on with no new prompt) = working; a permission prompt or
///   a question (elicitation) = waiting; the "still idle" reminder a
///   minute after a turn = Idle (a turn stopped with Esc never says Stop);
///   other notifications (signed in, …) = nothing; Stop and StopFailure =
///   done; SessionEnd = gone.
/// - Codex's notify only tells a turn's end: done.
pub fn event_of(agent: Agent, name: &str, ntype: &str) -> Ev {
    match (agent, name) {
        (Agent::Claude, "UserPromptSubmit" | "PostToolUse") => Ev::To(St::Work),
        (Agent::Claude, "Notification") if ntype.contains("idle") => Ev::Idle,
        // (An older Claude sends no type: its notifications were questions.)
        (Agent::Claude, "Notification") if ntype.is_empty() || ntype.contains("permission") || ntype.contains("elicitation") => Ev::To(St::Wait),
        (Agent::Claude, "PermissionRequest") => Ev::To(St::Wait),
        (Agent::Claude, "Stop") => Ev::To(St::Done),
        (Agent::Claude, "StopFailure") => Ev::Failure,
        (Agent::Claude, "SessionEnd") => Ev::End,
        (Agent::Codex, "agent-turn-complete") => Ev::To(St::Done),
        _ => Ev::Ignore,
    }
}

/// The longest text kept from a line (a folder, an exe path, an id).
const MAX_TEXT: usize = 512;
/// Sessions kept at most (the oldest finished ones go first).
const MAX_SESSIONS: usize = 24;
/// A working session nothing was heard from for this long no longer keeps
/// the PC awake (a turn ended without a word: a crash, a lost line).
pub const STALE: Duration = Duration::from_secs(30 * 60);

/// One line from a hook (hook.rs), every field bounded.
#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub agent: Agent,
    pub ev: Ev,
    pub session: String,
    pub cwd: String,
    /// The agent's own process (gone: the session is gone).
    pub agent_pid: u32,
    /// The app it shows in (Orca, Windows Terminal), its window and exe.
    pub host_pid: u32,
    pub hwnd: isize,
    pub host_exe: String,
    /// When the hook ran (FILETIME, 100 ns; 0: not told): a line older
    /// than the last one heard for its session is dropped (hooks race).
    pub t: u64,
}

fn text(v: &serde_json::Value, key: &str) -> String {
    let s = v.get(key).and_then(|s| s.as_str()).unwrap_or("");
    s.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect()
}

fn num(v: &serde_json::Value, key: &str) -> u64 {
    v.get(key).and_then(|n| n.as_u64()).unwrap_or(0)
}

impl Msg {
    /// Pure: the pipe's `{"type":"Agent",...}` line (hook::line). None:
    /// not one of ours, or nothing the board shows.
    pub fn parse(v: &serde_json::Value) -> Option<Msg> {
        let agent = Agent::parse(v.get("agent")?.as_str()?)?;
        let ev = event_of(agent, &text(v, "event"), &text(v, "ntype"));
        let session = text(v, "session");
        if ev == Ev::Ignore || session.is_empty() {
            return None;
        }
        Some(Msg {
            agent,
            ev,
            session,
            cwd: text(v, "cwd"),
            agent_pid: num(v, "agent_pid").min(u32::MAX as u64) as u32,
            host_pid: num(v, "host_pid").min(u32::MAX as u64) as u32,
            hwnd: v.get("hwnd").and_then(|n| n.as_i64()).unwrap_or(0) as isize,
            host_exe: text(v, "host_exe"),
            t: num(v, "t"),
        })
    }
}

/// Pure: a session's name: its working folder's (the worktree's) name.
pub fn name_of(cwd: &str, agent: Agent) -> String {
    let n = cwd.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or("").trim();
    if n.is_empty() || n.ends_with(':') {
        agent.title().to_string()
    } else {
        n.chars().take(64).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// "claude:<session id>".
    pub key: String,
    pub agent: Agent,
    pub name: String,
    pub st: St,
    pub failed: bool,
    /// When it came to this state.
    pub since: Instant,
    pub agent_pid: u32,
    pub host_pid: u32,
    pub hwnd: isize,
    pub host_exe: String,
    /// The last line heard: when (STALE) and its hook's time.
    heard: Instant,
    last_t: u64,
}

/// Every session heard of, in the order they came.
#[derive(Default)]
pub struct Board {
    sessions: Vec<Session>,
}

impl Board {
    pub const fn new() -> Board {
        Board { sessions: Vec::new() }
    }

    /// Pure: what `m` changes (true: something did).
    pub fn apply(&mut self, m: &Msg, now: Instant) -> bool {
        let key = format!("{}:{}", m.agent.id(), m.session);
        let at = self.sessions.iter().position(|s| s.key == key);
        let new_completion = m.t != 0 && at.is_some_and(|i| m.t > self.sessions[i].last_t);
        if let Some(i) = at {
            let s = &mut self.sessions[i];
            if m.t != 0 && m.t < s.last_t {
                return false;
            }
            s.heard = now;
            s.last_t = s.last_t.max(m.t);
        }
        let st = match m.ev {
            Ev::To(st) => st,
            Ev::Failure => St::Done,
            Ev::End => {
                return at.map(|i| self.sessions.remove(i)).is_some();
            }
            Ev::Idle => match at {
                Some(i) if self.sessions[i].st == St::Work => St::Done,
                _ => return false,
            },
            Ev::Ignore => return false,
        };
        let name = name_of(&m.cwd, m.agent);
        match at {
            Some(i) => {
                let s = &mut self.sessions[i];
                let before = s.clone();
                if s.st != st || s.failed != (m.ev == Ev::Failure)
                    || st == St::Done && new_completion
                {
                    s.st = st;
                    s.since = now;
                }
                s.failed = m.ev == Ev::Failure;
                if !m.cwd.is_empty() {
                    s.name = name;
                }
                // The latest word on where it runs (a moved tab, a new
                // window).
                if m.host_pid != 0 {
                    (s.host_pid, s.hwnd, s.host_exe) = (m.host_pid, m.hwnd, m.host_exe.clone());
                }
                if m.agent_pid != 0 {
                    s.agent_pid = m.agent_pid;
                }
                *s != before
            }
            None => {
                self.sessions.push(Session {
                    key,
                    agent: m.agent,
                    name,
                    st,
                    failed: m.ev == Ev::Failure,
                    since: now,
                    agent_pid: m.agent_pid,
                    host_pid: m.host_pid,
                    hwnd: m.hwnd,
                    host_exe: m.host_exe.clone(),
                    heard: now,
                    last_t: m.t,
                });
                while self.sessions.len() > MAX_SESSIONS {
                    // The oldest finished one, else the oldest.
                    let i = self.sessions.iter().position(|s| s.st == St::Done).unwrap_or(0);
                    self.sessions.remove(i);
                }
                true
            }
        }
    }

    /// Drops the sessions whose agent is gone (`alive`: a process still
    /// runs); true if any went.
    pub fn prune(&mut self, alive: impl Fn(u32) -> bool) -> bool {
        let n = self.sessions.len();
        self.sessions.retain(|s| {
            let pid = if s.agent_pid != 0 { s.agent_pid } else { s.host_pid };
            pid == 0 || alive(pid)
        });
        self.sessions.len() != n
    }

    /// A session works (and was heard from within STALE).
    pub fn working(&self, now: Instant) -> bool {
        self.sessions.iter().any(|s| s.st == St::Work && now.saturating_duration_since(s.heard) < STALE)
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

static BOARD: Mutex<Board> = Mutex::new(Board::new());

/// Run island integration checks against a real session without hooks or windows.
#[cfg(test)]
pub(crate) fn with_test_session(st: St, check: impl FnOnce()) {
    struct Restore(Option<Board>);
    impl Drop for Restore {
        fn drop(&mut self) {
            *BOARD.lock().unwrap() = self.0.take().unwrap();
        }
    }
    let mut board = Board::new();
    board.apply(&Msg {
        agent: Agent::Claude, ev: Ev::To(st), session: "island-regression".into(),
        cwd: "test-project".into(), agent_pid: 0, host_pid: 0, hwnd: 0,
        host_exe: String::new(), t: 0,
    }, Instant::now());
    let _restore = Restore(Some(std::mem::replace(&mut *BOARD.lock().unwrap(), board)));
    check();
}

/// Whether working sessions may prevent sleep (Settings > Agents).
static KEEP_AWAKE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn with<R>(f: impl FnOnce(&mut Board) -> R) -> Option<R> {
    BOARD.lock().ok().map(|mut b| f(&mut b))
}

/// A hook's line from the dock's pipe (its own thread): the board takes
/// it, the companion looks again, the PC is kept awake (or let go).
pub fn ingest(v: &serde_json::Value) {
    let Some(mut m) = Msg::parse(v) else { return };
    // The app's exe is read here, from its process, not taken from the
    // line: only a local executable path is accepted.
    m.host_exe = if m.host_pid != 0 { hook::exe_path(m.host_pid) } else { String::new() };
    if !local_exe(&m.host_exe) {
        m.host_exe.clear();
    }
    let changed = with(|b| b.apply(&m, Instant::now())).unwrap_or(false);
    if changed {
        changed_now();
        watch_processes();
    }
}

/// Everything that follows a change of the board.
fn changed_now() {
    crate::companion::refresh();
    let working = with(|b| b.working(Instant::now())).unwrap_or(false);
    awake::update(working, KEEP_AWAKE.load(std::sync::atomic::Ordering::SeqCst));
}

/// The companion follows the board independently of the keep-awake setting.
pub fn companion_signal(now: Instant) -> crate::companion::model::Signal {
    with(|b| crate::companion::model::Signal {
        waits: b
            .sessions
            .iter()
            .filter(|s| s.st == St::Wait)
            .map(|s| (s.key.clone(), s.since))
            .collect(),
        working: b.working(now),
        result: b
            .sessions
            .iter()
            .filter(|s| s.st == St::Done)
            .max_by_key(|s| s.since)
            .map(|s| (s.since, s.failed)),
    })
    .unwrap_or_default()
}

/// A process still runs.
fn alive(pid: u32) -> bool {
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(h);
        // STILL_ACTIVE.
        ok && code == 259
    }
}

/// How often the sessions' processes are looked at while any are listed
/// (an agent closed without its SessionEnd: a crash, a closed tab).
const PRUNE_EVERY: Duration = Duration::from_secs(30);

static WATCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Looks at the sessions' processes every PRUNE_EVERY while there are
/// sessions (a service timer; nothing runs with none), and lets a stale
/// working session go from keeping the PC awake.
fn watch_processes() {
    use std::sync::atomic::Ordering;
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    fn tick() {
        let (pruned, empty) = with(|b| (b.prune(alive), b.is_empty())).unwrap_or((false, true));
        if pruned {
            changed_now();
        } else {
            let working = with(|b| b.working(Instant::now())).unwrap_or(false);
            awake::update(working, KEEP_AWAKE.load(std::sync::atomic::Ordering::SeqCst));
        }
        if !empty && crate::service::after("agents", PRUNE_EVERY, tick).is_some() {
            return;
        }
        WATCHING.store(false, Ordering::SeqCst);
        // A session that came in meanwhile is watched again.
        if !with(|b| b.is_empty()).unwrap_or(true) {
            watch_processes();
        }
    }
    if crate::service::after("agents", PRUNE_EVERY, tick).is_none() {
        WATCHING.store(false, Ordering::SeqCst);
    }
}

/// What Settings > Agents asks for now (claude, codex).
static WANT: Mutex<Option<(bool, bool)>> = Mutex::new(None);
/// What the agents' files were last made to say this run, per agent
/// (None: not looked at yet).
static FILES: Mutex<[Option<bool>; 2]> = Mutex::new([None, None]);
/// One connect or disconnect at a time (each run does what is asked last).
static CONNECTING: Mutex<()> = Mutex::new(());

/// Explicit settings action: report errors before the UI saves the new value.
/// Shares the startup worker's lock, including Codex's shared configuration file.
pub fn set_notifications(agent: Agent, on: bool) -> Result<(), String> {
    let _one = CONNECTING.lock().map_err(|e| e.to_string())?;
    let places = connect::Places::of_this_pc();
    connect::notifications(agent, &places, if on { Job::Connect } else { Job::Disconnect }, |enabled| {
        crate::companion::chat::connect::set(agent == Agent::Claude, enabled)
    })?;
    FILES.lock().map_err(|e| e.to_string())?[usize::from(agent == Agent::Codex)] = Some(on);
    Ok(())
}

/// Settings > Agents (the dock's config, at start and on every change):
/// keeping awake and connecting or disconnecting an agent.
/// The files are written off the UI thread, one run at a time, each doing
/// what the switches say when it runs.
pub fn apply_config(cfg: &crate::config::Config) {
    let a = &cfg.agents;
    if KEEP_AWAKE.swap(a.keep_awake, std::sync::atomic::Ordering::SeqCst) != a.keep_awake {
        changed_now();
    }
    let want = (a.claude, a.codex);
    if WANT.lock().ok().map(|mut w| w.replace(want)) == Some(Some(want)) {
        return;
    }
    let _ = std::thread::Builder::new().name("agents-connect".into()).spawn(sync_files);
}

/// Pure: what to do with an agent's files: the switch `on`, what they were
/// made to say (`done`; None: not yet this run). Some(Ensure) at start:
/// only mend (no entry of ours, or one whose exe is gone), so another
/// copy's (a dev build's) start never takes the installed one's hooks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    Ensure,
    Connect,
    Disconnect,
}

pub fn job_for(on: bool, done: Option<bool>) -> Option<Job> {
    match (on, done) {
        (true, None) => Some(Job::Ensure),
        (_, Some(d)) if d == on => None,
        (false, None) => None,
        (true, Some(_)) => Some(Job::Connect),
        (false, Some(_)) => Some(Job::Disconnect),
    }
}

fn sync_files() {
    let Ok(_one) = CONNECTING.lock() else { return };
    let Some(want) = WANT.lock().ok().and_then(|w| *w) else { return };
    let places = connect::Places::of_this_pc();
    for (i, agent, on) in [(0, Agent::Claude, want.0), (1, Agent::Codex, want.1)] {
        let done = FILES.lock().map(|f| f[i]).unwrap_or(None);
        let r = match job_for(on, done) {
            None => {
                if done.is_none() {
                    // Off at start: nothing of the agents' is read.
                    let _ = FILES.lock().map(|mut f| f[i] = Some(false));
                }
                continue;
            }
            Some(job) => connect::notifications(agent, &places, job, |on| {
                crate::companion::chat::connect::set(agent == Agent::Claude, on)
            }),
        };
        match r {
            Ok(n) => {
                let _ = FILES.lock().map(|mut f| f[i] = Some(on));
                crate::log!("agents: {} {} ({n} file(s))", agent.id(), if on { "connected" } else { "disconnected" });
            }
            Err(e) => crate::log!("agents: {} {} failed: {e}", agent.id(), if on { "connect" } else { "disconnect" }),
        }
    }
}

/// A local program's path (a drive letter, an .exe), accepted from the host.
pub fn local_exe(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() > 7 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') && p.to_ascii_lowercase().ends_with(".exe")
}

/// WM_POWERBROADCAST: back from sleep, the request is made again.
pub fn on_power(event: usize) {
    awake::on_power(event);
}

/// Quitting.
pub fn release() {
    awake::release();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(agent: Agent, session: &str, ev: Ev) -> Msg {
        Msg { agent, ev, session: session.into(), cwd: format!(r"C:\work\{session}"), agent_pid: 10, host_pid: 20, hwnd: 30, host_exe: r"C:\Orca\Orca.exe".into(), t: 0 }
    }

    #[test]
    fn hooks_map_to_states() {
        assert_eq!(event_of(Agent::Claude, "UserPromptSubmit", ""), Ev::To(St::Work));
        assert_eq!(event_of(Agent::Claude, "PostToolUse", ""), Ev::To(St::Work));
        assert_eq!(event_of(Agent::Claude, "Notification", "permission_prompt"), Ev::To(St::Wait));
        assert_eq!(event_of(Agent::Claude, "Notification", ""), Ev::To(St::Wait));
        // The minute-later "still idle" reminder is not a new question.
        assert_eq!(event_of(Agent::Claude, "Notification", "idle_prompt"), Ev::Idle);
        assert_eq!(event_of(Agent::Claude, "Notification", "elicitation_dialog"), Ev::To(St::Wait));
        assert_eq!(event_of(Agent::Claude, "Notification", "auth_success"), Ev::Ignore);
        assert_eq!(event_of(Agent::Claude, "StopFailure", ""), Ev::Failure);
        assert_eq!(event_of(Agent::Claude, "Stop", ""), Ev::To(St::Done));
        assert_eq!(event_of(Agent::Claude, "SessionEnd", ""), Ev::End);
        assert_eq!(event_of(Agent::Claude, "PreCompact", ""), Ev::Ignore);
        assert_eq!(event_of(Agent::Codex, "agent-turn-complete", ""), Ev::To(St::Done));
        assert_eq!(event_of(Agent::Codex, "Stop", ""), Ev::Ignore);
    }

    #[test]
    fn a_line_is_parsed_and_bounded() {
        let long = "x".repeat(5000);
        let v = serde_json::json!({"type":"Agent","agent":"claude","event":"Stop","session":"s1","cwd":long,"agent_pid":1,"host_pid":2,"hwnd":3,"host_exe":"a\u{7}b"});
        let m = Msg::parse(&v).unwrap();
        assert_eq!(m.ev, Ev::To(St::Done));
        assert_eq!(m.cwd.len(), MAX_TEXT);
        assert_eq!(m.host_exe, "ab");
        assert!(Msg::parse(&serde_json::json!({"agent":"gpt","event":"Stop","session":"s"})).is_none());
        assert!(Msg::parse(&serde_json::json!({"agent":"claude","event":"Stop","session":""})).is_none());
        assert!(Msg::parse(&serde_json::json!({"agent":"claude","event":"Nope","session":"s"})).is_none());
    }

    #[test]
    fn names_are_the_folders() {
        assert_eq!(name_of(r"C:\Users\me\orca\workspaces\sample-repo\agent-board", Agent::Claude), "agent-board");
        assert_eq!(name_of("/home/me/sample-project/", Agent::Codex), "sample-project");
        assert_eq!(name_of(r"C:\", Agent::Codex), "Codex");
        assert_eq!(name_of("", Agent::Claude), "Claude");
    }

    #[test]
    fn the_board_follows_its_sessions() {
        let t0 = Instant::now();
        let mut b = Board::new();
        assert!(b.apply(&msg(Agent::Claude, "a", Ev::To(St::Work)), t0));
        assert!(!b.apply(&msg(Agent::Claude, "a", Ev::To(St::Work)), t0), "nothing new");
        assert!(b.apply(&msg(Agent::Codex, "b", Ev::To(St::Done)), t0));
        assert!(b.apply(&msg(Agent::Claude, "c", Ev::To(St::Wait)), t0));
        assert_eq!(b.sessions.len(), 3);
        assert!(b.working(t0));
        assert!(!b.working(t0 + STALE), "nothing heard for STALE: not keeping the PC awake");
        // SessionEnd removes; a gone process too.
        assert!(b.apply(&msg(Agent::Claude, "a", Ev::End), t0));
        assert!(!b.apply(&msg(Agent::Claude, "a", Ev::End), t0));
        assert!(b.prune(|pid| pid != 10));
        assert!(b.is_empty());
    }

    #[test]
    fn the_board_keeps_a_bounded_list() {
        let t0 = Instant::now();
        let mut b = Board::new();
        b.apply(&msg(Agent::Claude, "work", Ev::To(St::Work)), t0);
        for i in 0..MAX_SESSIONS + 5 {
            b.apply(&msg(Agent::Codex, &format!("d{i}"), Ev::To(St::Done)), t0);
        }
        assert_eq!(b.sessions.len(), MAX_SESSIONS);
        assert!(b.sessions.iter().any(|s| s.name == "work"), "finished ones go first");
    }

    #[test]
    fn esc_older_lines_and_paths() {
        let t0 = Instant::now();
        let mut b = Board::new();
        // Stopped with Esc: no Stop; the idle reminder ends the turn.
        b.apply(&msg(Agent::Claude, "a", Ev::To(St::Work)), t0);
        assert!(b.apply(&msg(Agent::Claude, "a", Ev::Idle), t0));
        assert_eq!(b.sessions[0].st, St::Done);
        // A waiting session stays waiting.
        b.apply(&msg(Agent::Claude, "w", Ev::To(St::Wait)), t0);
        assert!(!b.apply(&msg(Agent::Claude, "w", Ev::Idle), t0));
        assert!(!b.apply(&msg(Agent::Claude, "new", Ev::Idle), t0), "never makes a session");
        // A line older than the last heard is dropped (hooks race).
        let at = |ev, t| Msg { t, ..msg(Agent::Claude, "r", ev) };
        b.apply(&at(Ev::To(St::Wait), 200), t0);
        assert!(!b.apply(&at(Ev::To(St::Work), 100), t0));
        assert!(b.apply(&at(Ev::To(St::Work), 300), t0));
        assert!(local_exe(r"C:\Program Files\Orca\Orca.exe"));
        assert!(!local_exe(r"\\host\share\x.exe"));
        assert!(!local_exe(r"C:\pics\x.png"));
        assert!(!local_exe("shell:AppsFolder\\x"));
    }

    #[test]
    fn files_are_touched_only_when_asked() {
        assert_eq!(job_for(true, None), Some(Job::Ensure));
        assert_eq!(job_for(false, None), None, "off at start: nothing read");
        assert_eq!(job_for(true, Some(false)), Some(Job::Connect));
        assert_eq!(job_for(false, Some(true)), Some(Job::Disconnect));
        assert_eq!(job_for(true, Some(true)), None);
    }

    #[test]
    fn a_new_state_restarts_its_clock() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(5);
        let mut b = Board::new();
        b.apply(&msg(Agent::Claude, "a", Ev::To(St::Work)), t0);
        b.apply(&msg(Agent::Claude, "a", Ev::To(St::Wait)), t1);
        assert_eq!(b.sessions[0].since, t1);
    }

    #[test]
    fn distinct_completions_restart_but_duplicate_and_old_events_do_not() {
        let t = Instant::now();
        let mut b = Board::new();
        let mut m = msg(Agent::Codex, "same", Ev::To(St::Done));
        m.t = 100;
        b.apply(&m, t);
        m.t = 200;
        b.apply(&m, t + Duration::from_secs(6));
        assert_eq!(b.sessions[0].since, t + Duration::from_secs(6));
        b.apply(&m, t + Duration::from_secs(7));
        m.t = 150;
        b.apply(&m, t + Duration::from_secs(8));
        assert_eq!(b.sessions[0].since, t + Duration::from_secs(6));
        let failure = msg(Agent::Claude, "fail", Ev::Failure);
        b.apply(&failure, t);
        assert!(b
            .sessions
            .iter()
            .any(|s| s.key == "claude:fail" && s.st == St::Done && s.failed));
    }
}
