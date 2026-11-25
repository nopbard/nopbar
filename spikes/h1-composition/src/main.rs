//! Harmony 묶음 S: Composition 쪽 확인 스파이크 (S1, S4, S5, S7).
//!
//!   cargo run -p h1-composition --release
//!
//! 창을 보이지 않는다(숨은 창 하나가 컴포지터의 주인일 뿐). 화면을 캡처하지
//! 않는다: 우리 비주얼 트리 하나만 `GraphicsCaptureItem::CreateFromVisual`로
//! 오프스크린에서 읽는다. 결과는 표준 출력의 숫자와
//! `target/harmony-spikes/*.png`.
//!
//! S1  SpringScalarNaturalMotionAnimation.Period가 감쇠 전 고유 주기인가
//! S4  달리는 속성에 스프링을 다시 걸면 속도를 잇는가
//! S5  DropShadow.BlurRadius와 가우시안 시그마(CSS 번짐 = 2 시그마)의 비
//! S7  그라데이션 획 1 기기 픽셀, 획·그림자 비주얼의 불투명도를 판과 따로

use std::path::{Path, PathBuf};

use windows::core::{w, Interface, Result, HSTRING};
use windows::Foundation::{IReference, PropertyValue, TimeSpan};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::UI::Color;
use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Foundation::{GENERIC_WRITE, HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, RO_INIT_SINGLETHREADED};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Vector2, Vector3};

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(h, msg, wp, lp)
}

struct Rig {
    c: Compositor,
    root: ContainerVisual,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    d3d: IDirect3DDevice,
    _target: windows::UI::Composition::Desktop::DesktopWindowTarget,
    _queue: windows::System::DispatcherQueueController,
}

/// One captured frame: its time (100 ns, the system's relative time) and
/// BGRA pixels.
struct Frame {
    t: i64,
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Frame {
    fn at(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.w + x) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }
    fn lum(&self, x: usize, y: usize) -> f32 {
        let p = self.at(x, y);
        (p[0] as f32 + p[1] as f32 + p[2] as f32) / (3.0 * 255.0)
    }
}

struct Cap {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
}

impl Drop for Cap {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

fn qpc_100ns() -> i64 {
    unsafe {
        let (mut c, mut f) = (0i64, 0i64);
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
        ((c as i128 * 10_000_000) / f as i128) as i64
    }
}

fn pump() {
    unsafe {
        let mut m = MSG::default();
        while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
            DispatchMessageW(&m);
        }
    }
}

