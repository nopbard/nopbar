//! The Settings window drawn offscreen into PNGs, to look at (not to
//! assert on): every page and sub-page, the first-run flow's steps, light
//! and dark, at 1x and 1.5x, in Korean, Japanese, Chinese, English and
//! German, plus an "i" hovered (its tip up) and focused, a key field
//! focused; and per language a sheet of the app's shared wrapping
//! (`wrap/<lang>.png`: DirectWrite alone on the left, the app's on the
//! right, a red line at the width). The window's
//! own App and paint (layout_tests' harness), a WARP device, the pixels
//! copied back and written with WIC. Nothing runs on screen and nothing
//! is captured from it.
//!
//! Run on demand:
//!
//! ```text
//! cargo test -p dock settings::snapshots -- --ignored --nocapture
//! ```
//!
//! The pictures land in `target/ui-snapshots/settings/<lang>/<theme>-<scale>x/`
//! (the states in `.../states/`). NOPBAR_SNAP_LANGS (e.g. `ko`) and
//! NOPBAR_SNAP_PAGES (a part of the file name, e.g. `shortcuts`) narrow a run.

use std::path::{Path, PathBuf};

use windows::core::{Interface, HSTRING};
use windows::Win32::Foundation::{GENERIC_WRITE, HMODULE};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_MAP_OPTIONS_READ,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA, IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows_numerics::Matrix3x2;

use super::layout_tests::{fill, harness, Harness};
use super::{model, tooltip, Hit, SIZE};

/// A device context drawing into a `w` x `h` pixel bitmap, and that bitmap.
fn target(w: u32, h: u32) -> (ID2D1DeviceContext, ID2D1Bitmap1) {
    unsafe {
        let mut dev = None;
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None).unwrap();
        let dxgi: IDXGIDevice = dev.unwrap().cast().unwrap();
        let f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
        let dc = f.CreateDevice(&dxgi).unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap();
        let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
        let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET, ..Default::default() };
        let bmp = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap();
        dc.SetTarget(&bmp);
        (dc, bmp)
    }
}

/// The bitmap's pixels (premultiplied BGRA, tightly packed).
fn pixels(dc: &ID2D1DeviceContext, bmp: &ID2D1Bitmap1, w: u32, h: u32) -> Vec<u8> {
    unsafe {
        let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
        let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW, ..Default::default() };
        let cpu = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap();
        cpu.CopyFromBitmap(None, bmp, None).unwrap();
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

fn save_png(px: &[u8], w: u32, h: u32, path: &Path) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
        let bmp = wic.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppPBGRA, w * 4, px).unwrap();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        let stream = wic.CreateStream().unwrap();
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0).unwrap();
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).unwrap();
        enc.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut()).unwrap();
        let frame = frame.unwrap();
        frame.Initialize(None).unwrap();
        frame.SetSize(w, h).unwrap();
        let mut fmt = GUID_WICPixelFormat32bppPBGRA;
        frame.SetPixelFormat(&mut fmt).unwrap();
        frame.WriteSource(&bmp, std::ptr::null()).unwrap();
        frame.Commit().unwrap();
        enc.Commit().unwrap();
    }
}

/// The light or dark look, Flat (opaque), Windows' animations on.
fn set_look(h: &mut Harness, light: bool) {
    set_look_in(h, light, "flat");
}

/// set_look in `finish` ("flat" or "glass": Windows' transparency on,
/// the blur on; paint() lays a stand-in for DWM's acrylic under it).
fn set_look_in(h: &mut Harness, light: bool, finish: &str) {
    let mut sys = crate::syslook::current();
    sys.light = light;
    sys.transparency = finish == "glass";
    sys.high_contrast = None;
    sys.animations = true;
    h.app.cfg.theme.mode = String::new();
    h.app.cfg.theme.finish = finish.into();
    h.app.cfg.effects.blur = true;
    h.app.refresh_look(sys);
    assert_eq!(!h.app.solid, finish == "glass" && !crate::gfx::software_in_force(), "{finish}");
}

/// The finishes a run draws (NOPBAR_SNAP_FINISH: flat or glass; both
/// unset).
fn finishes() -> Vec<&'static str> {
    ["flat", "glass"].into_iter().filter(|f| wanted("NOPBAR_SNAP_FINISH", f)).collect()
}

