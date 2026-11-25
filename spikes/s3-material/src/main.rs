//! S3 spike: DWM GPU cost of the bar materials.
//!
//!   s3-material <none|solid|acrylic> [--animate] [--behind]
//!   --behind: a full-screen normal window under the bars with a big block
//!   sliding back and forth (what a video behind the bar would do)
//!
//! Opens a top bar (full width x 32 DIP) and a dock-sized window (720x80 DIP)
//! on every monitor, filled with the chosen material. --animate loops a
//! compositor animation on the tint so DWM recomposes every frame (worst
//! case: something moving inside the bar). Measure with measure-dwm-gpu.ps1.
//! A pre-blurred static image (planned "mica-static") costs DWM the same as a
//! textured quad, so `solid` stands in for it.

use windows::core::{w, Interface, BOOL, HSTRING, PCWSTR};
use windows::Foundation::TimeSpan;
use windows::UI::Color;
use windows::UI::Composition::Compositor;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::AnimationIterationBehavior;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
    RO_INIT_SINGLETHREADED,
};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Vector2;

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        PostQuitMessage(0);
    }
    DefWindowProcW(h, msg, wp, lp)
}

fn monitors() -> Vec<RECT> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<RECT>);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(m, &mut mi).as_bool() {
            out.push(mi.rcMonitor);
        }
        BOOL(1)
    }
    let mut v: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let material = args.get(1).cloned().unwrap_or_else(|| "acrylic".into());
    let animate = args.iter().any(|a| a == "--animate");
    let behind = args.iter().any(|a| a == "--behind");
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        RoInitialize(RO_INIT_SINGLETHREADED)?;
        let _queue = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?;
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("S3Material"),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let c = Compositor::new()?;
        let interop: ICompositorDesktopInterop = c.cast()?;
        let mut keep: Vec<DesktopWindowTarget> = Vec::new();
        let anim = c.CreateScalarKeyFrameAnimation()?;
        anim.InsertKeyFrame(0.0, 0.35)?;
        anim.InsertKeyFrame(0.5, 0.55)?;
        anim.InsertKeyFrame(1.0, 0.35)?;
        anim.SetDuration(TimeSpan { Duration: 10_000_000 })?;
        anim.SetIterationBehavior(AnimationIterationBehavior::Forever)?;

        if behind {
            let slide = c.CreateVector3KeyFrameAnimation()?;
            slide.InsertKeyFrame(0.0, windows_numerics::Vector3 { X: 0.0, Y: 0.0, Z: 0.0 })?;
            slide.InsertKeyFrame(0.5, windows_numerics::Vector3 { X: 1200.0, Y: 0.0, Z: 0.0 })?;
            slide.InsertKeyFrame(1.0, windows_numerics::Vector3 { X: 0.0, Y: 0.0, Z: 0.0 })?;
            slide.SetDuration(TimeSpan { Duration: 20_000_000 })?;
            slide.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
            for m in monitors() {
                let (bw, bh) = (m.right - m.left, m.bottom - m.top);
                let hwnd = CreateWindowExW(
                    WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("S3Material"),
                    PCWSTR::null(),
                    WS_POPUP,
                    m.left,
                    m.top,
                    bw,
                    bh,
                    None,
                    None,
                    Some(hinst.into()),
                    None,
                )?;
                let target = interop.CreateDesktopWindowTarget(hwnd, false)?;
                let root = c.CreateContainerVisual()?;
                target.SetRoot(&root)?;
                let bg = c.CreateSpriteVisual()?;
                bg.SetBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: 20, G: 60, B: 120 })?)?;
                bg.SetSize(Vector2 { X: bw as f32, Y: bh as f32 })?;
                root.Children()?.InsertAtTop(&bg)?;
                let block = c.CreateSpriteVisual()?;
                block.SetBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: 240, G: 200, B: 40 })?)?;
                block.SetSize(Vector2 { X: bw as f32 * 0.4, Y: bh as f32 })?;
                root.Children()?.InsertAtTop(&block)?;
                block.StartAnimation(&HSTRING::from("Offset"), &slide)?;
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                keep.push(target);
            }
        }

        for m in monitors().into_iter().filter(|_| material != "none") {
            // Scale fixed at 2.0 (test machine); fine for a cost probe.
            let s = 2.0f32;
            let mw = m.right - m.left;
            let dw = (720.0 * s) as i32;
            let bars = [
                (m.left, m.top, mw, (32.0 * s) as i32),
                (m.left + (mw - dw) / 2, m.bottom - (100.0 * s) as i32, dw, (80.0 * s) as i32),
            ];
            for (x, y, bw, bh) in bars {
                let hwnd = CreateWindowExW(
                    WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                    w!("S3Material"),
                    PCWSTR::null(),
                    WS_POPUP,
                    x,
                    y,
                    bw,
                    bh,
                    None,
                    None,
                    Some(hinst.into()),
                    None,
                )?;
                let enable = BOOL(1);
                let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &enable as *const _ as *const _, 4);
                let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
                let root = c.CreateContainerVisual()?;
                root.SetSize(Vector2 { X: bw as f32, Y: bh as f32 })?;
                target.SetRoot(&root)?;
                if material == "acrylic" {
                    let backdrop = c.CreateSpriteVisual()?;
                    backdrop.SetBrush(&c.CreateHostBackdropBrush()?)?;
                    backdrop.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
                    root.Children()?.InsertAtTop(&backdrop)?;
                }
                let tint = c.CreateSpriteVisual()?;
                let alpha = if material == "acrylic" { 110 } else { 235 };
                tint.SetBrush(&c.CreateColorBrushWithColor(Color { A: alpha, R: 30, G: 30, B: 34 })?)?;
                tint.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
                root.Children()?.InsertAtTop(&tint)?;
                if animate {
                    tint.StartAnimation(&HSTRING::from("Opacity"), &anim)?;
                }
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                keep.push(target);
            }
        }
        println!("s3-material {material} animate={animate}: {} windows", keep.len());
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
