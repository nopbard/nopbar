//! Settings > Agents' connect and disconnect: NopBar's own entries in the
//! agents' settings, nothing else of theirs touched.
//!
//! - Claude Code: hooks (UserPromptSubmit, PostToolUse, Notification,
//!   Stop, SessionEnd) running `nopbar.exe --agent claude`, in every
//!   settings.json Claude reads on this PC: `~/.claude` (or
//!   CLAUDE_CONFIG_DIR) and each of Orca's profiles (Orca starts Claude
//!   with CLAUDE_CONFIG_DIR set to `%APPDATA%\orca\claude-profiles\<id>\home`,
//!   whose settings.json is a copy of its own, not a link).
//! - Codex: its `notify` program (the only state Codex tells one; its own
//!   hooks need each command trusted in Codex first). Codex takes one
//!   notify program: one that is someone else's is left as it is and the
//!   connect fails.
//!
//! Ours is recognized by `--agent <agent>` after an exe named nopbar.exe;
//! a connect replaces ours (this exe may have moved with an update) and
//! keeps every other entry (Orca's hooks, the user's own) where it was.
//! Each file is copied to `<file>.nopbar.bak` before it is written, and
//! written whole through a temporary file. A file that cannot be read as
//! what it should be is left alone.

use std::path::{Path, PathBuf};

use super::Agent;

/// The Claude Code events NopBar listens to (agents::event_of).
pub const CLAUDE_EVENTS: [&str; 6] = ["UserPromptSubmit", "PostToolUse", "Notification", "Stop", "StopFailure", "SessionEnd"];
/// A hook's limit in Claude (seconds; ours ends in a fraction of one).
const HOOK_TIMEOUT: u64 = 5;

/// Where the agents' settings are, and the exe the entries run.
#[derive(Clone, Debug)]
pub struct Places {
    /// Claude's settings.json files that exist (and `~/.claude`'s, made
    /// if missing).
    pub claude: Vec<PathBuf>,
    /// Claude's settings that may be made if missing.
    pub claude_make: Option<PathBuf>,
    /// Codex's config.toml, and whether its folder is there.
    pub codex: Option<PathBuf>,
    pub exe: String,
}

impl Places {
    pub fn of_this_pc() -> Places {
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
        let mut claude = Vec::new();
        let main = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
        let claude_make = main.is_dir().then(|| main.join("settings.json"));
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let profiles = PathBuf::from(appdata).join("orca").join("claude-profiles");
            if let Ok(rd) = std::fs::read_dir(&profiles) {
                let mut found: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("home").join("settings.json")).filter(|p| p.is_file()).collect();
                found.sort();
                claude.extend(found);
            }
        }
        let codex_dir = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
        let codex = codex_dir.is_dir().then(|| codex_dir.join("config.toml"));
        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
        Places { claude, claude_make, codex, exe }
    }

    fn claude_files(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.claude_make.iter().cloned().chain(self.claude.iter().cloned()).collect();
        v.dedup();
        v
    }
}

/// Connects `agent`: the files written (an error when none could be).
pub fn connect(agent: Agent, p: &Places) -> Result<usize, String> {
    if p.exe.is_empty() {
        return Err("no exe path".into());
    }
    match agent {
        Agent::Claude => each_claude(p, true, &Scope::All),
        Agent::Codex => {
            let f = p.codex.as_ref().ok_or("Codex is not set up on this PC")?;
            let text = read_or_empty(f)?;
            let out = codex_connect(&text, &p.exe)?;
            write_if_changed(f, &text, &out)
        }
    }
}

/// One notification operation, serialized by the caller. In particular,
/// Codex's hook and bubble registrations share a file and must not race.
pub(super) fn notifications(
    agent: Agent,
    p: &Places,
    job: super::Job,
    bubbles: impl FnOnce(bool) -> Result<(), String>,
) -> Result<usize, String> {
    let n = match job {
        super::Job::Ensure => ensure(agent, p)?,
        super::Job::Connect => connect(agent, p)?,
        super::Job::Disconnect => disconnect(agent, p)?,
    };
    bubbles(job != super::Job::Disconnect)?;
    Ok(n)
}

