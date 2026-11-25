//! Image pieces: a picture pasted on the desktop (Ctrl+V there with a
//! picture on the clipboard, desklayer::deskpaste) or a piece of the
//! screen pinned where it was (the "snip" shortcut: overlay.rs) stays on it
//! as a widget of its own, on the desktop layer like the notes. A piece
//! just cut from the screen stays in front, so it looks like the content
//! stayed, until another app comes to the front; then it goes to the
//! desktop like the rest. It can be moved (drag
//! it), resized from its edges (its shape kept) and deleted (the × on
//! hover, or Delete; the picture goes to the Recycle Bin).
//!
//! Each one is drawn once at its size (the file decoded, drawn, the pixels
//! dropped): nothing runs while it sits on the desktop. On the dock's
//! thread; stored by store.rs.

pub mod capture;
pub mod overlay;
pub mod snip;
pub mod store;

use std::cell::RefCell;
use std::rc::Rc;

use windows::core::{w, Interface, Result, HSTRING};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionStretch, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_BRUSH_PROPERTIES, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DELETE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use crate::desklayer::layer::Layer;
use crate::desklayer::load;
use crate::desklayer::place::{self, Place};
use crate::desklayer::win as lw;
use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::theme::Theme;
use crate::desklayer::disk::Store;
use store::Index;

const CLASS: windows::core::PCWSTR = w!("NopBar.ImagePiece");
/// A new piece: at most this long on its longer side, at least this on
/// its shorter (DIP).
pub const FIRST_MAX: f32 = 320.0;
pub const MIN_SIDE: f32 = 48.0;
/// The × in the corner (DIP).
const CLOSE_D: f32 = 22.0;
const CLOSE_INSET: f32 = 6.0;
const EDGE: f32 = 6.0;
/// Where a listed piece goes on screen now: a pasted one inside its
/// display's work area at least MIN_SIDE big; one cut from the screen at
/// its own px against the whole display (place::to_px_screen).
fn px_of(p: &Place, screen: bool, mons: &[crate::monitors::Monitor]) -> Option<RECT> {
    if screen {
        place::to_px_screen(p, mons)
    } else {
        place::to_px(p, mons, (MIN_SIDE, MIN_SIDE))
    }
}

struct Piece {
    id: String,
    hwnd: HWND,
    /// The picture's shape (w / h).
    ratio: f32,
    _target: DesktopWindowTarget,
    image: SpriteVisual,
    close: SpriteVisual,
    hover: bool,
    pressed_close: bool,
}

struct Pieces {
    gfx: Rc<Gfx>,
    theme: Theme,
    list: Vec<Piece>,
    /// pieces.json (never written over while it does not read).
    store: Store<Index>,
    layer: Layer,
    enabled: bool,
    made: u32,
    /// Pieces just cut from the screen, in front until another app's
    /// window (not the one each gave the focus back to) comes to the front.
    fresh: Vec<(HWND, isize)>,
}

thread_local! {
    static PIECES: RefCell<Option<Pieces>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Pieces) -> R) -> Option<R> {
    PIECES.with(|p| p.try_borrow_mut().ok()?.as_mut().map(f))
}

fn scale_of(hwnd: HWND) -> f32 {
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 {
        1.0
    } else {
        d as f32 / 96.0
    }
}

fn client_px(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(1), (r.bottom - r.top).max(1))
}

/// Pure: what a point of a `w`x`h` piece (DIP) is: the × (`close`, in
/// the top corner at the reading end), an edge to resize from, else the
/// piece itself (moved by dragging).
pub fn hit_test(x: f32, y: f32, w: f32, h: f32, rtl: bool) -> u32 {
    let (cx, cy) = close_center(w, rtl);
    if (x - cx).powi(2) + (y - cy).powi(2) <= (CLOSE_D / 2.0).powi(2) {
        return HTCLIENT;
    }
    let (l, r, t, b) = (x < EDGE, x >= w - EDGE, y < EDGE, y >= h - EDGE);
    match (l, r, t, b) {
        (true, _, true, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, ..) => HTLEFT,
        (_, true, ..) => HTRIGHT,
        (_, _, true, _) => HTTOP,
        (.., true) => HTBOTTOM,
        _ => HTCAPTION,
    }
}