impl Rig {
    unsafe fn new() -> Result<Rig> {
        RoInitialize(RO_INIT_SINGLETHREADED)?;
        let queue = CreateDispatcherQueueController(DispatcherQueueOptions { dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32, threadType: DQTYPE_THREAD_CURRENT, apartmentType: DQTAT_COM_STA })?;
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: w!("HarmonySpike"), ..Default::default() };
        RegisterClassExW(&wc);
        // Never shown: only something for the compositor's target to hang on.
        let hwnd = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, w!("HarmonySpike"), w!(""), WS_POPUP, 0, 0, 16, 16, None, None, Some(hinst.into()), None)?;
        let c = Compositor::new()?;
        let interop: ICompositorDesktopInterop = c.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, false)?;
        let root = c.CreateContainerVisual()?;
        target.SetRoot(&root)?;
        let make = |kind: D3D_DRIVER_TYPE| -> Result<(ID3D11Device, ID3D11DeviceContext)> {
            let (mut dev, mut ctx) = (None, None);
            D3D11CreateDevice(None, kind, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, Some(&mut ctx))?;
            Ok((dev.unwrap(), ctx.unwrap()))
        };
        let (device, ctx) = make(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| make(D3D_DRIVER_TYPE_WARP))?;
        let dxgi: IDXGIDevice = device.cast()?;
        let d3d: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
        Ok(Rig { c, root, device, ctx, d3d, _target: target, _queue: queue })
    }

    /// A new scene `w` x `h` px under the root (the old one is dropped).
    fn scene(&self, w: f32, h: f32) -> Result<ContainerVisual> {
        self.root.Children()?.RemoveAll()?;
        let s = self.c.CreateContainerVisual()?;
        s.SetSize(Vector2 { X: w, Y: h })?;
        self.root.Children()?.InsertAtTop(&s)?;
        Ok(s)
    }

    fn sprite(&self, parent: &ContainerVisual, x: f32, y: f32, w: f32, h: f32, col: (u8, u8, u8, u8)) -> Result<windows::UI::Composition::SpriteVisual> {
        let v = self.c.CreateSpriteVisual()?;
        v.SetBrush(&self.c.CreateColorBrushWithColor(Color { A: col.3, R: col.0, G: col.1, B: col.2 })?)?;
        v.SetSize(Vector2 { X: w, Y: h })?;
        v.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
        parent.Children()?.InsertAtTop(&v)?;
        Ok(v)
    }

    fn capture(&self, v: &ContainerVisual) -> Result<Cap> {
        let visual: Visual = v.cast()?;
        let size = v.Size()?;
        let item = GraphicsCaptureItem::CreateFromVisual(&visual)?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&self.d3d, DirectXPixelFormat::B8G8R8A8UIntNormalized, 3, SizeInt32 { Width: size.X.ceil() as i32, Height: size.Y.ceil() as i32 })?;
        let session = pool.CreateCaptureSession(&item)?;
        session.StartCapture()?;
        Ok(Cap { pool, session })
    }

    fn next(&self, cap: &Cap) -> Option<Frame> {
        let frame = cap.pool.TryGetNextFrame().ok()?;
        let out = (|| -> Result<Frame> {
            unsafe {
                let t = frame.SystemRelativeTime()?.Duration;
                let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                let tex: ID3D11Texture2D = access.GetInterface()?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                tex.GetDesc(&mut desc);
                desc.Usage = D3D11_USAGE_STAGING;
                desc.BindFlags = 0;
                desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                desc.MiscFlags = 0;
                let mut staging = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut staging))?;
                let staging = staging.unwrap();
                self.ctx.CopyResource(&staging, &tex);
                let mut map = D3D11_MAPPED_SUBRESOURCE::default();
                self.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
                let (w, h) = (desc.Width as usize, desc.Height as usize);
                let mut px = Vec::with_capacity(w * h * 4);
                for y in 0..h {
                    px.extend_from_slice(std::slice::from_raw_parts((map.pData as *const u8).add(y * map.RowPitch as usize), w * 4));
                }
                self.ctx.Unmap(&staging, 0);
                Ok(Frame { t, w, h, px })
            }
        })();
        let _ = frame.Close();
        out.ok()
    }

    /// Frames for `ms` (every one the compositor produces).
    fn record(&self, cap: &Cap, ms: u64, mut each: impl FnMut(&Frame)) {
        let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < end {
            pump();
            let mut got = false;
            while let Some(f) = self.next(cap) {
                each(&f);
                got = true;
            }
            if !got {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// The last frame once the scene is still (`ms` to settle).
    fn still(&self, v: &ContainerVisual, ms: u64) -> Result<Frame> {
        let cap = self.capture(v)?;
        let mut last = None;
        self.record(&cap, ms, |f| last = Some(Frame { t: f.t, w: f.w, h: f.h, px: f.px.clone() }));
        last.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
    }
}

fn out_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("harmony-spikes")
}

fn save_png(f: &Frame, name: &str) {
    unsafe {
        let path = out_dir().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
        let bmp = wic.CreateBitmapFromMemory(f.w as u32, f.h as u32, &GUID_WICPixelFormat32bppBGRA, f.w as u32 * 4, &f.px).unwrap();
        let stream = wic.CreateStream().unwrap();
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0).unwrap();
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).unwrap();
        enc.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut()).unwrap();
        let frame = frame.unwrap();
        frame.Initialize(None).unwrap();
        frame.SetSize(f.w as u32, f.h as u32).unwrap();
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt).unwrap();
        frame.WriteSource(&bmp, std::ptr::null()).unwrap();
        frame.Commit().unwrap();
        enc.Commit().unwrap();
    }
}

fn single(v: f32) -> Result<IReference<f32>> {
    PropertyValue::CreateSingle(v)?.cast()
}

// ---- the spring's closed form (dock/src/motion.rs's) -------------------------

/// The white bar's center (px) on row `y`, by weight.
fn bar_x(f: &Frame, y: usize) -> Option<f32> {
    let (mut sum, mut wsum) = (0.0f32, 0.0f32);
    for x in 0..f.w {
        let l = f.lum(x, y);
        sum += l * (x as f32 + 0.5);
        wsum += l;
    }
    (wsum > 0.5).then(|| sum / wsum)
}