/// Which of NopBar's entries a disconnect takes out.
#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    /// Every one (Settings > Agents switched off).
    All,
    /// NopBar being uninstalled: the ones that run this exe, or an exe
    /// that is gone (a dev build's uninstall leaves the installed copy's).
    Leftover(String),
}

/// Pure: the exe a command or notify line runs (inside its first quotes).
fn exe_of(command: &str) -> Option<&str> {
    let i = command.find(['"', '\''])?;
    let q = command[i..].chars().next()?;
    let rest = &command[i + 1..];
    rest.find(q).map(|j| &rest[..j])
}

fn same_path(a: &str, b: &str) -> bool {
    let n = |p: &str| p.replace('/', "\\").to_ascii_lowercase();
    n(a) == n(b)
}

/// `command` is one of NopBar's for `agent` that `scope` takes out.
fn dropped(command: &str, agent: Agent, scope: &Scope) -> bool {
    if !is_ours(command, agent) {
        return false;
    }
    match scope {
        Scope::All => true,
        Scope::Leftover(exe) => exe_of(command).is_none_or(|e| same_path(e, exe) || !Path::new(e).is_file()),
    }
}

/// At NopBar's start with the switch on: mends only. A file that already
/// runs a NopBar that is there (this one, or another copy: the installed
/// one while a dev build starts) is left as it is; one with no entry of
/// ours, or only ones whose exe is gone, is connected to this exe.
pub fn ensure(agent: Agent, p: &Places) -> Result<usize, String> {
    let live = |line: &str| is_ours(line, agent) && exe_of(line).is_some_and(|e| Path::new(e).is_file());
    match agent {
        Agent::Claude => {
            let mut n = 0;
            for f in p.claude_files() {
                let text = read_or_empty(&f)?;
                let has = parse_settings(&text).ok().is_some_and(|root| hook_lines(&root).iter().any(|l| live(l)));
                if !has {
                    let out = claude_connect(&text, &p.exe)?;
                    n += write_if_changed(&f, &text, &out)?;
                }
            }
            Ok(n)
        }
        Agent::Codex => {
            let Some(f) = p.codex.as_ref() else { return Err("Codex is not set up on this PC".into()) };
            let text = read_or_empty(f)?;
            let (_, body) = split_bom(&text);
            let lines: Vec<&str> = body.lines().collect();
            if notify_at(&lines).is_some_and(|i| live(lines[i])) {
                return Ok(0);
            }
            connect(agent, p)
        }
    }
}

/// Every hook in a settings file, as hook_line puts it.
fn hook_lines(root: &Node) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(Node::Obj(events)) = root.get("hooks") {
        for (_, groups) in events {
            if let Node::Arr(gs) = groups {
                for g in gs {
                    if let Some(Node::Arr(hs)) = g.get("hooks") {
                        out.extend(hs.iter().filter_map(hook_line));
                    }
                }
            }
        }
    }
    out
}

/// A byte-order mark at the start, and the rest.
fn split_bom(text: &str) -> (&str, &str) {
    match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text),
    }
}

/// Disconnects `agent`: only NopBar's entries go (those `scope` names).
pub fn disconnect(agent: Agent, p: &Places) -> Result<usize, String> {
    disconnect_in(agent, p, &Scope::All)
}

fn disconnect_in(agent: Agent, p: &Places, scope: &Scope) -> Result<usize, String> {
    match agent {
        Agent::Claude => each_claude(p, false, scope),
        Agent::Codex => {
            let Some(f) = p.codex.as_ref().filter(|f| f.is_file()) else { return Ok(0) };
            let text = std::fs::read_to_string(f).map_err(|e| e.to_string())?;
            write_if_changed(f, &text, &codex_disconnect_in(&text, scope))
        }
    }
}

/// Uninstalling NopBar: its hooks and notify entry go (this exe's, and any
/// whose exe is gone), so the agents never run a missing program. The
/// files written, and what failed.
pub fn remove_on_uninstall(p: &Places) -> (usize, Vec<String>) {
    let scope = Scope::Leftover(p.exe.clone());
    let mut n = 0;
    let mut failed = Vec::new();
    for agent in [Agent::Claude, Agent::Codex] {
        match disconnect_in(agent, p, &scope) {
            Ok(k) => n += k,
            Err(e) => failed.push(format!("{} hooks: {e}", agent.id())),
        }
    }
    (n, failed)
}

