//! What the promo pictures share (tests only; promo/README.md): a software
//! (WARP) Direct2D target to draw on, its pixels written as a PNG, the
//! theme as the film uses it, and the folder the pictures go to
//! (`promo/public/shots/`). Nothing is shown on screen and the screen is
//! never read: each scene is drawn here by NopBar's own drawing code.
//!
//! The pictures are made by the ignored tests named `promo_*`, once for
//! each language of the film (`LANGS`; `PROMO_LANG=en` narrows it to one):
//!
//! ```text
//! cargo test -p dock promo_ -- --ignored
//! ```

use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use windows::core::{Interface, HSTRING};
use windows::Win32::Foundation::{GENERIC_WRITE, HMODULE};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ,
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1, D2D1_MAP_OPTIONS_READ, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA, IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows_numerics::Matrix3x2;

use crate::theme::{Rgba, Theme};

/// Every picture is drawn at twice its DIPs, or at `HARMONY_SCALE`
/// (the audit's pictures at 1 or 1.5, as the screens are).
pub fn scale() -> f32 {
    std::env::var("HARMONY_SCALE").ok().and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite() && *v >= 0.5 && *v <= 4.0).unwrap_or(2.0)
}

/// The two looks, by the folder their pictures go to.
pub const LOOKS: [(&str, bool); 2] = [("light", true), ("dark", false)];

/// The film's languages (the i18n catalogs' tags). No others are made.
pub const LANGS: [&str; 2] = ["ko", "en"];

/// The languages this run draws: all of `LANGS`, or the one named in the
/// environment (`PROMO_LANG=en`).
pub fn langs() -> Vec<&'static str> {
    langs_for(std::env::var("PROMO_LANG").ok().as_deref())
}

fn langs_for(only: Option<&str>) -> Vec<&'static str> {
    match only.map(str::trim).filter(|s| !s.is_empty()) {
        Some(tag) => {
            let picked: Vec<_> = LANGS.into_iter().filter(|l| l.eq_ignore_ascii_case(tag)).collect();
            assert!(!picked.is_empty(), "PROMO_LANG={tag}: the film is made in {LANGS:?} only");
            picked
        }
        None => LANGS.to_vec(),
    }
}

/// Every picture of a scene, once for each language of this run: NopBar's
/// own words come from that language's catalog (the switch is held while
/// `scene` draws), the made-up ones from `say`, and the pictures go to
/// that language's folder (`out_dir`).
pub fn each_lang(mut scene: impl FnMut()) {
    for lang in langs() {
        let _l = crate::i18n::test_lock(lang);
        scene();
    }
}

/// A made-up word of the film (an app's name, a note's text) in the
/// language being drawn.
pub fn say(ko: &'static str, en: &'static str) -> &'static str {
    if crate::i18n::current() == "ko" { ko } else { en }
}