const TRAVEL: f32 = 300.0;
const BAR: f32 = 8.0;

/// Runs a spring on the bar's Offset.X from 0 to TRAVEL; returns (ms since
/// the StartAnimation call, value 0..1).
fn run_spring(rig: &Rig, period_s: f32, ratio: f32, ms: u64, retarget_at_ms: Option<(u64, bool)>) -> Result<(Vec<(f32, f32)>, Option<f32>)> {
    let scene = rig.scene(TRAVEL + 200.0, 16.0)?;
    rig.sprite(&scene, 0.0, 0.0, TRAVEL + 200.0, 16.0, (0, 0, 0, 255))?;
    let bar = rig.sprite(&scene, 20.0, 0.0, BAR, 16.0, (255, 255, 255, 255))?;
    let cap = rig.capture(&scene)?;
    // Let the first frames come (the scene at rest).
    rig.record(&cap, 250, |_| {});
    let spring = |to: f32, from: Option<f32>| -> Result<windows::UI::Composition::SpringScalarNaturalMotionAnimation> {
        let a = rig.c.CreateSpringScalarAnimation()?;
        a.SetPeriod(TimeSpan { Duration: (period_s * 1e7) as i64 })?;
        a.SetDampingRatio(ratio)?;
        if let Some(f) = from {
            a.SetInitialValue(&single(f)?)?;
        }
        a.SetFinalValue(&single(to)?)?;
        Ok(a)
    };
    let a = spring(20.0 + TRAVEL, Some(20.0))?;
    let t0 = qpc_100ns();
    bar.StartAnimation(&HSTRING::from("Offset.X"), &a)?;
    let mut samples: Vec<(f32, f32)> = Vec::new();
    let mut push = |f: &Frame| {
        if let Some(x) = bar_x(f, 8) {
            samples.push(((f.t - t0) as f32 / 10_000.0, (x - BAR / 2.0 - 20.0) / TRAVEL));
        }
    };
    let mut switched = None;
    if let Some((at, hand_over)) = retarget_at_ms {
        rig.record(&cap, at, &mut push);
        // Back to the start, no initial value: from wherever it is.
        let b = spring(20.0, None)?;
        let now = (qpc_100ns() - t0) as f32 / 10_000.0;
        if hand_over {
            // The velocity it has now by our own closed form (px / s), a
            // frame ahead (the retarget lands with the next commit).
            let t = (now + 16.0) / 1000.0;
            let v = (closed(1.0 / period_s, ratio, t + 0.0005) - closed(1.0 / period_s, ratio, t - 0.0005)) / 0.001 * TRAVEL;
            b.SetInitialVelocity(v)?;
        }
        switched = Some(now);
        bar.StartAnimation(&HSTRING::from("Offset.X"), &b)?;
        rig.record(&cap, ms.saturating_sub(at), &mut push);
    } else {
        rig.record(&cap, ms, &mut push);
    }
    samples.retain(|s| s.0 >= -50.0);
    Ok((samples, switched))
}

/// The closed form of a spring released at rest, 0 -> 1 (motion.rs's
/// Spring::left, as 1 - left).
fn closed(w0: f32, z: f32, t: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    let left = if z < 1.0 - 1e-4 {
        let wd = w0 * (1.0 - z * z).sqrt();
        (-z * w0 * t).exp() * ((wd * t).cos() + (z * w0 / wd) * (wd * t).sin())
    } else if z <= 1.0 + 1e-4 {
        (-w0 * t).exp() * (1.0 + w0 * t)
    } else {
        let s = (z * z - 1.0).sqrt();
        let (r1, r2) = (-w0 * (z - s), -w0 * (z + s));
        (r2 * (r1 * t).exp() - r1 * (r2 * t).exp()) / (r2 - r1)
    };
    1.0 - left
}

/// Best RMS of `samples` against closed(w0, z) over a start offset of
/// 0..60 ms (the first frames after StartAnimation).
fn fit(samples: &[(f32, f32)], w0: f32, z: f32) -> (f32, f32) {
    let mut best = (f32::MAX, 0.0);
    let mut off = 0.0f32;
    while off <= 60.0 {
        let e: f32 = samples.iter().map(|&(t, x)| (closed(w0, z, (t - off) / 1000.0) - x).powi(2)).sum();
        let rms = (e / samples.len().max(1) as f32).sqrt();
        if rms < best.0 {
            best = (rms, off);
        }
        off += 2.0;
    }
    best
}