fn each_claude(p: &Places, on: bool, scope: &Scope) -> Result<usize, String> {
    let mut written = 0;
    let mut errors = Vec::new();
    for f in p.claude_files() {
        let made = !f.is_file();
        if made && !on {
            continue;
        }
        let r = read_or_empty(&f).and_then(|text| {
            let out = if on { claude_connect(&text, &p.exe) } else { claude_edit(&text, None, scope) }?;
            write_if_changed(&f, &text, &out)
        });
        match r {
            Ok(n) => written += n,
            Err(e) => errors.push(format!("{}: {e}", f.display())),
        }
    }
    if !errors.is_empty() {
        crate::log!("agents: {}", errors.join("; "));
        if written == 0 && on {
            return Err(errors.join("; "));
        }
    }
    Ok(written)
}

fn read_or_empty(f: &Path) -> Result<String, String> {
    match std::fs::read_to_string(f) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// Writes `out` over `f` when it differs from `before` (the old one
/// copied to `<file>.nopbar.bak` first); 1 if written.
fn write_if_changed(f: &Path, before: &str, out: &str) -> Result<usize, String> {
    if before == out {
        return Ok(0);
    }
    if f.is_file() {
        let mut bak = f.as_os_str().to_owned();
        bak.push(".nopbar.bak");
        std::fs::copy(f, PathBuf::from(bak)).map_err(|e| format!("backup: {e}"))?;
    }
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let mut tmp = f.as_os_str().to_owned();
    tmp.push(format!(".nopbar.{}.{}.tmp", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, f).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })?;
    Ok(1)
}

// ---------------------------------------------------------------- Claude

/// Pure: NopBar's hook for this exe as one line (hook_line's form).
#[cfg(test)]
pub fn claude_command(exe: &str) -> String {
    format!("\"{}\" --agent claude", exe.replace('\\', "/"))
}

/// Pure: a command is NopBar's for `agent`.
pub fn is_ours(command: &str, agent: Agent) -> bool {
    // (Codex's notify is a list: `'--agent', 'codex'`.)
    let c = command.to_ascii_lowercase().replace(['\'', '"', ','], "");
    let c = c.split_whitespace().collect::<Vec<_>>().join(" ");
    c.contains("nopbar.exe") && c.contains(&format!("--agent {}", agent.id()))
}

/// A JSON value with its objects' keys in the order they were written
/// (settings.json is the user's: a rewrite must not reorder it).
#[derive(Clone, Debug, PartialEq)]
enum Node {
    Obj(Vec<(String, Node)>),
    Arr(Vec<Node>),
    Val(serde_json::Value),
}

impl<'de> serde::Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Node, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Node;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
                Ok(Node::Val(v.into()))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Node, E> {
                Ok(Node::Val(v.into()))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
                Ok(Node::Val(v.into()))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Node, E> {
                Ok(Node::Val(serde_json::Number::from_f64(v).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null)))
            }
            fn visit_str<E>(self, v: &str) -> Result<Node, E> {
                Ok(Node::Val(v.into()))
            }
            fn visit_string<E>(self, v: String) -> Result<Node, E> {
                Ok(Node::Val(v.into()))
            }
            fn visit_unit<E>(self) -> Result<Node, E> {
                Ok(Node::Val(serde_json::Value::Null))
            }
            fn visit_none<E>(self) -> Result<Node, E> {
                Ok(Node::Val(serde_json::Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
                let mut v = Vec::new();
                while let Some(n) = a.next_element()? {
                    v.push(n);
                }
                Ok(Node::Arr(v))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
                let mut v = Vec::new();
                while let Some((k, n)) = a.next_entry::<String, Node>()? {
                    v.push((k, n));
                }
                Ok(Node::Obj(v))
            }
        }
        d.deserialize_any(V)
    }
}