fn close_center(w: f32, rtl: bool) -> (f32, f32) {
    let x = if rtl { CLOSE_INSET + CLOSE_D / 2.0 } else { w - CLOSE_INSET - CLOSE_D / 2.0 };
    (x, CLOSE_INSET + CLOSE_D / 2.0)
}

/// The pieces from disk, on the desktop (on the dock's thread, once).
pub fn init(gfx: &Rc<Gfx>, theme: &Theme, cfg: &crate::config::Config) {
    if with(|_| ()).is_some() {
        return;
    }
    let st = store::open();
    // Temporary files a write could not remove (ours only).
    let _ = crate::desklayer::disk::sweep_tmp(&store::dir(), &[""]);
    let listed = st.data.pieces.clone();
    PIECES.with(|p| *p.borrow_mut() = Some(Pieces { gfx: gfx.clone(), theme: theme.clone(), list: Vec::new(), store: st, layer: Layer::Desktop, enabled: cfg.widgets.pieces, made: 0, fresh: Vec::new() }));
    for e in listed {
        // A listed picture that is not there (or a line not ours) is
        // skipped; its line stays.
        if store::valid_id(&e.id) && store::picture(&e.id).exists() {
            open(&e.id, e.place, e.screen);
        }
    }
    report();
}

/// A new piece from a pasted picture (`png`, `w`x`h` px) centered at `at`
/// (screen px). False when it could not be made.
pub fn create(png: &[u8], size: (u32, u32), at: POINT) -> bool {
    let hmon = unsafe { windows::Win32::Graphics::Gdi::MonitorFromPoint(at, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST) };
    let Some(m) = crate::monitors::info(hmon.0 as isize).or_else(crate::monitors::primary) else { return false };
    let s = m.scale();
    let (w, h) = store::first_size(size.0, size.1, s, FIRST_MAX, MIN_SIDE * 2.0);
    let (wp, hp) = ((w * s).round() as i32, (h * s).round() as i32);
    let r = place::clamp(RECT { left: at.x - wp / 2, top: at.y - hp / 2, right: at.x - wp / 2 + wp, bottom: at.y - hp / 2 + hp }, m.work);
    create_in(png, r, false).is_some()
}

/// A new piece of a piece of the screen (`png`) exactly at screen px `r`,
/// shown in front (so the content looks like it stayed) until another
/// app's window than `back` (the one the focus went back to) comes to the
/// front. False when it could not be made.
pub fn create_snip(png: &[u8], r: RECT, back: HWND) -> bool {
    let Some(h) = create_in(png, r, true) else { return false };
    let desk = with(|x| {
        // Another one made before it stays in front with it, and goes
        // with it (never left on top).
        x.fresh.push((h, back.0 as isize));
        x.layer == Layer::Desktop
    })
    .unwrap_or(false);
    if desk {
        lw::to_front(h);
    }
    true
}

/// May a snip make a piece now (the pieces are on and their list reads)?
pub fn can_make() -> bool {
    with(|x| x.enabled && x.store.writable()).unwrap_or(false)
}