/// The scales a run draws pages at (NOPBAR_SNAP_SCALES, e.g. `2`; 1x and
/// 1.5x unset).
fn scales() -> Vec<f32> {
    match std::env::var("NOPBAR_SNAP_SCALES") {
        Ok(v) if !v.trim().is_empty() => v.split(',').filter_map(|p| p.trim().parse().ok()).collect(),
        _ => vec![1.0, 1.5],
    }
}

/// A stand-in for DWM's acrylic behind a Glass window (what the page is
/// drawn over on screen): a neutral grey desktop with a document window
/// on it (a title, lines of text, a dark picture), blurred for real
/// (GaussianBlur, as DWM's), under the system's acrylic tint.
unsafe fn acrylic(dc: &ID2D1DeviceContext, w: f32, h: f32, light: bool) {
    use windows::core::Interface;
    use windows::Win32::Graphics::Direct2D::Common::D2D1_COMPOSITE_MODE_SOURCE_OVER;
    use windows::Win32::Graphics::Direct2D::{ID2D1Image, CLSID_D2D1GaussianBlur, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_PROPERTY_TYPE_FLOAT, D2D1_ROUNDED_RECT};
    // Drawn in pixels (the canvas's scale `k` taken off while recording
    // and drawing the picture, so nothing is scaled twice).
    let mut t = Matrix3x2::default();
    dc.GetTransform(&mut t);
    let k = t.M11.abs().max(0.01);
    dc.SetTransform(&Matrix3x2::identity());
    let rect = |l: f32, t: f32, r: f32, b: f32| D2D_RECT_F { left: l * k, top: t * k, right: r * k, bottom: b * k };
    let g = |v: f32| D2D1_COLOR_F { r: v, g: v, b: v, a: 1.0 };
    let fill = |dc: &ID2D1DeviceContext, r: D2D_RECT_F, rad: f32, c: D2D1_COLOR_F| {
        dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad * k, radiusY: rad * k }, &dc.CreateSolidColorBrush(&c, None).unwrap());
    };
    let all = rect(0.0, 0.0, w, h);
    let cl = dc.CreateCommandList().unwrap();
    let old = dc.GetTarget().unwrap();
    dc.SetTarget(&cl);
    fill(dc, all, 0.0, if light { g(0.78) } else { g(0.20) });
    // The document: from a third of the way across to past the right.
    let (dx, dy) = (w * 0.34, 40.0);
    fill(dc, rect(dx, dy, w + 40.0, h - 30.0), 8.0, g(0.98));
    fill(dc, rect(dx + 30.0, dy + 40.0, dx + 330.0, dy + 66.0), 4.0, g(0.12));
    for i in 0..((h - 200.0) / 26.0).max(0.0) as usize {
        let len = [420.0, 380.0, 440.0, 300.0, 410.0, 350.0][i % 6];
        fill(dc, rect(dx + 30.0, dy + 96.0 + i as f32 * 26.0, dx + 30.0 + len, dy + 106.0 + i as f32 * 26.0), 5.0, g(0.22));
    }
    fill(dc, rect(w * 0.70, h * 0.55, w - 30.0, h - 60.0), 6.0, g(0.17));
    dc.SetTarget(&old);
    cl.Close().unwrap();
    let blur = dc.CreateEffect(&CLSID_D2D1GaussianBlur).unwrap();
    let src: ID2D1Image = cl.cast().unwrap();
    blur.SetInput(0, &src, true);
    blur.SetValue(0, D2D1_PROPERTY_TYPE_FLOAT, &(30.0 * k).to_le_bytes()).unwrap();
    // (Hard border: the edges keep the desktop's grey.)
    blur.SetValue(2, windows::Win32::Graphics::Direct2D::D2D1_PROPERTY_TYPE_ENUM, &1u32.to_le_bytes()).unwrap();
    dc.DrawImage(&blur.GetOutput().unwrap(), None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
    dc.SetTransform(&t);
    let all = D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h };
    let tint = if light { D2D1_COLOR_F { r: 0.95, g: 0.95, b: 0.95, a: 0.55 } } else { D2D1_COLOR_F { r: 0.13, g: 0.13, b: 0.13, a: 0.55 } };
    dc.FillRectangle(&all, &dc.CreateSolidColorBrush(&tint, None).unwrap());
}