impl Node {
    fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, n)| n),
            _ => None,
        }
    }

    fn str(&self) -> Option<&str> {
        match self {
            Node::Val(serde_json::Value::String(s)) => Some(s),
            _ => None,
        }
    }

    /// Printed as JSON.stringify(v, null, 2) prints (Claude's own way).
    fn print(&self, out: &mut String, depth: usize) {
        let pad = |out: &mut String, d: usize| out.push_str(&"  ".repeat(d));
        match self {
            Node::Val(v) => out.push_str(&serde_json::to_string(v).unwrap_or_else(|_| "null".into())),
            Node::Arr(v) if v.is_empty() => out.push_str("[]"),
            Node::Obj(v) if v.is_empty() => out.push_str("{}"),
            Node::Arr(v) => {
                out.push_str("[\n");
                for (i, n) in v.iter().enumerate() {
                    pad(out, depth + 1);
                    n.print(out, depth + 1);
                    out.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
                }
                pad(out, depth);
                out.push(']');
            }
            Node::Obj(v) => {
                out.push_str("{\n");
                for (i, (k, n)) in v.iter().enumerate() {
                    pad(out, depth + 1);
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push_str(": ");
                    n.print(out, depth + 1);
                    out.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
                }
                pad(out, depth);
                out.push('}');
            }
        }
    }
}

fn parse_settings(text: &str) -> Result<Node, String> {
    if text.trim().is_empty() {
        return Ok(Node::Obj(Vec::new()));
    }
    // (A byte-order mark is not JSON; Notepad may have left one.)
    let n: Node = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("not JSON: {e}"))?;
    match n {
        Node::Obj(_) => Ok(n),
        _ => Err("not a JSON object".into()),
    }
}

fn print_settings(n: &Node, crlf: bool) -> String {
    let mut s = String::new();
    n.print(&mut s, 0);
    s.push('\n');
    if crlf {
        s.replace('\n', "\r\n")
    } else {
        s
    }
}

/// A hook group (`{"matcher":..., "hooks":[...]}`) with only NopBar's
/// commands left out; None if nothing of it is left.
fn without_ours(group: &Node, scope: &Scope) -> Option<Node> {
    let Node::Obj(fields) = group else { return Some(group.clone()) };
    let Some(Node::Arr(hooks)) = group.get("hooks") else { return Some(group.clone()) };
    let kept: Vec<Node> = hooks.iter().filter(|h| !hook_line(h).is_some_and(|c| dropped(&c, Agent::Claude, scope))).cloned().collect();
    if kept.len() == hooks.len() {
        return Some(group.clone());
    }
    if kept.is_empty() {
        return None;
    }
    Some(Node::Obj(fields.iter().map(|(k, n)| if k == "hooks" { (k.clone(), Node::Arr(kept.clone())) } else { (k.clone(), n.clone()) }).collect()))
}

/// A hook as one line: its command, and its args after it (the exec
/// form: `"<program>" <args>`), for telling NopBar's apart.
fn hook_line(h: &Node) -> Option<String> {
    let cmd = h.get("command").and_then(Node::str)?;
    match h.get("args") {
        Some(Node::Arr(args)) => Some(format!("\"{cmd}\" {}", args.iter().filter_map(Node::str).collect::<Vec<_>>().join(" "))),
        _ => Some(cmd.to_string()),
    }
}

/// NopBar's hook: the exec form (the program and its args, no shell: no
/// Git Bash or PowerShell started for it) run in the background
/// (`async`: Claude never waits for it, its output and exit code are not
/// Claude's business). Both checked on Claude Code 2.1.322.
fn our_group(event: &str, exe: &str) -> Node {
    let hook = Node::Obj(vec![
        ("type".into(), Node::Val("command".into())),
        ("command".into(), Node::Val(exe.replace('\\', "/").into())),
        ("args".into(), Node::Arr(vec![Node::Val("--agent".into()), Node::Val("claude".into())])),
        ("async".into(), Node::Val(true.into())),
        ("timeout".into(), Node::Val(HOOK_TIMEOUT.into())),
    ]);
    let mut g = Vec::new();
    if event == "PostToolUse" {
        g.push(("matcher".to_string(), Node::Val("*".into())));
    }
    g.push(("hooks".to_string(), Node::Arr(vec![hook])));
    Node::Obj(g)
}

