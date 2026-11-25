//! How much each app is used, from Windows' own record (UserAssist): what
//! "Tidy Home" puts on the first page. Read-only and local, off the UI
//! thread; nothing is kept after Launchpad closes.
//!
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist\
//! {GUID}\Count holds one value per program started from the shell: its
//! name ROT13-encoded (a path, often led by a KNOWNFOLDERID, or an AUMID),
//! its data the run count at offset 4 and the last run's FILETIME at
//! offset 60 (Windows 7 and later; Windows XP's 16-byte form has the count
//! plus five at 4 and the FILETIME at 8). Two GUIDs matter: programs
//! (exe paths and AUMIDs) and shortcuts (.lnk paths).
//!
//! An entry is matched to a Launchpad app (its shell:AppsFolder id: an
//! AUMID, or a known-folder path of the program or its shortcut) by the
//! whole id (known folders expanded on both sides), else by the program's
//! file name, else (a shortcut) by its name against the app's. The score
//! is the run count weighted by how recent the last run was. With
//! UserAssist empty or turned off every score is 0: Tidy Home keeps the
//! current order for its first page.

use std::collections::{HashMap, HashSet};

/// UserAssist's GUID for programs (exe paths, AUMIDs).
pub const EXE: &str = "{CEBFF5CD-ACE2-4F4F-9178-9926F41749EA}";
/// UserAssist's GUID for shortcuts (.lnk paths).
pub const LNK: &str = "{F4E57C4B-2036-45F0-A9AB-443BCFE33D9F}";

/// A run's weight halves every this many days since it was last run.
pub const HALF_LIFE_DAYS: f64 = 30.0;
/// The weight of an entry that says when it last ran nowhere.
pub const UNDATED: f64 = 0.25;

/// FILETIME ticks (100 ns) in a day.
const DAY: f64 = 864_000_000_000.0;
/// 2000-01-01 as a FILETIME: a run recorded before it is no real time
/// (UserAssist is younger).
const SINCE: u64 = 125_911_584_000_000_000;

/// The registry's binary value type (REG_BINARY): the only one UserAssist
/// entries are.
pub const BINARY: u32 = 3;
/// A Windows 7+ entry is exactly this long; Windows XP's this.
pub const WIN7_LEN: usize = 72;
pub const XP_LEN: usize = 16;

/// What one GUID's Count key holds: (value name as stored, value type,
/// data).
pub trait Reader {
    fn values(&self, guid: &str) -> Vec<(String, u32, Vec<u8>)>;
}

/// Which list an entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Exe,
    Lnk,
}

/// One program as UserAssist records it (name decoded).
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub count: u32,
    /// FILETIME of the last run (0: not recorded).
    pub last: u64,
}

/// ROT13 on ASCII letters (UserAssist's value names); the rest as it is.
pub fn rot13(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a') + 13) % 26 + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A') + 13) % 26 + b'A') as char,
            _ => c,
        })
        .collect()
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    d.get(at..at + 8).map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

/// A value's data: (run count, last run FILETIME). Windows 7 and later:
/// exactly 72 bytes, the count at 4, the time at 60. Windows XP: exactly
/// 16 bytes, the count plus five at 4, the time at 8. Any other length
/// (cut short, or something else): None.
pub fn parse(d: &[u8]) -> Option<(u32, u64)> {
    match d.len() {
        WIN7_LEN => Some((u32_at(d, 4)?, u64_at(d, 60)?)),
        XP_LEN => Some((u32_at(d, 4)?.saturating_sub(5), u64_at(d, 8)?)),
        _ => None,
    }
}

/// Every entry of both lists (bookkeeping values, unreadable data and
/// entries never run left out).
pub fn entries(r: &dyn Reader) -> Vec<Entry> {
    let mut out = Vec::new();
    for (guid, kind) in [(EXE, Kind::Exe), (LNK, Kind::Lnk)] {
        for (raw, kind_of_value, data) in r.values(guid) {
            let name = rot13(&raw);
            // (Only binary values are entries: anything else is not ours.)
            if kind_of_value != BINARY || name.is_empty() || name.starts_with("UEME_") {
                continue;
            }
            let Some((count, last)) = parse(&data) else { continue };
            if count == 0 && last == 0 {
                continue;
            }
            out.push(Entry { kind, name, count, last });
        }
    }
    out
}

