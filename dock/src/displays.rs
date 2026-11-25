//! Displays as people know them: the number Windows shows for each one,
//! and a key that names the same monitor again after a restart or after
//! it was unplugged and plugged back in (per-display docks keep their
//! settings by it: config `dock.per_display`).
//!
//! **Who a display is** (the key). For every active display path,
//! QueryDisplayConfig + DisplayConfigGetDeviceInfo give the target's
//! monitor device path (`\\?\DISPLAY#GSM5B09#5&2a1b3c4d&0&UID4352#{...}`)
//! and its EDID maker / product ids. The device path names the monitor's
//! PnP instance, whose registry key keeps the EDID the monitor sent
//! (`HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY\GSM5B09\5&...\Device
//! Parameters\EDID`): its serial (the 0xFF descriptor's text, else the
//! 32-bit number at byte 12) tells two monitors of one model apart. So:
//!
//! - maker + product + serial: `GSM5B09-104NTAB12345` (the same monitor on
//!   any port, any PC);
//! - no serial: `GSM5B09@1a2b3c4d` - the model and the connector it is on
//!   (an 8-digit hash of its PnP instance, which Windows keeps per port):
//!   two monitors of one model never share a key, and unplugging one never
//!   hands its key (and its dock) to the other. The same monitor on another
//!   port is a new display (it starts as "Same as Dock 1");
//! - nothing from the monitor (virtual and some remote displays): its
//!   resolution and connector, `res-2560x1440@1a2b3c4d`;
//! - two with the same serial (a monitor reporting a placeholder): while
//!   both are connected each gets its connector too. A key never depends
//!   on the order or number of the displays connected.
//!
//! **The display number** (Settings > System > Display > Identify). Windows
//! does not publish how its Settings numbers displays; the documented
//! piece is the GDI device name each display path's source has
//! (`\\.\DISPLAY3`, DisplayConfigGetDeviceInfo's source name), which is
//! the number the older Control Panel showed and which the Settings app
//! follows on ordinary setups. NopBar sorts the active displays by that
//! number (then by adapter and source id, then by place, for anything
//! that does not have one) and numbers them 1, 2, 3 in that order: no
//! gaps, and the same numbers for as long as Windows keeps the same
//! sources (it keeps them per arrangement, across restarts). NopBar's own
//! "Identify" shows these numbers on the screens, so what Settings calls
//! "Dock 2" is always the display showing 2.

use windows::core::HSTRING;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO, MONITORINFOEXW};

/// One display now.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    /// HMONITOR as isize.
    pub handle: isize,
    pub rect: RECT,
    pub work: RECT,
    /// The main display.
    pub primary: bool,
    /// 1, 2, 3 (see the module comment).
    pub number: u32,
    /// Who it is (see the module comment).
    pub key: String,
    /// Its own name ("DELL U2720Q"; empty when Windows has none).
    pub name: String,
}

/// What DisplayConfig says about one active path.
#[derive(Clone, Debug, Default)]
struct PathInfo {
    gdi: String,
    device_path: String,
    maker: u16,
    product: u16,
    name: String,
    adapter: u64,
    source: u32,
}

fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

fn paths() -> Vec<PathInfo> {
    let mut out = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS || np == 0 || np > 64 || nm > 128 {
            return out;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return out;
        }
        paths.truncate(np as usize);
        for p in &paths {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let mut info = PathInfo {
                gdi: wide(&src.viewGdiDeviceName),
                adapter: ((p.sourceInfo.adapterId.HighPart as u32 as u64) << 32) | p.sourceInfo.adapterId.LowPart as u64,
                source: p.sourceInfo.id,
                ..Default::default()
            };
            let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut tgt.header) == 0 {
                info.device_path = wide(&tgt.monitorDevicePath);
                info.name = wide(&tgt.monitorFriendlyDeviceName);
                // Only when the flags say the ids are real.
                if tgt.flags.Anonymous.value & 0x4 != 0 {
                    info.maker = tgt.edidManufactureId;
                    info.product = tgt.edidProductCodeId;
                }
            }
            out.push(info);
        }
    }
    out
}

fn gdi_name(handle: isize) -> String {
    unsafe {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(HMONITOR(handle as *mut _), &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            wide(&mi.szDevice)
        } else {
            String::new()
        }
    }
}

/// The EDID the monitor sent, as Windows kept it for its PnP instance.
fn edid_of(device_path: &str) -> Option<Vec<u8>> {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY};
    let inst = instance_of(device_path)?;
    let sub = HSTRING::from(format!("SYSTEM\\CurrentControlSet\\Enum\\{inst}\\Device Parameters"));
    let mut buf = vec![0u8; 1024];
    let mut size = buf.len() as u32;
    unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, &sub, &HSTRING::from("EDID"), RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).ok().ok()?;
    }
    buf.truncate(size as usize);
    Some(buf)
}