/// Pure: settings.json's text with NopBar's hooks `on` (ours appended
/// after the others in each event, once) or off; everything else as it
/// was, in its order.
fn claude_edit(text: &str, exe: Option<&str>, scope: &Scope) -> Result<String, String> {
    let mut root = parse_settings(text)?;
    let Node::Obj(fields) = &mut root else { unreachable!() };
    let at = match fields.iter().position(|(k, _)| k == "hooks") {
        Some(i) => i,
        None if exe.is_none() => return Ok(text.to_string()),
        None => {
            fields.push(("hooks".into(), Node::Obj(Vec::new())));
            fields.len() - 1
        }
    };
    let Node::Obj(events) = &mut fields[at].1 else { return Err("\"hooks\" is not an object".into()) };
    // Ours out of every event first (a connect puts them back fresh).
    let mut emptied = Vec::new();
    for (name, groups) in events.iter_mut() {
        if let Node::Arr(gs) = groups {
            let before = gs.len();
            let kept: Vec<Node> = gs.iter().filter_map(|g| without_ours(g, scope)).collect();
            if kept.is_empty() && before > 0 {
                emptied.push(name.clone());
            }
            *gs = kept;
        }
    }
    if let Some(exe) = exe {
        for ev in CLAUDE_EVENTS {
            match events.iter_mut().find(|(k, _)| k == ev) {
                Some((_, Node::Arr(gs))) => gs.push(our_group(ev, exe)),
                Some(_) => return Err(format!("\"hooks.{ev}\" is not a list")),
                None => events.push((ev.to_string(), Node::Arr(vec![our_group(ev, exe)]))),
            }
        }
    } else {
        // An event list that held only ours goes with them.
        events.retain(|(k, n)| !(emptied.contains(k) && matches!(n, Node::Arr(g) if g.is_empty())));
        if events.is_empty() {
            fields.remove(at);
        }
    }
    let out = print_settings(&root, text.contains("\r\n"));
    // Nothing of ours changed: the file stays byte for byte.
    if parse_settings(&out).ok() == parse_settings(text).ok() {
        return Ok(text.to_string());
    }
    Ok(out)
}

pub fn claude_connect(text: &str, exe: &str) -> Result<String, String> {
    claude_edit(text, Some(exe), &Scope::All)
}

#[cfg(test)]
pub fn claude_disconnect(text: &str) -> Result<String, String> {
    claude_edit(text, None, &Scope::All)
}

// ---------------------------------------------------------------- Codex

/// Pure: NopBar's notify line (TOML literal strings: no escapes).
pub fn codex_line(exe: &str) -> String {
    format!("notify = ['{exe}', '--agent', 'codex']")
}

/// The top-level `notify = ...` line's index (before any table), if any.
fn notify_at(lines: &[&str]) -> Option<usize> {
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with('[') {
            return None;
        }
        if let Some(rest) = t.strip_prefix("notify") {
            if rest.trim_start().starts_with('=') {
                return Some(i);
            }
        }
    }
    None
}

/// Pure: config.toml's text with NopBar as its notify program (first
/// line); an error if another program has it.
pub fn codex_connect(text: &str, exe: &str) -> Result<String, String> {
    let (bom, body) = split_bom(text);
    codex_connect_body(body, exe).map(|out| if out == body { text.to_string() } else { format!("{bom}{out}") })
}

fn codex_connect_body(text: &str, exe: &str) -> Result<String, String> {
    if exe.contains('\'') {
        return Err("the exe path has a quote".into());
    }
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<&str> = text.lines().collect();
    let ours = codex_line(exe);
    match notify_at(&lines) {
        Some(i) if lines[i] == ours => return Ok(text.to_string()),
        Some(i) if is_ours(lines[i], Agent::Codex) => lines[i] = &ours,
        Some(_) => return Err("Codex's notify is set to another program".into()),
        None => lines.insert(0, &ours),
    }
    let mut out = lines.join(nl);
    out.push_str(nl);
    Ok(out)
}

/// Pure: config.toml's text without NopBar's notify line.
#[cfg(test)]
pub fn codex_disconnect(text: &str) -> String {
    codex_disconnect_in(text, &Scope::All)
}

fn codex_disconnect_in(text: &str, scope: &Scope) -> String {
    let (bom, body) = split_bom(text);
    let out = codex_disconnect_body(body, scope);
    if out == body { text.to_string() } else { format!("{bom}{out}") }
}