thread_local! {
    /// Where this thread's pictures go instead (None: their own folders).
    static SHOT_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn shot_root() -> Option<PathBuf> {
    SHOT_ROOT.with(|r| r.borrow().clone())
}

/// Runs `f` with every picture this thread makes (out_dir, harmony_dir)
/// going to `root/<light|dark>/` instead (the font comparison's folder).
pub fn with_shot_root<R>(root: PathBuf, f: impl FnOnce() -> R) -> R {
    let before = SHOT_ROOT.with(|r| r.replace(Some(root)));
    let out = f();
    SHOT_ROOT.with(|r| *r.borrow_mut() = before);
    out
}

/// `promo/public/shots/<language>/`: the film reads its pictures from
/// there (the language is the one being drawn: see `each_lang`).
pub fn out_dir() -> PathBuf {
    if let Some(root) = shot_root() {
        return root;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("promo").join("public").join("shots").join(crate::i18n::current())
}

/// The built-in theme as the film shows it: glass (blur on, Windows'
/// transparency on), no high contrast, and a grey for Windows' accent
/// (the film has no color: design direction). `HARMONY_FINISH` (flat or
/// glass) draws the audit pictures in that finish; unset, the theme's own
/// (Flat).
pub fn theme(light: bool) -> Theme {
    let grey = if light { 0x3A } else { 0xE6 };
    let sys = crate::syslook::SysLook { light, accent: Rgba { r: grey, g: grey, b: grey, a: 255 }, transparency: true, high_contrast: None, animations: true, ui_font: 0 };
    let mut raw = Theme::graphite();
    if let Some(f) = run_finish() {
        raw.finish = f;
    }
    raw.resolve_fx(&sys, true)
}

/// The finish this run draws in (`HARMONY_FINISH`: flat or glass; None:
/// unset). Anything else stops the run (2026-11-11: the two finishes).
pub fn run_finish() -> Option<crate::theme::Finish> {
    std::env::var("HARMONY_FINISH").ok().filter(|f| !f.trim().is_empty()).map(|f| harmony_finish(&f).unwrap_or_else(|| panic!("HARMONY_FINISH={f:?}: flat or glass")))
}

/// Pure: HARMONY_FINISH's text as a finish (None: not one of the two).
fn harmony_finish(f: &str) -> Option<crate::theme::Finish> {
    match f.trim().to_ascii_lowercase().as_str() {
        "flat" => Some(crate::theme::Finish::Flat),
        "glass" => Some(crate::theme::Finish::Glass),
        _ => None,
    }
}

/// The finish of an audit sheet's `look`: "glass" is itself; the
/// sheet's own look ("flat", the base
/// every sheet is drawn in) is the run's finish (run_finish; unset:
/// Flat), so a run in each finish draws every sheet in it (final audit 7:
/// the bar's, the dock's, Launchpad's, the menus' states and the
/// island's cards stayed Flat whatever HARMONY_FINISH said).
pub fn look_finish(look: &str) -> crate::theme::Finish {
    finish_of(look, run_finish())
}

/// Pure: look_finish with the run's finish `run`.
fn finish_of(look: &str, run: Option<crate::theme::Finish>) -> crate::theme::Finish {
    use crate::theme::Finish;
    match look {
        "glass" => Finish::Glass,
        _ => run.unwrap_or(Finish::Flat),
    }
}

pub fn col(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

/// A color as CSS writes it.
pub fn css(c: Rgba) -> String {
    format!("rgba({},{},{},{:.3})", c.r, c.g, c.b, c.a as f32 / 255.0)
}

/// Shadow layers (x, y, blur, color) as the film's JSON.
pub fn shadows_json(layers: &[(f32, f32, f32, Rgba)]) -> serde_json::Value {
    serde_json::Value::Array(layers.iter().map(|(x, y, blur, c)| serde_json::json!({ "x": x, "y": y, "blur": blur, "color": css(*c) })).collect())
}

/// A transparent picture `w` x `h` DIPs at scale(), being drawn on (in DIPs).
pub struct Canvas {
    pub dc: ID2D1DeviceContext,
    pub factory: ID2D1Factory1,
    bmp: ID2D1Bitmap1,
    pub w: u32,
    pub h: u32,
}

impl Canvas {
    pub fn new(w_dip: f32, h_dip: f32) -> Canvas {
        Self::at(w_dip, h_dip, scale())
    }

    pub fn at(w_dip: f32, h_dip: f32, scale: f32) -> Canvas {
        let factory: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap() };
        Self::of(factory, w_dip, h_dip, scale)
    }

    /// A canvas of `factory` (a Gfx's: the geometries a panel makes with
    /// its own factory can be drawn on it).
    pub fn of(factory: ID2D1Factory1, w_dip: f32, h_dip: f32, scale: f32) -> Canvas {
        let (w, h) = ((w_dip * scale).ceil().max(1.0) as u32, (h_dip * scale).ceil().max(1.0) as u32);
        unsafe {
            let mut dev = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None).unwrap();
            let dxgi: IDXGIDevice = dev.unwrap().cast().unwrap();
            let dc = factory.CreateDevice(&dxgi).unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap();
            let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
            let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET, ..Default::default() };
            let bmp = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap();
            dc.SetTarget(&bmp);
            dc.BeginDraw();
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
            Canvas { dc, factory, bmp, w, h }
        }
    }

    /// Premultiplied BGRA pixels drawn at (`x`, `y`) DIPs, `side` DIPs a
    /// side; `squircle`: cut to the dock's squircle (gfx::squircle_geometry,
    /// as Gfx::surface_fit cuts the dock's icons).
    pub fn icon(&self, icon: &crate::icons::Icon, x: f32, y: f32, side: f32, squircle: bool) {
        self.icon_at(icon, x, y, side, squircle, 1.0)
    }

    /// `icon` at `opacity` (a pressed dock icon's dim).
    pub fn icon_at(&self, icon: &crate::icons::Icon, x: f32, y: f32, side: f32, squircle: bool, opacity: f32) {
        unsafe {
            if squircle {
                let geom = super::squircle_geometry(&self.factory, x, y, side).unwrap();
                let params = D2D1_LAYER_PARAMETERS1 {
                    contentBounds: D2D_RECT_F { left: x, top: y, right: x + side, bottom: y + side },
                    geometricMask: ManuallyDrop::new(Some(geom.cast().unwrap())),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity: 1.0,
                    opacityBrush: ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                };
                self.dc.PushLayer(&params, None);
                let _ = ManuallyDrop::into_inner(params.geometricMask);
            }
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: ManuallyDrop::new(None),
            };
            let bmp = self.dc.CreateBitmap(D2D_SIZE_U { width: icon.width, height: icon.height }, Some(icon.pixels.as_ptr() as _), icon.width * 4, &props).unwrap();
            let dest = D2D_RECT_F { left: x, top: y, right: x + side, bottom: y + side };
            self.dc.DrawBitmap(&bmp, Some(&dest), opacity, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
            if squircle {
                self.dc.PopLayer();
            }
        }
    }

    pub fn fill_rr(&self, r: D2D_RECT_F, radius: f32, c: Rgba) {
        unsafe {
            let b = self.dc.CreateSolidColorBrush(&col(c), None).unwrap();
            self.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &b);
        }
    }

    pub fn stroke_rr(&self, r: D2D_RECT_F, radius: f32, c: Rgba, width: f32) {
        unsafe {
            let b = self.dc.CreateSolidColorBrush(&col(c), None).unwrap();
            self.dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &b, width, None);
        }
    }

    /// The picture's pixels (premultiplied BGRA) and its size in px.
    pub fn finish(self) -> (Vec<u8>, u32, u32) {
        unsafe {
            self.dc.SetTransform(&Matrix3x2::identity());
            self.dc.EndDraw(None, None).unwrap();
            let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
            let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW, ..Default::default() };
            let cpu = self.dc.CreateBitmap(D2D_SIZE_U { width: self.w, height: self.h }, None, 0, &props).unwrap();
            cpu.CopyFromBitmap(None, &self.bmp, None).unwrap();
            let m = cpu.Map(D2D1_MAP_OPTIONS_READ).unwrap();
            let mut out = Vec::with_capacity((self.w * self.h * 4) as usize);
            for y in 0..self.h as usize {
                out.extend_from_slice(std::slice::from_raw_parts(m.bits.add(y * m.pitch as usize), (self.w * 4) as usize));
            }
            cpu.Unmap().unwrap();
            (out, self.w, self.h)
        }
    }

    /// Written to `<out_dir>/<rel>` (a PNG).
    pub fn save(self, rel: &str) {
        let (px, w, h) = self.finish();
        save_png(&px, w, h, &out_dir().join(rel));
    }
}