/// Every display now, the main one first (crate::monitors' order), each
/// with its number and key. Reads DisplayConfig and the registry (a few
/// milliseconds): call it when displays change, not per frame.
pub fn current() -> Vec<Display> {
    let config = paths();
    let mons = crate::monitors::all();
    let mut found: Vec<(Option<&PathInfo>, &crate::monitors::Monitor)> = Vec::with_capacity(mons.len());
    for m in &mons {
        let gdi = gdi_name(m.handle);
        // Mirrored displays share a source: the first path names it.
        found.push((config.iter().find(|p| !p.gdi.is_empty() && p.gdi.eq_ignore_ascii_case(&gdi)), m));
    }
    let order: Vec<Order> = found
        .iter()
        .map(|(p, m)| Order { gdi: p.and_then(|p| gdi_number(&p.gdi)), adapter: p.map_or(u64::MAX, |p| p.adapter), source: p.map_or(u32::MAX, |p| p.source), x: m.rect.left, y: m.rect.top })
        .collect();
    let numbers = numbers(&order);
    let ids: Vec<Ident> = found
        .iter()
        .map(|(p, m)| {
            let (maker, product) = p.map_or((0, 0), |p| (p.maker, p.product));
            let serial = p.filter(|_| maker != 0 || product != 0).and_then(|p| edid_of(&p.device_path)).and_then(|e| edid_serial(&e));
            // The connector: the PnP instance (kept per port), else the
            // display path's adapter and target.
            let port = p.map(|p| match instance_of(&p.device_path) {
                Some(inst) => port_of(&inst),
                None => port_of(&format!("{}:{}", p.adapter, p.source)),
            });
            Ident { maker, product, serial, w: m.rect.right - m.rect.left, h: m.rect.bottom - m.rect.top, port }
        })
        .collect();
    let keys = unique_keys(&ids);
    found
        .iter()
        .enumerate()
        .map(|(i, (p, m))| Display { handle: m.handle, rect: m.rect, work: m.work, primary: m.primary, number: numbers[i], key: keys[i].clone(), name: p.map(|p| p.name.clone()).unwrap_or_default() })
        .collect()
}

/// The EDIDs of the PC's own screens (embedded connectors: a laptop's or a
/// tablet's panel), on or off (a shut lid's panel counts: QDC_ALL_PATHS).
/// DisplayConfig and the registry: a few milliseconds, for a helper thread.
pub fn internal_edids() -> Vec<Vec<u8>> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
        QDC_ALL_PATHS,
    };
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ALL_PATHS, &mut np, &mut nm) != ERROR_SUCCESS || np == 0 || np > 4096 || nm > 8192 {
            return out;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ALL_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return out;
        }
        paths.truncate(np as usize);
        for p in &paths {
            let t = p.targetInfo.outputTechnology;
            let internal = t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
                || t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
                || t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED
                || t == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS;
            if !internal || !p.targetInfo.targetAvailable.as_bool() {
                continue;
            }
            let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut tgt.header) != 0 {
                continue;
            }
            // Every source a target can show is a path of its own.
            let path = wide(&tgt.monitorDevicePath);
            if path.is_empty() || seen.iter().any(|s| s.eq_ignore_ascii_case(&path)) {
                continue;
            }
            if let Some(e) = edid_of(&path) {
                out.push(e);
            }
            seen.push(path);
        }
    }
    out
}

/// The key of the display monitor `handle` is (None: not a display now).
pub fn key_of(list: &[Display], handle: isize) -> Option<String> {
    list.iter().find(|d| d.handle == handle).map(|d| d.key.clone())
}

// ---- pure parts (tested) -------------------------------------------------------

/// "\\.\DISPLAY3" -> 3.
pub fn gdi_number(name: &str) -> Option<u32> {
    let n = name.trim().trim_start_matches(r"\\.\").to_ascii_uppercase();
    let digits = n.strip_prefix("DISPLAY")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|&v| v > 0)
}

/// What a display is sorted by for its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Order {
    /// Its source's \\.\DISPLAYn (None: DisplayConfig did not name it).
    pub gdi: Option<u32>,
    pub adapter: u64,
    pub source: u32,
    pub x: i32,
    pub y: i32,
}