fn codex_disconnect_body(text: &str, scope: &Scope) -> String {
    let lines: Vec<&str> = text.lines().collect();
    match notify_at(&lines) {
        Some(i) if dropped(lines[i], Agent::Codex, scope) => {
            let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
            let mut rest: Vec<&str> = lines;
            rest.remove(i);
            let mut out = rest.join(nl);
            if !out.is_empty() {
                out.push_str(nl);
            }
            out
        }
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_notification_operation_changes_hooks_and_bubbles_in_temp_files() {
        let dir = std::env::temp_dir().join(format!("nopbar-notifications-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let hooks = dir.join("settings.json");
        let bubbles = dir.join("claude.json");
        let codex = dir.join("config.toml");
        std::fs::write(&hooks, r#"{"other":true}"#).unwrap();
        std::fs::write(&bubbles, r#"{"other":true}"#).unwrap();
        std::fs::write(&codex, "# retained\nmodel = 'example'\n").unwrap();
        let places = Places { claude: vec![], claude_make: Some(hooks.clone()), codex: Some(codex.clone()), exe: EXE.into() };
        for agent in [Agent::Claude, Agent::Codex] {
            for job in [super::super::Job::Connect, super::super::Job::Disconnect] {
                notifications(agent, &places, job, |on| {
                    let path = if agent == Agent::Claude { &bubbles } else { &codex };
                    let text = std::fs::read_to_string(path).unwrap();
                    let edited = if agent == Agent::Claude {
                        crate::companion::chat::connect::claude(&text, EXE, on)?
                    } else {
                        crate::companion::chat::connect::codex(&text, EXE, on)?
                    };
                    std::fs::write(path, edited).map_err(|e| e.to_string())
                }).unwrap();
                let on = job == super::super::Job::Connect;
                let hook_text = std::fs::read_to_string(if agent == Agent::Claude { &hooks } else { &codex }).unwrap();
                let bubble_text = std::fs::read_to_string(if agent == Agent::Claude { &bubbles } else { &codex }).unwrap();
                assert_eq!(hook_text.contains("--agent"), on);
                assert_eq!(bubble_text.contains("nopbar-companion"), on);
                assert!(bubble_text.contains(if agent == Agent::Claude { "other" } else { "# retained" }));
            }
        }
        // Only this UUID-named test directory is removed.
        std::fs::remove_dir_all(dir).unwrap();
    }

    const EXE: &str = r"C:\Users\me\AppData\Local\Programs\NopBar\nopbar.exe";

    /// Orca's hooks and the user's own, as Claude writes the file.
    const SETTINGS: &str = r#"{
  "permissions": {
    "allow": [
      "Bash(ls:*)"
    ]
  },
  "model": "opus",
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "node \"C:/hooks/guard.js\" --remind",
            "timeout": 15
          }
        ]
      },
      {
        "hooks": [
          {
            "type": "command",
            "command": "C:/Users/me/.orca/agent-hooks/orca-agent-hook.exe claude",
            "timeout": 10
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "C:/Users/me/.orca/agent-hooks/orca-agent-hook.exe claude",
            "timeout": 10
          }
        ]
      }
    ]
  },
  "theme": "dark"
}
"#;

    fn commands(text: &str, ev: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        v["hooks"][ev].as_array().map(|gs| gs.iter().flat_map(|g| g["hooks"].as_array().unwrap().iter().map(|h| match h["args"].as_array() {
            Some(a) => format!("\"{}\" {}", h["command"].as_str().unwrap(), a.iter().map(|x| x.as_str().unwrap()).collect::<Vec<_>>().join(" ")),
            None => h["command"].as_str().unwrap().to_string(),
        })).collect()).unwrap_or_default()
    }

    #[test]
    fn claude_connects_beside_the_others_and_goes_without_a_trace() {
        let on = claude_connect(SETTINGS, EXE).unwrap();
        // Ours after Orca's and the user's, once per event.
        let cmd = claude_command(EXE);
        assert_eq!(commands(&on, "UserPromptSubmit").last(), Some(&cmd));
        assert_eq!(commands(&on, "UserPromptSubmit").len(), 3);
        for ev in CLAUDE_EVENTS {
            assert_eq!(commands(&on, ev).iter().filter(|c| **c == cmd).count(), 1, "{ev}");
        }
        assert!(cmd.starts_with("\"C:/Users/me/") && cmd.ends_with("nopbar.exe\" --agent claude"));
        // The rest in its place and order.
        let keys: Vec<&str> = on.lines().filter(|l| l.starts_with("  \"")).map(|l| l.trim().split('"').nth(1).unwrap()).collect();
        assert_eq!(keys, ["permissions", "model", "hooks", "theme"]);
        // Ours: the exec form (no shell), in the background.
        let v: serde_json::Value = serde_json::from_str(&on).unwrap();
        let ours = v["hooks"]["Stop"].as_array().unwrap().last().unwrap()["hooks"][0].clone();
        assert_eq!(ours["args"], serde_json::json!(["--agent", "claude"]));
        assert_eq!(ours["async"], serde_json::json!(true));
        assert!(ours["command"].as_str().unwrap().ends_with("NopBar/nopbar.exe"));
        // Again: nothing changes (no second set); a moved exe replaces ours.
        assert_eq!(claude_connect(&on, EXE).unwrap(), on);
        let moved = claude_connect(&on, r"D:\NopBar\nopbar.exe").unwrap();
        assert_eq!(commands(&moved, "Stop").iter().filter(|c| c.contains("nopbar.exe")).count(), 1);
        assert!(commands(&moved, "Stop").iter().any(|c| c.starts_with("\"D:/NopBar/")));
        // Off: exactly the file it was.
        assert_eq!(claude_disconnect(&on).unwrap(), SETTINGS);
        assert_eq!(claude_disconnect(SETTINGS).unwrap(), SETTINGS, "nothing of ours: untouched");
    }

    #[test]
    fn claude_mixed_groups_keep_their_other_hooks() {
        let mixed = format!(
            r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"x.exe"}},{{"type":"command","command":"{}"}}]}}]}}}}"#,
            claude_command(EXE).replace('"', "\\\"")
        );
        let off = claude_disconnect(&mixed).unwrap();
        assert_eq!(commands(&off, "Stop"), ["x.exe"]);
    }

    #[test]
    fn claude_new_files_and_bad_ones() {
        let on = claude_connect("", EXE).unwrap();
        assert_eq!(commands(&on, "Notification"), [claude_command(EXE)]);
        assert_eq!(claude_disconnect(&on).unwrap(), "{}\n");
        assert!(claude_connect("{ nope", EXE).is_err());
        assert!(claude_connect("[1]", EXE).is_err());
        assert!(claude_connect(r#"{"hooks": 3}"#, EXE).is_err());
        // Windows line ends stay.
        let crlf = SETTINGS.replace('\n', "\r\n");
        let on = claude_connect(&crlf, EXE).unwrap();
        assert!(on.contains("\r\n") && !on.replace("\r\n", "").contains('\n'));
        assert_eq!(claude_disconnect(&on).unwrap(), crlf);
    }

    const TOML: &str = "[tui]\nscreen_reader_detection_done = true\n\n[mcp_servers.figma]\nurl = \"x\"\n";

    #[test]
    fn codex_takes_notify_only_when_free() {
        let on = codex_connect(TOML, EXE).unwrap();
        assert!(on.starts_with(&format!("{}\n[tui]", codex_line(EXE))));
        assert_eq!(codex_connect(&on, EXE).unwrap(), on, "once");
        let moved = codex_connect(&on, r"D:\nopbar.exe").unwrap();
        assert!(moved.starts_with("notify = ['D:\\nopbar.exe'"));
        assert_eq!(codex_disconnect(&on), TOML);
        assert_eq!(codex_disconnect(TOML), TOML);
        // Someone else's notify: left alone.
        let theirs = format!("model = \"o3\"\nnotify = [\"python\", \"n.py\"]\n{TOML}");
        assert!(codex_connect(&theirs, EXE).is_err());
        assert_eq!(codex_disconnect(&theirs), theirs);
        // A table's own `notify` key is not the top-level one.
        let in_table = "[tui]\nnotify = true\n";
        assert!(codex_connect(in_table, EXE).unwrap().starts_with("notify = ['"));
        assert_eq!(codex_connect("", EXE).unwrap(), format!("{}\n", codex_line(EXE)));
    }

    #[test]
    fn an_uninstall_takes_this_exes_entries_and_dead_ones_only() {
        let dir = std::env::temp_dir().join(format!("nopbar-agents-u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Another copy that is still there (the installed one, while a dev
        // build uninstalls itself).
        let other = dir.join("nopbar.exe");
        std::fs::write(&other, b"").unwrap();
        let other = other.display().to_string();
        let this = format!("{EXE}");
        let mine = claude_connect(SETTINGS, &this).unwrap();
        let leftover = Scope::Leftover(this.clone());
        assert_eq!(claude_edit(&mine, None, &leftover).unwrap(), SETTINGS, "this exe's: gone");
        let theirs = claude_connect(SETTINGS, &other).unwrap();
        assert_eq!(claude_edit(&theirs, None, &leftover).unwrap(), theirs, "a living copy's: kept");
        let dead = claude_connect(SETTINGS, r"C:\gone\nopbar.exe").unwrap();
        assert_eq!(claude_edit(&dead, None, &leftover).unwrap(), SETTINGS, "a missing exe's: gone");
        // Codex alike.
        let on = codex_connect(TOML, &other).unwrap();
        assert_eq!(codex_disconnect_in(&on, &leftover), on);
        assert_eq!(codex_disconnect_in(&codex_connect(TOML, &this).unwrap(), &leftover), TOML);
        assert_eq!(exe_of(&codex_line(r"D:\a b\nopbar.exe")), Some(r"D:\a b\nopbar.exe"));
        assert_eq!(exe_of(&claude_command(r"D:\a b\nopbar.exe")), Some("D:/a b/nopbar.exe"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_keeps_a_byte_order_mark_in_front() {
        let bom = format!("\u{feff}{TOML}");
        let on = codex_connect(&bom, EXE).unwrap();
        assert!(on.starts_with(&format!("\u{feff}{}", codex_line(EXE))));
        assert_eq!(codex_connect(&on, EXE).unwrap(), on, "found behind the mark: once");
        assert_eq!(codex_disconnect(&on), bom);
    }

    #[test]
    fn a_start_mends_but_never_takes_another_copys_hooks() {
        let dir = std::env::temp_dir().join(format!("nopbar-agents-e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let installed = dir.join("nopbar.exe");
        std::fs::write(&installed, b"").unwrap();
        let installed = installed.display().to_string();
        let f = dir.join(".claude").join("settings.json");
        let theirs = claude_connect(SETTINGS, &installed).unwrap();
        std::fs::write(&f, &theirs).unwrap();
        let dev = Places { claude: vec![], claude_make: Some(f.clone()), codex: None, exe: r"C:\dev\target\nopbar.exe".into() };
        assert_eq!(ensure(Agent::Claude, &dev), Ok(0), "a living copy's hooks stay");
        // Its exe gone: mended to this one.
        std::fs::write(&f, claude_connect(SETTINGS, r"C:\gone\nopbar.exe").unwrap()).unwrap();
        assert_eq!(ensure(Agent::Claude, &dev), Ok(1));
        assert!(std::fs::read_to_string(&f).unwrap().contains("C:/dev/target/nopbar.exe"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_are_backed_up_and_written_whole() {
        let dir = std::env::temp_dir().join(format!("nopbar-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let profile = dir.join("orca").join("p1").join("home");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        std::fs::write(profile.join("settings.json"), SETTINGS).unwrap();
        std::fs::write(dir.join(".codex").join("config.toml"), TOML).unwrap();
        let p = Places {
            claude: vec![profile.join("settings.json")],
            claude_make: Some(dir.join(".claude").join("settings.json")),
            codex: Some(dir.join(".codex").join("config.toml")),
            exe: EXE.into(),
        };
        assert_eq!(connect(Agent::Claude, &p), Ok(2));
        assert_eq!(connect(Agent::Claude, &p), Ok(0), "already");
        assert_eq!(std::fs::read_to_string(profile.join("settings.json.nopbar.bak")).unwrap(), SETTINGS);
        assert_eq!(connect(Agent::Codex, &p), Ok(1));
        assert_eq!(disconnect(Agent::Claude, &p), Ok(2));
        assert_eq!(disconnect(Agent::Codex, &p), Ok(1));
        assert_eq!(std::fs::read_to_string(profile.join("settings.json")).unwrap(), SETTINGS);
        assert_eq!(std::fs::read_to_string(dir.join(".codex").join("config.toml")).unwrap(), TOML);
        assert!(!profile.join("settings.json.nopbar.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