fn s1(rig: &Rig) -> Result<bool> {
    println!("
== S1  SpringScalarNaturalMotionAnimation.Period ==");
    let tau = std::f32::consts::TAU;
    let cases = [("SNAP 600/48", 0.257f32, 0.98f32), ("GLIDE 400/38", 0.314, 0.95), ("probe", 0.400, 0.50), ("probe", 0.300, 0.30), ("probe", 0.100, 0.50), ("GLIDE as 1 / w0 (1 / sqrt 400)", 0.050, 0.95), ("SNAP as 1 / w0 (1 / sqrt 600)", 0.040_825, 0.98), ("FOLLOW as 1 / w0 (1 / sqrt 1600)", 0.025, 1.0)];
    for (name, period, ratio) in cases {
        let (samples, _) = run_spring(rig, period, ratio, 3000, None)?;
        println!("{name}: Period {period}s ratio {ratio}: {} frames", samples.len());
        // The named readings of Period, the ratio as given.
        for (what, w0) in [("natural period (w0 = 2pi / P)", tau / period), ("damped period (wd = 2pi / P)", tau / period / (1.0 - ratio * ratio).sqrt()), ("time constant (w0 = 1 / P)", 1.0 / period)] {
            let (e, o) = fit(&samples, w0, ratio);
            println!("  {what:<32} rms {e:.4} (start offset {o:+.0} ms)");
        }
        // And a free fit: which (w0, ratio) is it really.
        let mut best = (f32::MAX, 0.0, 0.0);
        let mut k = 0.4f32;
        while k <= 9.0 {
            for zi in 2..=30 {
                let z = zi as f32 * 0.05;
                let (e, _) = fit(&samples, k / period, z);
                if e < best.0 {
                    best = (e, k, z);
                }
            }
            k += 0.05;
        }
        println!("  free fit: w0 * Period = {:.2} (2pi = 6.28), ratio {:.2}, rms {:.4}", best.1, best.2, best.0);
        let peak = samples.iter().fold((0.0f32, 0.0f32), |m, s| if s.1 > m.1 { *s } else { m });
        println!("  peak {:.3} at {:.0} ms", peak.1, peak.0);
        let row: Vec<String> = samples.iter().step_by(4).map(|s| format!("{:.0}:{:.3}", s.0, s.1)).collect();
        println!("  samples (ms:value) {}", row.join(" "));
    }
    Ok(true)
}

/// A spring towards 0 from (x0, v0), `t` s on (under-damped or critical).
fn free(w0: f32, z: f32, x0: f32, v0: f32, t: f32) -> f32 {
    if z < 1.0 - 1e-4 {
        let wd = w0 * (1.0 - z * z).sqrt();
        (-z * w0 * t).exp() * (x0 * (wd * t).cos() + (v0 + z * w0 * x0) / wd * (wd * t).sin())
    } else {
        (-w0 * t).exp() * (x0 + (v0 + w0 * x0) * t)
    }
}

fn s4(rig: &Rig) -> Result<()> {
    println!("
== S4  retargeting a running spring ==");
    for hand_over in [false, true] {
        println!("-- {} --", if hand_over { "the new animation is given InitialVelocity (our closed form's)" } else { "the new animation is only started (no initial value, no velocity)" });
        for (name, period, ratio, at) in [("GLIDE", 0.050f32, 0.95f32, 70u64), ("SNAP", 0.040_825, 0.98, 75), ("probe slow", 0.127, 0.90, 150), ("probe slower", 0.250, 0.70, 250), ("probe bouncy", 0.080, 0.40, 90)] {
            let (samples, switched) = run_spring(rig, period, ratio, 1800, Some((at, hand_over)))?;
            let sw = switched.unwrap();
            let w0 = 1.0 / period;
            let before: Vec<(f32, f32)> = samples.iter().copied().filter(|s| s.0 < sw).collect();
            let after: Vec<(f32, f32)> = samples.iter().copied().filter(|s| s.0 >= sw).collect();
            if before.len() < 3 || after.len() < 5 {
                println!("{name}: too few frames ({} before, {} after)", before.len(), after.len());
                continue;
            }
            // The first leg's start offset, from its own frames.
            let (_, off) = fit(&before, w0, ratio);
            let leg1 = |t_ms: f32| closed(w0, ratio, (t_ms - off) / 1000.0);
            let vel1 = |t_ms: f32| (leg1(t_ms + 0.5) - leg1(t_ms - 0.5)) / 0.001;
            // The second leg: switched at ts (the commit lands a little
            // after the call), from leg 1's value there, with velocity v0.
            let rms = |ts: f32, v0: f32| {
                let e: f32 = after.iter().map(|&(t, x)| (if t < ts { leg1(t) } else { free(w0, ratio, leg1(ts), v0, (t - ts) / 1000.0) } - x).powi(2)).sum();
                (e / after.len() as f32).sqrt()
            };
            let (mut best, mut kept, mut rest) = ((f32::MAX, 0.0, 0.0), (f32::MAX, 0.0), (f32::MAX, 0.0));
            let mut ts = sw - 10.0;
            while ts <= sw + 70.0 {
                let (k, r) = (rms(ts, vel1(ts)), rms(ts, 0.0));
                if k < kept.0 {
                    kept = (k, ts);
                }
                if r < rest.0 {
                    rest = (r, ts);
                }
                let mut v0 = -3.0f32;
                while v0 <= 14.0 {
                    let e = rms(ts, v0);
                    if e < best.0 {
                        best = (e, ts, v0);
                    }
                    v0 += 0.1;
                }
                ts += 1.0;
            }
            println!("{name} (Period {period}, ratio {ratio}): retargeted at {:.0} ms", sw);
            println!("  velocity kept:    rms {:.4} (lands at {:.0} ms, where leg 1 runs at {:.2}/s)", kept.0, kept.1, vel1(kept.1));
            println!("  restarts at rest: rms {:.4} (lands at {:.0} ms)", rest.0, rest.1);
            println!("  free fit:         rms {:.4} (lands at {:.0} ms, velocity {:.2}/s; leg 1 runs at {:.2}/s there)", best.0, best.1, best.2, vel1(best.1));
            let row: Vec<String> = samples.iter().filter(|s| s.0 >= sw - 45.0 && s.0 < sw + 200.0).map(|s| format!("{:.0}:{:.3}", s.0, s.1)).collect();
            println!("  around it  {}", row.join(" "));
        }
    }
    Ok(())
}

fn s5(rig: &Rig) -> Result<()> {
    println!("\n== S5  DropShadow.BlurRadius vs Gaussian sigma ==");
    let (w, h) = (360.0f32, 760.0f32);
    for blur in [8.0f32, 16.0, 24.0, 32.0, 40.0] {
        let scene = rig.scene(w, h)?;
        rig.sprite(&scene, 0.0, 0.0, w, h, (255, 255, 255, 255))?;
        // The caster sits above; its shadow is thrown 400 px down so its
        // whole edge shows.
        let caster = rig.sprite(&scene, 80.0, 20.0, 200.0, 200.0, (255, 255, 255, 255))?;
        let shadow = rig.c.CreateDropShadow()?;
        shadow.SetBlurRadius(blur)?;
        shadow.SetOffset(Vector3 { X: 0.0, Y: 400.0, Z: 0.0 })?;
        shadow.SetColor(Color { A: 255, R: 0, G: 0, B: 0 })?;
        caster.SetShadow(&shadow)?;
        let f = rig.still(&scene, 400)?;
        // Darkness down the middle column across the shadow's top edge
        // (y = 420): an edge blurred by a Gaussian is its error function;
        // 15.87 % and 84.13 % are one sigma either side.
        let dark = |y: usize| 1.0 - f.lum(180, y);
        let cross = |level: f32| -> f32 {
            for y in 300..560 {
                let (a, b) = (dark(y), dark(y + 1));
                if a <= level && b > level {
                    return y as f32 + 0.5 + (level - a) / (b - a);
                }
            }
            f32::NAN
        };
        let (lo, mid, hi) = (cross(0.1587), cross(0.5), cross(0.8413));
        let sigma = (hi - lo) / 2.0;
        println!("BlurRadius {blur:>4}: sigma {:.2} px (edge at {:.1}), sigma / BlurRadius {:.3}, CSS blur equal to it {:.1} (CSS / BlurRadius {:.3}), peak darkness {:.3}", sigma, mid, sigma / blur, sigma * 2.0, sigma * 2.0 / blur, dark(520));
        if blur == 32.0 {
            save_png(&f, "s5-dropshadow-blur32.png");
        }
    }
    Ok(())
}

/// A panel as popup::chrome_at builds it, with the v2.1 rim: shadows,
/// tint, a gradient stroke one device pixel thick. `strength` is the rim's
/// and the shadows' opacity (the tint stays).
fn panel(rig: &Rig, s: f32, light: bool, strength: f32, snap: bool) -> Result<(ContainerVisual, (f32, f32, f32, f32))> {
    let c = &rig.c;
    // (Odd sizes: at 125 % and 150 % the far edges fall between pixels.)
    let (w, h, radius, m) = (203.0f32, 117.0f32, 20.0f32, 48.0f32);
    let (cw, ch) = (((w + m * 2.0) * s).ceil(), ((h + m * 2.0) * s).ceil());
    let scene = rig.scene(cw, ch)?;
    let v = if light { 0xC4 } else { 0x3A };
    rig.sprite(&scene, 0.0, 0.0, cw, ch, (v, v, v, 255))?;
    let outer = c.CreateContainerVisual()?;
    outer.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
    outer.SetOffset(Vector3 { X: (m * s).round(), Y: (m * s).round(), Z: 0.0 })?;
    scene.Children()?.InsertAtTop(&outer)?;
    // E3, v2.1: 6 / 14 / .10 -> .30 and 16 / 32 / .14 -> .34 (CSS blur).
    let layers = if light { [(6.0, 14.0, 0.10f32), (16.0, 32.0, 0.14)] } else { [(6.0, 14.0, 0.30), (16.0, 32.0, 0.34)] };
    // The shadows' mask: the plate's shape (the product draws it into a
    // surface; here a shape visual seen through a visual surface).
    let mask_shape = c.CreateRoundedRectangleGeometry()?;
    mask_shape.SetSize(Vector2 { X: w, Y: h })?;
    mask_shape.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
    let filled = c.CreateSpriteShapeWithGeometry(&mask_shape)?;
    filled.SetFillBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: 255, G: 255, B: 255 })?)?;
    let mask_visual = c.CreateShapeVisual()?;
    mask_visual.Shapes()?.Append(&filled)?;
    mask_visual.SetSize(Vector2 { X: w, Y: h })?;
    let surface = c.CreateVisualSurface()?;
    surface.SetSourceVisual(&mask_visual)?;
    surface.SetSourceSize(Vector2 { X: w, Y: h })?;
    let mask = c.CreateSurfaceBrushWithSurface(&surface)?;
    for (y, blur, a) in layers {
        let sv = c.CreateSpriteVisual()?;
        sv.SetSize(Vector2 { X: w, Y: h })?;
        let shadow = c.CreateDropShadow()?;
        shadow.SetBlurRadius(blur)?;
        shadow.SetOffset(Vector3 { X: 0.0, Y: y, Z: 0.0 })?;
        shadow.SetColor(Color { A: (a * 255.0).round() as u8, R: 0, G: 0, B: 0 })?;
        shadow.SetMask(&mask)?;
        sv.SetShadow(&shadow)?;
        sv.SetOpacity(strength)?;
        outer.Children()?.InsertAtTop(&sv)?;
    }
    let root = c.CreateContainerVisual()?;
    root.SetSize(Vector2 { X: w, Y: h })?;
    let clip = c.CreateRoundedRectangleGeometry()?;
    clip.SetSize(Vector2 { X: w, Y: h })?;
    clip.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
    root.SetClip(&c.CreateGeometricClipWithGeometry(&clip)?)?;
    outer.Children()?.InsertAtTop(&root)?;
    let t = if light { 0xF5 } else { 0x26 };
    let tint = c.CreateSpriteVisual()?;
    tint.SetBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: t, G: t, B: t })?)?;
    tint.SetSize(Vector2 { X: w, Y: h })?;
    root.Children()?.InsertAtTop(&tint)?;
    // The rim: a stroke one device pixel thick just inside the edge, white
    // from the top (.85 / .20) through 38 % (.50 / .07) to the bottom
    // (.15 / .03).
    let px = 1.0 / s;
    // Snapped: the far edges' stroke on the last whole device pixel of the
    // plate (its size in px floored).
    let (gw, gh) = if snap { (((w * s).floor() - 1.0) / s, ((h * s).floor() - 1.0) / s) } else { (w - px, h - px) };
    let geom = c.CreateRoundedRectangleGeometry()?;
    geom.SetSize(Vector2 { X: gw, Y: gh })?;
    geom.SetOffset(Vector2 { X: px / 2.0, Y: px / 2.0 })?;
    geom.SetCornerRadius(Vector2 { X: (radius - px / 2.0).max(0.0), Y: (radius - px / 2.0).max(0.0) })?;
    let shape = c.CreateSpriteShapeWithGeometry(&geom)?;
    let grad = c.CreateLinearGradientBrush()?;
    grad.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
    grad.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 })?;
    let stops = if light { [(0.0, 0.85f32), (0.38, 0.50), (1.0, 0.15)] } else { [(0.0, 0.20), (0.38, 0.07), (1.0, 0.03)] };
    for (at, a) in stops {
        grad.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(at, Color { A: (a * 255.0).round() as u8, R: 255, G: 255, B: 255 })?)?;
    }
    shape.SetStrokeBrush(&grad)?;
    shape.SetStrokeThickness(px)?;
    let border = c.CreateShapeVisual()?;
    border.Shapes()?.Append(&shape)?;
    border.SetSize(Vector2 { X: w, Y: h })?;
    border.SetOpacity(strength)?;
    root.Children()?.InsertAtTop(&border)?;
    Ok((scene, ((m * s).round(), (m * s).round(), w * s, h * s)))
}