/// One paint of the window at `w` x `h` DIPs and `scale`: its hits, the
/// content's height, and the pixels.
fn paint(h: &mut Harness, w: f32, ht: f32, scale: f32) -> (Vec<(D2D_RECT_F, Hit)>, f32, Vec<u8>, u32, u32) {
    SIZE.with(|s| s.set((w, ht)));
    h.app.scale = scale;
    h.app.page_anim = None;
    let (pw, ph) = ((w * scale).round() as u32, (ht * scale).round() as u32);
    let (dc, bmp) = target(pw, ph);
    let mut hits = Vec::new();
    let content_h = unsafe {
        dc.BeginDraw();
        dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
        if !h.app.solid {
            acrylic(&dc, w, ht, h.app.light);
        }
        let c = h.app.paint(&dc, &mut hits).unwrap();
        dc.EndDraw(None, None).unwrap();
        c
    };
    let px = pixels(&dc, &bmp, pw, ph);
    (hits, content_h, px, pw, ph)
}

/// The window as tall as page `section` needs (no scrolling: all of it
/// in one picture), at least its default height.
fn full_height(h: &mut Harness) -> f32 {
    let (_, content_h, ..) = paint(h, super::W0, 4000.0, 1.0);
    let top = match &h.app.welcome {
        Some(_) => 0.0,
        None => h.app.content_top(),
    };
    if h.app.welcome.is_some() {
        super::H0
    } else {
        (top + content_h + super::FOOTER).max(super::H0).ceil()
    }
}

#[test]
#[ignore = "writes the companion settings page offscreen"]
fn harmony_companion_settings() {
    use crate::gfx::promo_kit as kit;
    for lang in ["ko", "en"] {
        let _l = crate::i18n::test_lock(lang);
        let mut h = harness(false);
        h.app.section = h
            .app
            .sections
            .iter()
            .position(|s| s.tab == model::Tab::Companion)
            .unwrap();
        for light in [true, false] {
            for dpi in [1.0, 1.5] {
                set_look(&mut h, light);
                let ht = full_height(&mut h);
                let (_, _, px, w, hh) = paint(&mut h, super::W0, ht, dpi * 2.0);
                let folder = format!(
                    "companion/{}-{}",
                    if light { "light" } else { "dark" },
                    (dpi * 100.0) as u32
                );
                kit::save_png(
                    &px,
                    w,
                    hh,
                    &kit::harmony_dir()
                        .join(folder)
                        .join(format!("settings-{lang}.png")),
                );
            }
        }
    }
}

#[test]
#[ignore = "writes companion bubble settings offscreen; no live connections"]
fn harmony_companion_bubble_settings() {
    use crate::companion::chat::{cli,session};
    let _lang=crate::i18n::test_lock("ko");
    let _detection=cli::test_detections(cli::Status::Missing,cli::Status::Missing);
    let _account=session::test_snapshot(session::Snapshot::default());
    let root=out_root().parent().unwrap().join("companion-bubble");
    let mut h=harness(false);
    h.app.section=h.app.sections.iter().position(|s|s.tab==model::Tab::CompanionAi).unwrap();
    h.app.cfg.chat=Default::default();
    let mut index=String::new();
    for light in [true,false] {for dpi in [1.,1.5] {for enabled in [false,true] {
        set_look(&mut h,light);h.app.cfg.chat.keep_history=enabled;
        let(_,_,px,w,hh)=paint(&mut h,super::W0,super::H0,dpi);
        let name=format!("settings-{}-dpi{}-keep{}.png",if light{"light"}else{"dark"},(dpi*100.) as u32,u8::from(enabled));
        save_png(&px,w,hh,&root.join(&name));index+=&format!("- [{name}]({name})\n");
    }}}
    use std::io::Write;
    let mut file=std::fs::OpenOptions::new().create(true).append(true).open(root.join("INDEX.md")).unwrap();
    file.write_all(index.as_bytes()).unwrap();
}