pub fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// Premultiplied BGRA pixels as a PNG at `path` (its folders made).
pub fn save_png(px: &[u8], w: u32, h: u32, path: &Path) {
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

/// `<out_dir>/<rel>` written as JSON (what the film needs to know of a
/// scene: sizes, places and the theme's colors, as NopBar has them).
pub fn save_json(rel: &str, v: &serde_json::Value) {
    let p = out_dir().join(rel);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(p, serde_json::to_string_pretty(v).unwrap()).unwrap();
}

/// A hidden window and what the composition-backed surfaces need on this
/// thread (a dispatcher queue, a software Gfx): for the panels that are
/// made as in the app and then painted on a Canvas.
pub struct Stage {
    pub gfx: std::rc::Rc<crate::gfx::Gfx>,
    pub hwnd: windows::Win32::Foundation::HWND,
    _queue: windows::System::DispatcherQueueController,
}

thread_local! {
    /// This thread's stage: made once and kept (a thread has one dispatcher
    /// queue; tests run one after the other share their thread).
    static STAGE: std::cell::RefCell<Option<std::rc::Rc<Stage>>> = const { std::cell::RefCell::new(None) };
}

/// COM kept up for the whole test process. windows-rs keeps the
/// Compositor's activation factory for the process; when the one thread
/// with an apartment ends, COM shuts down and unloads what served it, and
/// the next test's `Compositor::new` calls into the kept factory: an access
/// violation. A use of the multithreaded apartment held for good keeps
/// COM, and that factory, alive between tests.
pub(crate) fn keep_com() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let _ = windows::Win32::System::Com::CoIncrementMTAUsage();
    });
}

