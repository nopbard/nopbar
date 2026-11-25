//! What "About this PC" (about.rs, from the system menu: a click on the
//! Windows button, panels::Panel::System) says about this PC, and the
//! recent files the menu lists.
//!
//! The PC's facts (its name, maker and model, the Windows edition, version
//! and build, the CPU, the graphics, the memory installed, what kind of PC
//! it is: form.rs) are registry, DXGI and firmware reads, read once and
//! kept: warm() reads them on a helper thread when the bar starts, so
//! nothing on the UI thread waits for them. Recent files come from
//! Windows' Recent folder (search::sources::recent_newest) on a short-lived
//! helper under a deadline; a slow answer leaves the last list shown.
//!
//! The naming (Windows 11 from the build number), the short CPU and GPU
//! names, the maker line and the memory size are pure and tested here.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ};

/// The facts "About this PC" shows ("" or 0: unknown, left out).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SysInfo {
    /// The device name (Settings > System > About).
    pub pc: String,
    /// Maker and model ("Example Works X14").
    pub maker: String,
    /// "Windows 11 Home".
    pub windows: String,
    /// "25H2".
    pub version: String,
    /// "26200.7171".
    pub build: String,
    /// The CPU's name as Windows has it (cpu_name; about.rs shortens it).
    pub cpu: String,
    /// The graphics ("AMD Radeon 780M Graphics"; two GPUs: both).
    pub gpu: String,
    /// Memory installed, KB.
    pub ram_kb: u64,
    /// What kind of PC it is (form::decide), all but the posture.
    pub form: super::form::Inputs,
}

impl SysInfo {
    /// "Windows 11 Home · 25H2".
    pub fn os_line(&self) -> String {
        match (self.windows.is_empty(), self.version.trim().is_empty()) {
            (false, false) => format!("{} · {}", self.windows, self.version.trim()),
            (false, true) => self.windows.clone(),
            (true, _) => self.version.trim().to_string(),
        }
    }
}

/// The first Windows 11 build: ProductName still says "Windows 10" there.
const FIRST_11: u32 = 22000;

/// Windows' name from the registry's ProductName, DisplayVersion and
/// CurrentBuild: 11 from its first build on, whatever ProductName says.
pub fn windows_name(product: &str, version: &str, build: u32) -> String {
    let product = product.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut name = if product.is_empty() {
        match build {
            FIRST_11.. => "Windows 11".to_string(),
            10240.. => "Windows 10".to_string(),
            _ => "Windows".to_string(),
        }
    } else if build >= FIRST_11 {
        product.replacen("Windows 10", "Windows 11", 1)
    } else {
        product
    };
    let version = version.trim();
    if !version.is_empty() {
        name.push(' ');
        name.push_str(version);
    }
    name
}

/// The CPU's name without the marks and padding vendors put in it
/// ("Intel(R) Core(TM) i7-1165G7 CPU @ 2.80GHz   ").
pub fn cpu_name(raw: &str) -> String {
    let mut s = raw.to_string();
    for mark in ["(R)", "(r)", "(TM)", "(tm)", "®", "™"] {
        s = s.replace(mark, "");
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A CPU's name as people say it: no integrated graphics ("w/ Radeon
/// 780M Graphics"), clock ("@ 2.80GHz"), "CPU", "Processor" or core count
/// ("8-Core"). From cpu_name's output.
pub fn cpu_short(name: &str) -> String {
    let mut s = name.trim().to_string();
    let lower = s.to_ascii_lowercase();
    // ASCII lowercase keeps every byte where it was.
    if let Some(i) = [" w/ ", " with ", " @ "].iter().filter_map(|cut| lower.find(cut)).min() {
        s.truncate(i);
    }
    let mut words: Vec<&str> = s.split_whitespace().collect();
    while let Some(last) = words.last() {
        let l = last.to_ascii_lowercase();
        if l == "cpu" || l == "processor" || l.ends_with("-core") {
            words.pop();
        } else {
            break;
        }
    }
    let short = words.join(" ");
    if short.is_empty() {
        name.trim().to_string()
    } else {
        short
    }
}

/// The graphics line from the adapters' names (DXGI, in its order): marks
/// dropped (cpu_name), each name once, at most two.
pub fn gpu_line(names: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = cpu_name(n);
        if !n.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&n)) {
            out.push(n);
        }
    }
    out.truncate(2);
    out.join(", ")
}