fn s7(rig: &Rig) -> Result<()> {
    println!("\n== S7  gradient rim at one device pixel; rim and shadow opacity apart from the plate ==");
    for light in [false, true] {
        let mode = if light { "light" } else { "dark" };
        for s in [1.0f32, 1.25, 1.5, 2.0] {
            for snap in [false, true] {
                let (scene, (x, y, w, h)) = panel(rig, s, light, 1.0, snap)?;
                let f = rig.still(&scene, 350)?;
                let midx = (x + w / 2.0) as usize;
                let midy = (y + h / 2.0) as usize;
                let (x0, y0) = (x as usize, y as usize);
                // Rows around the top edge, the bottom edge, and columns
                // around the left edge: (grey level) per device pixel.
                let top: Vec<u8> = (y0 - 2..y0 + 4).map(|yy| f.at(midx, yy)[1]).collect();
                let yb = (y + h).round() as usize;
                let bottom: Vec<u8> = (yb - 4..yb + 2).map(|yy| f.at(midx, yy)[1]).collect();
                let left: Vec<u8> = (x0 - 2..x0 + 4).map(|xx| f.at(xx, midy)[1]).collect();
                let xr = (x + w).round() as usize;
                let right: Vec<u8> = (xr - 4..xr + 2).map(|xx| f.at(xx, midy)[1]).collect();
                println!("{mode} {:>4}% snap {snap:<5}: top {top:?} bottom {bottom:?} left(mid) {left:?} right(mid) {right:?}", (s * 100.0) as i32);
                if snap {
                    save_png(&f, &format!("s7-rim-{mode}-{}.png", (s * 100.0) as i32));
                }
            }
        }
        for strength in [0.0f32, 0.5, 1.0] {
            let (scene, (x, y, w, h)) = panel(rig, 1.5, light, strength, true)?;
            let f = rig.still(&scene, 350)?;
            let midx = (x + w / 2.0) as usize;
            let plate = f.at(midx, (y + h / 2.0) as usize)[1];
            let rim = f.at(midx, y as usize)[1];
            let under = f.at(midx, (y + h + 12.0 * 1.5) as usize)[1];
            println!("{mode} strength {strength}: plate {plate}, rim row {rim}, 12 DIP under the plate {under}");
            save_png(&f, &format!("s7-strength-{mode}-{}.png", (strength * 100.0) as i32));
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let only: Vec<String> = std::env::args().skip(1).collect();
    let want = |n: &str| only.is_empty() || only.iter().any(|a| a == n);
    unsafe {
        let rig = Rig::new()?;
        if want("s1") {
            s1(&rig)?;
        }
        if want("s4") {
            s4(&rig)?;
        }
        if want("s5") {
            s5(&rig)?;
        }
        if want("s7") {
            s7(&rig)?;
        }
    }
    Ok(())
}
