//! The snip overlay drawn off screen, as it shows: the still picture, the
//! dim around the box and the box's edge (the same shapes and colors the
//! overlay's composition rectangles get), saved as PNG under
//! target/snip-render (to look at) and checked pixel by pixel: inside the
//! box the picture is untouched, outside it is dimmed, the edge is light
//! and sits outside the box.

use windows::core::{Interface, HSTRING};
use windows::Win32::Foundation::{GENERIC_WRITE, HMODULE, RECT};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_MAP_OPTIONS_READ,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA, IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use super::{paint_frame, shapes};
use crate::pieces::snip::{self, Frame, State};

/// A software device context drawing into a `w`x`h` px target.
fn warp(w: u32, h: u32) -> (ID2D1DeviceContext, ID2D1Bitmap1) {
    unsafe {
        let mut dev = None;
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None).unwrap();
        let dxgi: IDXGIDevice = dev.unwrap().cast().unwrap();
        let f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
        let dc = f.CreateDevice(&dxgi).unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap();
        let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
        let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET, ..Default::default() };
        let target = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap();
        dc.SetTarget(&target);
        (dc, target)
    }
}

/// The target's pixels (premultiplied BGRA, rows packed).
fn read_back(dc: &ID2D1DeviceContext, target: &ID2D1Bitmap1, w: u32, h: u32) -> Vec<u8> {
    unsafe {
        let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
        let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW, ..Default::default() };
        let cpu = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap();
        cpu.CopyFromBitmap(None, target, None).unwrap();
        let m = cpu.Map(D2D1_MAP_OPTIONS_READ).unwrap();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            let row = std::slice::from_raw_parts(m.bits.add(y * m.pitch as usize), (w * 4) as usize);
            out.extend_from_slice(row);
        }
        cpu.Unmap().unwrap();
        out
    }
}

/// Saves premultiplied BGRA pixels as a PNG.
fn save_png(path: &std::path::Path, w: u32, h: u32, px: &[u8]) {
    unsafe {
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
        let bmp = f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppPBGRA, w * 4, px).unwrap();
        let stream = f.CreateStream().unwrap();
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0).unwrap();
        let enc = f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).unwrap();
        enc.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
        let mut fe: Option<IWICBitmapFrameEncode> = None;
        enc.CreateNewFrame(&mut fe, std::ptr::null_mut()).unwrap();
        let fe = fe.unwrap();
        fe.Initialize(None).unwrap();
        fe.SetSize(w, h).unwrap();
        let mut fmt = GUID_WICPixelFormat32bppPBGRA;
        fe.SetPixelFormat(&mut fmt).unwrap();
        fe.WriteSource(&bmp, std::ptr::null()).unwrap();
        fe.Commit().unwrap();
        enc.Commit().unwrap();
    }
}

/// A made-up desktop: a cool-to-warm wash with two "windows" on it.
fn desktop(w: u32, h: u32) -> Frame {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let in_win = |l: u32, t: u32, r: u32, b: u32| x >= l && x < r && y >= t && y < b;
            let p: [u8; 4] = if in_win(150, 100, 460, 300) {
                // A light window with a title strip and text-like lines.
                if y < 124 {
                    [230, 230, 230, 255]
                } else if (y - 124) % 18 < 3 && x > 170 && x < 170 + (x * 7 % 260).max(120) {
                    [60, 60, 60, 255]
                } else {
                    [250, 250, 250, 255]
                }
            } else if in_win(380, 230, 600, 380) {
                [70, 50, 40, 255]
            } else {
                [(120 + x * 100 / w) as u8, (90 + y * 60 / h) as u8, (160 - x * 120 / w) as u8, 255]
            };
            px.extend_from_slice(&p);
        }
    }
    Frame { w, h, px }
}

unsafe fn fill(dc: &ID2D1DeviceContext, r: &RECT, a: f32, v: f32) {
    if r.right <= r.left || r.bottom <= r.top {
        return;
    }
    let b = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: v, g: v, b: v, a }, None).unwrap();
    dc.FillRectangle(&D2D_RECT_F { left: r.left as f32, top: r.top as f32, right: r.right as f32, bottom: r.bottom as f32 }, &b);
}

fn draw(name: &str, f: &Frame, s: State, line: i32) -> Vec<u8> {
    let (dc, target) = warp(f.w, f.h);
    unsafe {
        dc.BeginDraw();
        paint_frame(&dc, f, 0.0, 0.0).unwrap();
        let (dims, lines) = shapes(s, f.w as i32, f.h as i32, line);
        for r in &dims {
            fill(&dc, r, snip::DIM_ALPHA, 0.0);
        }
        for r in &lines {
            fill(&dc, r, snip::LINE_ALPHA, 1.0);
        }
        dc.EndDraw(None, None).unwrap();
    }
    let px = read_back(&dc, &target, f.w, f.h);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join("snip-render");
    let _ = std::fs::create_dir_all(&dir);
    save_png(&dir.join(format!("{name}.png")), f.w, f.h, &px);
    px
}

fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

fn lum(p: [u8; 4]) -> u32 {
    p[0] as u32 + p[1] as u32 + p[2] as u32
}

#[test]
fn the_overlay_dims_all_but_the_box() {
    let f = desktop(640, 400);
    let sel = RECT { left: 140, top: 90, right: 470, bottom: 310 };
    for (name, s, line) in [
        ("snip-ready", State::Ready, 1),
        ("snip-dragging", State::Dragging { from: (sel.left, sel.top), to: (sel.right, sel.bottom) }, 1),
        ("snip-dragging-150", State::Dragging { from: (sel.right, sel.bottom), to: (sel.left, sel.top) }, snip::line_px(1.5)),
    ] {
        let px = draw(name, &f, s, line);
        let orig = |x, y| at(&f.px, f.w, x, y);
        let got = |x, y| at(&px, f.w, x, y);
        // Outside the box (and everywhere before a drag): darker than the picture.
        assert!(lum(got(20, 20)) < lum(orig(20, 20)) * 7 / 10, "{name}: dimmed {:?} vs {:?}", got(20, 20), orig(20, 20));
        if s == State::Ready {
            assert!(lum(got(300, 200)) < lum(orig(300, 200)) * 7 / 10, "{name}: all dimmed before a drag");
            continue;
        }
        // Inside: the picture as it was.
        for (x, y) in [(141, 91), (300, 200), (469, 309)] {
            assert_eq!(got(x, y), orig(x, y), "{name}: ({x}, {y}) untouched");
        }
        // The edge just outside the box: light, over the dim.
        let e = got(300, (sel.top - 1) as u32);
        assert!(lum(e) > 600, "{name}: the edge {e:?}");
        assert!(lum(got(300, (sel.top - line - 2) as u32)) < lum(e), "{name}: the edge is {line} px");
    }
}

/// The overlay for the design audit (tests only).
#[path = "render_harmony.rs"]
mod harmony;