/// Writes and shows a new piece at screen px `r`: its window, or None
/// (nothing made; a list that does not read takes no new line and no
/// picture is written for one).
fn create_in(png: &[u8], r: RECT, screen: bool) -> Option<HWND> {
    let Some(n) = with(|p| {
        p.made = p.made.wrapping_add(1);
        p.store.writable().then_some(p.made)
    })
    .flatten() else {
        crate::log!("pieces: the list did not read; no new piece");
        return None;
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let id = store::new_id(now, n);
    let p = place::from_px(r, &crate::monitors::all())?;
    if !with(|x| store::save_new(&mut x.store, &id, png, p, screen)).unwrap_or(false) {
        return None;
    }
    open(&id, p, screen);
    report();
    with(|x| x.list.iter().find(|pc| pc.id == id).map(|pc| pc.hwnd)).flatten()
}

fn open(id: &str, p: Place, screen: bool) {
    let Some((gfx, layer, enabled)) = with(|x| (x.gfx.clone(), x.layer, x.enabled)) else { return };
    let Some(r) = px_of(&p, screen, &crate::monitors::all()) else { return };
    let made = unsafe { make(&gfx, id, r) };
    let piece = match made {
        Ok(pc) => pc,
        Err(e) => {
            crate::log!("pieces: a piece not shown ({e})");
            return;
        }
    };
    let hwnd = piece.hwnd;
    with(|x| x.list.push(piece));
    save_place(hwnd);
    draw(hwnd);
    if enabled || layer == Layer::Summoned {
        // Home screen only: never above other programs (owner 0.1.89).
        let _ = layer;
        lw::to_desktop(hwnd, lw::shell_host());
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

unsafe fn make(gfx: &Rc<Gfx>, id: &str, r: RECT) -> Result<Piece> {
    let hinst = GetModuleHandleW(None)?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: CLASS,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassExW(&wc);
    let hwnd = create_window(r)?;
    let built = (|| -> Result<Piece> {
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as *const _, 4);
        let c = gfx.compositor.clone();
        let interop: ICompositorDesktopInterop = c.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, false)?;
        let root: ContainerVisual = c.CreateContainerVisual()?;
        target.SetRoot(&root)?;
        let image = c.CreateSpriteVisual()?;
        let close = c.CreateSpriteVisual()?;
        close.SetIsVisible(false)?;
        root.Children()?.InsertAtTop(&image)?;
        root.Children()?.InsertAtTop(&close)?;
        let size = picture_size(&store::picture(id)).unwrap_or((1, 1));
        Ok(Piece { id: id.to_string(), hwnd, ratio: size.0 as f32 / size.1.max(1) as f32, _target: target, image, close, hover: false, pressed_close: false })
    })();
    if built.is_err() {
        let _ = DestroyWindow(hwnd);
    }
    built
}

unsafe fn create_window(r: RECT) -> Result<HWND> {
    let hwnd = CreateWindowExW(
        WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW,
        CLASS,
        &HSTRING::from(crate::i18n::t("pieces.window")),
        WS_POPUP,
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        None,
        None,
        Some(GetModuleHandleW(None)?.into()),
        None,
    )?;
    crate::desklayer::privacy::register(hwnd, false);
    crate::desklayer::icons::register_card(hwnd, 0);
    Ok(hwnd)
}

fn factory() -> Option<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.ok()
}

/// A picture file's size (px).
fn picture_size(path: &std::path::Path) -> Option<(u32, u32)> {
    let f = factory()?;
    unsafe {
        let d = f.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, windows::Win32::Foundation::GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = d.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        (w > 0 && h > 0).then_some((w, h))
    }
}

/// The picture decoded at `w`x`h` px (premultiplied BGRA).
fn decode_at(path: &std::path::Path, w: u32, h: u32) -> Option<Vec<u8>> {
    let f = factory()?;
    unsafe {
        let d = f.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, windows::Win32::Foundation::GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = d.GetFrame(0).ok()?;
        let scaler = f.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, w, h, WICBitmapInterpolationModeFant).ok()?;
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut px = vec![0u8; crate::gfx::bgra_len(w, h)?];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut px).ok()?;
        Some(px)
    }
}

