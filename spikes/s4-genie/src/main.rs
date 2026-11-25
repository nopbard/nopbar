//! S4 spike: building blocks for a genie minimize effect.
//!
//!   s4-genie target                 striped red/white 900x600 window (own process)
//!   s4-genie capture [class]        WGC first-frame latency x10 (+ minimized case)
//!   s4-genie transitions [class]    DWMWA_TRANSITIONS_FORCEDISABLED on a foreign window
//!   s4-genie genie [class]          capture -> strip overlay -> minimize (no OS anim) -> animate
//!
//! The genie is drawn as N horizontal strips of the captured window, each a
//! SpriteVisual whose offset/scale are keyframe-animated by the compositor, so
//! the animation runs in DWM and costs this process ~nothing per frame.
//! Log: stdout and %TEMP%\s4-genie.log

use std::io::Write;
use std::time::{Duration, Instant};

use windows::core::{w, Interface, BOOL, HSTRING, PCWSTR};
use windows::Foundation::{Size, TimeSpan};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::UI::Composition::{CompositionStretch, Compositor};
use windows::Win32::Foundation::{COLORREF, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_TRANSITIONS_FORCEDISABLED,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetMonitorInfoW, MonitorFromWindow,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop, ICompositorInterop};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
    RO_INIT_SINGLETHREADED,
};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Vector2, Vector3};

const STRIPS: usize = 40;
const GENIE_MS: i64 = 450;

fn log(line: &str) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let line = format!("[{}.{:03}] {}", t.as_secs() % 100_000, t.subsec_millis(), line);
    println!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("s4-genie.log")) {
        let _ = writeln!(f, "{line}");
    }
}

// ---------- target window ----------

