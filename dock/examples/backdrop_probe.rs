//! Harmony 묶음 S, S3: HostBackdropBrush가 창이 비활성일 때 단색으로
//! 떨어지는가. 오프스크린으로는 답이 안 나와서(DWM이 합성한다) 주인이 직접
//! 봐야 한다. nopbar.exe가 아니고, 기본 빌드에도 들어가지 않는다.
//!
//!   cargo run -p dock --example backdrop_probe
//!
//! 창 넷이 화면 가운데에 나란히 뜬다. 창마다 아래에 이름표가 있다.
//!   (가) NopBar와 같은 창(활성화 안 되는 도구 창) + HostBackdropBrush
//!   (나) 같은 창 + DWM의 시스템 백드롭(DWMSBT_TRANSIENTWINDOW)
//!   (다) (가)와 같은 그리기, 보통 창(누르면 활성화됨)
//!   (라) (나)와 같은 그리기, 보통 창
//! 틴트는 v2.1 다크 패널(#262626 @ .82), 모서리 20.
//!
//! 볼 것: 다른 앱의 창을 눌러 활성으로 만들었을 때, 그리고 (다)·(라)를 눌러
//! 이 창들 가운데 하나가 활성일 때, 네 창 뒤의 흐림이 남는가 아니면 단색으로
//! 바뀌는가. 닫기: Esc(어느 창이 활성이든), 또는 아무 시험 창이나 오른쪽 클릭.
//! (떠 있는 동안 Esc는 이 프로그램이 가져간다.)

use windows::core::{w, Interface, Result, BOOL, PCWSTR};
use windows::UI::Color;
use windows::UI::Composition::Compositor;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, RO_INIT_SINGLETHREADED};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Vector2, Vector3};

/// DWMWA_SYSTEMBACKDROP_TYPE and DWMSBT_TRANSIENTWINDOW (Windows 11 22H2+).
const DWMWA_SYSTEMBACKDROP_TYPE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(38);
const DWMSBT_TRANSIENTWINDOW: i32 = 3;
/// DWMWA_WINDOW_CORNER_PREFERENCE and DWMWCP_ROUND.
const DWMWA_WINDOW_CORNER_PREFERENCE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(33);
const DWMWCP_ROUND: i32 = 2;

const W: f32 = 280.0;
const H: f32 = 180.0;
const GAP: f32 = 24.0;
const LABEL_H: f32 = 44.0;
const RADIUS: f32 = 20.0;
const HOTKEY: i32 = 1;

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_RBUTTONUP | WM_CLOSE => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        RoInitialize(RO_INIT_SINGLETHREADED)?;
        let _queue = CreateDispatcherQueueController(DispatcherQueueOptions { dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32, threadType: DQTYPE_THREAD_CURRENT, apartmentType: DQTAT_COM_STA })?;
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("NopBarBackdropProbe"),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let s = GetDpiForSystem() as f32 / 96.0;
        let mut work = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let px = |v: f32| (v * s).round() as i32;
        let total = px(W) * 4 + px(GAP) * 3;
        let x0 = work.left + (work.right - work.left - total) / 2;
        let y0 = work.top + (work.bottom - work.top - px(H) - px(LABEL_H)) / 2;

        let c = Compositor::new()?;
        let interop: ICompositorDesktopInterop = c.cast()?;
        let mut keep = Vec::new();

        // (name, like NopBar's windows (never activated), DWM's own backdrop)
        let probes: [(PCWSTR, bool, bool); 4] = [
            (w!("(가) NopBar와 같은 창\nHostBackdropBrush"), true, false),
            (w!("(나) NopBar와 같은 창\nDWMSBT_TRANSIENTWINDOW"), true, true),
            (w!("(다) 보통 창(누르면 활성)\nHostBackdropBrush"), false, false),
            (w!("(라) 보통 창(누르면 활성)\nDWMSBT_TRANSIENTWINDOW"), false, true),
        ];
        for (i, (name, like_nopbar, system)) in probes.into_iter().enumerate() {
            let x = x0 + i as i32 * (px(W) + px(GAP));
            let ex = if like_nopbar { WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE } else { WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST };
            let hwnd = CreateWindowExW(ex, w!("NopBarBackdropProbe"), w!("NopBar backdrop probe"), WS_POPUP, x, y0, px(W), px(H), None, None, Some(hinst.into()), None)?;
            let on = BOOL(1);
            if system {
                // DWM draws the backdrop itself, behind the whole client
                // area (the frame extended over it), dark, rounded.
                let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as *const _, 4);
                let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND as *const _ as *const _, 4);
                let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 });
                let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW as *const _ as *const _, 4);
            } else {
                // As popup.rs, the dock, the stacks and the previews do.
                let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &on as *const _ as *const _, 4);
            }
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let root = c.CreateContainerVisual()?;
            root.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
            root.SetSize(Vector2 { X: W, Y: H })?;
            let size = Vector2 { X: W, Y: H };
            if !system {
                let clip = c.CreateRoundedRectangleGeometry()?;
                clip.SetSize(size)?;
                clip.SetCornerRadius(Vector2 { X: RADIUS, Y: RADIUS })?;
                root.SetClip(&c.CreateGeometricClipWithGeometry(&clip)?)?;
                let backdrop = c.CreateSpriteVisual()?;
                backdrop.SetBrush(&c.CreateHostBackdropBrush()?)?;
                backdrop.SetSize(size)?;
                root.Children()?.InsertAtTop(&backdrop)?;
            }
            // The v2.1 dark panel tint: #262626 at .82.
            let tint = c.CreateSpriteVisual()?;
            tint.SetBrush(&c.CreateColorBrushWithColor(Color { A: 209, R: 0x26, G: 0x26, B: 0x26 })?)?;
            tint.SetSize(size)?;
            root.Children()?.InsertAtTop(&tint)?;
            target.SetRoot(&root)?;
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            // Its name tag: a plain system STATIC under it.
            let label = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                w!("STATIC"),
                name,
                // SS_CENTER (1): the text centered.
                WS_POPUP | WINDOW_STYLE(1),
                x,
                y0 + px(H) + px(8.0),
                px(W),
                px(LABEL_H - 8.0),
                None,
                None,
                Some(hinst.into()),
                None,
            )?;
            let _ = ShowWindow(label, SW_SHOWNOACTIVATE);
            keep.push((target, root, hwnd, label));
        }

        // Esc from anywhere (the windows like NopBar's never get the keys).
        let hot = RegisterHotKey(None, HOTKEY, HOT_KEY_MODIFIERS(0), VK_ESCAPE.0 as u32).is_ok();
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).as_bool() {
            if m.message == WM_HOTKEY && m.wParam.0 == HOTKEY as usize {
                break;
            }
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        if hot {
            let _ = UnregisterHotKey(None, HOTKEY);
        }
        for (_, _, hwnd, label) in keep.drain(..) {
            let _ = DestroyWindow(hwnd);
            let _ = DestroyWindow(label);
        }
    }
    Ok(())
}