#[test]
#[ignore = "offscreen quota and local history settings with a fixture account"]
fn gnome_fixes_settings_matrix() {
    use crate::companion::chat::{cli, session, state::Provider};
    let _language = crate::i18n::test_lock("ko");
    let _detection = cli::test_detections(cli::Status::Missing, cli::Status::Missing);
    let _account = session::test_snapshot(session::Snapshot {
        key: Some("snapshot-only".into()),
        label: "테스트 계정".into(),
        accounts: vec!["테스트 계정".into()],
    });
    let _dialog = crate::companion::chat::install::test_snapshot(None);
    let root = PathBuf::from(std::env::var_os("NOPBAR_GNOME_RENDER_DIR").expect("render output directory"));
    let mut h = harness(false);
    h.app.cfg.chat.activate(Provider::ChatGpt);
    h.app.section = h.app.sections.iter().position(|s| s.tab == model::Tab::CompanionAi).unwrap();
    for (light, name) in [(true, "light"), (false, "dark")] {
        set_look(&mut h, light);
        for (daily_limit, keep_history) in [(100, false), (0, true)] {
            h.app.cfg.chat.daily_limit = daily_limit;
            h.app.cfg.chat.keep_history = keep_history;
            let height = full_height(&mut h);
            let (_, _, px, w, height) = paint(&mut h, super::W0, height, 1.);
            let file = root.join("settings").join(format!("{name}-limit-{daily_limit}.png"));
            save_png(&px, w, height, &file);
        }
    }
}

#[test]
#[ignore = "offscreen companion capture settings and quota boundary states"]
fn gnome_fixes_review2_settings() {
    use crate::companion::chat::{cli, session, state::Provider};
    let _language = crate::i18n::test_lock("ko");
    let _detection = cli::test_detections(cli::Status::Missing, cli::Status::Missing);
    let _account = session::test_snapshot(session::Snapshot {
        key: Some("snapshot-only".into()),
        label: "테스트 계정".into(),
        accounts: vec!["테스트 계정".into()],
    });
    let _dialog = crate::companion::chat::install::test_snapshot(None);
    let root = PathBuf::from(std::env::var_os("NOPBAR_GNOME_RENDER_DIR").expect("render output directory"))
        .join("review2");
    let mut h = harness(false);
    h.app.cfg.chat.activate(Provider::ChatGpt);
    for (light, palette) in [(true, "light"), (false, "dark")] {
        set_look(&mut h, light);
        for (tab, name, limit) in [
            (model::Tab::Companion, "companion-capture", 10),
            (model::Tab::CompanionAi, "limit-10", 10),
            (model::Tab::CompanionAi, "limit-1000", 1000),
            (model::Tab::CompanionAi, "limit-unlimited", 0),
        ] {
            h.app.section = h.app.sections.iter().position(|s| s.tab == tab).unwrap();
            h.app.cfg.chat.daily_limit = limit;
            let height = full_height(&mut h);
            let (_, _, px, w, height) = paint(&mut h, super::W0, height, 1.);
            save_png(&px, w, height, &root.join(format!("{palette}-{name}.png")));
        }
    }
}

fn out_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("NOPBAR_SNAP_DIR") {
        return PathBuf::from(dir);
    }
    // CARGO_MANIFEST_DIR is dock/: the workspace's target/ is one up.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join("ui-snapshots").join("settings")
}

fn wanted(list: &str, name: &str) -> bool {
    match std::env::var(list) {
        Ok(v) if !v.trim().is_empty() => v.split(',').any(|p| name.contains(p.trim())),
        _ => true,
    }
}

/// A file-name-safe name for page `i`.
fn page_name(h: &Harness, i: usize) -> String {
    let sec = &h.app.sections[i];
    let tab = format!("{:?}", sec.tab).to_lowercase();
    let title = sec.title.rsplit('.').next().unwrap_or("page");
    format!("{i:02}-{tab}-{title}")
}

fn shoot(h: &mut Harness, dir: &Path, name: &str, scale: f32, ht: f32, out: &mut Vec<PathBuf>) {
    let (_, _, px, pw, ph) = paint(h, super::W0, ht, scale);
    let p = dir.join(format!("{name}.png"));
    save_png(&px, pw, ph, &p);
    out.push(p);
}

/// Every list filled and every fold open, Win+digit on, two shortcuts on
/// the same keys (layout_tests' fill: the notes under rows show), and the
/// widgets measured (their heading's word shows).
fn filled(h: &mut Harness) {
    fill(&mut h.app);
    h.app.client.widgets = Some(crate::desklayer::load::status(
        &[
            (crate::desklayer::load::Kind::Notes, crate::desklayer::load::Usage { count: 2, mem: 1 << 20, cpu: 0.0 }),
            (crate::desklayer::load::Kind::Pieces, crate::desklayer::load::Usage { count: 1, mem: 6 << 20, cpu: 0.0 }),
        ],
        crate::desklayer::load::Spec { ram: 16 << 30, cores: 8 },
    ));
}