/// `s` with a leading "{KNOWNFOLDERID}" replaced by that folder's path
/// (`known`: the GUID with braces -> its path; unknown: as it is).
pub fn expand(s: &str, known: &dyn Fn(&str) -> Option<String>) -> String {
    if s.starts_with('{') {
        if let Some(end) = s.find('}') {
            if let Some(dir) = known(&s[..=end]) {
                let rest = &s[end + 1..];
                let dir = dir.trim_end_matches(['\\', '/']);
                return format!("{dir}{rest}");
            }
        }
    }
    s.to_string()
}

/// A path or AUMID as compared: lower case, backslashes.
fn norm(s: &str) -> String {
    s.trim().replace('/', "\\").to_lowercase()
}

/// The last part of a path (lower case).
fn file_name(s: &str) -> &str {
    s.rsplit(['\\', '/']).next().unwrap_or(s)
}

/// A file name without its extension.
fn stem(s: &str) -> &str {
    match s.rfind('.') {
        Some(i) if i > 0 => &s[..i],
        _ => s,
    }
}

/// Launchpad's apps indexed the ways an entry can name them.
struct Index {
    whole: HashMap<String, String>,
    exe: HashMap<String, String>,
    name: HashMap<String, String>,
}

/// `k` -> `id` unless another id has `k` too (then nobody: no guessing).
fn add_unique(map: &mut HashMap<String, String>, clash: &mut HashSet<String>, k: String, id: &str) {
    if clash.contains(&k) {
        return;
    }
    match map.get(&k) {
        Some(other) if other != id => {
            map.remove(&k);
            clash.insert(k);
        }
        Some(_) => {}
        None => {
            map.insert(k, id.to_string());
        }
    }
}

impl Index {
    fn new(apps: &[(String, String)], known: &dyn Fn(&str) -> Option<String>) -> Index {
        let mut whole = HashMap::new();
        let (mut exe, mut name) = (HashMap::new(), HashMap::new());
        let (mut exe_clash, mut name_clash) = (HashSet::new(), HashSet::new());
        for (id, n) in apps {
            if id.is_empty() {
                continue;
            }
            let raw = norm(id);
            let full = norm(&expand(id, known));
            whole.entry(raw.clone()).or_insert_with(|| id.clone());
            whole.entry(full.clone()).or_insert_with(|| id.clone());
            let f = file_name(&full);
            if f.ends_with(".exe") {
                add_unique(&mut exe, &mut exe_clash, f.to_string(), id);
            }
            let n = n.trim().to_lowercase();
            if !n.is_empty() {
                add_unique(&mut name, &mut name_clash, n, id);
            }
        }
        Index { whole, exe, name }
    }

    /// The app entry `e` is about, if any.
    fn find(&self, e: &Entry, known: &dyn Fn(&str) -> Option<String>) -> Option<&String> {
        let raw = norm(&e.name);
        let full = norm(&expand(&e.name, known));
        if let Some(id) = self.whole.get(&raw).or_else(|| self.whole.get(&full)) {
            return Some(id);
        }
        let f = file_name(&full);
        match e.kind {
            Kind::Exe if f.ends_with(".exe") => self.exe.get(f),
            Kind::Lnk if f.ends_with(".lnk") => self.name.get(stem(f)),
            _ => None,
        }
    }
}

/// How much a run `last` (FILETIME) counts at `now`: 1 for a run now,
/// halving every HALF_LIFE_DAYS; UNDATED when it says no time.
pub fn recency(last: u64, now: u64) -> f64 {
    // No time, or no believable one (before 2000, or more than a day
    // ahead of now: a damaged entry): as if undated.
    if last < SINCE || last as f64 > now as f64 + DAY {
        return UNDATED;
    }
    let days = (now.saturating_sub(last)) as f64 / DAY;
    0.5f64.powf(days / HALF_LIFE_DAYS)
}

/// Each app's score (`apps`: (id, name); missing: 0): its entries' run
/// count (at least one when a time is recorded) times their recency, the
/// best of its entries (a program started from its shortcut is in both
/// lists).
pub fn scores(entries: &[Entry], apps: &[(String, String)], now: u64, known: &dyn Fn(&str) -> Option<String>) -> HashMap<String, f64> {
    let index = Index::new(apps, known);
    let mut out: HashMap<String, f64> = HashMap::new();
    for e in entries {
        let Some(id) = index.find(e, known) else { continue };
        let s = f64::from(e.count.max(1)) * recency(e.last, now);
        let v = out.entry(id.clone()).or_insert(0.0);
        if s > *v {
            *v = s;
        }
    }
    out
}

