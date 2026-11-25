//! S7 spike: can we read taskbar progress (ITaskbarList3) from explorer's
//! hidden taskbar through UI Automation, cheaply?
//!
//!   s7-progress source <secs>   window "S7 progress" that drives its own taskbar
//!                               progress 0 -> 100 over <secs>
//!   s7-progress dump            list taskbar UIA elements (name, type, RangeValue)
//!   s7-progress watch <secs>    property-changed events (RangeValue.Value, Name)
//!                               on the taskbar subtree + own CPU every 5 s
//! Log: stdout and %TEMP%\s7-progress.log

use std::io::Write;
use std::time::{Duration, Instant};

use windows::core::{w, Ref, Result, BSTR, PCWSTR};
use windows::Win32::Foundation::{FILETIME, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList, TBPF_NORMAL, TBPF_NOPROGRESS};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::implement;

fn log(line: &str) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let line = format!("[{}.{:03}] {}", t.as_secs() % 100_000, t.subsec_millis(), line);
    println!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("s7-progress.log")) {
        let _ = writeln!(f, "{line}");
    }
}

fn cpu_ms() -> f64 {
    let (mut a, mut b, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u);
    }
    let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 10_000.0;
    f(k) + f(u)
}

// ---------- source ----------

unsafe extern "system" fn src_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        PostQuitMessage(0);
    }
    DefWindowProcW(h, msg, wp, lp)
}

fn source(secs: u64) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(src_proc),
            hInstance: hinst.into(),
            lpszClassName: w!("S7Source"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let h = CreateWindowExW(WINDOW_EX_STYLE(0), w!("S7Source"), w!("S7 progress"), WS_OVERLAPPEDWINDOW, 100, 100, 500, 300, None, None, Some(hinst.into()), None)?;
        let _ = ShowWindow(h, SW_SHOWMINNOACTIVE);
        // Let the taskbar create the button before we talk to it.
        let t0 = Instant::now();
        let mut msg = MSG::default();
        while t0.elapsed() < Duration::from_millis(1500) {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let tb: ITaskbarList3 = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
        tb.HrInit()?;
        tb.SetProgressState(h, TBPF_NORMAL)?;
        let steps = 20u64;
        for i in 0..=steps {
            tb.SetProgressValue(h, i, steps)?;
            log(&format!("SOURCE progress {}%", i * 100 / steps));
            let until = Instant::now() + Duration::from_millis(secs * 1000 / steps);
            while Instant::now() < until {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        tb.SetProgressState(h, TBPF_NOPROGRESS)?;
        let _ = DestroyWindow(h);
    }
    Ok(())
}

// ---------- UIA ----------

unsafe fn taskbar_root(uia: &IUIAutomation) -> Result<IUIAutomationElement> {
    let h = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null())?;
    uia.ElementFromHandle(h)
}

unsafe fn describe(e: &IUIAutomationElement) -> String {
    let name = e.CurrentName().unwrap_or_default();
    let ct = e.CurrentControlType().map(|c| c.0).unwrap_or(0);
    let class = e.CurrentClassName().unwrap_or_default();
    let mut rv = String::new();
    if let Ok(p) = e.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId) {
        rv = format!(" RangeValue={:.1}/{:.1}", p.CurrentValue().unwrap_or(-1.0), p.CurrentMaximum().unwrap_or(-1.0));
    }
    format!("type={ct} class={class} name={name:?}{rv}")
}

fn dump() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let root = taskbar_root(&uia)?;
        let all = root.FindAll(TreeScope_Descendants, &uia.CreateTrueCondition()?)?;
        let n = all.Length()?;
        log(&format!("taskbar subtree: {n} elements"));
        for i in 0..n {
            let e = all.GetElement(i)?;
            let d = describe(&e);
            if d.contains("S7") || d.contains("RangeValue") || d.contains("type=50000") || d.contains("type=50012") {
                log(&format!("  [{i}] {d}"));
            }
        }
    }
    Ok(())
}

fn variant_str(v: &VARIANT) -> String {
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        match inner.vt.0 {
            5 => format!("{}", inner.Anonymous.dblVal),
            8 => format!("{:?}", inner.Anonymous.bstrVal.to_string()),
            3 => format!("{}", inner.Anonymous.lVal),
            vt => format!("<vt {vt}>"),
        }
    }
}

#[implement(IUIAutomationPropertyChangedEventHandler)]
struct Handler;

impl IUIAutomationPropertyChangedEventHandler_Impl for Handler_Impl {
    fn HandlePropertyChangedEvent(&self, sender: Ref<IUIAutomationElement>, propertyid: UIA_PROPERTY_ID, newvalue: &VARIANT) -> Result<()> {
        let (name, aid, class) = sender
            .as_ref()
            .map(|e| unsafe {
                (
                    e.CurrentName().unwrap_or_default(),
                    e.CurrentAutomationId().unwrap_or_default(),
                    e.CurrentClassName().unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        log(&format!("EVENT prop={} on {:?} aid={:?} class={:?}: {}", propertyid.0, name, aid, class, variant_str(newvalue)));
        Ok(())
    }
}

fn watch(secs: u64) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let root = taskbar_root(&uia)?;
        let handler: IUIAutomationPropertyChangedEventHandler = Handler.into();
        let props = [UIA_RangeValueValuePropertyId, UIA_NamePropertyId, UIA_HelpTextPropertyId, UIA_ItemStatusPropertyId];
        uia.AddPropertyChangedEventHandlerNativeArray(&root, TreeScope_Subtree, None, &handler, &props)?;
        log(&format!("watching taskbar subtree for {secs}s"));
        let t0 = Instant::now();
        let mut c0 = cpu_ms();
        while t0.elapsed() < Duration::from_secs(secs) {
            std::thread::sleep(Duration::from_secs(5));
            let c = cpu_ms();
            log(&format!("CPU last 5s: {:.1} ms ({:.3}%)", c - c0, (c - c0) / 5000.0 * 100.0));
            c0 = c;
        }
        let _ = uia.RemoveAllEventHandlers();
        let _ = BSTR::new();
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let n = |i: usize, d: u64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    match args.get(1).map(String::as_str) {
        Some("source") => source(n(2, 20)),
        Some("dump") => dump(),
        Some("watch") => watch(n(2, 30)),
        _ => {
            println!("usage: s7-progress source <secs> | dump | watch <secs>");
            Ok(())
        }
    }
}