/// Every page, sub-page and first-run step in `lang`, light and dark, at
/// 1x and 1.5x; then every page again filled (`filled`).
fn pages(lang: &str, out: &mut Vec<PathBuf>) {
    let _l = crate::i18n::test_lock(lang);
    let mut h = harness(false);
    for (variant, full) in [("", false), ("-filled", true)] {
        if full {
            filled(&mut h);
        }
        for (light, finish) in [true, false].into_iter().flat_map(|l| finishes().into_iter().map(move |f| (l, f))) {
            set_look_in(&mut h, light, finish);
            for scale in scales() {
                let dir = out_root().join(lang).join(format!("{}-{finish}-{scale}x{variant}", if light { "light" } else { "dark" }));
                for i in 0..h.app.sections.len() {
                    let name = page_name(&h, i);
                    if !wanted("NOPBAR_SNAP_PAGES", &name) {
                        continue;
                    }
                    h.app.section = i;
                    h.app.scroll = 0.0;
                    let ht = full_height(&mut h);
                    shoot(&mut h, &dir, &name, scale, ht, out);
                }
            }
        }
    }
    drop(h);
    let mut h = harness(true);
    for (light, finish) in [true, false].into_iter().flat_map(|l| finishes().into_iter().map(move |f| (l, f))) {
        set_look_in(&mut h, light, finish);
        for scale in scales() {
            let dir = out_root().join(lang).join(format!("{}-{finish}-{scale}x", if light { "light" } else { "dark" }));
            for step in 0..super::welcome::STEPS {
                let name = format!("welcome-{step}");
                if !wanted("NOPBAR_SNAP_PAGES", &name) {
                    continue;
                }
                if let Some(w) = h.app.welcome.as_mut() {
                    w.flow.step = step;
                    w.rows = model::welcome_rows(step);
                }
                h.app.focus = None;
                shoot(&mut h, &dir, &name, scale, super::H0, out);
            }
        }
    }
}

/// The Shortcuts page (General and Top bar too), filled, with an "i"
/// hovered (its tip up), an "i" focused by the keyboard, a key field and a
/// list button focused, an item of the bar's picture hovered, at 1x and 2x
/// (to see the ring against the glyph).
fn states(lang: &str, out: &mut Vec<PathBuf>) {
    let _l = crate::i18n::test_lock(lang);
    let mut h = harness(false);
    filled(&mut h);
    let shortcuts = h.app.sections.iter().position(|s| s.kind == model::Kind::Shortcuts);
    let general = h.app.sections.iter().position(|s| s.tab == model::Tab::General);
    let topbar = h.app.sections.iter().position(|s| s.kind == model::Kind::TopBar);
    for (light, finish) in [true, false].into_iter().flat_map(|l| finishes().into_iter().map(move |f| (l, f))) {
        set_look_in(&mut h, light, finish);
        let theme = format!("{}-{finish}", if light { "light" } else { "dark" });
        for i in [shortcuts, general, topbar].into_iter().flatten() {
            h.app.section = i;
            h.app.scroll = 0.0;
            let name = page_name(&h, i);
            let ht = full_height(&mut h);
            let (hits, ..) = paint(&mut h, super::W0, ht, 1.0);
            let info = hits.iter().map(|(_, x)| *x).find(|x| matches!(x, Hit::Info(_)));
            let key = hits.iter().map(|(_, x)| *x).find(|x| matches!(x, Hit::Shortcut(_)));
            let menu = hits.iter().map(|(_, x)| *x).find(|x| matches!(x, Hit::Menu(_)));
            let cell = hits.iter().map(|(_, x)| *x).filter(|x| matches!(x, Hit::BarCell(_))).nth(2);
            let dir = out_root().join(lang).join("states");
            for scale in [1.0f32, 2.0] {
                if let Some(Hit::Info(id)) = info {
                    h.app.keyboard = false;
                    h.app.focus = None;
                    h.app.hover = Some(Hit::Info(id));
                    h.app.tip = Some(tooltip::Tip { ri: id, from: std::time::Instant::now() - std::time::Duration::from_secs(5), hide: None });
                    shoot(&mut h, &dir, &format!("{name}-{theme}-{scale}x-info-hover"), scale, ht, out);
                    h.app.hover = None;
                    h.app.keyboard = true;
                    h.app.focus = Some(Hit::Info(id));
                    shoot(&mut h, &dir, &format!("{name}-{theme}-{scale}x-info-focus"), scale, ht, out);
                    h.app.tip = None;
                }
                for (what, f) in [("key", key), ("menu", menu)] {
                    let Some(f) = f else { continue };
                    h.app.keyboard = true;
                    h.app.focus = Some(f);
                    h.app.hover = None;
                    shoot(&mut h, &dir, &format!("{name}-{theme}-{scale}x-{what}-focus"), scale, ht, out);
                }
                if let Some(c) = cell {
                    h.app.keyboard = false;
                    h.app.focus = None;
                    h.app.hover = Some(c);
                    shoot(&mut h, &dir, &format!("{name}-{theme}-{scale}x-bar-hover"), scale, ht, out);
                }
                h.app.keyboard = false;
                h.app.focus = None;
                h.app.hover = None;
            }
            // The notes off: the row under them hidden; every widget off:
            // no word on the heading.
            if Some(i) == general {
                let dir = out_root().join(lang).join("states");
                h.app.cfg.widgets.notes = false;
                let off_h = full_height(&mut h);
                shoot(&mut h, &dir, &format!("{name}-{theme}-1x-parents-off"), 1.0, off_h, out);
                h.app.cfg.widgets.pieces = false;
                shoot(&mut h, &dir, &format!("{name}-{theme}-1x-widgets-off"), 1.0, off_h, out);
                (h.app.cfg.widgets.notes, h.app.cfg.widgets.pieces) = (true, true);
            }
        }
    }
}