/// Draws piece `hwnd` at its size: the picture (decoded for this, then
/// dropped) and its ×.
fn draw(hwnd: HWND) {
    let Some((gfx, theme, id)) = with(|x| x.list.iter().find(|p| p.hwnd == hwnd).map(|p| (x.gfx.clone(), x.theme.clone(), p.id.clone()))).flatten() else { return };
    let (pw, ph) = client_px(hwnd);
    let s = scale_of(hwnd);
    let rad = theme.radii().card * s;
    let pixels = decode_at(&store::picture(&id), pw as u32, ph as u32);
    let surface = gfx.draw(pw as u32, ph as u32, |dc, ox, oy| unsafe {
        dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        let r = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: ox, top: oy, right: ox + pw as f32, bottom: oy + ph as f32 };
        match &pixels {
            Some(px) => {
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let bmp = dc.CreateBitmap(D2D_SIZE_U { width: pw as u32, height: ph as u32 }, Some(px.as_ptr() as _), pw as u32 * 4, &props)?;
                let bprops = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_INTERPOLATION_MODE_LINEAR };
                let brush_props = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: ox, M32: oy } };
                let brush = dc.CreateBitmapBrush(&bmp, Some(&bprops), Some(&brush_props))?;
                dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &brush);
            }
            None => {
                // The file did not read: a quiet card with a picture mark.
                let bg = crate::theme::Rgba { a: 255, ..theme.popup_tint() };
                let b = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: bg.r as f32 / 255.0, g: bg.g as f32 / 255.0, b: bg.b as f32 / 255.0, a: 1.0 }, None)?;
                dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &b);
                let ink = theme.text_secondary();
                crate::glyphs::draw(dc, Glyph::Image, ox + pw as f32 / 2.0, oy + ph as f32 / 2.0, 20.0 * s, D2D1_COLOR_F { r: ink.r as f32 / 255.0, g: ink.g as f32 / 255.0, b: ink.b as f32 / 255.0, a: ink.a as f32 / 255.0 })?;
            }
        }
        Ok(())
    });
    drop(pixels);
    let d = (CLOSE_D * s).ceil();
    let close = gfx.draw(d as u32, d as u32, |dc, ox, oy| unsafe {
        dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        let (bg, ink) = match theme.hc.filter(|_| theme.high_contrast) {
            Some(h) => (h.window, h.text),
            None => (crate::theme::Rgba { a: 235, ..theme.popup_tint() }, theme.ink()),
        };
        let c = |x: crate::theme::Rgba| D2D1_COLOR_F { r: x.r as f32 / 255.0, g: x.g as f32 / 255.0, b: x.b as f32 / 255.0, a: x.a as f32 / 255.0 };
        let b = dc.CreateSolidColorBrush(&c(bg), None)?;
        dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: ox + d / 2.0, Y: oy + d / 2.0 }, radiusX: d / 2.0, radiusY: d / 2.0 }, &b);
        crate::glyphs::draw(dc, Glyph::Close, ox + d / 2.0, oy + d / 2.0, 9.0 * s, c(crate::theme::Rgba { a: 255, ..ink }))
    });
    with(|x| {
        let Some(p) = x.list.iter().find(|p| p.hwnd == hwnd) else { return };
        match surface {
            Ok(surface) => {
                let _ = p.image.SetSize(Vector2 { X: pw as f32, Y: ph as f32 });
                if let Ok(brush) = x.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) {
                    let _ = brush.SetStretch(CompositionStretch::Fill);
                    let _ = p.image.SetBrush(&brush);
                }
            }
            Err(e) => crate::log!("pieces: not drawn ({e})"),
        }
        if let Ok(surface) = close {
            let _ = p.close.SetSize(Vector2 { X: d, Y: d });
            let (cx, cy) = close_center(pw as f32 / s, crate::i18n::rtl());
            let _ = p.close.SetOffset(Vector3 { X: cx * s - d / 2.0, Y: cy * s - d / 2.0, Z: 0.0 });
            if let Ok(brush) = x.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) {
                let _ = p.close.SetBrush(&brush);
            }
        }
    });
}