/// This thread's stage (made on first use).
pub fn stage() -> std::rc::Rc<Stage> {
    keep_com();
    STAGE.with(|s| s.borrow_mut().get_or_insert_with(|| std::rc::Rc::new(Stage::new())).clone())
}

impl Stage {
    fn new() -> Stage {
        use windows::core::w;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, RO_INIT_SINGLETHREADED};
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, RegisterClassExW, WNDCLASSEXW, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPED};
        unsafe extern "system" fn proc(h: windows::Win32::Foundation::HWND, m: u32, w: windows::Win32::Foundation::WPARAM, l: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
            DefWindowProcW(h, m, w, l)
        }
        crate::gfx::set_render_choice("software");
        unsafe {
            let _ = RoInitialize(RO_INIT_SINGLETHREADED);
            let queue = CreateDispatcherQueueController(DispatcherQueueOptions { dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32, threadType: DQTYPE_THREAD_CURRENT, apartmentType: DQTAT_COM_STA }).unwrap();
            let instance = GetModuleHandleW(None).unwrap();
            let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(proc), hInstance: instance.into(), lpszClassName: w!("NopBarPromoStage"), ..Default::default() };
            let _ = RegisterClassExW(&wc);
            // Never shown: only something to own the hidden panels.
            let hwnd = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP, w!("NopBarPromoStage"), w!(""), WS_OVERLAPPED, 0, 0, 100, 100, None, None, Some(instance.into()), None).unwrap();
            Stage { gfx: std::rc::Rc::new(crate::gfx::Gfx::new().unwrap()), hwnd, _queue: queue }
        }
    }

    /// A canvas `w` x `h` DIPs at scale() on this stage's factory.
    pub fn canvas(&self, w_dip: f32, h_dip: f32) -> Canvas {
        Canvas::of(self.gfx.d2d_factory().clone(), w_dip, h_dip, scale())
    }
}

/// A panel's plate as the film draws it (popup::chrome_at: its material's
/// tint over the blur, the rim, the contact shadow and the shadows of
/// elevation `e`), `w` x `h` DIPs.
pub fn panel_json(theme: &Theme, w: f32, h: f32, radius: f32, e: crate::theme::Elevation) -> serde_json::Value {
    serde_json::json!({
        "w": w,
        "h": h,
        "radius": radius,
        "tint": css(theme.material(e.material())),
        "border": css(theme.popup_border()),
        "highlight": theme.panel_highlight().map(css),
        "rim": rim_json(theme),
        "rimStops": crate::theme::RIM_STOPS,
        "contact": css(theme.contact()),
        "shadows": shadows_json(&theme.elevation(e)),
    })
}

/// The rim's three colors as the film reads them (null: none).
pub fn rim_json(theme: &Theme) -> serde_json::Value {
    match theme.rim() {
        Some(stops) => serde_json::Value::Array(stops.iter().map(|c| serde_json::Value::String(css(*c))).collect()),
        None => serde_json::Value::Null,
    }
}

// ---- the design audit's pictures ("harmony"; tests only) ---------------------
// Each picture is a surface on its plate over an opaque backdrop, written
// to `target/harmony-shots/<light|dark>/<name>.png` (nothing of the promo
// film's). Run: `cargo test -p dock harmony_ -- --ignored`.

/// Room around a surface in an audit picture (DIP).
pub const HARMONY_M: f32 = 32.0;