/// The app's shared wrapping (gfx::text_layout_wrapped_leading: tips,
/// panels, the dock's stack names) on real sentences of `lang`, each at
/// three widths: on the left as DirectWrite alone breaks it, on the right
/// as the app does (gfx::keep_words). One picture per language.
fn wrap_sheet(lang: &str, out: &mut Vec<PathBuf>) {
    use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
    let _l = crate::i18n::test_lock(lang);
    let mut h = harness(false);
    set_look(&mut h, false);
    let keys = ["shortcuts.note.hook", "settings.input.remember.hint", "shortcuts.win_tap.hint", "settings.topbar.island_screenshot.hint", "settings.general.tray_icon.hint"];
    let widths = [150.0f32, 200.0, 260.0];
    let (col_w, gap, size, scale) = (280.0f32, 16.0f32, 13.0f32, 1.5f32);
    let row_h = 120.0;
    let (w, ht) = (16.0 + 2.0 * (col_w + gap), 16.0 + (keys.len() * widths.len()) as f32 * row_h);
    let (pw, ph) = ((w * scale) as u32, (ht * scale) as u32);
    let (dc, bmp) = target(pw, ph);
    let app = &h.app;
    unsafe {
        dc.BeginDraw();
        dc.Clear(Some(&D2D1_COLOR_F { r: 0.13, g: 0.13, b: 0.13, a: 1.0 }));
        dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
        let ink = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.92, g: 0.92, b: 0.92, a: 1.0 }, None).unwrap();
        let guide = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.9, g: 0.3, b: 0.3, a: 0.6 }, None).unwrap();
        let fam = app.look.font.family.clone();
        let mut y = 16.0;
        for key in keys {
            let text = crate::i18n::t(key);
            for bw in widths {
                for (col, joined) in [(0.0f32, false), (1.0, true)] {
                    let x = 16.0 + col * (col_w + gap);
                    let l = if joined {
                        app.gfx.text_layout_wrapped_leading(text, &fam, size, DWRITE_FONT_WEIGHT_NORMAL, bw, 20).unwrap().0
                    } else {
                        let (l, ..) = app.gfx.text_layout(text, &fam, size, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
                        l.SetMaxWidth(bw).unwrap();
                        l
                    };
                    dc.FillRectangle(&D2D_RECT_F { left: x + bw, top: y, right: x + bw + 1.0, bottom: y + row_h - 12.0 }, &guide);
                    dc.DrawTextLayout(windows_numerics::Vector2 { X: x, Y: y }, &l, &ink, windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                }
                y += row_h;
            }
        }
        dc.EndDraw(None, None).unwrap();
    }
    let px = pixels(&dc, &bmp, pw, ph);
    let p = out_root().join("wrap").join(format!("{lang}.png"));
    save_png(&px, pw, ph, &p);
    out.push(p);
}