/// Display numbers 1..=n for `list` (same order as given): by GDI number,
/// then adapter and source, then left to right and top to bottom.
pub fn numbers(list: &[Order]) -> Vec<u32> {
    let mut idx: Vec<usize> = (0..list.len()).collect();
    idx.sort_by_key(|&i| {
        let o = list[i];
        (o.gdi.unwrap_or(u32::MAX), o.adapter, o.source, o.x, o.y, i)
    });
    let mut out = vec![0; list.len()];
    for (rank, i) in idx.into_iter().enumerate() {
        out[i] = rank as u32 + 1;
    }
    out
}

/// "\\?\DISPLAY#GSM5B09#5&2a1b3c4d&0&UID4352#{guid}" ->
/// "DISPLAY\GSM5B09\5&2a1b3c4d&0&UID4352" (the PnP instance under Enum).
/// None for anything that is not that shape (never a path elsewhere).
pub fn instance_of(device_path: &str) -> Option<String> {
    let rest = device_path.strip_prefix(r"\\?\")?;
    let parts: Vec<&str> = rest.split('#').collect();
    if parts.len() < 4 || !parts[0].eq_ignore_ascii_case("DISPLAY") {
        return None;
    }
    let ok = |s: &str| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'&' | b'_' | b'-' | b'.')) && s != "." && s != "..";
    (ok(parts[1]) && ok(parts[2])).then(|| format!("DISPLAY\\{}\\{}", parts[1], parts[2]))
}

/// The monitor's serial from its EDID: the 0xFF descriptor's text, else
/// the number at bytes 12..16 (when it is not 0 or a placeholder).
pub fn edid_serial(e: &[u8]) -> Option<String> {
    if e.len() < 128 || e[..8] != [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0] {
        return None;
    }
    for d in [54usize, 72, 90, 108] {
        let b = &e[d..d + 18];
        if b[0] == 0 && b[1] == 0 && b[2] == 0 && b[3] == 0xFF {
            let text: String = b[5..].iter().take_while(|&&c| c != 0x0A && c != 0).map(|&c| c as char).collect();
            let clean = clean_serial(&text);
            if !clean.is_empty() {
                return Some(clean);
            }
        }
    }
    let n = u32::from_le_bytes([e[12], e[13], e[14], e[15]]);
    (n != 0 && n != 0x0101_0101 && n != u32::MAX).then(|| format!("{n:08X}"))
}

/// Letters, digits and '-' only (a JSON key people can read); placeholder
/// serials ("0", "000000") count as none.
fn clean_serial(s: &str) -> String {
    let c: String = s.trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(32).collect();
    if c.chars().all(|x| x == '0' || x == '-') {
        String::new()
    } else {
        c
    }
}

/// EDID maker letters from DisplayConfig's edidManufactureId (the EDID's
/// big-endian bytes read as a little-endian u16): "GSM".
pub fn edid_maker(id: u16) -> String {
    let v = id.swap_bytes();
    [(v >> 10) & 31, (v >> 5) & 31, v & 31].iter().map(|&c| if (1..=26).contains(&c) { (b'A' + c as u8 - 1) as char } else { '?' }).collect()
}

/// A connector's short name: 8 hex digits of `id` (a PnP instance).
pub fn port_of(id: &str) -> String {
    format!("{:08x}", crate::icons::fnv64(id.to_ascii_uppercase().as_bytes()) as u32)
}

/// Who a display is.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Ident {
    pub maker: u16,
    pub product: u16,
    pub serial: Option<String>,
    /// Its size in pixels (the fallback).
    pub w: i32,
    pub h: i32,
    /// Its connector (port_of; None: not known).
    pub port: Option<String>,
}

impl Ident {
    /// The monitor itself: with a serial, wherever it is plugged in.
    fn serial_key(&self) -> Option<String> {
        if self.maker == 0 && self.product == 0 {
            return None;
        }
        let s = self.serial.as_ref().filter(|s| !s.is_empty())?;
        Some(format!("{}{:04X}-{s}", edid_maker(self.maker), self.product))
    }

    /// The key alone (unique_keys adds the connector to serial twins).
    pub fn key(&self) -> String {
        if let Some(k) = self.serial_key() {
            return k;
        }
        let base = if self.maker == 0 && self.product == 0 { format!("res-{}x{}", self.w.max(0), self.h.max(0)) } else { format!("{}{:04X}", edid_maker(self.maker), self.product) };
        match &self.port {
            Some(p) => format!("{base}@{p}"),
            None => base,
        }
    }
}

/// Every display's key: Ident::key, and for two monitors claiming the same
/// serial (both connected now) the connector added to each. Nothing here
/// depends on display numbers or on which other displays are there, so a
/// display's key does not change when another one comes or goes.
pub fn unique_keys(ids: &[Ident]) -> Vec<String> {
    let keys: Vec<String> = ids.iter().map(Ident::key).collect();
    keys.iter()
        .zip(ids)
        .map(|(k, id)| match (&id.port, keys.iter().filter(|x| *x == k).count() > 1) {
            (Some(p), true) if !k.contains('@') => format!("{k}@{p}"),
            _ => k.clone(),
        })
        .collect()
}

