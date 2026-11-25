//! Availability for notification switches; detection never launches a tool.
use crate::companion::chat::{cli, state::Provider};

pub(super) fn provider(key: &str) -> Option<u8> {
    match key {
        "chat.claude_mcp" => Some(0),
        "chat.codex_mcp" => Some(1),
        "chat.antigravity_mcp" => Some(2),
        _ => None,
    }
}

pub(super) fn installed(key: &str) -> bool {
    match provider(key) {
        Some(0) => cli::cached(Provider::ClaudeCode).status != cli::Status::Missing,
        Some(2) => cli::cached(Provider::Antigravity).status != cli::Status::Missing,
        Some(1) => codex_installed(),
        _ => true,
    }
}

#[cfg(test)]
thread_local! { static CODEX_INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) }; }

fn codex_installed() -> bool {
    #[cfg(test)]
    { CODEX_INSTALLED.with(|v| v.get()) }
    #[cfg(not(test))]
    {
        use std::{path::PathBuf, sync::Mutex, time::{Duration, Instant}};
        static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, found)) = *cache {
            if at.elapsed() < Duration::from_secs(15) { return found; }
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).filter(|p| p.is_absolute()).collect();
        if let Some(home) = std::env::var_os("USERPROFILE") { dirs.push(PathBuf::from(home).join(".local/bin")); }
        if let Some(appdata) = std::env::var_os("APPDATA") { dirs.push(PathBuf::from(appdata).join("npm")); }
        let found = dirs.iter().any(|dir| ["codex.exe", "codex.cmd"].iter().any(|name| dir.join(name).is_file()));
        *cache = Some((Instant::now(), found));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tools_are_locked_for_pointer_and_direct_activation() {
        let _lang = crate::i18n::test_lock("ko");
        let _detection = cli::test_detections(cli::Status::Missing, cli::Status::Missing);
        CODEX_INSTALLED.with(|v| v.set(false));
        let mut h = super::super::layout_tests::harness(false);
        h.app.section = h.app.sections.iter().position(|s| s.tab == super::super::model::Tab::Agents).unwrap();
        let before = h.app.cfg.clone();
        for key in ["chat.claude_mcp", "chat.codex_mcp", "chat.antigravity_mcp"] {
            assert!(!installed(key));
            assert!(h.app.row_locked(key));
            let ri = h.app.rows().iter().position(|r| r.key == key).unwrap();
            h.app.click(super::super::Hit::Toggle(ri), 0.);
            assert_eq!(h.app.cfg, before);
        }
        CODEX_INSTALLED.with(|v| v.set(true));
    }
}
