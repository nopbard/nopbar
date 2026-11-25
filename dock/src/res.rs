//! What the process holds: memory, threads, handles, GDI / USER objects.
//!
//! A one-line summary goes to dock.log every ten minutes on the existing
//! service thread, including release builds. No thread or timer per sample.
//!
//! Also `trim`: hand freed heap pages and the idle working set back to
//! Windows after big frees (startup done, Launchpad's icons dropped).

use std::time::Duration;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::Memory::{GetProcessHeap, HeapCompact, HEAP_FLAGS};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetGuiResources, GetProcessHandleCount, SetProcessWorkingSetSize, GR_GDIOBJECTS,
    GR_USEROBJECTS,
};

const EVERY: Duration = Duration::from_secs(10 * 60);

/// PROCESS_MEMORY_COUNTERS_EX (psapi), declared here to avoid another
/// windows-rs feature for one call.
#[repr(C)]
#[derive(Default)]
struct MemCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged_pool: usize,
    quota_paged_pool: usize,
    quota_peak_non_paged_pool: usize,
    quota_non_paged_pool: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn K32GetProcessMemoryInfo(process: HANDLE, counters: *mut MemCounters, cb: u32) -> i32;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub private_kb: u64,
    pub working_kb: u64,
    pub peak_working_kb: u64,
    pub threads: u32,
    pub handles: u32,
    pub gdi: u32,
    pub user: u32,
}

/// The process's numbers now (0 for any that cannot be read).
pub fn usage() -> Usage {
    let mut u = Usage::default();
    unsafe {
        let me = GetCurrentProcess();
        let mut m = MemCounters { cb: std::mem::size_of::<MemCounters>() as u32, ..Default::default() };
        if K32GetProcessMemoryInfo(me, &mut m, m.cb) != 0 {
            u.private_kb = m.private_usage as u64 / 1024;
            u.working_kb = m.working_set as u64 / 1024;
            u.peak_working_kb = m.peak_working_set as u64 / 1024;
        }
        let mut n = 0u32;
        if GetProcessHandleCount(me, &mut n).is_ok() {
            u.handles = n;
        }
        u.gdi = GetGuiResources(me, GR_GDIOBJECTS);
        u.user = GetGuiResources(me, GR_USEROBJECTS);
        u.threads = thread_count().unwrap_or(0);
    }
    u
}

fn thread_count() -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let me = GetCurrentProcessId();
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            if e.th32ProcessID == me {
                found = Some(e.cntThreads);
                break;
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
        found
    }
}

/// The dock.log line.
pub fn line(u: &Usage, uptime: Duration) -> String {
    let mb = |kb: u64| kb as f64 / 1024.0;
    format!(
        "res: handles={} threads={} gdi={} user={} private={:.1}MB ws={:.1}MB up={}m",
        u.handles, u.threads, u.gdi, u.user, mb(u.private_kb), mb(u.working_kb), uptime.as_secs()/60
    )
}

/// Starts the periodic resource line once, including release builds.
pub fn start_log() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        struct Every;
        impl crate::service::Watch for Every {
            fn name(&self) -> &'static str {
                "resource log"
            }
            fn handles(&self) -> Vec<HANDLE> {
                Vec::new()
            }
            fn wake(&mut self, why: crate::service::Wake) -> crate::service::Next {
                if why != crate::service::Wake::Start {
                    let up = crate::START.get().map(|s| s.elapsed()).unwrap_or_default();
                    crate::log!("{}", line(&usage(), up));
                }
                crate::service::Next::Sleep(EVERY)
            }
        }
        crate::service::add(Box::new(Every));
    });
}

/// Gives freed memory back: decommits free heap blocks (real private bytes)
/// and trims the working set (pages still in use come back on touch, from
/// the standby list: no disk read). Cheap; call after big frees, not often.
pub fn trim(why: &str) {
    let before = crate::log::debug_enabled().then(usage);
    unsafe {
        if let Ok(heap) = GetProcessHeap() {
            HeapCompact(heap, HEAP_FLAGS(0));
        }
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
    if let Some(b) = before {
        let a = usage();
        crate::log!(
            "res: trimmed ({why}): private {:.1} -> {:.1} MB, working set {:.1} -> {:.1} MB",
            b.private_kb as f64 / 1024.0,
            a.private_kb as f64 / 1024.0,
            b.working_kb as f64 / 1024.0,
            a.working_kb as f64 / 1024.0
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_process() {
        let u = usage();
        assert!(u.private_kb > 0, "{u:?}");
        assert!(u.working_kb > 0, "{u:?}");
        assert!(u.threads >= 1, "{u:?}");
        assert!(u.handles > 0, "{u:?}");
    }

    #[test]
    fn line_reads_well() {
        let u = Usage { private_kb: 30 * 1024 + 512, working_kb: 50 * 1024, peak_working_kb: 70 * 1024, threads: 14, handles: 800, gdi: 30, user: 25 };
        assert_eq!(
            line(&u, Duration::from_secs(3 * 3600 + 120)),
            "res: handles=800 threads=14 gdi=30 user=25 private=30.5MB ws=50.0MB up=182m"
        );
    }
}

#[cfg(test)]
mod assistant_measurements {
    #[test]
    #[ignore="isolated resource sampling cost"]
    fn resource_sample_cost(){
        let start=std::time::Instant::now();let mut samples=Vec::new();
        for _ in 0..20{let at=std::time::Instant::now();let u=super::usage();assert!(u.handles>0&&u.threads>0);samples.push(at.elapsed().as_secs_f64()*1000.);}
        let mean=start.elapsed().as_secs_f64()*1000./20.;let max=samples.into_iter().fold(0f64,f64::max);
        eprintln!("resource sample: mean={mean:.3}ms max={max:.3}ms cadence={}s",super::EVERY.as_secs());
        assert_eq!(super::EVERY.as_secs(),600);
    }
}