fn report() {
    with(|x| {
        // Shown or hidden, every piece holds its drawing: counted either way.
        let shown = x.enabled || x.layer == Layer::Summoned;
        let mem: u64 = x
            .list
            .iter()
            .map(|p| {
                let (w, h) = client_px(p.hwnd);
                load::surface_bytes(w, h)
            })
            .sum();
        load::report(load::Kind::Pieces, mem, if shown { x.list.len() as u32 } else { 0 });
    });
}

fn hwnds() -> Vec<HWND> {
    with(|x| x.list.iter().map(|p| p.hwnd).collect()).unwrap_or_default()
}

/// Every piece on the desktop (hidden if Settings has them off).
pub fn dismiss() {
    let enabled = with(|x| {
        x.layer = Layer::Desktop;
        x.fresh.clear();
        x.enabled
    })
    .unwrap_or(true);
    let host = lw::shell_host();
    for h in hwnds() {
        if enabled {
            lw::to_desktop(h, host);
        } else {
            lw::placing(|| unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            });
        }
    }
    report();
}

/// Pure: does a piece just cut from the screen go to the desktop now that
/// `front` came to the front (`back`: the window the focus went back to
/// when it was made)?
pub fn fresh_ends(front: isize, back: isize) -> bool {
    front != back
}

/// Pure: the pieces just cut that stay in front, and those that go to the
/// desktop, now that `front` came to the front.
pub fn split_fresh<T: Copy>(fresh: &[(T, isize)], front: isize) -> (Vec<(T, isize)>, Vec<T>) {
    let mut keep = Vec::new();
    let mut gone = Vec::new();
    for &(h, back) in fresh {
        if fresh_ends(front, back) {
            gone.push(h);
        } else {
            keep.push((h, back));
        }
    }
    (keep, gone)
}

/// The foreground changed to `fg` (desklayer::host's hook; other apps only):
/// a piece just cut goes to the desktop; with Show desktop the pieces
/// come back over the desktop.
pub fn on_foreground(fg: HWND, shell: bool) {
    let ended = with(|x| {
        let (keep, gone) = split_fresh(&x.fresh, fg.0 as isize);
        x.fresh = keep;
        if x.layer == Layer::Desktop {
            gone
        } else {
            Vec::new()
        }
    })
    .unwrap_or_default();
    if !ended.is_empty() {
        let host = lw::shell_host();
        for h in ended {
            lw::to_desktop(h, host);
        }
    }
    let Some((layer, enabled)) = with(|x| (x.layer, x.enabled)) else { return };
    if layer != Layer::Desktop || !enabled || !shell {
        return;
    }
    let host = lw::shell_host();
    for h in hwnds() {
        unsafe {
            if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            }
        }
        lw::to_desktop(h, host);
    }
}

/// Explorer restarted.
pub fn reattach() {
    if with(|x| x.layer == Layer::Desktop && x.enabled).unwrap_or(false) {
        let host = lw::shell_host();
        for h in hwnds() {
            lw::to_desktop(h, host);
        }
    }
}

