//! How far the inner installer is, from its log (`/LOG=`): Inno Setup
//! writes a line per step, so the bar follows real steps, never a timer.
//! The last stretch (it exits) is the caller's: 1.0 comes only from exit
//! code 0.

/// The installer's files ([Files] in packaging/inno/nopbar.iss: nopbar.exe
/// nopbar-mcp.exe and LICENSE.txt).
pub const FILES: usize = 3;

/// 0.0 (nothing yet) .. 0.97 (log closed) for what the log says so far.
pub fn fraction(log: &str) -> f32 {
    let has = |s: &str| log.contains(s);
    let mut f: f32 = 0.0;
    if !log.trim().is_empty() {
        f = 0.04;
    }
    if has("Created temporary directory") {
        f = 0.10;
    }
    // After PrepareToInstall (nopbar.exe --quit, up to 5 s).
    if has("Starting the installation process") {
        f = 0.30;
    }
    let files = log.matches("-- File entry --").count().min(FILES);
    if files > 0 {
        f = f.max(0.30 + 0.30 * files as f32 / FILES as f32);
    }
    if has("-- Icon entry --") {
        f = f.max(0.70);
    }
    if has("Installation process succeeded") {
        f = f.max(0.88);
    }
    if has("-- Run entry --") {
        f = f.max(0.94);
    }
    if has("Log closed") {
        f = f.max(0.97);
    }
    f
}

/// A log file's text: UTF-8 (with or without BOM) or UTF-16 LE with BOM,
/// whatever the Inno Setup version writes. A missing or locked file is "".
pub fn read_log(path: &std::path::Path) -> String {
    std::fs::read(path).map(|b| decode(&b)).unwrap_or_default()
}

pub fn decode(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Inno Setup 6 log of a NopBar install, cut at each step.
    const LOG: &str = "\
2026-11-07 10:00:00.000   Log opened. (Time zone: UTC+09:00)
2026-11-07 10:00:00.000   Setup version: Inno Setup version 6.4.3
2026-11-07 10:00:00.000   Original Setup EXE: C:\\Users\\u\\AppData\\Local\\Temp\\NopBarSetup-1-2\\NopBar-0.1.41-inner.exe
2026-11-07 10:00:00.000   Setup command line: /SL5=\"$1,2,3,C:\\x\" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL /LOG=C:\\x\\setup.log
2026-11-07 10:00:00.010   Created temporary directory: C:\\Users\\u\\AppData\\Local\\Temp\\is-ABCDE.tmp
2026-11-07 10:00:00.200   nopbar.exe --quit: exit code 0
2026-11-07 10:00:00.300   Starting the installation process.
2026-11-07 10:00:00.310   Creating directory: C:\\Users\\u\\AppData\\Local\\Programs\\NopBar
2026-11-07 10:00:00.320   -- File entry --
2026-11-07 10:00:00.320   Dest filename: C:\\Users\\u\\AppData\\Local\\Programs\\NopBar\\nopbar.exe
2026-11-07 10:00:00.400   Successfully installed the file.
2026-11-07 10:00:00.410   -- File entry --
2026-11-07 10:00:00.410   Dest filename: C:\\Users\\u\\AppData\\Local\\Programs\\NopBar\\LICENSE.txt
2026-11-07 10:00:00.420   Successfully installed the file.
2026-11-07 10:00:00.425   -- File entry --
2026-11-07 10:00:00.425   Dest filename: C:\\Users\\u\\AppData\\Local\\Programs\\NopBar\\nopbar-mcp.exe
2026-11-07 10:00:00.426   Successfully installed the file.
2026-11-07 10:00:00.430   -- Icon entry --
2026-11-07 10:00:00.440   Installation process succeeded.
2026-11-07 10:00:00.450   -- Run entry --
2026-11-07 10:00:00.460   Need to restart Windows? No
2026-11-07 10:00:00.470   Deinitializing Setup.
2026-11-07 10:00:00.480   Log closed.
";

    #[test]
    fn steps_only_move_forward() {
        let mut last = 0.0;
        let mut seen = Vec::new();
        for n in 0..=LOG.lines().count() {
            let cut: String = LOG.lines().take(n).map(|l| format!("{l}\n")).collect();
            let f = fraction(&cut);
            assert!(f >= last, "line {n}: {f} < {last}");
            assert!(f < 1.0, "1.0 is the exit code's");
            last = f;
            seen.push(f);
        }
        assert_eq!(fraction(""), 0.0);
        assert_eq!(last, 0.97);
        // Each step shows: start, temp dir, install, both files, icon,
        // success, run, close.
        let mut distinct: Vec<f32> = seen.iter().map(|f| (f * 100.0).round() / 100.0).collect();
        distinct.dedup();
        assert_eq!(distinct, [0.0, 0.04, 0.10, 0.30, 0.40, 0.50, 0.60, 0.70, 0.88, 0.94, 0.97]);
    }

    #[test]
    fn more_file_entries_than_expected_stay_in_their_band() {
        let log = "Starting the installation process.\n-- File entry --\n-- File entry --\n-- File entry --\n";
        assert_eq!(fraction(log), 0.60);
    }

    #[test]
    fn logs_decode_in_any_encoding() {
        assert_eq!(decode(b"\xEF\xBB\xBFLog closed."), "Log closed.");
        assert_eq!(decode(b"Log closed."), "Log closed.");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("Log closed.".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode(&utf16), "Log closed.");
        assert_eq!(fraction(&decode(&utf16)), 0.97);
        assert_eq!(read_log(std::path::Path::new(r"C:\does\not\exist.log")), "");
    }
}