/// `target/harmony-shots/`.
pub fn harmony_dir() -> PathBuf {
    if let Some(root) = shot_root() {
        return root;
    }
    // A run of its own (`HARMONY_OUT`: one finish at one scale).
    if let Some(out) = std::env::var_os("HARMONY_OUT").filter(|v| !v.is_empty()) {
        return PathBuf::from(out);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join("harmony-shots")
}

/// The opaque backdrop of an audit picture.
pub fn harmony_backdrop(light: bool) -> Rgba {
    let v = if light { 0xC4 } else { 0x3A };
    Rgba { r: v, g: v, b: v, a: 255 }
}

/// A canvas for a surface `w` x `h` DIPs with HARMONY_M around it, the
/// backdrop filled in. The surface's top left is (HARMONY_M, HARMONY_M).
pub fn harmony_canvas(stage: &Stage, w: f32, h: f32, light: bool) -> Canvas {
    let cv = stage.canvas(w + HARMONY_M * 2.0, h + HARMONY_M * 2.0);
    unsafe { cv.dc.Clear(Some(&col(harmony_backdrop(light)))) };
    cv
}

/// A surface's plate: what the compositor draws under a panel on screen.
pub struct Plate {
    pub radius: f32,
    pub tint: Rgba,
    /// High contrast's 1 DIP outline (None: none).
    pub border: Option<Rgba>,
    pub highlight: Option<Rgba>,
    pub shadows: Vec<(f32, f32, f32, Rgba)>,
    /// The rim (theme::rim): white at theme::RIM_STOPS down the plate,
    /// `rim_w` DIP wide (Theme::rim_width at the canvas's scale).
    pub rim: Option<[Rgba; 3]>,
    pub rim_w: f32,
    /// The contact shadow (theme::contact; clear: none).
    pub contact: Rgba,
    /// Flat v2's fall from top to bottom (Theme::flat_fill; None: `tint`).
    pub fall: Option<(Rgba, Rgba)>,
    /// The grain (glass::light_of: a glass's, Flat v2's; 0: none).
    pub grain: f32,
    /// What shows through a see-through tint (None: nothing, the tint is
    /// opaque or the plate has no backdrop): on screen the backdrop brush
    /// is opaque (DWM's blur through the glass's graph), so the shadows
    /// under the plate never show through it. Here: the picture's backdrop
    /// through glass::model.
    pub ground: Option<Rgba>,
    /// The glass's top light (glass::light_of; 0: none).
    pub spec: f32,
}

/// What a plate of material `m` shows under its tint (Plate::ground) over
/// the picture's backdrop, as the app's backdrop brush draws it: DWM's blur
/// of a one-colored desktop is that color, through the glass's graph
/// (saturation, the Luminosity layer: glass::model, no tint). None when
/// the tint hides it (`opaque`).
pub fn plate_ground(theme: &Theme, m: crate::theme::Material, opaque: bool) -> Option<Rgba> {
    if opaque {
        return None;
    }
    let bg = harmony_backdrop(theme.is_light);
    let clear = Rgba { a: 0, ..bg };
    let c = match theme.glass_fx(m) {
        Some(fx) => crate::glass::model([bg.r, bg.g, bg.b].map(|v| v as f32 / 255.0), &fx, clear).map(|v| (v * 255.0).round() as u8),
        None => [bg.r, bg.g, bg.b],
    };
    Some(Rgba { r: c[0], g: c[1], b: c[2], a: 255 })
}

/// popup::chrome_at's plate at elevation `e`: its material's tint (opaque
/// when the material is solid or blur is off), the rim, the contact
/// shadow, the theme's own top light and the shadows of `e` (none with the
/// "Shadows" effect off); high contrast's outline instead of the light.
pub fn popup_plate(theme: &Theme, radius: f32, e: crate::theme::Elevation) -> Plate {
    let solid = theme.popup.material == "solid" || !crate::effects::items().blur;
    let m = e.material();
    let t = theme.material(m);
    let rim = theme.rim_of(m);
    let opaque = solid || t.a == 255 || m == crate::theme::Material::Tip;
    let (spec, grain) = crate::glass::light_of(theme, m);
    Plate {
        radius,
        tint: Rgba { a: if solid { 255 } else { t.a }, ..t },
        border: rim.is_none().then(|| theme.popup_border()),
        highlight: theme.panel_highlight(),
        shadows: if crate::effects::items().shadows { theme.elevation(e) } else { Vec::new() },
        rim,
        rim_w: theme.rim_width(e.material(), scale()),
        contact: theme.contact(),
        fall: theme.flat_fill(m),
        grain: if opaque { theme.flat_grain(m) } else { grain },
        ground: plate_ground(theme, m, opaque),
        spec: if opaque { 0.0 } else { spec },
    }
}

/// Draws plate `p` at (`x`, `y`), `w` x `h` DIPs: shadows, the contact
/// ring, tint, top light, rim or outline (as popup::chrome_at stacks them).
pub fn draw_plate(cv: &Canvas, x: f32, y: f32, w: f32, h: f32, p: &Plate) {
    let r = rect(x, y, x + w, y + h);
    unsafe { crate::gfx::soft_shadows(&cv.dc, r, p.radius, &p.shadows).unwrap() };
    if p.contact.a > 0 {
        let k = crate::theme::CONTACT_W;
        cv.stroke_rr(rect(x - k / 2.0, y - k / 2.0, x + w + k / 2.0, y + h + k / 2.0), p.radius + k / 2.0, p.contact, k);
    }
    // The backdrop brush: opaque on screen, so nothing under the plate
    // (its own shadows) shows through the tint.
    if let Some(g) = p.ground {
        cv.fill_rr(r, p.radius, g);
    }
    match p.fall {
        Some(_) => unsafe { crate::gfx::parts::plate_fill(&cv.dc, r, p.radius, p.fall, p.tint).unwrap() },
        None => cv.fill_rr(r, p.radius, p.tint),
    }
    if p.spec > 0.0 {
        draw_top_light(cv, r, p.radius, p.spec);
    }
    if p.grain > 0.0 {
        draw_grain(cv, r, p.radius, p.grain, scale());
    }
    if let Some(hl) = p.highlight {
        cv.fill_rr(rect(x + p.radius, y + 1.0, x + w - p.radius, y + 2.0), 0.0, hl);
    }
    if let Some(stops) = p.rim {
        draw_rim_px(cv, x, y, w, h, p.radius, stops, p.rim_w);
    }
    if let Some(b) = p.border {
        cv.stroke_rr(rect(x + 0.5, y + 0.5, x + w - 0.5, y + h - 0.5), (p.radius - 0.5).max(0.0), b, 1.0);
    }
}

/// A glass's top light over a plate `r` (corners `radius`), as
/// glass::top_light lays it: white at `spec` at the top, gone along
/// glass::SPEC_LINE, and the glow at the top's middle where there is one
/// (glass::SPEC_GLOW; glass A has none).
pub fn draw_top_light(cv: &Canvas, r: D2D_RECT_F, radius: f32, spec: f32) {
    use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
    use windows::Win32::Graphics::Direct2D::{ID2D1RenderTarget, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES};
    let white = |a: f32| D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: a.clamp(0.0, 1.0) };
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
    unsafe {
        let rt: ID2D1RenderTarget = cv.dc.cast().unwrap();
        let line: Vec<D2D1_GRADIENT_STOP> = crate::glass::SPEC_LINE.iter().map(|(at, a)| D2D1_GRADIENT_STOP { position: *at, color: white(spec * a) }).collect();
        let coll = rt.CreateGradientStopCollection(&line, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).unwrap();
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: windows_numerics::Vector2 { X: 0.0, Y: r.top }, endPoint: windows_numerics::Vector2 { X: 0.0, Y: r.top + h } };
        cv.dc.FillRoundedRectangle(&rr, &rt.CreateLinearGradientBrush(&props, None, &coll).unwrap());
        let (gx, gy, k) = crate::glass::SPEC_GLOW;
        if k <= 0.0 {
            return;
        }
        let glow = [D2D1_GRADIENT_STOP { position: 0.0, color: white(spec * k) }, D2D1_GRADIENT_STOP { position: 1.0, color: white(0.0) }];
        let coll = rt.CreateGradientStopCollection(&glow, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).unwrap();
        let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
            center: windows_numerics::Vector2 { X: r.left + w / 2.0, Y: r.top },
            gradientOriginOffset: windows_numerics::Vector2 { X: 0.0, Y: 0.0 },
            radiusX: w * gx,
            radiusY: h * gy,
        };
        cv.dc.FillRoundedRectangle(&rr, &rt.CreateRadialGradientBrush(&props, None, &coll).unwrap());
    }
}