/// Now as a FILETIME.
pub fn now_filetime() -> u64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() / 100).unwrap_or(0) as u64;
    unix + 116_444_736_000_000_000
}

/// The real thing: this user's UserAssist in the registry.
pub struct Registry;

impl Reader for Registry {
    fn values(&self, guid: &str) -> Vec<(String, u32, Vec<u8>)> {
        use windows::core::{HSTRING, PWSTR};
        use windows::Win32::System::Registry::{RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ};
        let path = format!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist\\{guid}\\Count");
        let mut out = Vec::new();
        unsafe {
            let mut key = HKEY::default();
            if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, KEY_READ, &mut key).is_err() {
                return out;
            }
            for i in 0..8192u32 {
                let mut name = [0u16; 1024];
                let mut name_len = name.len() as u32;
                let mut data = [0u8; 256];
                let mut data_len = data.len() as u32;
                let mut kind = 0u32;
                let r = RegEnumValueW(key, i, Some(PWSTR(name.as_mut_ptr())), &mut name_len, None, Some(&mut kind), Some(data.as_mut_ptr()), Some(&mut data_len));
                if r.is_err() {
                    // (More data than this: not an entry of ours; the end: done.)
                    if r == windows::Win32::Foundation::ERROR_MORE_DATA {
                        continue;
                    }
                    break;
                }
                let n = (name_len as usize).min(name.len());
                let d = (data_len as usize).min(data.len());
                out.push((String::from_utf16_lossy(&name[..n]), kind, data[..d].to_vec()));
            }
            let _ = RegCloseKey(key);
        }
        out
    }
}