/// What firmware writes when the maker left a field blank.
fn placeholder(s: &str) -> bool {
    const BLANK: [&str; 13] = [
        "to be filled by o.e.m.",
        "default string",
        "system manufacturer",
        "system product name",
        "o.e.m.",
        "oem",
        "not applicable",
        "not specified",
        "none",
        "unknown",
        "invalid",
        "type1productconfigid",
        "123456789",
    ];
    let l = s.trim().to_ascii_lowercase();
    l.is_empty() || BLANK.contains(&l.as_str())
}

/// "Example Works X14": maker and model, without placeholders, the maker once
/// ("LENOVO" + "LENOVO 20XW" -> "LENOVO 20XW"); "" when neither is known.
pub fn maker_model(maker: &str, model: &str) -> String {
    let tidy = |s: &str| if placeholder(s) { String::new() } else { s.split_whitespace().collect::<Vec<_>>().join(" ") };
    let (maker, model) = (tidy(maker), tidy(model));
    match (maker.is_empty(), model.is_empty()) {
        (true, true) => String::new(),
        (false, true) => maker,
        (true, false) => model,
        (false, false) if model.to_lowercase().starts_with(&maker.to_lowercase()) => model,
        (false, false) => format!("{maker} {model}"),
    }
}

/// Memory installed (KB) in whole GB, as the PC is sold ("16"): rounded,
/// at least 1.
pub fn ram_gb(kb: u64) -> String {
    let gb = (kb as f64 / (1024.0 * 1024.0)).round().max(1.0);
    format!("{gb:.0}")
}

const CURRENT_VERSION: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
const CPU0: &str = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0";
const BIOS: &str = "HARDWARE\\DESCRIPTION\\System\\BIOS";
const OEM: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\OEMInformation";

/// A string value under HKLM\`sub` ("" when missing).
fn reg_string(sub: &str, value: PCWSTR) -> String {
    let sub = HSTRING::from(sub);
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    unsafe {
        let mut size = 0u32;
        if RegGetValueW(HKEY_LOCAL_MACHINE, &sub, value, flags, None, None, Some(&mut size)).is_err() || size == 0 || size > 4096 {
            return String::new();
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        if RegGetValueW(HKEY_LOCAL_MACHINE, &sub, value, flags, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)).is_err() {
            return String::new();
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    }
}

fn reg_dword(sub: &str, value: PCWSTR) -> Option<u32> {
    let sub = HSTRING::from(sub);
    let mut data = 0u32;
    let mut size = 4u32;
    unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, &sub, value, RRF_RT_REG_DWORD, None, Some(&mut data as *mut u32 as *mut _), Some(&mut size)).ok().ok()? };
    Some(data)
}

fn pc_name() -> String {
    use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
    let mut buf = [0u16; 256];
    let mut n = buf.len() as u32;
    match unsafe { GetComputerNameExW(ComputerNameDnsHostname, Some(PWSTR(buf.as_mut_ptr())), &mut n) } {
        Ok(()) => String::from_utf16_lossy(&buf[..(n as usize).min(buf.len())]),
        Err(_) => std::env::var("COMPUTERNAME").unwrap_or_default(),
    }
}

/// Installed memory (the firmware's count), else what Windows can use.
fn ram_kb() -> u64 {
    use windows::Win32::System::SystemInformation::{GetPhysicallyInstalledSystemMemory, GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut kb = 0u64;
    if unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) }.is_ok() && kb > 0 {
        return kb;
    }
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() {
        return m.ullTotalPhys / 1024;
    }
    0
}

/// The hardware graphics adapters' names (no software or remote ones).
fn gpu_names() -> Vec<String> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_REMOTE, DXGI_ADAPTER_FLAG_SOFTWARE};
    let mut out = Vec::new();
    unsafe {
        let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
        for i in 0..8 {
            let Ok(a) = f.EnumAdapters1(i) else { break };
            let Ok(d) = a.GetDesc1() else { continue };
            if d.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 | DXGI_ADAPTER_FLAG_REMOTE.0 as u32) != 0 {
                continue;
            }
            let n = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
            out.push(String::from_utf16_lossy(&d.Description[..n]));
        }
    }
    out
}

/// Maker and model: the firmware's (as Windows keeps them), else what the
/// maker wrote for Settings' About page.
fn maker() -> String {
    let fw = maker_model(&reg_string(BIOS, w!("SystemManufacturer")), &reg_string(BIOS, w!("SystemProductName")));
    if !fw.is_empty() {
        return fw;
    }
    maker_model(&reg_string(OEM, w!("Manufacturer")), &reg_string(OEM, w!("Model")))
}