/// The grain over a plate `r` (corners `radius`): glass::noise_tile tiled
/// one tile pixel to a device pixel (the canvas at `s` px per DIP), at
/// `opacity` (as the compositor lays it over the tint).
pub fn draw_grain(cv: &Canvas, r: D2D_RECT_F, radius: f32, opacity: f32, s: f32) {
    use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_BRUSH_PROPERTIES, D2D1_EXTEND_MODE_WRAP, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR};
    let tile = crate::glass::noise_tile();
    let side = crate::glass::TILE;
    unsafe {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            ..Default::default()
        };
        let bmp = cv.dc.CreateBitmap(D2D_SIZE_U { width: side, height: side }, Some(tile.as_ptr() as _), side * 4, &props).unwrap();
        let bp = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_WRAP, extendModeY: D2D1_EXTEND_MODE_WRAP, interpolationMode: D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR };
        let k = 1.0 / s;
        let brush_props = D2D1_BRUSH_PROPERTIES { opacity, transform: Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: 0.0, M32: 0.0 } };
        let brush = cv.dc.CreateBitmapBrush(&bmp, Some(&bp), Some(&brush_props)).unwrap();
        cv.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &brush);
    }
}

/// The rim of a plate at (`x`, `y`), `w` x `h` DIPs: a stroke one device
/// pixel thick just inside its edge, `stops` at theme::RIM_STOPS from its
/// top to its bottom (as popup::chrome_at's and the dock's).
pub fn draw_rim(cv: &Canvas, x: f32, y: f32, w: f32, h: f32, radius: f32, stops: [Rgba; 3]) {
    draw_rim_px(cv, x, y, w, h, radius, stops, 1.0 / scale())
}

