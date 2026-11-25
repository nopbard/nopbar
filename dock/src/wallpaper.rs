//! The "mica" material: the desktop wallpaper, shrunk and blurred once on a
//! worker thread, shown behind the bar as a static image. No per-frame GPU
//! blur, so it looks the same whether or not DWM's host backdrop works.
//!
//! Made once per monitor and wallpaper and kept (`MicaCache`): a change of
//! NopBar's look never takes another snapshot; only a new wallpaper
//! (SPI_SETDESKWALLPAPER), a display change or a session coming back does
//! (`invalidate`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use windows::core::{Interface, HSTRING};
use windows::Win32::Foundation::{COLORREF, GENERIC_READ, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject, FillRect,
    GdiFlush, GetDC, ReleaseDC, SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HALFTONE, SRCCOPY,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::core::BOOL;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, FindWindowExW, FindWindowW, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    SystemParametersInfoW, SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::icons::Icon;

/// The wallpaper image file Windows is showing (single-image or the
/// transcoded slideshow frame).
pub fn current_path() -> Option<PathBuf> {
    let existing = |s: String| {
        let path = PathBuf::from(s);
        (!path.as_os_str().is_empty() && path.is_file()).then_some(path)
    };
    current_said().and_then(existing)
        .or_else(|| existing(user_string(r"Control Panel\Desktop", "WallPaper")))
}

/// What Windows' one wallpaper setting names (SPI_GETDESKWALLPAPER), as
/// it says it: "" for no picture (a plain color), else a path that may be
/// gone; None when it could not be asked.
pub fn current_said() -> Option<String> {
    let mut buf = [0u16; 1024];
    unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            Some(buf.as_mut_ptr() as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// How Windows places the wallpaper image (Settings > Personalization >
/// Background, "Choose a fit").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Fill,
    Fit,
    Stretch,
    Tile,
    Center,
    /// One image across every display.
    Span,
}

/// `WallpaperStyle` and `TileWallpaper` (HKCU\Control Panel\Desktop) as
/// Windows writes them: "10" fill, "6" fit, "2" stretch, "0" center (tile
/// with TileWallpaper "1"), "22" span. No style at all is Windows' default
/// (fill). None for anything else: not guessed.
pub fn placement(style: &str, tile: &str) -> Option<Placement> {
    match (style.trim(), tile.trim()) {
        ("0", "1") => Some(Placement::Tile),
        ("0", _) => Some(Placement::Center),
        ("2", _) => Some(Placement::Stretch),
        ("6", _) => Some(Placement::Fit),
        ("10" | "", _) => Some(Placement::Fill),
        ("22", _) => Some(Placement::Span),
        _ => None,
    }
}

/// "R G B" (HKCU\Control Panel\Colors\Background): the colour around a
/// fitted or centred image. None if it does not read as one.
pub fn parse_rgb(s: &str) -> Option<[u8; 3]> {
    let v: Vec<u8> = s.split_whitespace().map(|p| p.parse().ok()).collect::<Option<_>>()?;
    match v[..] {
        [r, g, b] => Some([r, g, b]),
        _ => None,
    }
}

/// Where the image goes on the mica canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Plan {
    /// Crop the middle of the image to this rect and scale it to the canvas.
    Cover(WICRect),
    /// Scale the whole image to `w` x `h` and put it at (`x`, `y`) on a
    /// canvas of the background colour (the canvas crops it); `tile`:
    /// repeated from there to the right and down.
    Place { w: u32, h: u32, x: i32, y: i32, tile: bool },
    /// The background colour only (span across displays, or a placement
    /// we do not know).
    Solid,
}

/// How a `sw` x `sh` image placed `p` lands on a `w` x `h` canvas that
/// stands for a `mw` x `mh` monitor.
pub fn plan(p: Option<Placement>, sw: u32, sh: u32, mw: u32, mh: u32, w: u32, h: u32) -> Plan {
    if sw == 0 || sh == 0 || w == 0 || h == 0 || mw == 0 || mh == 0 {
        return Plan::Solid;
    }
    let size = |k: f64| (((sw as f64 * k).round() as u32).max(1), ((sh as f64 * k).round() as u32).max(1));
    let centred = |dw: u32, dh: u32| Plan::Place { w: dw, h: dh, x: (w as i32 - dw as i32) / 2, y: (h as i32 - dh as i32) / 2, tile: false };
    // Canvas pixels per monitor pixel (the canvas is the monitor shrunk).
    let k = (w as f64 / mw as f64).min(h as f64 / mh as f64);
    match p {
        Some(Placement::Fill) => Plan::Cover(cover_crop(sw, sh, w, h)),
        Some(Placement::Stretch) => Plan::Place { w, h, x: 0, y: 0, tile: false },
        Some(Placement::Fit) => {
            let (dw, dh) = size((w as f64 / sw as f64).min(h as f64 / sh as f64));
            centred(dw, dh)
        }
        Some(Placement::Center) => {
            let (dw, dh) = size(k);
            centred(dw, dh)
        }
        Some(Placement::Tile) => {
            let (dw, dh) = size(k);
            Plan::Place { w: dw, h: dh, x: 0, y: 0, tile: true }
        }
        Some(Placement::Span) | None => Plan::Solid,
    }
}

/// `img` (premultiplied BGRA, `iw` x `ih`) drawn over `canvas` (opaque
/// BGRA, `w` x `h`) at (`x`, `y`), cropped to the canvas.
#[allow(clippy::too_many_arguments)]
fn blit(canvas: &mut [u8], w: u32, h: u32, img: &[u8], iw: u32, ih: u32, x: i32, y: i32) {
    for row in 0..ih as i32 {
        let cy = y + row;
        if cy < 0 || cy >= h as i32 {
            continue;
        }
        for col in 0..iw as i32 {
            let cx = x + col;
            if cx < 0 || cx >= w as i32 {
                continue;
            }
            let s = ((row as u32 * iw + col as u32) * 4) as usize;
            let d = ((cy as u32 * w + cx as u32) * 4) as usize;
            let a = img[s + 3] as u32;
            for c in 0..3 {
                canvas[d + c] = (img[s + c] as u32 + canvas[d + c] as u32 * (255 - a) / 255).min(255) as u8;
            }
            canvas[d + 3] = 255;
        }
    }
}

/// A `w` x `h` canvas (BGRA) of one colour.
fn solid_canvas(rgb: [u8; 3], w: u32, h: u32) -> Vec<u8> {
    [rgb[2], rgb[1], rgb[0], 255].repeat((w * h) as usize)
}

/// A string value under HKCU ("" when missing).
fn user_string(key: &str, value: &str) -> String {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(value), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
    if r.is_err() {
        return String::new();
    }
    let len = (size as usize / 2).min(buf.len());
    String::from_utf16_lossy(&buf[..len]).trim_end_matches('\0').to_string()
}

/// The desktop's background color (what shows with no wallpaper, or
/// around a centered one); None when Windows has none written.
pub fn background_rgb() -> Option<[u8; 3]> {
    parse_rgb(&user_string(r"Control Panel\Colors", "Background"))
}

/// Decodes `path` as Windows places it on a `mw` x `mh` monitor, at `w` x
/// `h`, premultiplied BGRA (needs COM). Fill, fit, stretch, center and
/// tile are drawn as the desktop shows them; span (and anything unknown)
/// is the background colour alone rather than a wrong crop.
fn load_placed(path: &PathBuf, mw: u32, mh: u32, w: u32, h: u32) -> windows::core::Result<Icon> {
    let style = placement(&user_string(r"Control Panel\Desktop", "WallpaperStyle"), &user_string(r"Control Panel\Desktop", "TileWallpaper"));
    let back = parse_rgb(&user_string(r"Control Panel\Colors", "Background")).unwrap_or([0, 0, 0]);
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let decoder = factory.CreateDecoderFromFilename(
            &HSTRING::from(path.as_os_str()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        let frame = decoder.GetFrame(0)?;
        let (mut sw, mut sh) = (0u32, 0u32);
        frame.GetSize(&mut sw, &mut sh)?;
        let decode = |src: &IWICBitmapSource, dw: u32, dh: u32| -> windows::core::Result<Vec<u8>> {
            let scaler = factory.CreateBitmapScaler()?;
            scaler.Initialize(src, dw, dh, WICBitmapInterpolationModeFant)?;
            let conv = factory.CreateFormatConverter()?;
            conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
            let mut pixels = vec![0u8; (dw * dh * 4) as usize];
            conv.CopyPixels(std::ptr::null(), dw * 4, &mut pixels)?;
            Ok(pixels)
        };
        let pixels = match plan(style, sw, sh, mw, mh, w, h) {
            // Fill (the Windows default): keep the aspect ratio and crop
            // the middle to the monitor's shape, like the desktop does.
            Plan::Cover(crop) => {
                let clipper = factory.CreateBitmapClipper()?;
                clipper.Initialize(&frame, &crop)?;
                decode(&clipper.cast()?, w, h)?
            }
            Plan::Place { w: dw, h: dh, x, y, tile } => {
                let img = decode(&frame.cast()?, dw, dh)?;
                let mut canvas = solid_canvas(back, w, h);
                let mut ty = y;
                loop {
                    let mut tx = x;
                    loop {
                        blit(&mut canvas, w, h, &img, dw, dh, tx, ty);
                        tx += dw as i32;
                        if !tile || tx >= w as i32 {
                            break;
                        }
                    }
                    ty += dh as i32;
                    if !tile || ty >= h as i32 {
                        break;
                    }
                }
                canvas
            }
            Plan::Solid => {
                crate::log!("mica: wallpaper placement {style:?} is not drawn; using the background colour");
                solid_canvas(back, w, h)
            }
        };
        Ok(Icon { width: w, height: h, pixels })
    }
}

/// In-place box blur (`passes` x horizontal + vertical, ~Gaussian for 3
/// passes). Edges are clamped.
pub fn blur_bgra(px: &mut [u8], w: usize, h: usize, radius: usize, passes: usize) {
    if w == 0 || h == 0 || radius == 0 {
        return;
    }
    let mut tmp = vec![0u8; px.len()];
    let win = (2 * radius + 1) as u32;
    for _ in 0..passes {
        // horizontal: px -> tmp
        for y in 0..h {
            for c in 0..4 {
                let at = |x: isize| px[(y * w + x.clamp(0, w as isize - 1) as usize) * 4 + c] as u32;
                let mut sum: u32 = (-(radius as isize)..=radius as isize).map(at).sum();
                for x in 0..w {
                    tmp[(y * w + x) * 4 + c] = (sum / win) as u8;
                    sum += at(x as isize + radius as isize + 1);
                    sum -= at(x as isize - radius as isize);
                }
            }
        }
        // vertical: tmp -> px
        for x in 0..w {
            for c in 0..4 {
                let at = |y: isize| tmp[(y.clamp(0, h as isize - 1) as usize * w + x) * 4 + c] as u32;
                let mut sum: u32 = (-(radius as isize)..=radius as isize).map(at).sum();
                for y in 0..h {
                    px[(y * w + x) * 4 + c] = (sum / win) as u8;
                    sum += at(y as isize + radius as isize + 1);
                    sum -= at(y as isize - radius as isize);
                }
            }
        }
    }
}

/// Centered crop of a `sw` x `sh` image with the aspect ratio of `w` x `h`.
pub fn cover_crop(sw: u32, sh: u32, w: u32, h: u32) -> WICRect {
    let (sw_f, sh_f) = (sw as f64, sh as f64);
    let target = w as f64 / h.max(1) as f64;
    if sw_f / sh_f.max(1.0) > target {
        let cw = (sh_f * target).round() as i32;
        WICRect { X: ((sw as i32 - cw) / 2).max(0), Y: 0, Width: cw.max(1), Height: sh as i32 }
    } else {
        let ch = (sw_f / target).round() as i32;
        WICRect { X: 0, Y: ((sh as i32 - ch) / 2).max(0), Width: sw as i32, Height: ch.max(1) }
    }
}

/// The window that shows the wallpaper. Live-wallpaper apps (Wallpaper
/// Engine and friends) draw into a WorkerW behind the desktop icons; plain
/// desktops show it in Progman.
unsafe fn wallpaper_window() -> Option<HWND> {
    let progman = FindWindowW(windows::core::w!("Progman"), None).ok()?;
    // Windows 11 24H2+: the WorkerW lives under Progman.
    if let Ok(w) = FindWindowExW(Some(progman), None, windows::core::w!("WorkerW"), None) {
        return Some(w);
    }
    // Older layout: a top-level WorkerW that does not hold the icons.
    let mut after: Option<HWND> = None;
    while let Ok(w) = FindWindowExW(None, after, windows::core::w!("WorkerW"), None) {
        if FindWindowExW(Some(w), None, windows::core::w!("SHELLDLL_DefView"), None).is_err() && IsWindowVisible(w).as_bool() {
            return Some(w);
        }
        after = Some(w);
    }
    Some(progman)
}

/// Painted before the snapshot; if it is still there afterwards, nothing
/// was drawn (magenta, 0x00BBGGRR).
const MARKER: u32 = 0x00FF_00FF;

/// PrintWindow's flags, in the order tried: a plain WM_PRINT first, then
/// PW_RENDERFULLCONTENT (2) only if that drew nothing. Full content makes
/// DWM redirect the window's DirectX output for the copy, which is what
/// made Wallpaper Engine and other live wallpapers blink; a live wallpaper
/// is never snapshotted at all (`source`), so only Explorer's own
/// wallpaper ever gets the second try.
const PRINT_TRIES: [u32; 2] = [0, 2];

/// One snapshot of the wallpaper layer (no other windows), the part under
/// monitor `mon` scaled to `w` x `h`. None if nothing was drawn.
/// PrintWindow asks the window's own process to draw, synchronously and
/// without a timeout, so this only ever runs on the capture thread below.
fn capture_desktop(mon: RECT, w: u32, h: u32) -> Option<Icon> {
    unsafe {
        let hwnd = wallpaper_window()?;
        // Looked at again here, on the thread that prints and for the
        // window it picked: the mica worker's look came earlier, and a live
        // wallpaper may have attached since. Still not atomic: an app that
        // parents its window in between this check and PrintWindow (a few
        // GDI calls later) would be printed once; Windows offers no way to
        // hold the desktop's children still across both.
        if !print_allowed(hwnd) {
            crate::log!("mica: the wallpaper layer is not Explorer's alone now; no snapshot");
            return None;
        }
        let mut wr = RECT::default();
        GetWindowRect(hwnd, &mut wr).ok()?;
        let (ww, wh) = (wr.right - wr.left, wr.bottom - wr.top);
        if ww <= 0 || wh <= 0 {
            return None;
        }
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let full_dc = CreateCompatibleDC(Some(screen));
        let small_dc = CreateCompatibleDC(Some(screen));
        let full_bmp = CreateCompatibleBitmap(screen, ww, wh);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let small = CreateDIBSection(Some(small_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok();

        let mut out = None;
        if let (false, false, false, Some(small)) = (full_dc.is_invalid(), small_dc.is_invalid(), full_bmp.is_invalid(), small)
        {
            let old_full = SelectObject(full_dc, full_bmp.into());
            let old_small = SelectObject(small_dc, small.into());
            for flags in PRINT_TRIES {
                let marker = CreateSolidBrush(COLORREF(MARKER));
                FillRect(full_dc, &RECT { left: 0, top: 0, right: ww, bottom: wh }, marker);
                let _ = DeleteObject(marker.into());
                if !PrintWindow(hwnd, full_dc, PRINT_WINDOW_FLAGS(flags)).as_bool() {
                    continue;
                }
                SetStretchBltMode(small_dc, HALFTONE);
                let copied = StretchBlt(
                    small_dc,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    Some(full_dc),
                    mon.left - wr.left,
                    mon.top - wr.top,
                    mon.right - mon.left,
                    mon.bottom - mon.top,
                    SRCCOPY,
                )
                .as_bool();
                // The DIB is read directly: make sure GDI has finished.
                let _ = GdiFlush();
                if copied && !bits.is_null() {
                    let pixels = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
                    if !is_blank(&pixels) {
                        out = Some(Icon { width: w, height: h, pixels });
                        break;
                    }
                }
            }
            SelectObject(small_dc, old_small);
            SelectObject(full_dc, old_full);
        }
        // Released on every path, captured or not.
        if let Some(small) = small {
            let _ = DeleteObject(small.into());
        }
        if !full_bmp.is_invalid() {
            let _ = DeleteObject(full_bmp.into());
        }
        let _ = DeleteDC(full_dc);
        let _ = DeleteDC(small_dc);
        ReleaseDC(None, screen);
        out
    }
}

/// True if most of the image is still the marker (nothing was drawn). A
/// genuinely dark or black wallpaper is not blank.
pub fn is_blank(px: &[u8]) -> bool {
    let n = px.len() / 4;
    if n == 0 {
        return true;
    }
    let marked = px.chunks_exact(4).filter(|p| p[0] >= 247 && p[1] <= 8 && p[2] >= 247).count();
    marked * 10 >= n * 9
}

struct CaptureReq {
    mon: RECT,
    w: u32,
    h: u32,
    reply: std::sync::mpsc::Sender<Option<Icon>>,
}

/// The capture thread's queue (its thread ends when idle).
static CAPTURE_Q: std::sync::OnceLock<crate::idle::IdleQueue<CaptureReq>> = std::sync::OnceLock::new();
/// The capture thread is inside PrintWindow. If a call hangs (explorer
/// stuck), this stays set and later requests skip the snapshot instead of
/// piling up threads.
static CAPTURE_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Snapshot with a deadline, on the capture thread (one at a time; it
/// exists only while snapshots are being taken).
fn capture_with_timeout(mon: RECT, w: u32, h: u32) -> Result<Icon, &'static str> {
    use std::sync::atomic::Ordering;
    if CAPTURE_BUSY.load(Ordering::SeqCst) {
        return Err("previous snapshot still running");
    }
    let q = CAPTURE_Q.get_or_init(|| {
        crate::idle::IdleQueue::new("mica-capture", std::time::Duration::from_secs(30), 0, |jobs: &mut crate::idle::Jobs<CaptureReq>| {
            while let Some(req) = jobs.next() {
                CAPTURE_BUSY.store(true, Ordering::SeqCst);
                let img = capture_desktop(req.mon, req.w, req.h);
                CAPTURE_BUSY.store(false, Ordering::SeqCst);
                let _ = req.reply.send(img);
            }
        })
    });
    let (reply, wait) = std::sync::mpsc::channel();
    q.send(CaptureReq { mon, w, h, reply }).map_err(|_| "capture thread unavailable")?;
    match wait.recv_timeout(CAPTURE_TIMEOUT) {
        Ok(img) => img.ok_or("wallpaper layer unavailable or blank"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err("snapshot timed out"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("capture thread disconnected"),
    }
}

// ---------------------------------------------------------------- cache

/// Bumped whenever the picture behind the desktop may be new (a wallpaper
/// set, a display change, the session back): every cached image is stale.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The wallpaper may have changed: the next mica request takes a new
/// picture (once per monitor).
pub fn invalidate() {
    crate::desklayer::live::refresh();
    GENERATION.fetch_add(1, Ordering::SeqCst);
    // The desk widgets' graphite glass shows it too.
    crate::desklayer::backdrop::changed();
    crate::topbar::mica_changed();
}

/// How many times the picture behind the desktop may have changed.
#[cfg_attr(test, allow(dead_code))]
pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// What a mica image was made for. The same key: the same picture (the
/// blur is fixed, the tint is drawn over it by the compositor, so nothing
/// NopBar's look changes is in here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// The monitor's rect (left, top, right, bottom), physical pixels.
    pub mon: (i32, i32, i32, i32),
    pub w: u32,
    pub h: u32,
    pub radius: usize,
    /// GENERATION when it was asked for.
    pub gen: u64,
    /// The registered wallpaper file and its last write (ms), if any.
    pub file: Option<(PathBuf, u64)>,
    pub plain: Option<[u8; 3]>,
}

/// Monitors remembered at most (an image per monitor; ~230 KB at 1440p).
pub const CACHE_MAX: usize = 6;

/// Measurement uses the original pixels. The display uses the blurred copy.
pub struct MicaPicture {
    pub source: Icon,
    pub blurred: Icon,
    dock_blur: std::sync::OnceLock<Icon>,
}

/// The mica images, newest last. Pure: tested.
#[derive(Default)]
pub struct MicaCache {
    entries: Vec<(Key, Arc<MicaPicture>)>,
}

impl MicaCache {
    /// A copy of the image made for exactly `key`.
    pub fn get(&self, key: &Key) -> Option<Arc<MicaPicture>> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, img)| Arc::clone(img))
    }

    /// Keeps `img` for `key`: replaces what that monitor had, drops the
    /// oldest past CACHE_MAX.
    pub fn put(&mut self, key: Key, img: Arc<MicaPicture>) {
        self.entries.retain(|(k, _)| k.mon != key.mon);
        self.entries.push((key, img));
        if self.entries.len() > CACHE_MAX {
            let extra = self.entries.len() - CACHE_MAX;
            self.entries.drain(..extra);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

static CACHE: std::sync::Mutex<MicaCache> = std::sync::Mutex::new(MicaCache { entries: Vec::new() });


/// Where a new mica picture comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// PrintWindow of Explorer's wallpaper window (a still wallpaper).
    Snapshot,
    /// The registered wallpaper file (or the background colour).
    File,
}

/// Pure: the source for a desktop with (`live`) or without a live
/// wallpaper. Trade-off: a live wallpaper is never snapshotted, since asking
/// its DirectX window to draw for a copy makes it blink; the mica shows
/// the registered still wallpaper instead (or falls back to solid), which
/// may not match the live scene. A blurred tint behind the dock is worth
/// less than a wallpaper that never flickers.
pub fn source(live: bool) -> Source {
    if live {
        Source::File
    } else {
        Source::Snapshot
    }
}

/// Pure: is there a live wallpaper? `children`: the processes owning the
/// windows inside the desktop's wallpaper layer; `shell`: Explorer's.
/// Another process's window in there is a live-wallpaper app (Wallpaper
/// Engine, Lively and friends parent their render window into it).
pub fn is_live(shell: u32, children: &[u32]) -> bool {
    children.iter().any(|&p| p != 0 && p != shell)
}

/// The processes owning every window inside Progman and the top-level
/// WorkerWs (the wallpaper layer on every Windows 11 layout), and
/// Explorer's. No messages are sent: safe on any thread.
unsafe fn layer_processes() -> Option<(u32, Vec<u32>)> {
    let cb = collect_pids;
    let progman = FindWindowW(windows::core::w!("Progman"), None).ok()?;
    let mut shell = 0u32;
    GetWindowThreadProcessId(progman, Some(&mut shell));
    let mut pids: Vec<u32> = Vec::new();
    let lp = LPARAM(&mut pids as *mut Vec<u32> as isize);
    let _ = EnumChildWindows(Some(progman), Some(cb), lp);
    let mut after: Option<HWND> = None;
    for _ in 0..64 {
        let Ok(w) = FindWindowExW(None, after, windows::core::w!("WorkerW"), None) else { break };
        let mut owner = 0u32;
        GetWindowThreadProcessId(w, Some(&mut owner));
        if owner == shell {
            let _ = EnumChildWindows(Some(w), Some(cb), lp);
        }
        after = Some(w);
    }
    Some((shell, pids))
}

/// EnumChildWindows: each window's owning process into the Vec<u32> at
/// `lp` (0 for one gone meanwhile).
unsafe extern "system" fn collect_pids(hwnd: HWND, lp: LPARAM) -> BOOL {
    let pids = &mut *(lp.0 as *mut Vec<u32>);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    pids.push(pid);
    // A handful at most; stop on anything absurd.
    BOOL::from(pids.len() < 512)
}

/// A live wallpaper is showing now.
pub fn live_wallpaper() -> bool {
    unsafe { layer_processes() }.is_some_and(|(shell, pids)| is_live(shell, &pids))
}

/// Pure: may the window about to be snapshotted be printed? Only when all
/// of it is known: Explorer's process (`shell`), the target's owner (must
/// be Explorer), and the owners of every window inside the target and in
/// the rest of the wallpaper layer (`inside`: none of another process).
/// Anything unknown: no (the registered file instead).
pub fn may_print(shell: Option<u32>, target: Option<u32>, inside: Option<&[u32]>) -> bool {
    match (shell, target, inside) {
        (Some(s), Some(t), Some(pids)) => s != 0 && t == s && !is_live(s, pids),
        _ => false,
    }
}

/// may_print for `target`, looked at now (on the capture thread, right
/// before PrintWindow).
unsafe fn print_allowed(target: HWND) -> bool {
    let mut owner = 0u32;
    let tid = GetWindowThreadProcessId(target, Some(&mut owner));
    let target_owner = (tid != 0 && owner != 0).then_some(owner);
    let layer = layer_processes();
    let mut inside: Vec<u32> = Vec::new();
    let _ = EnumChildWindows(Some(target), Some(collect_pids), LPARAM(&mut inside as *mut Vec<u32> as isize));
    let all = layer.map(|(shell, mut pids)| {
        pids.extend(inside);
        (shell, pids)
    });
    may_print(all.as_ref().map(|a| a.0), target_owner, all.as_ref().map(|a| a.1.as_slice()))
}

/// The cache key for monitor `mon` at `w` x `h` right now.
fn key_now(mon: RECT, w: u32, h: u32, radius: usize, gen: u64, path: Option<PathBuf>, plain: Option<[u8; 3]>) -> Key {
    let file = path.map(|p| {
        let stamp = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        (p, stamp)
    });
    Key { mon: (mon.left, mon.top, mon.right, mon.bottom), w, h, radius, gen, file, plain }
}

/// A nearly uniform BGRA picture. Alpha from GDI is unused.
fn is_flat(px: &[u8]) -> bool {
    let (mut lo, mut hi) = ([255u8; 3], [0u8; 3]);
    for p in px.as_chunks::<4>().0 {
        for c in 0..3 {
            lo[c] = lo[c].min(p[c]);
            hi[c] = hi[c].max(p[c]);
        }
    }
    (0..3).all(|c| hi[c].saturating_sub(lo[c]) <= 4)
}

/// Approximate the design's 40 DIP Gaussian with three boxes at 1/8 size.
pub fn mica_radius(scale: f32) -> usize {
    let sigma = 40. * scale / 8.;
    ((1. + 4. * sigma * sigma).sqrt() * 0.5 - 0.5).round() as usize
}

fn blurred(source: &Icon, radius: usize) -> Icon {
    let (width, height) = (source.width, source.height);
    let mut pixels = source.pixels.clone();
    blur_bgra(&mut pixels, width as usize, height as usize, radius, 3);
    for p in pixels.as_chunks_mut::<4>().0 {
        p[3] = 255;
    }
    Icon { width, height, pixels }
}

pub(crate) fn prepare_mica(source: Icon, radius: usize) -> MicaPicture {
    let blurred = blurred(&source, radius);
    MicaPicture { source, blurred, dock_blur: std::sync::OnceLock::new() }
}

impl MicaPicture {
    /// Keep the dock's four-pixel blur while sharing the capture with the
    /// DPI-scaled glass.
    fn for_dock(&self) -> &Icon {
        self.dock_blur.get_or_init(|| blurred(&self.source, 4))
    }
}

fn prepare_snapshot(img: Icon, radius: usize, has_file: bool) -> Result<MicaPicture, &'static str> {
    if !has_file {
        return Ok(prepare_mica(img, radius));
    }
    let pixels = img.pixels.as_chunks::<4>().0;
    if is_flat(&img.pixels) {
        return Err("flat snapshot");
    }
    // A failed print can leave a few stray pixels on an otherwise black canvas.
    if pixels.iter().filter(|p| p[..3].iter().all(|&v| v <= 12)).count() * 100 >= pixels.len() * 99 {
        return Err("nearly black snapshot");
    }
    let img = prepare_mica(img, radius);
    if is_flat(&img.blurred.pixels) {
        Err("snapshot became flat after blur")
    } else {
        Ok(img)
    }
}

fn select_mica(
    snapshot: Result<Icon, &'static str>,
    radius: usize,
    has_file: bool,
    load: impl FnOnce() -> Result<Icon, String>,
) -> (Result<MicaPicture, String>, Option<&'static str>) {
    match snapshot.and_then(|img| prepare_snapshot(img, radius, has_file)) {
        Ok(img) => (Ok(img), None),
        Err(reason) => (load().map(|img| prepare_mica(img, radius)), Some(reason)),
    }
}

/// Blurred, shrunken wallpaper for monitor `mon`: the one made already for
/// this monitor and wallpaper if there is one (no snapshot then), else a
/// snapshot of a still wallpaper layer, else the image Windows has
/// registered. 1/8 size; stretched back up, the blur hides it. Runs on the
/// mica worker thread (needs COM for WIC).
pub fn mica_picture(mon: RECT) -> Option<Arc<MicaPicture>> {
    let gen = generation();
    let (mw, mh) = ((mon.right - mon.left).max(1) as u32, (mon.bottom - mon.top).max(1) as u32);
    let (w, h) = ((mw / 8).max(16), (mh / 8).max(16));
    let scale = crate::monitors::all().iter().find(|m| m.rect == mon).map_or(1.0, |m| m.scale());
    let radius = mica_radius(scale);
    let mut desk = crate::topbar::about::desk(mon);
    let plain = desk.plain_color();
    if desk.path.is_none() && plain.is_none() {
        desk.path = current_path();
    }
    let key = key_now(mon, w, h, radius, gen, desk.path.clone(), plain);
    if let Some(img) = CACHE.lock().ok().and_then(|c| c.get(&key)) {
        return Some(img);
    }
    let snapshot = match (plain, source(live_wallpaper())) {
        (Some(_), _) => Err("solid desktop"),
        (None, Source::Snapshot) => capture_with_timeout(mon, w, h),
        (None, Source::File) => Err("live wallpaper"),
    };
    let (img, reason) = select_mica(snapshot, radius, key.file.is_some(), || {
        if let Some(rgb) = plain {
            return Ok(Icon { width: w, height: h, pixels: solid_canvas(rgb, w, h) });
        }
        let (path, _) = key.file.as_ref().ok_or("no registered wallpaper file")?;
        if let Some(img) = crate::topbar::about::compose(&desk, mon, crate::topbar::about::displays_box(), w.max(h)) {
            return Ok(img);
        }
        load_placed(path, mw, mh, w, h).map_err(|e| format!("cannot load wallpaper: {e}"))
    });
    let img = match img {
        Ok(img) => {
            if plain.is_some() {
                crate::log!("mica: {}x{} from desktop background colour", mw, mh);
            } else if let Some(reason) = reason {
                crate::log!("mica: {}x{} from registered file ({reason})", mw, mh);
            } else {
                crate::log!("mica: {}x{} from desktop snapshot", mw, mh);
            }
            Arc::new(img)
        }
        Err(e) => {
            let reason = reason.unwrap_or("snapshot unavailable");
            crate::log!("mica: {}x{} unavailable ({reason}, {e})", mw, mh);
            return None;
        }
    };
    if let Ok(mut c) = CACHE.lock() {
        if generation() == gen {
            c.put(key, Arc::clone(&img));
        }
    }
    Some(img)
}

/// The dock only needs the blurred pixels.
pub fn mica_image(mon: RECT) -> Option<Icon> {
    let picture = mica_picture(mon)?;
    let img = picture.for_dock();
    Some(Icon { width: img.width, height: img.height, pixels: img.pixels.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(mon: (i32, i32, i32, i32), gen: u64, file: Option<(&str, u64)>) -> Key {
        Key { mon, w: 320, h: 180, radius: 5, gen, file: file.map(|(p, s)| (PathBuf::from(p), s)), plain: None }
    }

    fn img(v: u8) -> Icon {
        Icon { width: 1, height: 1, pixels: vec![v; 4] }
    }

    #[test]
    fn only_a_usable_snapshot_wins_over_the_registered_file() {
        let gradient = || Icon {
            width: 32,
            height: 8,
            pixels: (0..32 * 8).flat_map(|i| [(i % 32 * 8) as u8, 40, 60, 0]).collect(),
        };
        let file = || Ok(gradient());
        let mut nearly_black = Icon { width: 32, height: 8, pixels: vec![0; 32 * 8 * 4] };
        nearly_black.pixels[0] = 250;
        for (snapshot, expected_reason) in [(img(20), "flat snapshot"), (nearly_black, "nearly black snapshot")] {
            let (out, reason) = select_mica(Ok(snapshot), 5, true, file);
            assert_eq!(reason, Some(expected_reason));
            let picture = out.unwrap();
            assert_eq!(picture.source.pixels, gradient().pixels);
            assert_eq!(picture.blurred.pixels, prepare_mica(gradient(), 5).blurred.pixels);
        }
        let (out, reason) = select_mica(Ok(gradient()), 5, true, || panic!("usable capture must skip the file"));
        assert_eq!(reason, None);
        assert!(out.unwrap().blurred.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        let (out, reason) = select_mica(Ok(img(0)), 5, true, || Err("no file".into()));
        assert_eq!(reason, Some("flat snapshot"));
        assert_eq!(out.err().as_deref(), Some("no file"));
    }

    #[test]
    fn a_solid_desktop_can_use_a_flat_snapshot_or_its_background_color() {
        let (out, reason) = select_mica(Ok(img(0)), 5, false, || panic!("a flat capture is usable without a file"));
        assert_eq!(reason, None);
        assert_eq!(out.unwrap().blurred.pixels, [0, 0, 0, 255]);
        let (out, _) = select_mica(Err("solid desktop"), 5, false, || {
            Ok(Icon { width: 2, height: 2, pixels: solid_canvas([20, 30, 40], 2, 2) })
        });
        assert_eq!(out.unwrap().blurred.pixels, [40, 30, 20, 255].repeat(4));
    }

    #[test]
    fn mica_blur_tracks_the_monitor_scale() {
        assert_eq!((mica_radius(1.0), mica_radius(1.5)), (5, 7));
        let source = Icon {
            width: 32, height: 16,
            pixels: (0..512).flat_map(|i| [if i % 32 < 16 { 0 } else { 255 }, 0, 0, 255]).collect(),
        };
        let picture = prepare_mica(source, mica_radius(1.5));
        assert_ne!(picture.blurred.pixels, picture.for_dock().pixels);
        assert_eq!(picture.for_dock().pixels, blurred(&picture.source, 4).pixels);
        assert!(std::ptr::eq(picture.for_dock(), picture.for_dock()));
    }

    /// A look change asks again with the same key: the image made already
    /// comes back, no new snapshot. A new wallpaper (file, its write time,
    /// or a bump after SPI_SETDESKWALLPAPER / a display change) or another
    /// monitor size misses.
    #[test]
    fn mica_cache_reuses_until_the_wallpaper_or_monitor_changes() {
        let a = (0, 0, 2560, 1440);
        let mut c = MicaCache::default();
        let k = key(a, 0, Some(("C:\\w.jpg", 7)));
        assert!(c.get(&k).is_none(), "first run: nothing yet");
        let picture = |v| Arc::new(prepare_mica(img(v), 0));
        c.put(k.clone(), picture(9));
        assert_eq!(c.get(&k).map(|i| i.blurred.pixels.clone()), Some(vec![9, 9, 9, 255]), "same monitor, same wallpaper: reused");
        assert!(c.get(&key(a, 1, Some(("C:\\w.jpg", 7)))).is_none(), "invalidated (wallpaper set, display change)");
        assert!(c.get(&key(a, 0, Some(("C:\\w.jpg", 8)))).is_none(), "the same file rewritten");
        assert!(c.get(&key(a, 0, Some(("C:\\v.jpg", 7)))).is_none(), "another file");
        assert!(c.get(&key(a, 0, None)).is_none(), "no file now");
        assert!(c.get(&key((0, 0, 1920, 1080), 0, Some(("C:\\w.jpg", 7)))).is_none(), "another resolution");
        let mut other = k.clone();
        other.w = 240;
        assert!(c.get(&other).is_none(), "another size");
        // A newer image for a monitor replaces its old one.
        c.put(key(a, 1, None), picture(3));
        assert_eq!(c.len(), 1);
        assert!(c.get(&k).is_none());
        // Several monitors, at most CACHE_MAX kept (the oldest goes).
        for i in 0..CACHE_MAX as i32 + 2 {
            c.put(key((i * 100, 0, i * 100 + 50, 50), 1, None), picture(i as u8));
        }
        assert_eq!(c.len(), CACHE_MAX);
        assert!(c.get(&key((0, 0, 50, 50), 1, None)).is_none(), "oldest dropped");
        let last = CACHE_MAX as i32 + 1;
        assert!(c.get(&key((last * 100, 0, last * 100 + 50, 50), 1, None)).is_some());
    }

    /// A live wallpaper is never asked to draw for a copy.
    #[test]
    fn live_wallpapers_are_not_snapshotted() {
        let explorer = 4242;
        assert!(!is_live(explorer, &[]), "plain desktop");
        assert!(!is_live(explorer, &[explorer, explorer]), "Explorer's own windows (icons, WorkerW)");
        assert!(!is_live(explorer, &[0]), "a window gone meanwhile");
        assert!(is_live(explorer, &[explorer, 9000]), "another process's window in the layer");
        assert_eq!(source(false), Source::Snapshot);
        assert_eq!(source(true), Source::File);
        // A still desktop: a plain print first, full content only after.
        assert_eq!(PRINT_TRIES, [0, 2]);
    }

    /// Right before PrintWindow, on the capture thread: printed only when
    /// the target and everything in the layer is Explorer's, and all of it
    /// could be read. A live wallpaper that attached after the mica
    /// worker's look (a new foreign child) stops it.
    #[test]
    fn the_capture_thread_prints_only_explorer_s_own_layer() {
        let ex = 4242;
        assert!(may_print(Some(ex), Some(ex), Some(&[])), "plain desktop, no children");
        assert!(may_print(Some(ex), Some(ex), Some(&[ex, ex, 0])), "icons, a window gone meanwhile");
        assert!(!may_print(Some(ex), Some(ex), Some(&[ex, 9000])), "attached since the first look");
        assert!(!may_print(Some(ex), Some(9000), Some(&[])), "the window picked is another process's");
        assert!(!may_print(None, Some(ex), Some(&[])), "no Progman: unclear");
        assert!(!may_print(Some(ex), None, Some(&[])), "the target's owner unknown (gone)");
        assert!(!may_print(Some(ex), Some(ex), None), "the layer could not be read");
        assert!(!may_print(Some(0), Some(0), Some(&[])), "no process at all");
    }

    #[test]
    fn blur_keeps_flat_color() {
        let mut px = [10u8, 20, 30, 255].repeat(8 * 6);
        let before = px.clone();
        blur_bgra(&mut px, 8, 6, 2, 3);
        assert_eq!(px, before);
    }

    #[test]
    fn blur_spreads_a_bright_pixel() {
        let (w, h) = (9usize, 9usize);
        let mut px = vec![0u8; w * h * 4];
        let mid = (4 * w + 4) * 4;
        px[mid..mid + 4].copy_from_slice(&[255, 255, 255, 255]);
        blur_bgra(&mut px, w, h, 1, 1);
        let center = px[mid] as u32;
        let neighbour = px[mid + 4] as u32;
        assert!(center < 255 && center > 0, "center {center}");
        assert!(neighbour > 0, "light must spread to neighbours");
        assert_eq!(px[0], 0, "far corner stays dark with radius 1");
    }

    #[test]
    fn cover_crop_keeps_aspect() {
        // 1000x1000 image on a 16:10 monitor: full width, middle band.
        let r = cover_crop(1000, 1000, 160, 100);
        assert_eq!((r.X, r.Y, r.Width, r.Height), (0, 187, 1000, 625)); // (1000-625)/2 rounds down
        // 4000x1000 panorama on 16:10: full height, middle slice.
        let r = cover_crop(4000, 1000, 160, 100);
        assert_eq!((r.X, r.Y, r.Width, r.Height), (1200, 0, 1600, 1000));
    }

    #[test]
    fn blank_only_when_marker_survives() {
        assert!(is_blank(&[255u8, 0, 255, 255].repeat(100)), "untouched marker = nothing drawn");
        assert!(!is_blank(&[7u8, 7, 7, 255].repeat(100)), "a dark wallpaper is a real capture");
        assert!(!is_blank(&[0u8, 0, 0, 255].repeat(100)), "pure black is a real capture");
        let mut mostly = [255u8, 0, 255, 255].repeat(100);
        for k in 0..20 {
            mostly[k * 4..k * 4 + 4].copy_from_slice(&[30, 90, 40, 255]);
        }
        assert!(!is_blank(&mostly), "80% marker means something was drawn");
        assert!(is_blank(&[]));
    }

    #[test]
    fn wallpaper_placement_is_read_as_windows_writes_it() {
        assert_eq!(placement("10", "0"), Some(Placement::Fill));
        assert_eq!(placement("", ""), Some(Placement::Fill), "never set: Windows' default");
        assert_eq!(placement("6", "0"), Some(Placement::Fit));
        assert_eq!(placement("2", "0"), Some(Placement::Stretch));
        assert_eq!(placement("0", "0"), Some(Placement::Center));
        assert_eq!(placement("0", "1"), Some(Placement::Tile));
        assert_eq!(placement("22", "0"), Some(Placement::Span));
        assert_eq!(placement("7", "0"), None);
        assert_eq!(parse_rgb("0 99 177"), Some([0, 99, 177]));
        assert_eq!(parse_rgb("1 2"), None);
        assert_eq!(parse_rgb("a b c"), None);
    }

    #[test]
    fn each_placement_lands_where_the_desktop_shows_it() {
        // A 1000x1000 image, a 1600x1000 monitor drawn on a 160x100 canvas.
        let p = |pl| plan(Some(pl), 1000, 1000, 1600, 1000, 160, 100);
        assert_eq!(p(Placement::Fill), Plan::Cover(cover_crop(1000, 1000, 160, 100)));
        assert_eq!(p(Placement::Stretch), Plan::Place { w: 160, h: 100, x: 0, y: 0, tile: false });
        // Fit: the whole image, as tall as the canvas, bars left and right.
        assert_eq!(p(Placement::Fit), Plan::Place { w: 100, h: 100, x: 30, y: 0, tile: false });
        // Center: its own size (1/10 here), in the middle.
        assert_eq!(p(Placement::Center), Plan::Place { w: 100, h: 100, x: 30, y: 0, tile: false });
        // A small tile repeats from the corner.
        assert_eq!(plan(Some(Placement::Tile), 200, 100, 1600, 1000, 160, 100), Plan::Place { w: 20, h: 10, x: 0, y: 0, tile: true });
        // A big centred image is cropped by the canvas (negative offset).
        assert_eq!(plan(Some(Placement::Center), 3200, 2000, 1600, 1000, 160, 100), Plan::Place { w: 320, h: 200, x: -80, y: -50, tile: false });
        assert_eq!(p(Placement::Span), Plan::Solid);
        assert_eq!(plan(None, 1000, 1000, 1600, 1000, 160, 100), Plan::Solid);
        assert_eq!(plan(Some(Placement::Fill), 0, 0, 1600, 1000, 160, 100), Plan::Solid);
    }

    #[test]
    fn placed_images_sit_on_the_background_colour() {
        let mut canvas = solid_canvas([10, 20, 30], 4, 2);
        assert_eq!(&canvas[..4], &[30, 20, 10, 255], "BGRA");
        // A 2x1 white image at x 3 of row 1: one pixel lands, one is cropped.
        blit(&mut canvas, 4, 2, &[255u8; 8], 2, 1, 3, 1);
        assert_eq!(&canvas[(4 + 3) * 4..(4 + 3) * 4 + 4], &[255, 255, 255, 255]);
        assert_eq!(&canvas[4 * 4..4 * 4 + 4], &[30, 20, 10, 255], "left of it untouched");
        // Half-transparent black (premultiplied) darkens the colour by half.
        let mut c = solid_canvas([200, 200, 200], 1, 1);
        blit(&mut c, 1, 1, &[0, 0, 0, 128], 1, 1, 0, 0);
        assert_eq!(c[0], (200 * 127 / 255) as u8);
    }
}