unsafe extern "system" fn target_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(h, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(h, &mut rc);
            let band = 40;
            let mut y = 0;
            let mut i = 0;
            while y < rc.bottom {
                let color = if i % 2 == 0 { COLORREF(0x0000_20E8) } else { COLORREF(0x00F0_F0F0) };
                let b = CreateSolidBrush(color);
                let r = RECT { left: 0, top: y, right: rc.right, bottom: (y + band).min(rc.bottom) };
                FillRect(dc, &r, b);
                let _ = DeleteObject(b.into());
                y += band;
                i += 1;
            }
            let _ = EndPaint(h, &ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

unsafe fn register(class: PCWSTR, proc_: WNDPROC) -> windows::core::Result<HMODULE> {
    let hinst = GetModuleHandleW(None)?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: proc_,
        hInstance: hinst.into(),
        lpszClassName: class,
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    Ok(hinst)
}

fn pump_until(deadline: Option<Instant>) {
    unsafe {
        let mut msg = MSG::default();
        loop {
            if let Some(d) = deadline {
                if Instant::now() >= d {
                    return;
                }
                if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                } else {
                    std::thread::sleep(Duration::from_millis(1));
                }
            } else {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    return;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

// ---------- capture ----------

struct Gpu {
    d3d: ID3D11Device,
    ctx: ID3D11DeviceContext,
    winrt: IDirect3DDevice,
}

fn gpu() -> windows::core::Result<Gpu> {
    unsafe {
        let mut d3d = None;
        let mut ctx = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            Some(&mut ctx),
        )?;
        let d3d = d3d.unwrap();
        let dxgi: IDXGIDevice = d3d.cast()?;
        let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
        Ok(Gpu { d3d, ctx: ctx.unwrap(), winrt })
    }
}

/// One frame of `hwnd` via Windows.Graphics.Capture. Returns the texture and
/// how long the first frame took.
fn capture_once(g: &Gpu, hwnd: HWND) -> windows::core::Result<(ID3D11Texture2D, Duration)> {
    unsafe {
        let t0 = Instant::now();
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = interop.CreateForWindow(hwnd)?;
        let size = item.Size()?;
        let pool =
            Direct3D11CaptureFramePool::CreateFreeThreaded(&g.winrt, DirectXPixelFormat::B8G8R8A8UIntNormalized, 1, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);
        session.StartCapture()?;
        let deadline = t0 + Duration::from_millis(1000);
        let frame = loop {
            if let Ok(f) = pool.TryGetNextFrame() {
                break f;
            }
            if Instant::now() > deadline {
                let _ = session.Close();
                return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
            }
            std::thread::sleep(Duration::from_micros(500));
        };
        let elapsed = t0.elapsed();
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let tex: ID3D11Texture2D = access.GetInterface()?;
        let _ = frame.Close();
        let _ = session.Close();
        let _ = pool.Close();
        Ok((tex, elapsed))
    }
}

fn find_target(args: &[String]) -> windows::core::Result<HWND> {
    let class = args.get(2).cloned().unwrap_or_else(|| "S4Target".into());
    unsafe { FindWindowW(&HSTRING::from(class), PCWSTR::null()) }
}

fn frame_bounds(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32);
    }
    r
}

// ---------- genie ----------

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Strip i (v = 0 top .. 1 bottom) at progress p: (x, y, scale_x, scale_y).
/// Phase 1 (squeeze) pinches the bottom toward the icon first; phase 2
/// (slide) pulls every strip down into the icon.
fn strip_at(p: f32, v: f32, win: (f32, f32, f32, f32), icon: (f32, f32, f32)) -> (f32, f32, f32, f32) {
    let (wx, wy, ww, wh) = win;
    let (tx, ty, isz) = icon;
    let sh = wh / STRIPS as f32;
    let squeeze = ease(p * 1.6 - (1.0 - v) * 0.6);
    let slide = ease((p - 0.25) / 0.75);
    let width = ww + (isz - ww) * squeeze;
    let cx = (wx + ww / 2.0) + (tx - (wx + ww / 2.0)) * squeeze;
    let y0 = wy + v * wh;
    let y1 = (ty - isz / 2.0) + v * isz;
    let y = y0 + (y1 - y0) * slide;
    let sy = 1.0 + ((isz / wh) - 1.0) * slide;
    let _ = sh;
    (cx - width / 2.0, y, width / ww, sy)
}

unsafe extern "system" fn overlay_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(h, msg, wp, lp)
}

fn run_genie(args: &[String]) -> windows::core::Result<()> {
    unsafe {
        RoInitialize(RO_INIT_SINGLETHREADED)?;
        let _queue = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?;
        let target = find_target(args)?;
        let g = gpu()?;
        let t_total = Instant::now();
        let (tex, cap) = capture_once(&g, target)?;
        let fb = frame_bounds(target);
        let (ww, wh) = ((fb.right - fb.left) as f32, (fb.bottom - fb.top) as f32);

        // Monitor-sized click-through overlay.
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromWindow(target, MONITOR_DEFAULTTONEAREST), &mut mi);
        let m = mi.rcMonitor;
        let hinst = register(w!("S4Overlay"), Some(overlay_proc))?;
        let overlay = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED,
            w!("S4Overlay"),
            PCWSTR::null(),
            WS_POPUP,
            m.left,
            m.top,
            m.right - m.left,
            m.bottom - m.top,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        SetLayeredWindowAttributes(overlay, COLORREF(0), 255, LWA_ALPHA)?;

        let c = Compositor::new()?;
        let cdi: ICompositorDesktopInterop = c.cast()?;
        let ci: ICompositorInterop = c.cast()?;
        let gdev = ci.CreateGraphicsDevice(&g.d3d)?;
        let surface = gdev.CreateDrawingSurface(
            Size { Width: ww, Height: wh },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        let si: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut off = POINT::default();
        let dst: ID3D11Texture2D = si.BeginDraw(None, &mut off)?;
        g.ctx.CopySubresourceRegion(&dst, 0, off.x as u32, off.y as u32, 0, &tex, 0, None);
        si.EndDraw()?;

        let target_wt = cdi.CreateDesktopWindowTarget(overlay, true)?;
        let root = c.CreateContainerVisual()?;
        target_wt.SetRoot(&root)?;
        let lin = c.CreateLinearEasingFunction()?;
        let win = ((fb.left - m.left) as f32, (fb.top - m.top) as f32, ww, wh);
        let icon = (((m.right - m.left) / 2) as f32, (m.bottom - m.top) as f32 - 110.0, 100.0);
        let sh = wh / STRIPS as f32;
        let dur = TimeSpan { Duration: GENIE_MS * 10_000 };
        let mut sprites = Vec::new();
        for i in 0..STRIPS {
            let v = i as f32 / STRIPS as f32;
            let brush = c.CreateSurfaceBrushWithSurface(&surface)?;
            brush.SetStretch(CompositionStretch::None)?;
            brush.SetHorizontalAlignmentRatio(0.0)?;
            brush.SetVerticalAlignmentRatio(0.0)?;
            brush.SetOffset(Vector2 { X: 0.0, Y: -(i as f32) * sh })?;
            let s = c.CreateSpriteVisual()?;
            s.SetBrush(&brush)?;
            s.SetSize(Vector2 { X: ww, Y: sh + 1.0 })?;
            let (x, y, sx, sy) = strip_at(0.0, v, win, icon);
            s.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
            s.SetScale(Vector3 { X: sx, Y: sy, Z: 1.0 })?;
            let off_anim = c.CreateVector3KeyFrameAnimation()?;
            let scale_anim = c.CreateVector3KeyFrameAnimation()?;
            for k in 0..=12 {
                let p = k as f32 / 12.0;
                let (x, y, sx, sy) = strip_at(p, v, win, icon);
                off_anim.InsertKeyFrameWithEasingFunction(p, Vector3 { X: x, Y: y, Z: 0.0 }, &lin)?;
                scale_anim.InsertKeyFrameWithEasingFunction(p, Vector3 { X: sx, Y: sy, Z: 1.0 }, &lin)?;
            }
            off_anim.SetDuration(dur)?;
            scale_anim.SetDuration(dur)?;
            root.Children()?.InsertAtTop(&s)?;
            sprites.push((s, off_anim, scale_anim));
        }
        let _ = ShowWindow(overlay, SW_SHOWNOACTIVATE);
        // Let the compositor commit and DWM present the overlay before the
        // real window goes away, so there is no empty frame in between.
        let t_show = Instant::now();
        for _ in 0..3 {
            pump_until(Some(Instant::now() + Duration::from_millis(2)));
            let _ = windows::Win32::Graphics::Dwm::DwmFlush();
        }
        log(&format!("overlay presented after {:?}", t_show.elapsed()));

        // Hide the real window without the OS animation, then start.
        let off_attr = BOOL(1);
        let hr = DwmSetWindowAttribute(target, DWMWA_TRANSITIONS_FORCEDISABLED, &off_attr as *const _ as *const _, 4);
        let _ = ShowWindow(target, SW_MINIMIZE);
        let t_anim = Instant::now();
        for (s, oa, sa) in &sprites {
            s.StartAnimation(&HSTRING::from("Offset"), oa)?;
            s.StartAnimation(&HSTRING::from("Scale"), sa)?;
        }
        log(&format!(
            "genie: capture {:?}, setup->start {:?}, transitions_off {:?}, strips {STRIPS}, window {}x{}",
            cap,
            t_total.elapsed(),
            hr.map(|_| "ok"),
            ww,
            wh
        ));
        let cpu0 = cpu_ms();
        pump_until(Some(t_anim + Duration::from_millis(GENIE_MS as u64 + 150)));
        log(&format!("genie: process CPU during animation {:.1} ms", cpu_ms() - cpu0));
        let on = BOOL(0);
        let _ = DwmSetWindowAttribute(target, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const _ as *const _, 4);
        let _ = DestroyWindow(overlay);
    }
    Ok(())
}

fn cpu_ms() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut a, mut b, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u);
    }
    let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 10_000.0;
    f(k) + f(u)
}

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        match args.get(1).map(String::as_str) {
            Some("target") => {
                let hinst = register(w!("S4Target"), Some(target_proc))?;
                let h = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("S4Target"),
                    w!("S4 target"),
                    WS_OVERLAPPEDWINDOW,
                    200,
                    200,
                    900,
                    600,
                    None,
                    None,
                    Some(hinst.into()),
                    None,
                )?;
                let _ = ShowWindow(h, SW_SHOWNORMAL);
                log(&format!("target window {:#x}", h.0 as isize));
                pump_until(None);
            }
            Some("capture") => {
                RoInitialize(RO_INIT_SINGLETHREADED)?;
                let h = find_target(&args)?;
                let g = gpu()?;
                let mut ms: Vec<f64> = Vec::new();
                for _ in 0..10 {
                    match capture_once(&g, h) {
                        Ok((_, d)) => ms.push(d.as_secs_f64() * 1000.0),
                        Err(e) => log(&format!("capture error {e:?}")),
                    }
                }
                ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
                log(&format!("capture first-frame ms (sorted): {:?}", ms.iter().map(|v| format!("{v:.1}")).collect::<Vec<_>>()));
                let _ = ShowWindow(h, SW_MINIMIZE);
                std::thread::sleep(Duration::from_millis(600));
                match capture_once(&g, h) {
                    Ok((_, d)) => log(&format!("capture while minimized: frame in {:.1} ms", d.as_secs_f64() * 1000.0)),
                    Err(e) => log(&format!("capture while minimized: no frame ({e:?})")),
                }
                let _ = ShowWindow(h, SW_RESTORE);
            }
            Some("transitions") => {
                let h = find_target(&args)?;
                let on = BOOL(1);
                let hr = DwmSetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const _ as *const _, 4);
                log(&format!("TRANSITIONS_FORCEDISABLED on foreign {:#x}: {:?}", h.0 as isize, hr));
            }
            Some("transitions-off") => {
                let h = find_target(&args)?;
                let off = BOOL(0);
                let hr = DwmSetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as *const _, 4);
                log(&format!("TRANSITIONS re-enabled on {:#x}: {:?}", h.0 as isize, hr));
            }
            Some("genie") => run_genie(&args)?,
            _ => println!("usage: s4-genie target | capture [class] | transitions [class] | transitions-off [class] | genie [class]"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: (f32, f32, f32, f32) = (100.0, 100.0, 900.0, 600.0);
    const ICON: (f32, f32, f32) = (1280.0, 1490.0, 100.0);

    #[test]
    fn starts_at_window() {
        for i in 0..STRIPS {
            let v = i as f32 / STRIPS as f32;
            let (x, y, sx, sy) = strip_at(0.0, v, WIN, ICON);
            assert!((x - 100.0).abs() < 0.01 && (sx - 1.0).abs() < 0.01 && (sy - 1.0).abs() < 0.01);
            assert!((y - (100.0 + v * 600.0)).abs() < 0.01);
        }
    }

    #[test]
    fn ends_inside_icon() {
        for i in 0..STRIPS {
            let v = i as f32 / STRIPS as f32;
            let (x, y, sx, _) = strip_at(1.0, v, WIN, ICON);
            let width = sx * 900.0;
            assert!((width - 100.0).abs() < 0.5, "width {width}");
            assert!((x + width / 2.0 - 1280.0).abs() < 0.5);
            assert!(y >= 1490.0 - 50.0 - 0.5 && y <= 1490.0 + 50.0 + 0.5);
        }
    }

    #[test]
    fn bottom_pinches_before_top() {
        let (_, _, top_sx, _) = strip_at(0.3, 0.0, WIN, ICON);
        let (_, _, bot_sx, _) = strip_at(0.3, 0.97, WIN, ICON);
        assert!(bot_sx < top_sx);
    }
}