/// draw_rim on a canvas of another scale: `px` is its device pixel (DIP).
#[allow(clippy::too_many_arguments)]
pub fn draw_rim_px(cv: &Canvas, x: f32, y: f32, w: f32, h: f32, radius: f32, stops: [Rgba; 3], px: f32) {
    use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
    use windows::Win32::Graphics::Direct2D::{ID2D1RenderTarget, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES};
    unsafe {
        let list: Vec<D2D1_GRADIENT_STOP> = crate::theme::RIM_STOPS.iter().zip(stops).map(|(at, c)| D2D1_GRADIENT_STOP { position: *at, color: col(c) }).collect();
        let rt: ID2D1RenderTarget = cv.dc.cast().unwrap();
        let coll = rt.CreateGradientStopCollection(&list, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).unwrap();
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: windows_numerics::Vector2 { X: 0.0, Y: y }, endPoint: windows_numerics::Vector2 { X: 0.0, Y: y + h } };
        let brush = rt.CreateLinearGradientBrush(&props, None, &coll).unwrap();
        let rr = D2D1_ROUNDED_RECT { rect: rect(x + px / 2.0, y + px / 2.0, x + w - px / 2.0, y + h - px / 2.0), radiusX: (radius - px / 2.0).max(0.0), radiusY: (radius - px / 2.0).max(0.0) };
        cv.dc.DrawRoundedRectangle(&rr, &brush, px, None);
    }
}

/// Runs `f` with the canvas in DIPs from (`x`, `y`): a panel's own paint.
pub fn harmony_at<R>(cv: &Canvas, x: f32, y: f32, f: impl FnOnce(&ID2D1DeviceContext) -> R) -> R {
    unsafe {
        cv.dc.SetTransform(&Matrix3x2 { M11: scale(), M12: 0.0, M21: 0.0, M22: scale(), M31: x * scale(), M32: y * scale() });
        let r = f(&cv.dc);
        cv.dc.SetTransform(&Matrix3x2 { M11: scale(), M12: 0.0, M21: 0.0, M22: scale(), M31: 0.0, M32: 0.0 });
        r
    }
}

/// Runs `f` with the canvas in pixels (a paint that scales by itself).
pub fn harmony_px<R>(cv: &Canvas, f: impl FnOnce(&ID2D1DeviceContext) -> R) -> R {
    unsafe {
        cv.dc.SetTransform(&Matrix3x2::identity());
        let r = f(&cv.dc);
        cv.dc.SetTransform(&Matrix3x2 { M11: scale(), M12: 0.0, M21: 0.0, M22: scale(), M31: 0.0, M32: 0.0 });
        r
    }
}

/// Written to `target/harmony-shots/<folder>/<name>.png`.
pub fn harmony_save(cv: Canvas, folder: &str, name: &str) {
    let (px, w, h) = cv.finish();
    save_png(&px, w, h, &harmony_dir().join(folder).join(format!("{name}.png")));
}

impl Drop for Stage {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_film_is_made_in_korean_and_english_only() {
        assert_eq!(langs_for(None), ["ko", "en"]);
        assert_eq!(langs_for(Some("")), ["ko", "en"]);
        assert_eq!(langs_for(Some("EN")), ["en"]);
        assert_eq!(langs_for(Some(" ko ")), ["ko"]);
    }