/// The system drive's size and the space free to this user, bytes (a
/// filesystem call: a helper thread's).
pub fn system_disk() -> Option<(u64, u64)> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let drive = std::env::var("SystemDrive").ok().filter(|d| d.len() == 2 && d.ends_with(':')).unwrap_or_else(|| "C:".into());
    let (mut free, mut total) = (0u64, 0u64);
    unsafe { GetDiskFreeSpaceExW(&HSTRING::from(format!("{drive}\\")), Some(&mut free), Some(&mut total), None).ok()? };
    (total > 0).then_some((total, free.min(total)))
}

fn read() -> SysInfo {
    let build_s = reg_string(CURRENT_VERSION, w!("CurrentBuild"));
    let build_n: u32 = build_s.trim().parse().unwrap_or(0);
    let mut version = reg_string(CURRENT_VERSION, w!("DisplayVersion"));
    if version.trim().is_empty() {
        // Windows 10 before 20H2.
        version = reg_string(CURRENT_VERSION, w!("ReleaseId"));
    }
    let build = match (build_n, reg_dword(CURRENT_VERSION, w!("UBR"))) {
        (0, _) => String::new(),
        (b, Some(ubr)) => format!("{b}.{ubr}"),
        (b, None) => b.to_string(),
    };
    SysInfo {
        pc: pc_name(),
        maker: maker(),
        windows: windows_name(&reg_string(CURRENT_VERSION, w!("ProductName")), "", build_n),
        version: version.trim().to_string(),
        build,
        cpu: cpu_name(&reg_string(CPU0, w!("ProcessorNameString"))),
        gpu: gpu_line(&gpu_names()),
        ram_kb: ram_kb(),
        form: super::form::read(),
    }
}

static INFO: OnceLock<SysInfo> = OnceLock::new();

/// The PC's facts if the helper thread has read them (never waits: the
/// UI thread asks). Not yet: the read is started (again) if it is not
/// running, and About shows what it knows meanwhile.
pub fn info() -> Option<&'static SysInfo> {
    let got = INFO.get();
    if got.is_none() {
        warm();
    }
    got
}

/// Reads them on a helper thread (the bar's start), so About finds them
/// ready. Only the helper reads (the UI never waits on it); a
/// thread that could not start is tried again on the next call.
pub fn warm() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if INFO.get().is_some() || RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("sysinfo".into()).spawn(|| {
        let _ = INFO.get_or_init(read);
    });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// A recent file or folder: what the row says and what it opens.
#[derive(Clone, Debug, PartialEq)]
pub struct Recent {
    pub name: String,
    pub path: String,
}

/// Recent files the menu lists.
pub const RECENT_FILES: usize = 8;
/// How long the menu waits for them.
const RECENT_WAIT: Duration = Duration::from_millis(300);