// ---- Identify ------------------------------------------------------------------

/// How long the numbers stay up.
pub const IDENTIFY_MS: u32 = 3000;

/// Shows each display's number big in its middle for IDENTIFY_MS (a
/// topmost window that clicks go through and that never takes the focus;
/// high contrast draws it in the system colors, and it fades only when
/// Windows' animations are on). Asked again while up: the time starts over.
pub fn identify() {
    identify::show();
}

mod identify {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use windows::core::w;
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GetDC, ReleaseDC, SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER,
        ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE,
        FW_SEMIBOLD, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer, RegisterClassExW, SetTimer, ShowWindow, TranslateMessage, UpdateLayeredWindow, MSG,
        SW_SHOWNOACTIVATE, ULW_ALPHA, WM_NCHITTEST, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };

    /// The numbers are up (one thread draws them).
    static RUNNING: AtomicBool = AtomicBool::new(false);
    /// Monotonic ms when they go.
    static UNTIL: AtomicU64 = AtomicU64::new(0);
    const STEP_MS: u32 = 16;
    const FADE_MS: u64 = 180;
    const TIMER: usize = 1;

    fn now_ms() -> u64 {
        static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
    }

    pub fn show() {
        UNTIL.store(now_ms() + super::IDENTIFY_MS as u64, Ordering::SeqCst);
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        let spawned = std::thread::Builder::new().name("identify".into()).spawn(|| {
            unsafe { run() };
            RUNNING.store(false, Ordering::SeqCst);
        });
        if spawned.is_err() {
            RUNNING.store(false, Ordering::SeqCst);
        }
    }

    unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_NCHITTEST {
            // HTTRANSPARENT: clicks go to what is under it.
            return LRESULT(-1);
        }
        DefWindowProcW(h, msg, wp, lp)
    }

    /// One display's picture: premultiplied BGRA, `size` px square.
    struct Picture {
        hwnd: HWND,
        at: POINT,
        size: i32,
        dc: windows::Win32::Graphics::Gdi::HDC,
        bmp: windows::Win32::Graphics::Gdi::HBITMAP,
        old: HGDIOBJ,
    }

    unsafe fn run() {
        let look = crate::syslook::current();
        // (Windows read here too: the settings window may show them, without
        // the dock's effects: Windows' animations off is reduced motion.)
        let mode = if look.animations { crate::motion::now() } else { crate::motion::Mode::Reduced };
        // The panels' look: Graphite as the system shows it (light, dark,
        // high contrast).
        let theme = crate::theme::Theme::graphite().resolve(&look);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: w!("NopBarIdentify"), ..Default::default() };
        RegisterClassExW(&wc);
        let mut pics: Vec<Picture> = Vec::new();
        for d in super::current() {
            if let Some(p) = picture(&d, &theme, hinst.into()) {
                pics.push(p);
            }
        }
        if pics.is_empty() {
            return;
        }
        let start = now_ms();
        // As every fade is played (crate::motion::fade_plan: at the chosen
        // speed; 60 ms with reduced motion, the speed not stretching it).
        let fade_ms = fade_ms(mode, crate::effects::current().speed);
        let alpha = |t: u64| alpha_at(t, start, UNTIL.load(Ordering::SeqCst), fade_ms);
        for p in &pics {
            present(p, alpha(now_ms()));
            let _ = ShowWindow(p.hwnd, SW_SHOWNOACTIVATE);
        }
        let timer_hwnd = pics[0].hwnd;
        SetTimer(Some(timer_hwnd), TIMER, next_step(now_ms(), start, UNTIL.load(Ordering::SeqCst), fade_ms), None);
        let mut msg = MSG::default();
        loop {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            let t = now_ms();
            let until = UNTIL.load(Ordering::SeqCst);
            if t >= until {
                break;
            }
            if fade_ms > 0 {
                let a = alpha(t);
                for p in &pics {
                    present(p, a);
                }
            }
            // Frames only while fading; the hold in between waits it out
            // (polish audit F23). (A show() meanwhile moves `until`: the
            // wake-up finds the longer hold and waits again.)
            SetTimer(Some(timer_hwnd), TIMER, next_step(t, start, until, fade_ms), None);
        }
        let _ = KillTimer(Some(timer_hwnd), TIMER);
        for p in pics {
            SelectObject(p.dc, p.old);
            let _ = DeleteObject(p.bmp.into());
            let _ = DeleteDC(p.dc);
            let _ = DestroyWindow(p.hwnd);
        }
    }

    /// Pure: how long the numbers fade in and out in `mode` at animation
    /// speed `speed` (ms of real time).
    pub(super) fn fade_ms(mode: crate::motion::Mode, speed: f32) -> u64 {
        crate::motion::fade_plan(mode, FADE_MS as i64, crate::motion::EASE_OUT, speed).0.max(0) as u64
    }

    /// Pure: the card's colors from `theme` (design system 2.1, 12.3): the
    /// panels' surface (M2: its tint at its opacity), the number in the
    /// primary text color laid over it; high contrast: Window, WindowText
    /// and its outline (theme::outline_of: 1 DIP WindowText).
    pub(super) fn colors(theme: &crate::theme::Theme) -> (crate::theme::Rgba, crate::theme::Rgba, Option<(crate::theme::Rgba, f32)>) {
        use crate::theme::{Level, Role, State};
        let surface = match theme.hc {
            Some(h) => h.window,
            None => theme.popup_tint(),
        };
        let opaque = crate::theme::Rgba { a: 255, ..surface };
        let number = crate::theme::over(theme.text_of(Level::Primary, State::Rest), opaque);
        (surface, number, theme.outline_of(Role::Raised, State::Rest))
    }

    /// Pure: the numbers' opacity at `t` (ms) for a show from `start` to
    /// `until`, fading in and out over `fade` ms (0: no fade).
    pub(super) fn alpha_at(t: u64, start: u64, until: u64, fade: u64) -> u8 {
        if fade == 0 {
            return 255;
        }
        let a_in = (t.saturating_sub(start) as f32 / fade as f32).min(1.0);
        let a_out = (until.saturating_sub(t) as f32 / fade as f32).min(1.0);
        (a_in.min(a_out).clamp(0.0, 1.0) * 255.0) as u8
    }

    /// Pure: ms from `t` to the next change of the picture: a frame
    /// (STEP_MS) while fading, else the hold's end (or the show's).
    pub(super) fn next_step(t: u64, start: u64, until: u64, fade: u64) -> u32 {
        let hold_end = until.saturating_sub(fade);
        if fade > 0 && (t < start + fade || t >= hold_end) {
            return STEP_MS;
        }
        hold_end.saturating_sub(t).clamp(1, u32::MAX as u64) as u32
    }

    unsafe fn present(p: &Picture, alpha: u8) {
        let size = SIZE { cx: p.size, cy: p.size };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
        let _ = UpdateLayeredWindow(p.hwnd, None, Some(&p.at), Some(&size), Some(p.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
    }

    unsafe fn picture(d: &super::Display, theme: &crate::theme::Theme, hinst: windows::Win32::Foundation::HINSTANCE) -> Option<Picture> {
        let scale = crate::monitors::info(d.handle).map(|m| m.scale()).unwrap_or(1.0);
        let size = (200.0 * scale).round() as i32;
        let cx = (d.rect.left + d.rect.right) / 2;
        let cy = (d.rect.top + d.rect.bottom) / 2;
        let at = POINT { x: cx - size / 2, y: cy - size / 2 };
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("NopBarIdentify"),
            w!(""),
            WS_POPUP,
            at.x,
            at.y,
            size,
            size,
            None,
            None,
            Some(hinst),
            None,
        )
        .ok()?;
        // Colors: the card and the number, the theme's.
        let (bg, fg, edge) = colors(theme);
        let bg_a = bg.a;
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: size, biHeight: -size, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            let _ = DestroyWindow(hwnd);
            return None;
        };
        let old = SelectObject(dc, bmp.into());
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        // The number as a mask: white on black, greyscale antialiased.
        px.fill(0);
        let font = CreateFontW(
            -((128.0 * scale) as i32),
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            FF_DONTCARE.0 as u32,
            &windows::core::HSTRING::from(theme.font.family.as_str()),
        );
        let old_font = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        let mut text: Vec<u16> = d.number.to_string().encode_utf16().collect();
        let mut r = windows::Win32::Foundation::RECT { left: 0, top: 0, right: size, bottom: size };
        DrawTextW(dc, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(dc, old_font);
        let _ = DeleteObject(font.into());
        // The card under it (a panel's corners), the number over it,
        // premultiplied; its outline inside its edge in high contrast.
        let radius = theme.radii().panel * scale;
        let border = edge.map_or(0.0, |(_, w)| w) * scale;
        let s = size as f32;
        for y in 0..size {
            for x in 0..size {
                let i = (y * size + x) as usize;
                let mask = (px[i] & 0xFF) as f32 / 255.0;
                let inside = rounded_coverage(x as f32 + 0.5, y as f32 + 0.5, s, s, radius);
                let ring = edge.map(|_| inside - rounded_coverage_inset(x as f32 + 0.5, y as f32 + 0.5, s, s, radius, border)).unwrap_or(0.0).max(0.0);
                let card_a = inside * bg_a as f32 / 255.0;
                // Over the card: the border (high contrast), then the number.
                let mut c = [bg.r as f32, bg.g as f32, bg.b as f32];
                if let Some((e, _)) = edge {
                    c = mix(c, [e.r as f32, e.g as f32, e.b as f32], ring);
                }
                c = mix(c, [fg.r as f32, fg.g as f32, fg.b as f32], mask);
                let a = (card_a + mask * (1.0 - card_a)).clamp(0.0, 1.0);
                let pm = |v: f32| (v * a).round().clamp(0.0, 255.0) as u32;
                px[i] = ((a * 255.0).round() as u32) << 24 | pm(c[0]) << 16 | pm(c[1]) << 8 | pm(c[2]);
            }
        }
        Some(Picture { hwnd, at, size, dc, bmp, old })
    }

    fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
    }

    /// How much of the pixel centered at (x, y) is inside a w x h box
    /// with corners of radius r.
    fn rounded_coverage(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
        let qx = (r - x).max(x - (w - r)).max(0.0);
        let qy = (r - y).max(y - (h - r)).max(0.0);
        if qx > 0.0 && qy > 0.0 {
            // In a corner: by the distance to the arc.
            return (r - (qx * qx + qy * qy).sqrt() + 0.5).clamp(0.0, 1.0);
        }
        (x.min(y).min(w - x).min(h - y) + 0.5).clamp(0.0, 1.0)
    }

    fn rounded_coverage_inset(x: f32, y: f32, w: f32, h: f32, r: f32, inset: f32) -> f32 {
        rounded_coverage(x - inset, y - inset, w - 2.0 * inset, h - 2.0 * inset, (r - inset).max(0.0))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn card_shape_is_round_at_the_corners() {
            assert_eq!(rounded_coverage(100.0, 100.0, 200.0, 200.0, 24.0), 1.0);
            assert_eq!(rounded_coverage(0.5, 0.5, 200.0, 200.0, 24.0), 0.0, "the corner is cut");
            assert_eq!(rounded_coverage(100.0, 0.5, 200.0, 200.0, 24.0), 1.0, "the edge row is inside");
            assert_eq!(rounded_coverage(100.0, -0.5, 200.0, 200.0, 24.0), 0.0, "outside");
            assert!((rounded_coverage(24.0 - 16.9706, 24.0 - 16.9706, 200.0, 200.0, 24.0) - 0.5).abs() < 0.01, "on the arc: half");
            assert_eq!(rounded_coverage_inset(100.0, 1.0, 200.0, 200.0, 24.0, 3.0), 0.0, "inside the border ring's outer part");
        }

        #[test]
        fn the_card_is_the_themes() {
            use crate::syslook::{HighContrast, SysLook};
            use crate::theme::{Rgba, Theme};
            // Light and dark: the panels' surface, the number over it in
            // the primary text color; no outline.
            for light in [true, false] {
                let t = Theme::graphite().resolve(&SysLook { light, ..Default::default() });
                let (bg, fg, edge) = colors(&t);
                assert_eq!(bg, t.popup_tint());
                assert_eq!(fg.a, 255);
                assert!(edge.is_none());
                let lum = |c: Rgba| c.r as u32 + c.g as u32 + c.b as u32;
                assert_eq!(lum(fg) > lum(bg), !light, "dark number on light, light on dark");
            }
            // High contrast: Window, WindowText and its 1 DIP outline.
            let (k, w, sky) = (Rgba { r: 0, g: 0, b: 0, a: 255 }, Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 26, g: 235, b: 255, a: 255 });
            let t = Theme::graphite().resolve(&SysLook { high_contrast: Some(HighContrast { window: k, text: w, highlight: sky, highlight_text: k, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() });
            assert_eq!(colors(&t), (k, w, Some((w, 1.0))));
            // Reduced motion: 60 ms straight, whatever the speed.
            assert_eq!(fade_ms(crate::motion::Mode::Reduced, 0.25), crate::motion::REDUCED_MS as u64);
            assert_eq!(fade_ms(crate::motion::Mode::Full, 1.0), FADE_MS);
        }

        #[test]
        fn the_numbers_fade_in_hold_and_fade_out() {
            let (start, until, fade) = (1000, 4000, 180);
            assert_eq!(alpha_at(start, start, until, fade), 0);
            assert_eq!(alpha_at(start + 90, start, until, fade), 127);
            assert_eq!(alpha_at(2500, start, until, fade), 255);
            assert_eq!(alpha_at(until - 90, start, until, fade), 127);
            assert_eq!(alpha_at(until, start, until, fade), 0);
            // No fade: shown fully from start to end.
            assert_eq!(alpha_at(start, start, until, 0), 255);
            // At half speed (effects::ms) the fade takes twice as long.
            let slow = crate::effects::ms_at(FADE_MS as i64, 0.5) as u64;
            assert_eq!(alpha_at(start + 180, start, until, slow), 127);
        }

        #[test]
        fn the_hold_waits_instead_of_drawing_frames() {
            let (start, until, fade) = (1000, 4000, 180);
            assert_eq!(next_step(start, start, until, fade), STEP_MS, "fading in");
            // Faded in: one wait to the start of the fade out.
            assert_eq!(next_step(start + 180, start, until, fade), 4000 - 180 - 1180);
            assert_eq!(next_step(until - 180, start, until, fade), STEP_MS, "fading out");
            // No fade: one wait to the end.
            assert_eq!(next_step(start, start, until, 0), 3000);
            // Never a zero-length timer.
            assert_eq!(next_step(until, start, until, 0), 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gdi_numbers() {
        assert_eq!(gdi_number(r"\\.\DISPLAY1"), Some(1));
        assert_eq!(gdi_number(r"\\.\DISPLAY12"), Some(12));
        assert_eq!(gdi_number(r"\\.\display3"), Some(3));
        assert_eq!(gdi_number(r"\\.\DISPLAY"), None);
        assert_eq!(gdi_number(r"\\.\DISPLAY0"), None);
        assert_eq!(gdi_number(r"\\.\DISPLAYx"), None);
        assert_eq!(gdi_number(""), None);
    }

    fn o(gdi: Option<u32>, x: i32) -> Order {
        Order { gdi, adapter: 1, source: gdi.unwrap_or(9), x, y: 0 }
    }

    #[test]
    fn numbers_follow_the_gdi_numbers_without_gaps() {
        // DISPLAY1 main on the right, DISPLAY5 (hot-plugged) on the left.
        assert_eq!(numbers(&[o(Some(1), 1920), o(Some(5), 0)]), vec![1, 2]);
        assert_eq!(numbers(&[o(Some(5), 0), o(Some(1), 1920), o(Some(3), -1920)]), vec![3, 1, 2]);
        // Not named by DisplayConfig: after the named ones, by place.
        assert_eq!(numbers(&[o(None, 500), o(Some(2), 0), o(None, -500)]), vec![3, 1, 2]);
        // Two adapters with the same source ids: by adapter.
        let a = Order { gdi: Some(2), adapter: 7, source: 0, x: 0, y: 0 };
        let b = Order { gdi: Some(2), adapter: 3, source: 0, x: 0, y: 0 };
        assert_eq!(numbers(&[a, b]), vec![2, 1]);
        assert!(numbers(&[]).is_empty());
    }

    #[test]
    fn numbers_are_stable_whatever_the_listing_order() {
        let list = [o(Some(2), 0), o(Some(1), 1920), o(Some(3), 3840)];
        let n = numbers(&list);
        let mut rev = list;
        rev.reverse();
        let mut nr = numbers(&rev);
        nr.reverse();
        assert_eq!(n, nr);
        assert_eq!(n, vec![2, 1, 3]);
    }

    #[test]
    fn instance_paths() {
        let p = r"\\?\DISPLAY#GSM5B09#5&2a1b3c4d&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        assert_eq!(instance_of(p).as_deref(), Some(r"DISPLAY\GSM5B09\5&2a1b3c4d&0&UID4352"));
        assert_eq!(instance_of(r"\\?\USB#VID_1#2#{x}"), None, "only monitors");
        assert_eq!(instance_of(r"\\?\DISPLAY#..#x#{g}"), None);
        assert_eq!(instance_of(r"\\?\DISPLAY#A\B#x#{g}"), None, "no other registry keys");
        assert_eq!(instance_of("DISPLAY#A#B#{g}"), None);
        assert_eq!(instance_of(""), None);
    }

    fn edid(serial_num: u32, text: Option<&str>) -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        e[12..16].copy_from_slice(&serial_num.to_le_bytes());
        if let Some(t) = text {
            let d = 72;
            e[d + 3] = 0xFF;
            for (i, b) in t.bytes().take(13).enumerate() {
                e[d + 5 + i] = b;
            }
            if t.len() < 13 {
                e[d + 5 + t.len()] = 0x0A;
            }
        }
        e
    }

    #[test]
    fn serials_from_the_edid() {
        assert_eq!(edid_serial(&edid(0x1234, Some("104NTAB12345"))).as_deref(), Some("104NTAB12345"), "the text first");
        assert_eq!(edid_serial(&edid(0x00ABCDEF, None)).as_deref(), Some("00ABCDEF"));
        assert_eq!(edid_serial(&edid(0, None)), None);
        assert_eq!(edid_serial(&edid(0x0101_0101, None)), None, "a placeholder");
        assert_eq!(edid_serial(&edid(0, Some("0000000"))), None, "zeros are none");
        assert_eq!(edid_serial(&edid(0, Some(" AB/12 "))).as_deref(), Some("AB12"), "readable characters only");
        assert_eq!(edid_serial(&[0u8; 64]), None, "too short");
        let mut bad = edid(5, None);
        bad[0] = 1;
        assert_eq!(edid_serial(&bad), None, "not an EDID");
    }

    fn gsm() -> u16 {
        ((7u16 << 10) | (19 << 5) | 13).swap_bytes()
    }

    #[test]
    fn keys_name_the_monitor() {
        let with = Ident { maker: gsm(), product: 0x5B09, serial: Some("ABC1".into()), w: 2560, h: 1440, port: Some("0000aaaa".into()) };
        assert_eq!(with.key(), "GSM5B09-ABC1", "a serial: on any port");
        let without = Ident { serial: None, ..with.clone() };
        assert_eq!(without.key(), "GSM5B09@0000aaaa", "no serial: the model on its connector");
        let none = Ident { maker: 0, product: 0, serial: None, w: 1920, h: 1080, port: Some("0000bbbb".into()) };
        assert_eq!(none.key(), "res-1920x1080@0000bbbb");
        assert_eq!(Ident { port: None, ..none }.key(), "res-1920x1080");
        // The same monitor at another resolution is still itself.
        assert_eq!(Ident { w: 1920, h: 1080, ..with.clone() }.key(), with.key());
        assert_eq!(edid_maker(gsm()), "GSM");
        // Connectors: short, stable, case does not matter.
        assert_eq!(port_of(r"DISPLAY\DEL40F2\5&1&0&UID1"), port_of(r"display\del40f2\5&1&0&uid1"));
        assert_ne!(port_of(r"DISPLAY\DEL40F2\5&1&0&UID1"), port_of(r"DISPLAY\DEL40F2\5&1&0&UID2"));
        assert_eq!(port_of("x").len(), 8);
    }

    fn twin(port: &str, serial: Option<&str>) -> Ident {
        Ident { maker: gsm(), product: 0x40F2, serial: serial.map(String::from), w: 1920, h: 1080, port: Some(port.into()) }
    }

    #[test]
    fn twins_keep_their_own_keys_whoever_else_is_there() {
        // Two of one model without serials: each by its connector.
        let (a, b) = (twin("0000000a", None), twin("0000000b", None));
        let both = unique_keys(&[a.clone(), b.clone()]);
        assert_ne!(both[0], both[1]);
        // A unplugged: B keeps its key (it never takes A's settings).
        assert_eq!(unique_keys(&[b.clone()]), vec![both[1].clone()]);
        assert_eq!(unique_keys(&[a.clone()]), vec![both[0].clone()]);
        // Listed the other way round: the same keys.
        assert_eq!(unique_keys(&[b, a]), vec![both[1].clone(), both[0].clone()]);
        // A real serial: the monitor itself, on any port.
        let s1 = twin("0000000a", Some("S1"));
        let s1_elsewhere = twin("0000000c", Some("S1"));
        assert_eq!(unique_keys(&[s1.clone()]), unique_keys(&[s1_elsewhere]));
        // Two claiming one serial: told apart while both are there.
        let s2 = twin("0000000b", Some("S1"));
        let k = unique_keys(&[s1, s2]);
        assert_ne!(k[0], k[1]);
        assert!(k.iter().all(|x| x.contains('@')));
    }

    #[test]
    fn current_reads_without_touching_anything() {
        let list = current();
        assert!(!list.is_empty());
        assert!(list[0].primary);
        let mut nums: Vec<u32> = list.iter().map(|d| d.number).collect();
        nums.sort();
        assert_eq!(nums, (1..=list.len() as u32).collect::<Vec<_>>(), "1..n, no gaps");
        let mut keys: Vec<&str> = list.iter().map(|d| d.key.as_str()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), list.len(), "every key once");
        // Read twice: the same answers.
        let again = current();
        assert_eq!(list.iter().map(|d| (&d.key, d.number)).collect::<Vec<_>>(), again.iter().map(|d| (&d.key, d.number)).collect::<Vec<_>>());
    }
}