    /// A see-through plate's own shadows never show through its tint (on
    /// screen the backdrop brush covers them): below the top light it is
    /// one color from its middle to its bottom, on every finish, light and
    /// dark (final/issues.md 1: a thin light menu went #B1 to #89).
    #[test]
    fn a_plate_does_not_darken_toward_its_bottom() {
        use crate::syslook::SysLook;
        use crate::theme::{Elevation, Finish};
        let s = 1.5;
        let (w, h) = (200.0f32, 240.0f32);
        for finish in [Finish::Flat, Finish::Glass] {
            for light in [false, true] {
                let mut raw = Theme::graphite();
                raw.finish = finish;
                let t = raw.resolve_fx(&SysLook { light, transparency: true, ..Default::default() }, true);
                let plate = popup_plate(&t, 12.0, Elevation::E3);
                if t.glass() {
                    assert!(plate.ground.is_some(), "{finish:?} light {light}: a see-through plate shows its backdrop");
                }
                let m = HARMONY_M;
                let cv = Canvas::at(w + m * 2.0, h + m * 2.0, s);
                unsafe { cv.dc.Clear(Some(&col(harmony_backdrop(light)))) };
                draw_plate(&cv, m, m, w, h, &plate);
                let (px, pw, _) = cv.finish();
                let at = |y: f32| {
                    let (x, y) = (((m + w / 2.0) * s) as usize, ((m + y) * s) as usize);
                    px[(y * pw as usize + x) * 4] as i32
                };
                // (Flat v2 falls a little by design: Theme::flat_fill; a
                // glass's top light falls to its bottom: glass::SPEC_LINE.)
                let fall = t.flat_fill(crate::theme::Material::Panel).map_or(0, |(a, b)| (a.r as i32 - b.r as i32).abs());
                let tone = |y: f32| Rgba { r: 255, g: 255, b: 255, a: (plate.spec * (1.0 - y / h) * 255.0).round() as u8 };
                let (mid, low) = (at(h * 0.5), at(h - 6.0));
                let lift = crate::theme::over(tone(h * 0.5), Rgba { r: 0, g: 0, b: 0, a: 255 }).r as i32;
                assert!((mid - low).abs() <= fall + lift + 2, "{finish:?} light {light}: the middle {mid}, the bottom {low}");
                // And it is its tint over what the backdrop brush shows
                // under its tone, nothing darker under it (the grain at
                // most a level or two).
                if let Some(g) = plate.ground {
                    let want = |y: f32| crate::theme::over(tone(y), crate::theme::over(plate.tint, g)).r as i32;
                    for (what, v, want) in [("middle", mid, want(h * 0.5)), ("bottom", low, want(h - 6.0))] {
                        assert!((v - want).abs() <= 3, "{finish:?} light {light}: the {what} {v}, its tint over its backdrop {want}");
                    }
                }
            }
        }
    }

    /// Final audit 7: a sheet's own look follows the run's finish; the
    /// named glass is itself; unset, Flat as before. HARMONY_FINISH is
    /// flat or glass (2026-11-11), nothing else.
    #[test]
    fn a_sheet_is_drawn_in_the_runs_finish() {
        use crate::theme::Finish;
        for run in [None, Some(Finish::Flat), Some(Finish::Glass)] {
            assert_eq!(finish_of("flat", run), run.unwrap_or(Finish::Flat));
            assert_eq!(finish_of("hc", run), run.unwrap_or(Finish::Flat));
            assert_eq!(finish_of("glass", run), Finish::Glass);
        }
        assert_eq!((harmony_finish("flat"), harmony_finish(" Glass ")), (Some(Finish::Flat), Some(Finish::Glass)));
        for other in ["frost", "clear", "solid", "x"] {
            assert_eq!(harmony_finish(other), None, "{other}");
        }
        // Every harness's own look goes through it.
        for src in [
            include_str!("dock/dock_harmony.rs"),
            include_str!("launchpad/view/arrange_harmony.rs"),
            include_str!("launchpad/view/states_harmony.rs"),
            include_str!("popup_harmony.rs"),
            include_str!("topbar/island/card_harmony.rs"),
            include_str!("topbar/bar_harmony.rs"),
        ] {
            assert!(src.contains("look_finish(look)"));
        }
    }

    #[test]
    #[should_panic(expected = "the film is made in")]
    fn another_language_is_refused() {
        langs_for(Some("ja"));
    }

    #[test]
    fn each_language_has_its_words_and_its_folder() {
        for (lang, word) in [("ko", "독"), ("en", "Dock")] {
            let _l = crate::i18n::test_lock(lang);
            assert_eq!(say("독", "Dock"), word);
            assert!(out_dir().ends_with(Path::new("shots").join(lang)), "{:?}", out_dir());
        }
    }
}