/// The newest recent files (Windows' Recent folder), found on a helper
/// thread; past RECENT_WAIT the last list found is shown (a shortcut to a
/// dead network share must not hold up the menu).
pub fn recent_files() -> Vec<Recent> {
    static LAST: Mutex<Vec<Recent>> = Mutex::new(Vec::new());
    // One at a time: a stuck lookup keeps one thread, not one per open.
    static CAP: crate::worker::HelperCap = crate::worker::HelperCap::new(1);
    let last = || LAST.lock().map(|l| l.clone()).unwrap_or_default();
    let Some(slot) = CAP.try_take() else { return last() };
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("recent".into()).spawn(move || {
        let _slot = slot;
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let found: Vec<Recent> = crate::search::sources::recent_newest(RECENT_FILES, &|| false)
            .into_iter()
            .map(|h| Recent { name: h.title, path: h.target })
            .collect();
        if com {
            unsafe { CoUninitialize() };
        }
        if let Ok(mut l) = LAST.lock() {
            *l = found.clone();
        }
        let _ = tx.send(found);
    });
    if spawned.is_err() {
        return last();
    }
    rx.recv_timeout(RECENT_WAIT).unwrap_or_else(|_| last())
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "system_harmony.rs"]
pub(in crate::topbar) mod harmony;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_11_is_named_from_the_build() {
        // ProductName says 10 on Windows 11.
        assert_eq!(windows_name("Windows 10 Home", "25H2", 26200), "Windows 11 Home 25H2");
        assert_eq!(windows_name("Windows 10 Pro", "23H2", 22631), "Windows 11 Pro 23H2");
        assert_eq!(windows_name("Windows 10 Pro", "", 22000), "Windows 11 Pro");
        // Windows 10 stays 10.
        assert_eq!(windows_name("Windows 10 Pro", "22H2", 19045), "Windows 10 Pro 22H2");
        // Already right, or padded.
        assert_eq!(windows_name("  Windows 11  Enterprise ", " 24H2 ", 26100), "Windows 11 Enterprise 24H2");
        // Nothing in the registry: the build alone.
        assert_eq!(windows_name("", "", 26100), "Windows 11");
        assert_eq!(windows_name("", "1909", 18363), "Windows 10 1909");
        assert_eq!(windows_name("", "", 0), "Windows");
    }

    #[test]
    fn memory_is_shown_in_whole_gb() {
        assert_eq!(ram_gb(16 * 1024 * 1024), "16");
        assert_eq!(ram_gb(32 * 1024 * 1024), "32");
        // What Windows can use, a little under what is installed.
        assert_eq!(ram_gb(16_400_000), "16");
        assert_eq!(ram_gb(8_100_000), "8");
        assert_eq!(ram_gb(300_000), "1", "never 0");
    }

    #[test]
    fn cpu_names_lose_their_marks() {
        assert_eq!(cpu_name("Intel(R) Core(TM) i7-1165G7 CPU @ 2.80GHz   "), "Intel Core i7-1165G7 CPU @ 2.80GHz");
        assert_eq!(cpu_name("AMD Ryzen 7 8840U w/ Radeon 780M Graphics      "), "AMD Ryzen 7 8840U w/ Radeon 780M Graphics");
        assert_eq!(cpu_name(""), "");
    }

    #[test]
    fn cpu_names_are_shortened_for_about() {
        assert_eq!(cpu_short("AMD Ryzen 7 8840U w/ Radeon 780M Graphics"), "AMD Ryzen 7 8840U");
        assert_eq!(cpu_short("AMD Ryzen 7 7840U w/ Radeon 780M"), "AMD Ryzen 7 7840U");
        assert_eq!(cpu_short("Intel Core i7-1165G7 CPU @ 2.80GHz"), "Intel Core i7-1165G7");
        assert_eq!(cpu_short("AMD Ryzen 7 5800X 8-Core Processor"), "AMD Ryzen 7 5800X");
        assert_eq!(cpu_short("AMD Ryzen 5 3600 6-Core Processor             "), "AMD Ryzen 5 3600");
        assert_eq!(cpu_short("12th Gen Intel Core i7-1260P"), "12th Gen Intel Core i7-1260P");
        assert_eq!(cpu_short("Intel Core Ultra 7 155H"), "Intel Core Ultra 7 155H");
        assert_eq!(cpu_short("Snapdragon X Elite - X1E78100 - Qualcomm Oryon CPU"), "Snapdragon X Elite - X1E78100 - Qualcomm Oryon");
        assert_eq!(cpu_short(""), "");
        assert_eq!(cpu_short("Processor"), "Processor", "never empties a name");
    }

    #[test]
    fn graphics_are_named_once_each() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(gpu_line(&s(&["AMD Radeon(TM) 780M Graphics"])), "AMD Radeon 780M Graphics");
        // Two GPUs: both (the one Windows lists first, first).
        assert_eq!(gpu_line(&s(&["NVIDIA GeForce RTX 4060 Laptop GPU", "Intel(R) UHD Graphics"])), "NVIDIA GeForce RTX 4060 Laptop GPU, Intel UHD Graphics");
        // The same adapter twice (two outputs), and at most two.
        assert_eq!(gpu_line(&s(&["Intel(R) Arc(TM) Graphics", "Intel Arc Graphics", "A", "B"])), "Intel Arc Graphics, A");
        assert_eq!(gpu_line(&[]), "");
    }

    #[test]
    fn the_maker_line_skips_placeholders() {
        assert_eq!(maker_model("Example Works", "X14"), "Example Works X14");
        assert_eq!(maker_model("LENOVO", "LENOVO 20XW"), "LENOVO 20XW", "the maker once");
        assert_eq!(maker_model("Micro-Star International Co., Ltd.", "  MS-7C56  "), "Micro-Star International Co., Ltd. MS-7C56");
        assert_eq!(maker_model("To Be Filled By O.E.M.", "To Be Filled By O.E.M."), "");
        assert_eq!(maker_model("System manufacturer", "System Product Name"), "");
        assert_eq!(maker_model("ASUS", "Default string"), "ASUS");
        assert_eq!(maker_model("", "Surface Laptop 5"), "Surface Laptop 5");
    }

    #[test]
    fn the_os_line_joins_edition_and_version() {
        let i = |w: &str, v: &str| SysInfo { windows: w.into(), version: v.into(), ..Default::default() };
        assert_eq!(i("Windows 11 Home", "25H2").os_line(), "Windows 11 Home · 25H2");
        assert_eq!(i("Windows 11 Pro", "").os_line(), "Windows 11 Pro");
        assert_eq!(i("", "").os_line(), "");
    }
}