#[test]
#[ignore = "writes PNGs to look at: cargo test -p dock settings::snapshots -- --ignored"]
fn render_settings_snapshots() {
    let mut out = Vec::new();
    for lang in ["ko", "ja", "zh-cn", "en", "de"] {
        if !wanted("NOPBAR_SNAP_LANGS", lang) {
            continue;
        }
        wrap_sheet(lang, &mut out);
        pages(lang, &mut out);
        states(lang, &mut out);
    }
    println!("{} pictures under {}", out.len(), out_root().display());
    assert!(!out.is_empty());
}

#[test]
#[ignore = "notes entry settings acceptance: Korean, glass/flat, enabled/disabled"]
fn notes_entry_settings() {
    let _language = crate::i18n::test_lock("ko");
    let mut h = harness(false);
    h.app.section = h.app.sections.iter().position(|s| s.tab == model::Tab::General).unwrap();
    let root = crate::gfx::promo_kit::harmony_dir().join("notes-entry");
    let mut out = Vec::new();
    for (light, finish) in [(false, "glass"), (true, "flat")] {
        set_look_in(&mut h, light, finish);
        for scale in [1., 1.5] {
            for enabled in [true, false] {
                h.app.cfg.widgets.notes = enabled;
                let height = full_height(&mut h);
                shoot(&mut h, &root, &format!("settings-{}-{finish}-{scale}x-{}", if light { "light" } else { "dark" }, if enabled { "on" } else { "off" }), scale, height, &mut out);
            }
        }
    }
    assert_eq!(out.len(), 8);
}

/// The widgets' how-to cards (howto.rs) at a few moments of their loop,
/// light and dark, at 2x (`howto/<widget>-<theme>-<pct>.png`), and 24
/// frames of each loop, light at 1.5x, as raw premultiplied BGRA
/// (`howto/frames/<widget>-<nn>.bgra`, size in `<widget>.size`) to make
/// a GIF from.
#[test]
#[ignore = "writes PNGs to look at: cargo test -p dock settings::snapshots::render_howto -- --ignored"]
fn render_howto_cards() {
    use crate::desklayer::load::Kind;
    use super::howto;
    let _l = crate::i18n::test_lock("ko");
    let mut h = harness(false);
    let root = out_root().join("howto");
    let pad = 20.0f32;
    let shot = |h: &mut Harness, k: Kind, t: f32, scale: f32| -> (Vec<u8>, u32, u32) {
        let (w, ht) = (howto::W + pad * 2.0, howto::H + pad * 2.0);
        let (pw, ph) = ((w * scale).round() as u32, (ht * scale).round() as u32);
        let (dc, bmp) = target(pw, ph);
        let keys = h.app.widget_keys(k);
        unsafe {
            dc.BeginDraw();
            let g = if h.app.light { 0.95 } else { 0.13 };
            dc.Clear(Some(&D2D1_COLOR_F { r: g, g, b: g, a: 1.0 }));
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
            let r = D2D_RECT_F { left: pad, top: pad, right: pad + howto::W, bottom: pad + howto::H };
            h.app.paint_howto_card(&dc, r, k, t, &keys, 1.0).unwrap();
            dc.EndDraw(None, None).unwrap();
        }
        (pixels(&dc, &bmp, pw, ph), pw, ph)
    };
    let mut n = 0;
    for light in [true, false] {
        set_look(&mut h, light);
        let theme = if light { "light" } else { "dark" };
        for k in Kind::ALL {
            for pct in [0u32, 15, 30, 40, 55, 75, 85] {
                let (px, pw, ph) = shot(&mut h, k, pct as f32 / 100.0, 2.0);
                save_png(&px, pw, ph, &root.join(format!("{}-{theme}-{pct:02}.png", k.id())));
                n += 1;
            }
            if light {
                let frames = root.join("frames");
                std::fs::create_dir_all(&frames).unwrap();
                for i in 0..24 {
                    let (px, pw, ph) = shot(&mut h, k, i as f32 / 24.0, 1.5);
                    std::fs::write(frames.join(format!("{}-{i:02}.bgra", k.id())), &px).unwrap();
                    std::fs::write(frames.join(format!("{}.size", k.id())), format!("{pw} {ph}")).unwrap();
                }
            }
        }
    }
    println!("{n} cards under {}", root.display());
}

/// States for the design audit (tests only).
#[path = "snapshots_harmony.rs"]
mod harmony;
pub(crate) use harmony::font_compare;

#[path = "snapshots_companion_polish.rs"]
mod companion_polish;

#[path = "snapshots_settings_ia.rs"]
mod settings_ia;
#[path = "snapshots_device_battery.rs"]
mod device_battery;
