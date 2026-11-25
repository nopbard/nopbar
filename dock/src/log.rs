//! Tiny append-only log in %APPDATA%\NopBar\dock.log (for bug reports).

use std::io::Write;

pub fn write(msg: &str) {
    // Unit tests must not write into the user's real log.
    if cfg!(test) {
        return;
    }
    let path = crate::config::app_dir().join("dock.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 1_000_000 {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "{t} {msg}");
    }
}

/// Debug lines are written by debug builds, or with NOPBAR_DEBUG=1 set.
pub fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| cfg!(debug_assertions) || std::env::var("NOPBAR_DEBUG").is_ok_and(|v| v == "1"))
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::log::write(&format!($($t)*)) };
}

/// Like log!, only when debug_enabled() (the message is not even formatted
/// otherwise).
#[macro_export]
macro_rules! debug_log {
    ($($t:tt)*) => {
        if $crate::log::debug_enabled() {
            $crate::log::write(&format!($($t)*))
        }
    };
}