/// A known folder's path by its "{GUID}".
pub fn known_folder(guid: &str) -> Option<String> {
    let g = windows::core::GUID::try_from(guid.trim_start_matches('{').trim_end_matches('}')).ok()?;
    unsafe {
        let p = windows::Win32::UI::Shell::SHGetKnownFolderPath(&g, windows::Win32::UI::Shell::KF_FLAG_DONT_VERIFY, None).ok()?;
        let s = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Every app's score from this user's UserAssist (blocking: call it off
/// the UI thread).
pub fn read(apps: &[(String, String)]) -> HashMap<String, f64> {
    let e = entries(&Registry);
    // (Known folders are looked up once each.)
    let cache: std::cell::RefCell<HashMap<String, Option<String>>> = Default::default();
    let known = |g: &str| cache.borrow_mut().entry(g.to_lowercase()).or_insert_with(|| known_folder(g)).clone();
    scores(&e, apps, now_filetime(), &known)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value as the registry gives it: (name, type, data).
    type Value = (String, u32, Vec<u8>);

    struct Fake(Vec<(&'static str, Vec<Value>)>);

    impl Reader for Fake {
        fn values(&self, guid: &str) -> Vec<Value> {
            self.0.iter().find(|(g, _)| *g == guid).map(|(_, v)| v.clone()).unwrap_or_default()
        }
    }

    /// A binary value named `name` (ROT13'd as stored).
    fn bin(name: &str, data: Vec<u8>) -> Value {
        (rot13(name), BINARY, data)
    }

    /// A Windows 7+ value: 72 bytes, count at 4, FILETIME at 60.
    fn win7(count: u32, last: u64) -> Vec<u8> {
        let mut d = vec![0u8; 72];
        d[4..8].copy_from_slice(&count.to_le_bytes());
        d[60..68].copy_from_slice(&last.to_le_bytes());
        d
    }

    fn known(g: &str) -> Option<String> {
        match g.to_ascii_uppercase().as_str() {
            "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}" => Some("C:\\Windows\\System32".into()),
            "{6D809377-6AF0-444B-8957-A3773F02200E}" => Some("C:\\Program Files\\".into()),
            "{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}" => Some("C:\\Users\\me\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs".into()),
            _ => None,
        }
    }

    fn pair(id: &str, name: &str) -> (String, String) {
        (id.to_string(), name.to_string())
    }

    const NOW: u64 = 133_800_000_000_000_000;

    #[test]
    fn rot13_round_trips_and_leaves_the_rest() {
        assert_eq!(rot13("Hello, World 1!"), "Uryyb, Jbeyq 1!");
        assert_eq!(rot13(&rot13("{1AC14E77}\\notepad.exe")), "{1AC14E77}\\notepad.exe");
        assert_eq!(rot13("{1NP14R77-02R7}\\abgrcnq.rkr"), "{1AC14E77-02E7}\\notepad.exe", "GUID letters are encoded too");
        assert_eq!(rot13("메모장"), "메모장");
    }

    #[test]
    fn binary_data_both_formats() {
        assert_eq!(parse(&win7(12, 99)), Some((12, 99)));
        let mut xp = vec![0u8; 16];
        xp[4..8].copy_from_slice(&17u32.to_le_bytes());
        xp[8..16].copy_from_slice(&7u64.to_le_bytes());
        assert_eq!(parse(&xp), Some((12, 7)), "XP: the count plus five");
        xp[4..8].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(parse(&xp).map(|p| p.0), Some(0), "never below zero");
        assert_eq!(parse(&[0u8; 20]), None);
        assert_eq!(parse(&[]), None);
    }

    #[test]
    fn entries_decode_and_skip_bookkeeping() {
        let f = Fake(vec![
            (EXE, vec![bin("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\notepad.exe", win7(5, NOW)), bin("UEME_CTLSESSION", vec![0u8; 1612]), bin("Chrome", win7(0, 0)), bin("bad", vec![1, 2])]),
            (LNK, vec![bin("{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}\\Paint.lnk", win7(2, NOW))]),
        ]);
        let e = entries(&f);
        assert_eq!(e.len(), 2, "{e:?}");
        assert_eq!(e[0], Entry { kind: Kind::Exe, name: "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\notepad.exe".into(), count: 5, last: NOW });
        assert_eq!(e[1].kind, Kind::Lnk);
        assert!(entries(&Fake(vec![])).is_empty(), "UserAssist empty or off: nothing");
    }

    /// The review's cases: only REG_BINARY values of a known length are
    /// read; a damaged time counts as no time, a big count as it is.
    #[test]
    fn only_well_formed_binary_values_count() {
        const REG_SZ: u32 = 1;
        const REG_DWORD: u32 = 4;
        let f = Fake(vec![(
            EXE,
            vec![
                (rot13("a.exe"), REG_SZ, win7(9, NOW)),
                (rot13("b.exe"), REG_DWORD, win7(9, NOW)),
                bin("c.exe", win7(3, NOW)[..68].to_vec()),
                bin("d.exe", win7(3, NOW)[..71].to_vec()),
                bin("e.exe", [win7(3, NOW), vec![0]].concat()),
                bin("f.exe", win7(4, NOW)),
            ],
        )]);
        let e = entries(&f);
        assert_eq!(e.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["f.exe"], "not binary, cut short, too long: left out");
        assert_eq!(parse(&[0xFF; 72]), Some((u32::MAX, u64::MAX)), "read as it is");
        // All 0xFF: the count stands, the time is no time.
        assert_eq!(recency(u64::MAX, NOW), UNDATED);
        assert_eq!(recency(NOW + 2 * DAY as u64, NOW), UNDATED, "days ahead: damaged");
        assert_eq!(recency(SINCE - 1, NOW), UNDATED, "before 2000");
        assert_eq!(recency(NOW + 60 * 10_000_000, NOW), 1.0, "a clock a minute behind: now");
        let apps = vec![("big".to_string(), "Big".to_string()), ("ok".to_string(), "Ok".to_string())];
        let got = scores(
            &[Entry { kind: Kind::Exe, name: "big".into(), count: u32::MAX, last: u64::MAX }, Entry { kind: Kind::Exe, name: "ok".into(), count: u32::MAX, last: NOW }],
            &apps,
            NOW,
            &known,
        );
        assert_eq!(got["big"], f64::from(u32::MAX) * UNDATED, "a damaged time never counts as recent");
        assert_eq!(got["ok"], f64::from(u32::MAX), "a big count with a real time is fine");
    }

    #[test]
    fn known_folder_prefixes_expand() {
        assert_eq!(expand("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\notepad.exe", &known), "C:\\Windows\\System32\\notepad.exe");
        assert_eq!(expand("{6D809377-6AF0-444B-8957-A3773F02200E}\\Git\\git-bash.exe", &known), "C:\\Program Files\\Git\\git-bash.exe", "a trailing backslash is not doubled");
        assert_eq!(expand("{00000000-0000-0000-0000-000000000000}\\x.exe", &known), "{00000000-0000-0000-0000-000000000000}\\x.exe", "unknown: as it is");
        assert_eq!(expand("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", &known), "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App");
        assert_eq!(expand("{broken", &known), "{broken");
    }

    #[test]
    fn entries_find_their_apps() {
        let apps = vec![
            pair("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\notepad.exe", "Notepad"),
            pair("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", "Calculator"),
            pair("{6D809377-6AF0-444B-8957-A3773F02200E}\\Git\\git-bash.exe", "Git Bash"),
            pair("{6D809377-6AF0-444B-8957-A3773F02200E}\\Tool\\tool.exe", "Tool A"),
            pair("{6D809377-6AF0-444B-8957-A3773F02200E}\\Other\\tool.exe", "Tool B"),
            pair("Chrome", "Google Chrome"),
            pair("{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}\\Games\\Solitaire.lnk", "Solitaire"),
            pair("{6D809377-6AF0-444B-8957-A3773F02200E}\\Paint\\paint.exe", "Paint"),
        ];
        let e = |kind, name: &str, count| Entry { kind, name: name.into(), count, last: NOW };
        let got = scores(
            &[
                // The same id, GUID prefixes and all.
                e(Kind::Exe, "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\notepad.exe", 3),
                // An AUMID, any case.
                e(Kind::Exe, "microsoft.windowscalculator_8wekyb3d8bbwe!app", 7),
                // A full path where the app has a known folder.
                e(Kind::Exe, "C:\\Program Files\\Git\\git-bash.exe", 4),
                // Two apps with the same file name: neither by name alone.
                e(Kind::Exe, "D:\\elsewhere\\tool.exe", 9),
                e(Kind::Exe, "Chrome", 11),
                // A shortcut by its path; another by its name.
                e(Kind::Lnk, "{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}\\Games\\Solitaire.lnk", 2),
                e(Kind::Lnk, "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}\\Accessories\\Paint.lnk", 6),
                e(Kind::Exe, "{6D809377-6AF0-444B-8957-A3773F02200E}\\Paint\\paint.exe", 1),
                e(Kind::Exe, "C:\\nobody\\gone.exe", 50),
            ],
            &apps,
            NOW,
            &known,
        );
        let s = |id: &str| got.get(id).copied().unwrap_or(0.0);
        assert_eq!(s(&apps[0].0), 3.0);
        assert_eq!(s(&apps[1].0), 7.0);
        assert_eq!(s(&apps[2].0), 4.0);
        assert_eq!((s(&apps[3].0), s(&apps[4].0)), (0.0, 0.0), "no guessing between two");
        assert_eq!(s("Chrome"), 11.0);
        assert_eq!(s(&apps[6].0), 2.0);
        assert_eq!(s(&apps[7].0), 6.0, "the best of its entries, not their sum");
        assert_eq!(got.len(), 6);
    }

    #[test]
    fn recent_runs_count_more() {
        assert_eq!(recency(NOW, NOW), 1.0);
        let month = (30.0 * DAY) as u64;
        assert!((recency(NOW - month, NOW) - 0.5).abs() < 1e-9, "half after a half-life");
        assert!((recency(NOW - 2 * month, NOW) - 0.25).abs() < 1e-9);
        assert_eq!(recency(0, NOW), UNDATED);
        assert_eq!(recency(NOW + 5, NOW), 1.0, "a clock just behind: as now");
        // 10 runs two months ago lose to 4 runs this week.
        let apps = vec![pair("a", "A"), pair("b", "B")];
        let got = scores(
            &[Entry { kind: Kind::Exe, name: "a".into(), count: 10, last: NOW - 2 * month }, Entry { kind: Kind::Exe, name: "b".into(), count: 4, last: NOW - (2.0 * DAY) as u64 }],
            &apps,
            NOW,
            &known,
        );
        assert!(got["b"] > got["a"], "{got:?}");
        // Run, but no count recorded: once.
        let got = scores(&[Entry { kind: Kind::Exe, name: "a".into(), count: 0, last: NOW }], &apps, NOW, &known);
        assert_eq!(got["a"], 1.0);
    }

    #[test]
    fn now_is_a_filetime() {
        // After 2024-01-01 (133 485 408 000 000 000).
        assert!(now_filetime() > 133_485_408_000_000_000);
    }
}