/// Displays changed: every piece back to its place, kept on screen.
pub fn displays_changed() {
    let mons = crate::monitors::all();
    let places: Vec<(HWND, Place, bool)> = with(|x| {
        x.list
            .iter()
            .filter_map(|p| {
                let e = x.store.data.pieces.iter().find(|e| e.id == p.id)?;
                Some((p.hwnd, e.place, e.screen))
            })
            .collect()
    })
    .unwrap_or_default();
    for (h, p, screen) in places {
        if let Some(r) = px_of(&p, screen, &mons) {
            unsafe {
                let _ = SetWindowPos(h, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }
}

/// Settings changed: shown or not on the desktop.
pub fn apply_config(cfg: &crate::config::Config) {
    let Some((was, layer)) = with(|x| (std::mem::replace(&mut x.enabled, cfg.widgets.pieces), x.layer)) else { return };
    if was != cfg.widgets.pieces && layer == Layer::Desktop {
        dismiss();
    }
    report();
}

pub fn retheme(theme: &Theme) {
    let changed = with(|x| {
        let c = x.theme != *theme;
        x.theme = theme.clone();
        c
    })
    .unwrap_or(false);
    if changed {
        redraw_all();
    }
}

/// The rendering device was replaced: every piece drawn again.
pub fn redraw_all() {
    for h in hwnds() {
        draw(h);
    }
}

pub fn shutdown() {
    overlay::shutdown();
    if let Some(x) = PIECES.with(|p| p.borrow_mut().take()) {
        for p in x.list {
            crate::desklayer::privacy::forget(p.hwnd);
            unsafe {
                let _ = DestroyWindow(p.hwnd);
            }
        }
    }
}

/// Deletes piece `hwnd` (the user asked): its picture to the Recycle Bin,
/// its line out of the list, its window gone. A picture the shell does not
/// take keeps the piece.
fn delete(hwnd: HWND) {
    let Some(id) = with(|x| x.list.iter().find(|p| p.hwnd == hwnd).map(|p| p.id.clone())).flatten() else { return };
    // Its line must be able to go too.
    if !with(|x| x.store.writable()).unwrap_or(false) {
        crate::log!("pieces: the list did not read; the piece stays");
        return;
    }
    if !crate::desklayer::disk::recycle(&store::picture(&id)) && store::picture(&id).exists() {
        crate::log!("pieces: picture not moved to the Recycle Bin; kept");
        return;
    }
    with(|x| {
        x.list.retain(|p| p.hwnd != hwnd);
        x.fresh.retain(|(h, _)| *h != hwnd);
        if !x.store.update(|i| i.remove(&id)) {
            crate::log!("pieces: the list not saved after a delete");
        }
    });
    crate::desklayer::privacy::forget(hwnd);
    crate::desklayer::icons::forget(hwnd);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    report();
}

fn save_place(hwnd: HWND) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    let mons = crate::monitors::all();
    if let Some(mi) = crate::desklayer::grid::monitor_of(r, &mons) {
        let desk = crate::desklayer::widget::desk(mons[mi].scale());
        if let Some(g) = crate::desklayer::grid::grids(&desk, &mons)[mi] {
            let others = crate::desklayer::icons::widget_rects(Some(hwnd));
            if others.iter().any(|o| crate::desklayer::widget::intersects(r,*o)) {
                let (w,h) = (r.right-r.left,r.bottom-r.top);
                let cells=((w+g.cw-1)/g.cw,(h+g.ch-1)/g.ch);
                if let Some(n) = crate::desklayer::widget::nearest_free(&g,r,cells,0,&others) {
                    r=RECT {left:n.left,top:n.top,right:n.left+w,bottom:n.top+h};
                } else { r=crate::desklayer::icons::committed(hwnd).unwrap_or(r); }
                unsafe { let _=SetWindowPos(hwnd,None,r.left,r.top,0,0,SWP_NOSIZE|SWP_NOZORDER|SWP_NOACTIVATE); }
            }
        }
    }
    crate::desklayer::icons::commit(hwnd,r);
    let Some(p) = place::from_px(r, &crate::monitors::all()) else { return };
    with(|x| {
        let Some(id) = x.list.iter().find(|pc| pc.hwnd == hwnd).map(|pc| pc.id.clone()) else { return };
        if x.store.data.pieces.iter().find(|e| e.id == id).map(|e| e.place) != Some(p) && !x.store.update(|i| {
            i.set_place(&id, p);
        }) {
            crate::log!("pieces: the new place not saved");
        }
    });
}

fn lp_point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

fn set_hover(hwnd: HWND, on: bool) {
    with(|x| {
        if let Some(p) = x.list.iter_mut().find(|p| p.hwnd == hwnd) {
            if p.hover != on {
                p.hover = on;
                let _ = p.close.SetIsVisible(on);
            }
        }
    });
}

fn in_close(hwnd: HWND, x: i32, y: i32) -> bool {
    let s = scale_of(hwnd);
    let (w, h) = client_px(hwnd);
    hit_test(x as f32 / s, y as f32 / s, w as f32 / s, h as f32 / s, crate::i18n::rtl()) == HTCLIENT
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _span = load::span(load::Part::Widget(load::Kind::Pieces));
    match msg {
        WM_NCHITTEST => {
            let (sx, sy) = lp_point(lp);
            let mut p = POINT { x: sx, y: sy };
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p);
            let s = scale_of(hwnd);
            let (w, h) = client_px(hwnd);
            // Over the piece at all: its × shows.
            set_hover(hwnd, true);
            LRESULT(hit_test(p.x as f32 / s, p.y as f32 / s, w as f32 / s, h as f32 / s, crate::i18n::rtl()) as isize)
        }
        WM_NCMOUSEMOVE | WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE | if msg == WM_NCMOUSEMOVE { windows::Win32::UI::Input::KeyboardAndMouse::TME_NONCLIENT } else { Default::default() },
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        windows::Win32::UI::Controls::WM_MOUSELEAVE | WM_NCMOUSELEAVE => {
            let mut c = POINT::default();
            let _ = GetCursorPos(&mut c);
            if WindowFromPoint(c) != hwnd {
                set_hover(hwnd, false);
            }
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGING => {
            // A piece just cut from the screen stays in front (a click on it too).
            let layer = with(|x| if x.fresh.iter().any(|(h, _)| *h == hwnd) { Layer::Summoned } else { x.layer }).unwrap_or(Layer::Desktop);
            if let Some(wp) = (lp.0 as *mut WINDOWPOS).as_mut() {
                lw::pin(layer, hwnd, wp);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SIZING => {
            // The picture keeps its shape.
            if let Some(r) = (lp.0 as *mut RECT).as_mut() {
                let ratio = with(|x| x.list.iter().find(|p| p.hwnd == hwnd).map(|p| p.ratio)).flatten().unwrap_or(1.0);
                let edge = wp.0 as u32;
                let width_leads = !matches!(edge, WMSZ_TOP | WMSZ_BOTTOM);
                let (w, h) = store::keep_shape(((r.right - r.left) as f32, (r.bottom - r.top) as f32), ratio, width_leads);
                let (w, h) = (w.round() as i32, h.round() as i32);
                if matches!(edge, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT) {
                    r.left = r.right - w;
                } else {
                    r.right = r.left + w;
                }
                if matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT) {
                    r.top = r.bottom - h;
                } else {
                    r.bottom = r.top + h;
                }
            }
            LRESULT(1)
        }
        WM_GETMINMAXINFO => {
            if let Some(mm) = (lp.0 as *mut MINMAXINFO).as_mut() {
                let s = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                // MIN_SIDE on the longer side, the shape kept.
                let ratio = with(|x| x.list.iter().find(|p| p.hwnd == hwnd).map(|p| p.ratio)).flatten().unwrap_or(1.0);
                let (w, h) = store::min_track(ratio, MIN_SIDE);
                mm.ptMinTrackSize = POINT { x: (w * s).round() as i32, y: (h * s).round() as i32 };
            }
            LRESULT(0)
        }
        WM_SIZE => {
            // While sizing the drawing stretches; drawn anew when it ends.
            let (w, h) = client_px(hwnd);
            with(|x| {
                if let Some(p) = x.list.iter().find(|p| p.hwnd == hwnd) {
                    let _ = p.image.SetSize(Vector2 { X: w as f32, Y: h as f32 });
                    let s = scale_of(hwnd);
                    let d = (CLOSE_D * s).ceil();
                    let (cx, cy) = close_center(w as f32 / s, crate::i18n::rtl());
                    let _ = p.close.SetOffset(Vector3 { X: cx * s - d / 2.0, Y: cy * s - d / 2.0, Z: 0.0 });
                }
            });
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            draw(hwnd);
            save_place(hwnd);
            report();
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(r) = (lp.0 as *const RECT).as_ref() {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            draw(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = lp_point(lp);
            let on = in_close(hwnd, x, y);
            with(|s| {
                if let Some(p) = s.list.iter_mut().find(|p| p.hwnd == hwnd) {
                    p.pressed_close = on;
                }
            });
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let (x, y) = lp_point(lp);
            let was = with(|s| s.list.iter_mut().find(|p| p.hwnd == hwnd).map(|p| std::mem::replace(&mut p.pressed_close, false))).flatten().unwrap_or(false);
            if was && in_close(hwnd, x, y) {
                delete(hwnd);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wp.0 as u16;
            if vk == VK_DELETE.0 {
                delete(hwnd);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// A piece's picture for the promo film (tests only). Declared down
/// here, with the tests: the tests that read this file's code read it up
/// to its first `#[cfg(test)]`.
#[cfg(test)]
mod promo;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_is_all_client_area() {
        let r = RECT { left: -32000, top: -32000, right: -32000 + 200, bottom: -32000 + 120 };
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap();
            let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: CLASS, ..Default::default() };
            RegisterClassExW(&wc);
            create_window(r).unwrap()
        };
        let (mut wr, mut cr) = (RECT::default(), RECT::default());
        unsafe {
            let _ = GetWindowRect(hwnd, &mut wr);
            let _ = GetClientRect(hwnd, &mut cr);
        }
        assert_eq!((cr.right - cr.left, cr.bottom - cr.top), (wr.right - wr.left, wr.bottom - wr.top));
        assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool());
        crate::desklayer::privacy::forget(hwnd);
    crate::desklayer::icons::forget(hwnd);
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }

    #[test]
    fn a_fresh_piece_goes_back_once_another_app_comes_forward() {
        // The focus given back to the app it was cut from: stays in front.
        assert!(!fresh_ends(0x100, 0x100));
        // Any other app's window: to the desktop.
        assert!(fresh_ends(0x200, 0x100));
    }

    #[test]
    fn two_pieces_cut_in_a_row_both_go_back_together() {
        // Two cut from app A (0x100) one after the other: both stay while
        // A is in front, both go when B (0x200) comes.
        let fresh = [(1u8, 0x100isize), (2u8, 0x100)];
        let (keep, gone) = split_fresh(&fresh, 0x100);
        assert_eq!((keep.len(), gone.len()), (2, 0));
        let (keep, gone) = split_fresh(&fresh, 0x200);
        assert!(keep.is_empty());
        assert_eq!(gone, vec![1, 2], "none left on top");
        // One cut from A, one from C: B sends both, A keeps A's only.
        let mixed = [(1u8, 0x100isize), (2u8, 0x300)];
        assert_eq!(split_fresh(&mixed, 0x100), (vec![(1, 0x100)], vec![2]));
    }

    #[test]
    fn the_corner_x_deletes_edges_resize_the_rest_moves() {
        let (w, h) = (200.0, 120.0);
        let (cx, cy) = close_center(w, false);
        assert_eq!(hit_test(cx, cy, w, h, false), HTCLIENT);
        assert_eq!(hit_test(close_center(w, true).0, cy, w, h, true), HTCLIENT, "on the left right to left");
        assert_eq!(hit_test(100.0, 60.0, w, h, false), HTCAPTION);
        assert_eq!(hit_test(1.0, 60.0, w, h, false), HTLEFT);
        assert_eq!(hit_test(199.0, 119.0, w, h, false), HTBOTTOMRIGHT);
        assert_eq!(hit_test(100.0, 119.0, w, h, false), HTBOTTOM);
    }
}
