//! GPU plumbing: D3D11 -> D2D -> Windows.UI.Composition graphics device.

use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;

use windows::core::{w, Error, Interface, Result, BOOL, HRESULT};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, E_INVALIDARG, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Direct3D11::ID3D11Device4;
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_REMOVED;
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::System::WinRT::Composition::ICompositionGraphicsDeviceInterop;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_TEXT_METRICS,
};
use windows_numerics::Vector2;
use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::UI::Composition::{CompositionDrawingSurface, CompositionGraphicsDevice, Compositor};
use windows::Win32::Foundation::{HMODULE, POINT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1, D2D1_BITMAP_OPTIONS_NONE,
    D2D1_BITMAP_PROPERTIES1, D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG,
    D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::{ICompositionDrawingSurfaceInterop, ICompositorInterop};

/// D2DERR_RECREATE_TARGET (not exported by the windows crate's feature set).
const D2DERR_RECREATE_TARGET: u32 = 0x8899_000C;

/// HRESULTs that mean the GPU device is gone (driver reset / TDR, device
/// removed or hung, driver error): only a new device helps.
pub fn is_device_lost(code: HRESULT) -> bool {
    matches!(
        code.0 as u32,
        0x887A_0005 // DXGI_ERROR_DEVICE_REMOVED
            | 0x887A_0006 // DXGI_ERROR_DEVICE_HUNG
            | 0x887A_0007 // DXGI_ERROR_DEVICE_RESET
            | 0x887A_0020 // DXGI_ERROR_DRIVER_INTERNAL_ERROR
            | D2DERR_RECREATE_TARGET
    )
}

/// Largest surface side we ask for (D2D's own limit is 16384 on every
/// feature level we create; bigger requests are bugs or hostile input).
pub const MAX_SURFACE_SIDE: u32 = 16_384;

/// Byte length of a tightly packed `w`x`h` BGRA image, or None if it is
/// empty, larger than MAX_SURFACE_SIDE on a side, or overflows.
pub fn bgra_len(w: u32, h: u32) -> Option<usize> {
    if w == 0 || h == 0 || w > MAX_SURFACE_SIDE || h > MAX_SURFACE_SIDE {
        return None;
    }
    (w as usize).checked_mul(h as usize)?.checked_mul(4)
}

/// Waits between recovery attempts after a device loss (ms). The first try
/// is at once; when the list runs out the dock gives up (see dock/gpu.rs).
pub const RETRY_MS: [u32; 10] = [0, 250, 500, 1_000, 2_000, 4_000, 8_000, 15_000, 15_000, 15_000];
/// Attempts that insist on the hardware device (the driver is usually back
/// within a few seconds); after them the software (WARP) device is fine.
pub const HW_ONLY_ATTEMPTS: u32 = 5;
/// More losses than this within LOSS_WINDOW_MS: something is badly wrong
/// (a driver that keeps resetting); give up instead of looping.
pub const LOSS_MAX: usize = 5;
pub const LOSS_WINDOW_MS: u64 = 120_000;

/// Bounded recovery schedule after GPU device losses (pure, tested).
#[derive(Debug, Default)]
pub struct RecoveryPlan {
    /// Failed attempts in the current incident.
    attempt: u32,
    /// When recent incidents started (ms, any monotonic clock).
    losses: Vec<u64>,
}

impl RecoveryPlan {
    /// A new loss was noticed at `now_ms`. False: too many lately (give up).
    pub fn lost(&mut self, now_ms: u64) -> bool {
        self.attempt = 0;
        self.losses.retain(|&t| now_ms.saturating_sub(t) < LOSS_WINDOW_MS);
        self.losses.push(now_ms);
        self.losses.len() <= LOSS_MAX
    }
    /// Wait before the next attempt; None = attempts used up.
    pub fn next_delay(&self) -> Option<u32> {
        RETRY_MS.get(self.attempt as usize).copied()
    }
    /// The next attempt may fall back to the software device.
    pub fn allow_warp(&self) -> bool {
        self.attempt >= HW_ONLY_ATTEMPTS
    }
    pub fn failed(&mut self) {
        self.attempt = self.attempt.saturating_add(1);
    }
    pub fn succeeded(&mut self) {
        self.attempt = 0;
    }
}

/// The rendering device behind the composition graphics device: replaced
/// as a whole after a device loss.
struct Gpu {
    d3d: ID3D11Device,
    _d2d: ID2D1Device,
    /// Software (WARP) device.
    warp: bool,
    /// RegisterDeviceRemovedEvent cookie on `d3d`.
    cookie: Option<u32>,
}

impl Drop for Gpu {
    fn drop(&mut self) {
        if let Some(c) = self.cookie.take() {
            if let Ok(d4) = self.d3d.cast::<ID3D11Device4>() {
                unsafe { d4.UnregisterDeviceRemoved(c) };
            }
        }
    }
}

/// Where a line of text stands in its surface: the surface's height and the
/// line's baseline from its top, px (whole pixels; topbar::baseline). Cells
/// drawn on one TextLine share their baseline whatever font their letters
/// fall back to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextLine {
    pub box_h: f32,
    pub baseline: f32,
}

pub struct Gfx {
    pub compositor: Compositor,
    /// Stays the same object for the whole run (surfaces belong to it); its
    /// rendering device is swapped by recover().
    pub device: CompositionGraphicsDevice,
    gpu: RefCell<Gpu>,
    dwrite: IDWriteFactory,
    d2d_factory: ID2D1Factory1,
    /// The device is gone and recover() has not succeeded yet: drawing fails
    /// at once instead of hammering a dead device.
    lost: Cell<bool>,
    /// The loss notice could not be posted yet (see tell_owner).
    unnotified: Cell<bool>,
    /// Who hears about a loss: (window, message), 0 = nobody.
    notify: Cell<(isize, u32)>,
    /// Auto-reset event the D3D runtime signals on device removal (0 = none).
    removed_event: Cell<isize>,
    /// The rendering device's generation: one more every time swap_in()
    /// hands the composition device a new one (what was drawn into
    /// surfaces before is gone: caches of drawn surfaces check it).
    generation: Cell<u32>,
}

/// Posts `msg` to `hwnd`, retrying for a while if the queue is full
/// (helper threads only: it sleeps between tries).
fn post_retrying(hwnd: isize, msg: u32) {
    for i in 0..150u64 {
        if unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0)) }.is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20 * (i + 1).min(10)));
    }
    crate::log!("gfx: device-loss notice still not delivered; the next draw or display change retries");
}

/// Waits on the device-removed event (for the life of the process; the
/// event belongs to the Gfx, which also lives that long).
struct LossWatch {
    ev: isize,
    hwnd: isize,
    msg: u32,
}

impl crate::service::Watch for LossWatch {
    fn name(&self) -> &'static str {
        "gpu device removed"
    }
    fn handles(&self) -> Vec<HANDLE> {
        vec![HANDLE(self.ev as *mut _)]
    }
    fn wake(&mut self, why: crate::service::Wake) -> crate::service::Next {
        if matches!(why, crate::service::Wake::Signaled(_)) {
            let posted = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), self.msg, WPARAM(0), LPARAM(0)) }.is_ok();
            if !posted {
                // Retried with pauses: not on the shared thread.
                let (h, msg) = (self.hwnd, self.msg);
                let _ = std::thread::Builder::new().name("gpu-notice".into()).spawn(move || post_retrying(h, msg));
            }
        }
        crate::service::Next::Wait
    }
}

/// Device flags. PREVENT_INTERNAL_THREADING_OPTIMIZATIONS asks the runtime
/// and the driver not to start worker threads of their own (a dock draws a
/// few small surfaces now and then; drawing is unchanged). Measured on a
/// 24-thread AMD laptop: each hardware device costs 24 driver threads
/// without it and 22 with it. Most of that pool cannot be turned off, so
/// the bigger lever is keeping only one device alive while idle.
pub const DEVICE_FLAGS: D3D11_CREATE_DEVICE_FLAG =
    D3D11_CREATE_DEVICE_FLAG(D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS.0);

/// D3D11CreateDevice with DEVICE_FLAGS, or (a driver that refuses the
/// threading hint) with BGRA support only.
pub fn create_device(kind: D3D_DRIVER_TYPE, ctx: Option<*mut Option<ID3D11DeviceContext>>) -> Result<ID3D11Device> {
    let make = |flags| {
        let mut device = None;
        unsafe {
            D3D11CreateDevice(None, kind, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut device), None, ctx)?;
        }
        device.ok_or_else(|| Error::from(E_FAIL))
    };
    make(DEVICE_FLAGS).or_else(|e| {
        crate::log!("gfx: device without the threading hint ({e})");
        make(D3D11_CREATE_DEVICE_BGRA_SUPPORT)
    })
}

fn create_d3d(kind: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    create_device(kind, None)
}

/// NOPBAR_RENDER=software: draw with the software device (WARP) from the
/// start. An experiment switch for measuring, off unless set: on some GPU
/// drivers the hardware device alone costs ~24 MB private memory, 22
/// threads and 600 handles per process, WARP ~1.5 MB, 1 and 60. DWM still
/// composites on the GPU either way; only our drawing into surfaces moves
/// to the CPU.
///
/// The config file's `effects.render` (not in Settings) is the same switch:
/// "software" draws on WARP, "gpu" and "auto" on the hardware
/// device (WARP only as the recovery fallback). Read when the process
/// starts (set_render_choice), so a change applies on the next start.
pub fn prefer_software() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let env = std::env::var("NOPBAR_RENDER").is_ok_and(|v| v.eq_ignore_ascii_case("software"));
        let chosen = SOFTWARE_CHOSEN.load(std::sync::atomic::Ordering::SeqCst);
        if env {
            crate::log!("gfx: NOPBAR_RENDER=software: drawing on the software device");
        } else if chosen {
            crate::log!("gfx: rendering set to software: drawing on the software device");
        }
        env || chosen
    })
}

/// The config's rendering choice ("auto", "gpu", "software").
static SOFTWARE_CHOSEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The process's own device (the one watching for losses: the dock's, the
/// settings window's) is the software one (WARP), chosen or a recovery's
/// fallback. Glass counts it as blur off (theme::glass_blur).
static SOFTWARE_IN_FORCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Drawing on the software device now (see SOFTWARE_IN_FORCE).
pub fn software_in_force() -> bool {
    SOFTWARE_IN_FORCE.load(std::sync::atomic::Ordering::SeqCst)
}

/// Called once at startup, before the first device is made.
pub fn set_render_choice(choice: &str) {
    SOFTWARE_CHOSEN.store(choice.trim().eq_ignore_ascii_case("software"), std::sync::atomic::Ordering::SeqCst);
}

/// Which devices to try, in order (true = WARP). The user's software
/// choice comes before the recovery plan's hardware-only tries, at start
/// and on every recovery alike (a software dock stays software); hardware
/// is its fallback only if WARP itself fails.
pub fn device_order(software: bool, allow_warp: bool) -> &'static [bool] {
    match (software, allow_warp) {
        (true, _) => &[true, false],
        (false, true) => &[false, true],
        (false, false) => &[false],
    }
}

/// A D3D11 device without its D2D device yet: made by make_d3d, on any
/// thread (an ID3D11Device is free-threaded), and wrapped on the UI thread
/// (the D2D factory is single-threaded: its devices are made and used
/// there only).
pub struct MadeDevice {
    d3d: ID3D11Device,
    warp: bool,
}

impl MadeDevice {
    /// The software (WARP) device.
    pub fn warp(&self) -> bool {
        self.warp
    }
}

/// A D3D11 device (hardware; WARP if allowed and the hardware one fails,
/// or first with NOPBAR_RENDER=software). `software` is prefer_software(),
/// read by the caller (helper threads get it passed).
fn make_d3d(software: bool, allow_warp: bool) -> Result<MadeDevice> {
    let mut last = Error::from(E_FAIL);
    for &warp in device_order(software, allow_warp) {
        match create_d3d(if warp { D3D_DRIVER_TYPE_WARP } else { D3D_DRIVER_TYPE_HARDWARE }) {
            Ok(d3d) => return Ok(MadeDevice { d3d, warp }),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// The D2D device for a made D3D11 device (UI thread: `factory` is
/// single-threaded).
fn wrap(factory: &ID2D1Factory1, made: MadeDevice) -> Result<Gpu> {
    let dxgi: IDXGIDevice = made.d3d.cast()?;
    let d2d = unsafe { factory.CreateDevice(&dxgi)? };
    Ok(Gpu { d3d: made.d3d, _d2d: d2d, warp: made.warp, cookie: None })
}

/// A D3D11 device and its D2D device, made right here (startup, and the
/// settings window's own recovery).
fn create_gpu(factory: &ID2D1Factory1, allow_warp: bool) -> Result<Gpu> {
    wrap(factory, make_d3d(prefer_software(), allow_warp)?)
}

/// Devices made by the helper thread (make_device_async), by ticket, until
/// the UI thread takes them.
type Made = std::result::Result<MadeDevice, HRESULT>;
static MADE: std::sync::Mutex<Vec<(u64, Made)>> = std::sync::Mutex::new(Vec::new());
static NEXT_TICKET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// A helper is inside device creation. One at a time: a driver that hangs
/// creation (it is being replaced) holds one thread, not one per retry.
static MAKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Makes a D3D11 device on a helper thread: creating one can block for
/// seconds inside a GPU driver that is being reset or replaced, and the UI
/// thread must keep answering meanwhile (a hung dock is a dead taskbar).
/// When done the helper posts `msg` (wParam = the ticket returned here) to
/// `hwnd`, and the UI thread takes the device with take_made() and installs
/// it with Gfx::install(). None: a helper is still busy (the caller counts
/// that as a failed attempt) or no thread could be started.
pub fn make_device_async(allow_warp: bool, hwnd: HWND, msg: u32) -> Option<u64> {
    use std::sync::atomic::Ordering::SeqCst;
    if MAKING.swap(true, SeqCst) {
        return None;
    }
    let ticket = NEXT_TICKET.fetch_add(1, SeqCst);
    let software = prefer_software();
    let h = hwnd.0 as isize;
    let spawned = std::thread::Builder::new().name("gpu-device".into()).spawn(move || {
        let made = make_d3d(software, allow_warp).map_err(|e| e.code());
        if let Ok(mut m) = MADE.lock() {
            m.push((ticket, made));
        }
        MAKING.store(false, SeqCst);
        if !post_ticket(h, msg, ticket) {
            // Nobody will take it (the window is gone): released here.
            let gone = MADE.lock().ok().and_then(|mut m| m.iter().position(|(t, _)| *t == ticket).map(|i| m.swap_remove(i)));
            drop(gone);
        }
    });
    if spawned.is_err() {
        MAKING.store(false, SeqCst);
        return None;
    }
    Some(ticket)
}

/// Posts a made device's ticket, retrying briefly if the queue is full;
/// false once the window is gone or it never went through (the owner's
/// deadline counts the attempt as failed).
fn post_ticket(hwnd: isize, msg: u32, ticket: u64) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;
    let h = HWND(hwnd as *mut _);
    for i in 0..40u64 {
        if !unsafe { IsWindow(Some(h)) }.as_bool() {
            return false;
        }
        if unsafe { PostMessageW(Some(h), msg, WPARAM(ticket as usize), LPARAM(0)) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10 * (i + 1).min(10)));
    }
    false
}

/// Takes the device made for `ticket` (UI thread, on the helper's message).
/// Older results nobody asked for any more go too. None: not there (taken
/// already, or dropped because the window was gone).
pub fn take_made(ticket: u64) -> Option<Result<MadeDevice>> {
    let mut m = MADE.lock().ok()?;
    let mut found = None;
    let mut i = 0;
    while i < m.len() {
        if m[i].0 == ticket {
            found = Some(m.swap_remove(i).1);
        } else if m[i].0 < ticket {
            drop(m.swap_remove(i));
        } else {
            i += 1;
        }
    }
    drop(m);
    found.map(|r| r.map_err(Error::from))
}

thread_local! {
    /// This thread's long-lived Gfx (the dock's), for one-off panels.
    static SHARED: RefCell<std::rc::Weak<Gfx>> = RefCell::new(std::rc::Weak::new());
}

/// Offers `gfx` to one-off panels on this thread (a launch notice), so
/// they do not make a device of their own: a second hardware device costs
/// memory and driver threads, its creation can block in a driver being
/// replaced, and nobody would hear when it is lost.
pub fn share_on_this_thread(gfx: &std::rc::Rc<Gfx>) {
    SHARED.with(|s| *s.borrow_mut() = std::rc::Rc::downgrade(gfx));
}

/// The Gfx offered by share_on_this_thread(), if any.
pub fn shared() -> Option<std::rc::Rc<Gfx>> {
    SHARED.with(|s| s.borrow().upgrade())
}

impl Gfx {
    /// The DirectWrite factory (text measured without a layout kept).
    pub fn dwrite(&self) -> &IDWriteFactory {
        &self.dwrite
    }

    /// The metrics (DWRITE_FONT_METRICS, design units) of `family`'s face
    /// at `weight`, upright and normal width, as text_on draws it (the UI
    /// font mapped as fonts::map does; the built-in Pretendard from its own
    /// collection). None: not installed.
    pub fn font_metrics(&self, family: &str, weight: DWRITE_FONT_WEIGHT) -> Option<windows::Win32::Graphics::DirectWrite::DWRITE_FONT_METRICS> {
        use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_METRICS;
        unsafe {
            let mapped = crate::fonts::map(family);
            let fam = windows::core::HSTRING::from(crate::fonts::dwrite_name(&mapped));
            let coll = match crate::fonts::collection_for(&mapped) {
                Some(c) => c,
                None => {
                    let mut coll = None;
                    self.dwrite.GetSystemFontCollection(&mut coll, false).ok()?;
                    coll?
                }
            };
            let (mut index, mut exists) = (0u32, BOOL(0));
            coll.FindFamilyName(&fam, &mut index, &mut exists).ok()?;
            if !exists.as_bool() {
                return None;
            }
            let font = coll.GetFontFamily(index).ok()?.GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL).ok()?;
            let mut m = DWRITE_FONT_METRICS::default();
            font.GetMetrics(&mut m);
            Some(m)
        }
    }

    pub fn new() -> Result<Self> {
        let compositor = Compositor::new()?;
        unsafe {
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&D2D1_FACTORY_OPTIONS::default()))?;
            let gpu = create_gpu(&factory, true)?;
            let interop: ICompositorInterop = compositor.cast()?;
            let device = interop.CreateGraphicsDevice(&gpu._d2d)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Self {
                compositor,
                device,
                gpu: RefCell::new(gpu),
                dwrite,
                d2d_factory: factory,
                lost: Cell::new(false),
                unnotified: Cell::new(false),
                notify: Cell::new((0, 0)),
                removed_event: Cell::new(0),
                generation: Cell::new(0),
            })
        }
    }

    /// Posts `msg` to `hwnd` when the device is lost: from a watch on the
    /// service thread that waits on the D3D device-removed event (no
    /// polling, catches losses while nothing is being drawn) and from
    /// failed draws.
    pub fn watch_device_loss(&self, hwnd: HWND, msg: u32) {
        // The watched device is the process's own: what it runs on counts.
        SOFTWARE_IN_FORCE.store(self.on_warp(), std::sync::atomic::Ordering::SeqCst);
        self.notify.set((hwnd.0 as isize, msg));
        if self.removed_event.get() != 0 {
            return;
        }
        let Ok(ev) = (unsafe { CreateEventW(None, false, false, None) }) else {
            crate::log!("gfx: no device-removed event; losses are noticed when drawing");
            return;
        };
        let (raw_ev, raw_hwnd) = (ev.0 as isize, hwnd.0 as isize);
        if crate::service::add(Box::new(LossWatch { ev: raw_ev, hwnd: raw_hwnd, msg })).is_none() {
            unsafe {
                let _ = CloseHandle(ev);
            }
            crate::log!("gfx: no device-removed watch; losses are noticed when drawing");
            return;
        }
        self.removed_event.set(raw_ev);
        let mut gpu = self.gpu.borrow_mut();
        self.register_removed(&mut gpu);
    }

    fn register_removed(&self, gpu: &mut Gpu) {
        let ev = self.removed_event.get();
        if ev == 0 || gpu.cookie.is_some() {
            return;
        }
        match gpu.d3d.cast::<ID3D11Device4>().and_then(|d4| unsafe { d4.RegisterDeviceRemovedEvent(HANDLE(ev as *mut _)) }) {
            Ok(c) => gpu.cookie = Some(c),
            Err(e) => crate::log!("gfx: cannot watch for device removal ({e}); losses are noticed when drawing"),
        }
    }

    /// The device is gone and not replaced yet.
    pub fn is_lost(&self) -> bool {
        self.lost.get()
    }

    /// Asks the device whether it was removed (cheap). True if it is lost.
    pub fn check_device(&self) -> bool {
        if self.lost.get() {
            if self.unnotified.get() {
                self.tell_owner();
            }
            return true;
        }
        let r = unsafe { self.gpu.borrow().d3d.GetDeviceRemovedReason() };
        if let Err(e) = r {
            self.mark_lost(e.code());
        }
        self.lost.get()
    }

    /// Running on the software device (after a recovery fell back to it).
    pub fn on_warp(&self) -> bool {
        self.gpu.borrow().warp
    }

    /// The video memory this process uses on the adapter it renders with
    /// (bytes: dedicated plus shared, as Task Manager's "GPU memory"
    /// counts it). A cheap kernel query; None without a device or on an
    /// adapter that does not say (IDXGIAdapter3 is Windows 10 and later).
    pub fn video_memory(&self) -> Option<u64> {
        use windows::Win32::Graphics::Dxgi::{IDXGIAdapter3, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO};
        if self.lost.get() {
            return None;
        }
        let d3d = self.gpu.try_borrow().ok()?.d3d.clone();
        unsafe {
            let adapter: IDXGIAdapter3 = d3d.cast::<IDXGIDevice>().ok()?.GetAdapter().ok()?.cast().ok()?;
            let mut sum = 0u64;
            for group in [DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL] {
                let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                adapter.QueryVideoMemoryInfo(0, group, &mut info).ok()?;
                sum = sum.saturating_add(info.CurrentUsage);
            }
            Some(sum)
        }
    }

    fn mark_lost(&self, code: HRESULT) {
        if self.lost.replace(true) {
            return;
        }
        crate::log!("gfx: GPU device lost ({:#010x})", code.0 as u32);
        self.tell_owner();
    }

    /// Posts the loss notice to the owner. A notice that cannot be posted
    /// is retried from a helper, and again from the next draw or device
    /// check: the loss is never left without a recovery on its way.
    fn tell_owner(&self) {
        let (h, msg) = self.notify.get();
        if h == 0 {
            return;
        }
        let posted = unsafe { PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), LPARAM(0)) }.is_ok();
        self.unnotified.set(!posted);
        if !posted {
            crate::log!("gfx: could not post the device-loss notice; retrying");
            let _ = std::thread::Builder::new().name("gpu-notice".into()).spawn(move || post_retrying(h, msg));
        }
    }

    /// A drawing call failed: was it the device?
    fn note_failure(&self, e: &Error) {
        if self.lost.get() {
            return;
        }
        if is_device_lost(e.code()) {
            self.mark_lost(e.code());
        } else if let Err(r) = unsafe { self.gpu.borrow().d3d.GetDeviceRemovedReason() } {
            self.mark_lost(r.code());
        }
    }

    /// Makes a new D3D / D2D device and hands it to the composition graphics
    /// device (the documented way back from a device loss). Every surface
    /// keeps existing but has lost its pixels: the owner redraws them all.
    /// Returns whether the new device is the software one.
    pub fn recover(&self, allow_warp: bool) -> Result<bool> {
        let gpu = create_gpu(&self.d2d_factory, allow_warp)?;
        self.swap_in(gpu)
    }

    /// recover() with a device made on a helper thread (make_device_async):
    /// only the D2D device is made here, on the UI thread.
    pub fn install(&self, made: MadeDevice) -> Result<bool> {
        let gpu = wrap(&self.d2d_factory, made)?;
        self.swap_in(gpu)
    }

    /// Hands a new device to the composition graphics device.
    fn swap_in(&self, mut gpu: Gpu) -> Result<bool> {
        let interop: ICompositionGraphicsDeviceInterop = self.device.cast()?;
        unsafe { interop.SetRenderingDevice(&gpu._d2d)? };
        self.register_removed(&mut gpu);
        let warp = gpu.warp;
        SOFTWARE_IN_FORCE.store(warp, std::sync::atomic::Ordering::SeqCst);
        // The old device (and its removal registration) goes now.
        *self.gpu.borrow_mut() = gpu;
        self.lost.set(false);
        self.unnotified.set(false);
        self.generation.set(self.generation.get().wrapping_add(1));
        Ok(warp)
    }

    /// The rendering device's generation (see the field): it changes when
    /// every surface's pixels are lost.
    pub fn generation(&self) -> u32 {
        self.generation.get()
    }

    /// A small offscreen target on the shared device (the companion's
    /// per-pixel layered window). Recreate it when generation changes.
    pub fn offscreen_context(&self) -> Result<ID2D1DeviceContext> {
        unsafe {
            self.gpu.borrow()._d2d.CreateDeviceContext(
                windows::Win32::Graphics::Direct2D::D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
            )
        }
    }

    /// Creates a surface of `w`x`h` px and lets `f` draw into it. `f` gets the
    /// device context and the (x, y) offset to draw at.
    pub fn draw(
        &self,
        w: u32,
        h: u32,
        f: impl FnOnce(&ID2D1DeviceContext, f32, f32) -> Result<()>,
    ) -> Result<CompositionDrawingSurface> {
        if self.lost.get() {
            // Waiting for recover(); the owner redraws everything after it.
            if self.unnotified.get() {
                self.tell_owner();
            }
            return Err(Error::from(DXGI_ERROR_DEVICE_REMOVED));
        }
        if w > MAX_SURFACE_SIDE || h > MAX_SURFACE_SIDE {
            return Err(Error::from(E_INVALIDARG));
        }
        let r = self.draw_inner(w, h, f);
        if let Err(e) = &r {
            self.note_failure(e);
        }
        r
    }

    fn draw_inner(
        &self,
        w: u32,
        h: u32,
        f: impl FnOnce(&ID2D1DeviceContext, f32, f32) -> Result<()>,
    ) -> Result<CompositionDrawingSurface> {
        let surface = self.device.CreateDrawingSurface(
            Size { Width: w.max(1) as f32, Height: h.max(1) as f32 },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        self.redraw(&surface,f)?;
        Ok(surface)
    }

    /// Update retained content without allocating a new composition surface.
    pub fn redraw(&self,surface:&CompositionDrawingSurface,f:impl FnOnce(&ID2D1DeviceContext,f32,f32)->Result<()>)->Result<()> {
        if self.lost.get(){return Err(Error::from(DXGI_ERROR_DEVICE_REMOVED));}
        let result=(||unsafe {
            let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
            let mut offset = POINT::default();
            let dc: ID2D1DeviceContext = interop.BeginDraw(None, &mut offset)?;
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let r = f(&dc, offset.x as f32, offset.y as f32);
            interop.EndDraw()?;
            r
        })();
        if let Err(e)=&result {self.note_failure(e);}
        result
    }

    /// Uploads premultiplied BGRA pixels into a composition surface. The
    /// pixels must be exactly `w`x`h` tightly packed (checked: they come
    /// from shell extensions, wallpapers and other apps).
    pub fn surface_from_bgra(&self, w: u32, h: u32, pixels: &[u8]) -> Result<CompositionDrawingSurface> {
        if bgra_len(w, h) != Some(pixels.len()) {
            return Err(Error::from(E_INVALIDARG));
        }
        self.draw(w, h, |dc, ox, oy| unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: ManuallyDrop::new(None),
            };
            let bmp = dc.CreateBitmap(
                D2D_SIZE_U { width: w, height: h },
                Some(pixels.as_ptr() as _),
                w * 4,
                &props,
            )?;
            let dest = D2D_RECT_F { left: ox, top: oy, right: ox + w as f32, bottom: oy + h as f32 };
            dc.DrawBitmap(&bmp, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
            Ok(())
        })
    }

    /// Draws premultiplied BGRA pixels (`w`x`h`, checked like
    /// surface_from_bgra) into a `side`x`side` surface, fitted and centered
    /// (aspect kept), with D2D's high-quality cubic filter; `squircle`: cut
    /// to the squircle filling the square (antialiased by D2D at this size,
    /// so the brush samples one bitmap instead of a picture and a mask
    /// apart). A picture shown at its surface's own size is sharp; the
    /// composition brush only filters bilinearly without mipmaps, which
    /// steps the edges when it shrinks a picture well below its size (and
    /// blurs a little at any other ratio: dock/sharp.rs).
    pub fn surface_fit(&self, w: u32, h: u32, pixels: &[u8], side: u32, squircle: bool) -> Result<CompositionDrawingSurface> {
        use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1};
        if bgra_len(w, h) != Some(pixels.len()) || side == 0 {
            return Err(Error::from(E_INVALIDARG));
        }
        let (dw, dh) = crate::icons::fit_box(w, h, side);
        let factory = self.d2d_factory.clone();
        self.draw(side, side, |dc, ox, oy| unsafe {
            if squircle {
                let geom = squircle_geometry(&factory, ox, oy, side as f32)?;
                let params = D2D1_LAYER_PARAMETERS1 {
                    contentBounds: D2D_RECT_F { left: ox, top: oy, right: ox + side as f32, bottom: oy + side as f32 },
                    geometricMask: ManuallyDrop::new(Some(geom.cast()?)),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: windows_numerics::Matrix3x2::identity(),
                    opacity: 1.0,
                    opacityBrush: ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                };
                dc.PushLayer(&params, None);
                // Our reference goes (D2D holds its own while the layer is up).
                let _ = ManuallyDrop::into_inner(params.geometricMask);
            }
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: ManuallyDrop::new(None),
            };
            let drawn = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(pixels.as_ptr() as _), w * 4, &props).map(|bmp| {
                let (l, t) = (ox + (side as f32 - dw) / 2.0, oy + (side as f32 - dh) / 2.0);
                let dest = D2D_RECT_F { left: l, top: t, right: l + dw, bottom: t + dh };
                dc.DrawBitmap(&bmp, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
            });
            if squircle {
                // Popped on every way out (EndDraw follows).
                dc.PopLayer();
            }
            drawn
        })
    }

    /// The dock's name label text (design B3): footnote 12 in the UI font,
    /// cut with "…" past 240 DIP (never clipped mid-letter), on `bg` (the
    /// pill is the caller's visuals when `bg` is clear). Padding 8 across,
    /// at least 24 tall (both on the 4 DIP grid), its own pill's corners
    /// `radius` DIP (the theme's tip radius). Returns the surface and its
    /// size in DIPs (whole device pixels).
    pub fn label(&self, text: &str, scale: f32, bg: crate::theme::Rgba, fg: crate::theme::Rgba, radius: f32) -> Result<(CompositionDrawingSurface, f32, f32)> {
        let a = self.label_art(text, scale)?;
        let (w, h) = (a.w, a.h);
        let surface = self.draw(w as u32, h as u32, |dc, ox, oy| unsafe { paint_label(dc, ox, oy, &a, bg, fg, radius) })?;
        Ok((surface, w / scale, h / scale))
    }

    /// The name label's text laid out and its box (see `label`).
    pub fn label_art(&self, text: &str, scale: f32) -> Result<LabelArt> {
        use crate::theme::metrics::space;
        let (size, weight) = (crate::theme::ramp::FOOTNOTE.size, windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL);
        let (layout, tw, th) = line_layout(&self.dwrite, text, &crate::fonts::ui_family(), size * scale, weight, LABEL_MAX_W * scale)?;
        let (px, py) = (space::S3 * scale, space::S2 * scale);
        let w = (tw + px * 2.0).ceil();
        let h = (th + py * 2.0).max(LABEL_MIN_H * scale).ceil();
        Ok(LabelArt { layout, tw, th, w, h, scale })
    }

    /// Start button glyph: four rounded tiles.
    /// White superellipse |x|^5 + |y|^5 = 1 filling a px x px surface: the
    /// mask for squircle icons and backplates.
    pub fn squircle(&self, px: u32) -> Result<CompositionDrawingSurface> {
        let factory = self.d2d_factory.clone();
        self.draw(px, px, move |dc, ox, oy| unsafe {
            let geom = squircle_geometry(&factory, ox, oy, px as f32)?;
            let white = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)?;
            dc.FillGeometry(&geom, &white, None);
            Ok(())
        })
    }

    /// Plain text (no background) in `family` at `size` DIP: as tall as the
    /// text, or on `line` (the surface is the line's box, the text's first
    /// line stands on its baseline). Returns the surface and the text's
    /// size in DIPs.
    #[allow(clippy::too_many_arguments)]
    pub fn text_on(
        &self,
        text: &str,
        family: &str,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        color: D2D1_COLOR_F,
        scale: f32,
        line: Option<TextLine>,
    ) -> Result<(CompositionDrawingSurface, f32, f32)> {
        let a = self.text_art_on(text, family, size, weight, color, scale, line)?;
        let surface = self.paint_art(&a)?;
        Ok((surface, a.w / scale, a.h / scale))
    }

    /// text_on's art: what its surface is drawn from.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text_art_on(
        &self,
        text: &str,
        family: &str,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        color: D2D1_COLOR_F,
        scale: f32,
        line: Option<TextLine>,
    ) -> Result<TextArt> {
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            let fam = windows::core::HSTRING::from(&*crate::fonts::map(family));
            let format = ui_format(&self.dwrite, &fam, weight, size * scale * crate::fonts::scale())?;
            let layout = self.dwrite.CreateTextLayout(&wide, &format, 4000.0, 400.0)?;
            let rtl = read_rtl(&layout);
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            let w = m.widthIncludingTrailingWhitespace.ceil().max(1.0);
            if rtl {
                let _ = layout.SetMaxWidth(w);
            }
            let (h, y) = on_line(&layout, m.height, line);
            Ok(TextArt::new(layout, w, h, (0.0, y), color))
        }
    }

    /// One line of plain text at most `max_w` DIP wide: cut with "…" by
    /// its real width (never squeezed to fit). Returns the surface, the
    /// text's size in DIPs and whether it was cut.
    #[allow(clippy::too_many_arguments)]
    pub fn text_fit(
        &self,
        text: &str,
        family: &str,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        color: D2D1_COLOR_F,
        scale: f32,
        max_w: f32,
    ) -> Result<(CompositionDrawingSurface, f32, f32, bool)> {
        self.text_fit_on(text, family, size, weight, color, scale, max_w, None)
    }

    /// Like text_fit; on `line` (see text_on).
    #[allow(clippy::too_many_arguments)]
    pub fn text_fit_on(
        &self,
        text: &str,
        family: &str,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        color: D2D1_COLOR_F,
        scale: f32,
        max_w: f32,
        line: Option<TextLine>,
    ) -> Result<(CompositionDrawingSurface, f32, f32, bool)> {
        let (a, cut) = self.text_art_fit(text, family, size, weight, color, scale, max_w, line)?;
        let surface = self.paint_art(&a)?;
        Ok((surface, a.w / scale, a.h / scale, cut))
    }

    /// text_fit_on's art, and whether the text was cut.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text_art_fit(
        &self,
        text: &str,
        family: &str,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        color: D2D1_COLOR_F,
        scale: f32,
        max_w: f32,
        line: Option<TextLine>,
    ) -> Result<(TextArt, bool)> {
        use windows::Win32::Graphics::DirectWrite::{DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP};
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            let fam = windows::core::HSTRING::from(&*crate::fonts::map(family));
            let format = ui_format(&self.dwrite, &fam, weight, size * scale * crate::fonts::scale())?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            format.SetTrimming(&trim, &sign)?;
            let full = self.dwrite.CreateTextLayout(&wide, &format, 100_000.0, 400.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            full.GetMetrics(&mut m)?;
            let room = (max_w * scale).floor().max(1.0);
            let cut = m.widthIncludingTrailingWhitespace > room;
            let layout = self.dwrite.CreateTextLayout(&wide, &format, room, 400.0)?;
            let w = m.widthIncludingTrailingWhitespace.min(room).ceil().max(1.0);
            let (h, y) = on_line(&full, m.height, line);
            Ok((TextArt::new(layout, w, h, (0.0, y), color), cut))
        }
    }

    /// A surface of `a`.
    fn paint_art(&self, a: &TextArt) -> Result<CompositionDrawingSurface> {
        let (w, h) = a.size_px();
        self.draw(w, h, |dc, ox, oy| unsafe { a.paint(dc, ox, oy) })
    }

    /// Like text_fit, also giving the line's baseline (DIP from the top).
    /// `ellipsis` false: a cut line is not trimmed, just ends at `max_w`
    /// (the caller fades it out). On `line`, see text_on. Returns the
    /// surface, width, height, baseline and whether it was cut.
    #[allow(clippy::too_many_arguments)]
    pub fn text_line(
        &self,
        text: &str,
        family: &str,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        color: D2D1_COLOR_F,
        scale: f32,
        max_w: f32,
        ellipsis: bool,
        line: Option<TextLine>,
    ) -> Result<(CompositionDrawingSurface, f32, f32, f32, bool)> {
        use windows::Win32::Graphics::DirectWrite::{DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP};
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            let fam = windows::core::HSTRING::from(&*crate::fonts::map(family));
            let format = ui_format(&self.dwrite, &fam, weight, size * scale * crate::fonts::scale())?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            if ellipsis {
                let sign = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                format.SetTrimming(&trim, &sign)?;
            }
            let full = self.dwrite.CreateTextLayout(&wide, &format, 100_000.0, 400.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            full.GetMetrics(&mut m)?;
            // Same arithmetic as model::drawn_width.
            let room = (max_w * scale).floor().max(1.0);
            let cut = m.widthIncludingTrailingWhitespace > room;
            let layout = self.dwrite.CreateTextLayout(&wide, &format, room, 400.0)?;
            let w = m.widthIncludingTrailingWhitespace.min(room).ceil().max(1.0);
            // One line (no wrapping): its baseline; else most of its height.
            let (h, y) = on_line(&full, m.height, line);
            let baseline = y + first_baseline(&full, m.height);
            let surface = self.draw(w as u32, h as u32, |dc, ox, oy| {
                let fg = dc.CreateSolidColorBrush(&color, None)?;
                // An untrimmed cut line runs past `w`: kept to the surface.
                dc.PushAxisAlignedClip(&D2D_RECT_F { left: ox, top: oy, right: ox + w, bottom: oy + h }, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
                dc.DrawTextLayout(Vector2 { X: ox, Y: oy + y }, &layout, &fg, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                dc.PopAxisAlignedClip();
                Ok(())
            })?;
            Ok((surface, w / scale, h / scale, baseline / scale, cut))
        }
    }

    /// A text layout in px (for drawing several strings into one surface).
    /// Returns it with its width and height in px.
    pub fn text_layout(
        &self,
        text: &str,
        family: &str,
        size_px: f32,
        weight: DWRITE_FONT_WEIGHT,
    ) -> Result<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32, f32)> {
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            let fam = windows::core::HSTRING::from(&*crate::fonts::map(family));
            let format = ui_format(&self.dwrite, &fam, weight, size_px * crate::fonts::scale())?;
            let layout = self.dwrite.CreateTextLayout(&wide, &format, 4000.0, 400.0)?;
            let rtl = read_rtl(&layout);
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            let w = m.widthIncludingTrailingWhitespace.ceil();
            if rtl {
                // Its own width: it still draws from its left edge.
                let _ = layout.SetMaxWidth(w.max(1.0));
            }
            Ok((layout, w, m.height.ceil()))
        }
    }

    /// Like text_layout, on one line at most `max_w` px wide: a longer text
    /// is cut with "…". Returns it with its drawn width and height in px.
    pub fn text_layout_line(
        &self,
        text: &str,
        family: &str,
        size_px: f32,
        weight: DWRITE_FONT_WEIGHT,
        max_w: f32,
    ) -> Result<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32, f32)> {
        line_layout(&self.dwrite, text, family, size_px, weight, max_w)
    }

    /// Like text_layout, with tabular figures: every digit as wide as the
    /// others (a clock keeps its width as it ticks, when the font has
    /// them). Returns it with its width and height in px.
    pub fn text_layout_tabular(
        &self,
        text: &str,
        family: &str,
        size_px: f32,
        weight: DWRITE_FONT_WEIGHT,
    ) -> Result<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32, f32)> {
        use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_READING_DIRECTION_RIGHT_TO_LEFT, DWRITE_TEXT_RANGE};
        let (layout, w, h) = self.text_layout(text, family, size_px, weight)?;
        unsafe {
            let typo = self.dwrite.CreateTypography()?;
            typo.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 })?;
            let range = DWRITE_TEXT_RANGE { startPosition: 0, length: text.encode_utf16().count() as u32 };
            if layout.SetTypography(&typo, range).is_err() {
                return Ok((layout, w, h));
            }
            // A font whose tabular figures are only for Latin (Pretendard)
            // shapes digits after Hangul ("오후 3:05:07": one Hangul run)
            // proportional: every digit is given the widest's advance too.
            self.even_digits(&layout, text, family, size_px, weight);
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            let w = m.widthIncludingTrailingWhitespace.ceil();
            if layout.GetReadingDirection() == DWRITE_READING_DIRECTION_RIGHT_TO_LEFT {
                // Its own (new) width: it still draws from its left edge.
                let _ = layout.SetMaxWidth(w.max(1.0));
            }
            Ok((layout, w, m.height.ceil()))
        }
    }

    /// Every ASCII digit of `layout` (`text`'s) at least as wide as the
    /// font's widest digit, tabular or not (digit_advance), its glyph
    /// centered in that advance: the digits never change the text's
    /// width, in whatever script run they fall.
    fn even_digits(&self, layout: &windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, text: &str, family: &str, size_px: f32, weight: DWRITE_FONT_WEIGHT) {
        use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout1, DWRITE_TEXT_RANGE};
        let Ok(l1) = layout.cast::<IDWriteTextLayout1>() else { return };
        let adv = self.digit_advance(family, size_px, weight);
        if adv <= 0.0 {
            return;
        }
        for (start, len) in digit_runs(text) {
            unsafe {
                let _ = l1.SetCharacterSpacing(0.0, 0.0, adv, DWRITE_TEXT_RANGE { startPosition: start, length: len });
            }
        }
    }

    /// The widest advance of the ten digits of `family` at `size_px`,
    /// with tabular figures and without (cached per font and size).
    fn digit_advance(&self, family: &str, size_px: f32, weight: DWRITE_FONT_WEIGHT) -> f32 {
        use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_TEXT_RANGE};
        thread_local! {
            static CACHE: std::cell::RefCell<std::collections::HashMap<(String, u32, i32), f32>> = Default::default();
        }
        let key = (family.to_string(), size_px.to_bits(), weight.0);
        if let Some(w) = CACHE.with(|c| c.borrow().get(&key).copied()) {
            return w;
        }
        let fam = windows::core::HSTRING::from(&*crate::fonts::map(family));
        let mut widest = 0.0f32;
        unsafe {
            let Ok(format) = ui_format(&self.dwrite, &fam, weight, size_px * crate::fonts::scale()) else { return 0.0 };
            let typo = self.dwrite.CreateTypography().ok();
            if let Some(t) = &typo {
                let _ = t.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 });
            }
            for d in '0'..='9' {
                let wide: Vec<u16> = d.to_string().encode_utf16().collect();
                for tab in [false, true] {
                    let Ok(l) = self.dwrite.CreateTextLayout(&wide, &format, 1000.0, 400.0) else { continue };
                    if let (true, Some(t)) = (tab, &typo) {
                        let _ = l.SetTypography(t, DWRITE_TEXT_RANGE { startPosition: 0, length: 1 });
                    }
                    let mut m = DWRITE_TEXT_METRICS::default();
                    if l.GetMetrics(&mut m).is_ok() {
                        widest = widest.max(m.widthIncludingTrailingWhitespace);
                    }
                }
            }
        }
        CACHE.with(|c| c.borrow_mut().insert(key, widest));
        widest
    }

    /// One line of text with tabular figures, centered in a surface as
    /// wide as the wider of it and `room` (measured the same way; see
    /// topbar::status::steady_room), and at least `min_w` DIP: a clock whose
    /// digits change keeps its size. On `line`, see text_on. Returns the
    /// surface and the text's room in DIPs.
    #[allow(clippy::too_many_arguments)]
    pub fn text_steady(
        &self,
        text: &str,
        room: &str,
        family: &str,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        color: D2D1_COLOR_F,
        scale: f32,
        min_w: f32,
        line: Option<TextLine>,
    ) -> Result<(CompositionDrawingSurface, f32, f32)> {
        let a = self.text_art_steady(text, room, family, size, weight, color, scale, min_w, line)?;
        let surface = self.paint_art(&a)?;
        Ok((surface, a.w / scale, a.h / scale))
    }

    /// text_steady's art.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text_art_steady(
        &self,
        text: &str,
        room: &str,
        family: &str,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        color: D2D1_COLOR_F,
        scale: f32,
        min_w: f32,
        line: Option<TextLine>,
    ) -> Result<TextArt> {
        let (layout, tw, th) = self.text_layout_tabular(text, family, size * scale, weight)?;
        let (rw, rh) = if room == text { (tw, th) } else { self.text_layout_tabular(room, family, size * scale, weight).map(|(_, w, h)| (w, h))? };
        let (w, h) = (tw.max(rw).max((min_w * scale).ceil()).max(1.0), th.max(rh).max(1.0));
        let (h, y) = if line.is_some() { on_line(&layout, th, line) } else { (h, 0.0) };
        Ok(TextArt::new(layout, w, h, (((w - tw) / 2.0).round(), y), color))
    }

    /// A text layout `max_w` px wide, wrapped to at most `lines` lines,
    /// centered, ending in "…" when cut. Returns it with its height in px.
    pub fn text_layout_wrapped(
        &self,
        text: &str,
        family: &str,
        size_px: f32,
        weight: DWRITE_FONT_WEIGHT,
        max_w: f32,
        lines: u32,
    ) -> Result<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32)> {
        self.wrapped(text, family, size_px, weight, max_w, lines, true)
    }

    /// Like text_layout_wrapped, aligned to the reading start (left; right
    /// in a right-to-left language) instead of centered.
    pub fn text_layout_wrapped_leading(
        &self,
        text: &str,
        family: &str,
        size_px: f32,
        weight: DWRITE_FONT_WEIGHT,
        max_w: f32,
        lines: u32,
    ) -> Result<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32)> {
        self.wrapped(text, family, size_px, weight, max_w, lines, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn wrapped(
        &self,
        text: &str,
        family: &str,
        size_px: f32,
        weight: DWRITE_FONT_WEIGHT,
        max_w: f32,
        lines: u32,
        center: bool,
    ) -> Result<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32)> {
        use windows::Win32::Graphics::DirectWrite::{
            DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_WRAP,
        };
        unsafe {
            let fam = windows::core::HSTRING::from(&*crate::fonts::map(family));
            let format = ui_format(&self.dwrite, &fam, weight, size_px * crate::fonts::scale())?;
            format.SetTextAlignment(if center { DWRITE_TEXT_ALIGNMENT_CENTER } else { DWRITE_TEXT_ALIGNMENT_LEADING })?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            if crate::i18n::rtl() {
                format.SetReadingDirection(windows::Win32::Graphics::DirectWrite::DWRITE_READING_DIRECTION_RIGHT_TO_LEFT)?;
            }
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            format.SetTrimming(&trim, &sign)?;
            // One line's height, to cap the box at `lines` lines.
            let probe: Vec<u16> = "Ag".encode_utf16().collect();
            let one = self.dwrite.CreateTextLayout(&probe, &format, max_w, 1000.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            one.GetMetrics(&mut m)?;
            let max_h = (m.height * lines.max(1) as f32).ceil();
            let wide: Vec<u16> = keep_words(text).encode_utf16().collect();
            let layout = self.dwrite.CreateTextLayout(&wide, &format, max_w, max_h)?;
            layout.GetMetrics(&mut m)?;
            Ok((layout, m.height.min(max_h).ceil()))
        }
    }

    /// The D2D factory (path geometries for drawn shapes).
    pub fn d2d_factory(&self) -> &ID2D1Factory1 {
        &self.d2d_factory
    }

    /// The system has this font family installed (or it is the built-in
    /// Pretendard).
    #[allow(dead_code)]
    pub fn has_font(&self, family: &str) -> bool {
        if crate::fonts::collection_for(family).is_some() {
            return true;
        }
        unsafe {
            let mut coll = None;
            if self.dwrite.GetSystemFontCollection(&mut coll, false).is_err() {
                return false;
            }
            let Some(coll) = coll else { return false };
            let mut index = 0u32;
            let mut exists = BOOL(0);
            let fam = windows::core::HSTRING::from(family);
            coll.FindFamilyName(&fam, &mut index, &mut exists).is_ok() && exists.as_bool()
        }
    }

    /// One of NopBar's vector glyphs at `size` DIP (an em, glyphs::EM) in
    /// `color`, centered in a square surface padded so the snapped ink is
    /// never cut. Returns the surface and its size in DIPs (center it).
    pub fn glyph(&self, g: crate::glyphs::Glyph, size: f32, color: D2D1_COLOR_F, scale: f32) -> Result<(CompositionDrawingSurface, f32, f32)> {
        let side = (size * scale).round().max(1.0);
        let (fr, pad) = crate::glyphs::padded(g, side, side);
        let full = side + 2.0 * pad;
        let surface = self.draw(full as u32, full as u32, |dc, ox, oy| unsafe {
            draw_glyph_frame(dc, g, &crate::glyphs::Frame { x: fr.x + ox, y: fr.y + oy, ..fr }, color)
        })?;
        Ok((surface, full / scale, full / scale))
    }

    pub fn start_icon(&self, px: u32) -> Result<CompositionDrawingSurface> {
        self.start_icon_colored(px, (0.30, 0.72, 1.0, 1.0))
    }

    /// Start glyph in one color (the top bar uses the text color).
    pub fn start_icon_colored(&self, px: u32, rgba: (f32, f32, f32, f32)) -> Result<CompositionDrawingSurface> {
        self.draw(px, px, move |dc, ox, oy| unsafe { paint_start(dc, ox, oy, px as f32, rgba) })
    }
}

/// The Start glyph `s` px a side at (`ox`, `oy`) in one color (also the
/// top bar's design-audit pictures).
pub(crate) unsafe fn paint_start(dc: &ID2D1DeviceContext, ox: f32, oy: f32, s: f32, rgba: (f32, f32, f32, f32)) -> Result<()> {
    let tile = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: rgba.0, g: rgba.1, b: rgba.2, a: rgba.3 }, None)?;
    let m = s * 0.16;
    let gap = s * 0.05;
    let t = (s - m * 2.0 - gap) / 2.0;
    for (cx, cy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        let l = ox + m + cx * (t + gap);
        let tp = oy + m + cy * (t + gap);
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: l, top: tp, right: l + t, bottom: tp + t },
            radiusX: s * 0.04,
            radiusY: s * 0.04,
        };
        dc.FillRoundedRectangle(&rr, &tile);
    }
    Ok(())
}

/// The superellipse |x|^5 + |y|^5 = 1 filling the `side` square at
/// (ox, oy): the dock's squircle (icon masks and backplates).
pub(crate) fn squircle_geometry(factory: &ID2D1Factory1, ox: f32, oy: f32, side: f32) -> Result<windows::Win32::Graphics::Direct2D::ID2D1PathGeometry1> {
    use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED};
    unsafe {
        let geom = factory.CreatePathGeometry()?;
        let sink = geom.Open()?;
        let r = side / 2.0;
        let steps = 128usize;
        let pt = |i: usize| {
            let t = i as f32 / steps as f32 * std::f32::consts::TAU;
            let (sn, cs) = t.sin_cos();
            let x = cs.signum() * cs.abs().powf(2.0 / 5.0);
            let y = sn.signum() * sn.abs().powf(2.0 / 5.0);
            Vector2 { X: ox + r + x * r, Y: oy + r + y * r }
        };
        sink.BeginFigure(pt(0), D2D1_FIGURE_BEGIN_FILLED);
        let pts: Vec<Vector2> = (1..steps).map(pt).collect();
        sink.AddLines(&pts);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        Ok(geom)
    }
}

/// DirectWrite's locale name for a UI language tag (crate::i18n::current:
/// "ko", "zh-cn", "pt-br"): the region in capitals, and the usual region
/// for the CJK languages, whose Han characters differ by locale. Pure.
pub fn dwrite_locale(tag: &str) -> String {
    let tag = tag.trim();
    match tag.to_ascii_lowercase().as_str() {
        "" => "en-US".into(),
        "en" => "en-US".into(),
        "ko" => "ko-KR".into(),
        "ja" => "ja-JP".into(),
        "zh" | "zh-cn" | "zh-hans" => "zh-CN".into(),
        "zh-tw" | "zh-hant" => "zh-TW".into(),
        t => match t.split_once('-') {
            Some((lang, region)) => format!("{lang}-{}", region.to_ascii_uppercase()),
            None => t.to_string(),
        },
    }
}

/// The weight the UI draws with: the type ramp has 400 and 600 only, so
/// anything heavier (Bold, Black) is SemiBold. Pure. (Pretendard, the
/// default, has a real 600 for Korean too; Malgun Gothic has none: Korean
/// in it asked for SemiBold still comes out Bold, so keep 600 to titles.)
pub fn ui_weight(w: DWRITE_FONT_WEIGHT) -> DWRITE_FONT_WEIGHT {
    use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD;
    if w.0 > DWRITE_FONT_WEIGHT_SEMI_BOLD.0 {
        DWRITE_FONT_WEIGHT_SEMI_BOLD
    } else {
        w
    }
}

/// Hangul's blocks (jamo, compatibility jamo, extended jamo, syllables).
const HANGUL: [(u32, u32); 5] = [(0x1100, 0x11FF), (0x3130, 0x318F), (0xA960, 0xA97F), (0xAC00, 0xD7AF), (0xD7B0, 0xD7FF)];

thread_local! {
    /// Per factory: the system's font fallback with Hangul -> Malgun Gothic
    /// first (None: no Malgun Gothic, or DirectWrite too old).
    static HANGUL_FALLBACK: std::cell::RefCell<Vec<(usize, Option<windows::Win32::Graphics::DirectWrite::IDWriteFontFallback>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The font fallback text formats get: Korean (in a family without it) in
/// Malgun Gothic, Windows' Korean UI font, whatever the locale; the rest as
/// the system's fallback picks for the locale.
fn hangul_fallback(dw: &IDWriteFactory) -> Option<windows::Win32::Graphics::DirectWrite::IDWriteFontFallback> {
    use windows::Win32::Graphics::DirectWrite::{IDWriteFactory2, IDWriteFontCollection, DWRITE_UNICODE_RANGE};
    let key = dw.as_raw() as usize;
    if let Some(hit) = HANGUL_FALLBACK.with(|c| c.borrow().iter().find(|(k, _)| *k == key).map(|(_, f)| f.clone())) {
        return hit;
    }
    let built = (|| -> Result<windows::Win32::Graphics::DirectWrite::IDWriteFontFallback> {
        if !crate::fonts::installed("Malgun Gothic") {
            return Err(Error::from(E_FAIL));
        }
        unsafe {
            let f2: IDWriteFactory2 = dw.cast()?;
            let b = f2.CreateFontFallbackBuilder()?;
            let ranges = HANGUL.map(|(first, last)| DWRITE_UNICODE_RANGE { first, last });
            let malgun = w!("Malgun Gothic");
            b.AddMapping(&ranges, &[malgun.as_ptr()], None::<&IDWriteFontCollection>, windows::core::PCWSTR::null(), windows::core::PCWSTR::null(), 1.0)?;
            b.AddMappings(&f2.GetSystemFontFallback()?)?;
            b.CreateFontFallback()
        }
    })()
    .ok();
    HANGUL_FALLBACK.with(|c| c.borrow_mut().push((key, built.clone())));
    built
}

/// The dock's name label (Gfx::label): its widest text and least height (DIP).
pub const LABEL_MAX_W: f32 = 240.0;
pub const LABEL_MIN_H: f32 = 24.0;

/// A plain text surface's picture (text_on, text_fit_on, text_steady), px:
/// the text's box (`w` x `h`, the surface's size) and the layout's origin
/// in it.
pub(crate) struct TextArt {
    pub layout: windows::Win32::Graphics::DirectWrite::IDWriteTextLayout,
    pub w: f32,
    pub h: f32,
    pub at: (f32, f32),
    color: D2D1_COLOR_F,
}

impl TextArt {
    fn new(layout: windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, w: f32, h: f32, at: (f32, f32), color: D2D1_COLOR_F) -> TextArt {
        TextArt { layout, w, h, at, color }
    }

    /// The surface's size, px.
    pub fn size_px(&self) -> (u32, u32) {
        (self.w as u32, self.h as u32)
    }

    /// Draws it with the surface's top left at (`ox`, `oy`) px.
    pub unsafe fn paint(&self, dc: &ID2D1DeviceContext, ox: f32, oy: f32) -> Result<()> {
        let fg = dc.CreateSolidColorBrush(&self.color, None)?;
        let at = Vector2 { X: ox + self.at.0, Y: oy + self.at.1 };
        draw_text(dc, at, &self.layout, &fg, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        Ok(())
    }
}

/// The name label's text laid out (Gfx::label_art), in px: the text's size
/// and the label's box.
pub struct LabelArt {
    layout: windows::Win32::Graphics::DirectWrite::IDWriteTextLayout,
    tw: f32,
    th: f32,
    pub w: f32,
    pub h: f32,
    scale: f32,
}

/// Text `layout` at `at` (the target's units) in `brush`.
pub unsafe fn draw_text<P>(dc: &ID2D1DeviceContext, at: Vector2, layout: &windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, brush: P, opts: windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS)
where
    P: windows::core::Param<windows::Win32::Graphics::Direct2D::ID2D1Brush>,
{
    dc.DrawTextLayout(at, layout, brush, opts);
}

/// Glyph `g` (glyphs::draw: centered on `cx`, `cy`, `size` across, in the
/// target's units) in `color`: the one way a panel draws a glyph of its
/// own ink. glyphs::draw draws a mirrored transform's glyph unmirrored (in
/// device pixels); it is drawn under the transform mirrored back about the
/// glyph's center, so a mirrored panel's glyph lands where it was laid out.
pub unsafe fn draw_glyph(dc: &ID2D1DeviceContext, g: crate::glyphs::Glyph, cx: f32, cy: f32, size: f32, color: D2D1_COLOR_F) -> Result<()> {
    let mut t = windows_numerics::Matrix3x2::default();
    dc.GetTransform(&mut t);
    let upright = unmirror_about(t, cx, cy);
    if upright != t {
        dc.SetTransform(&upright);
    }
    let drawn = crate::glyphs::draw(dc, g, cx, cy, size, color);
    if upright != t {
        dc.SetTransform(&t);
    }
    drawn
}

/// Pure: transform `t` mirrored back about (`cx`, `cy`) where it mirrors
/// a plain scale (a negative M11 or M22, no rotation): that point lands
/// where it did. Any other transform as it is.
pub fn unmirror_about(t: windows_numerics::Matrix3x2, cx: f32, cy: f32) -> windows_numerics::Matrix3x2 {
    if t.M12 != 0.0 || t.M21 != 0.0 || (t.M11 >= 0.0 && t.M22 >= 0.0) {
        return t;
    }
    let mut u = t;
    if t.M11 < 0.0 {
        u.M11 = -t.M11;
        u.M31 = t.M31 + 2.0 * t.M11 * cx;
    }
    if t.M22 < 0.0 {
        u.M22 = -t.M22;
        u.M32 = t.M32 + 2.0 * t.M22 * cy;
    }
    u
}

/// Glyph `g` at frame `fr` (device pixels: glyphs::draw_device) in
/// `color`: draw_glyph for the glyphs placed in a cell's own square
/// (glyphs::fitted, padded: the top bar's cells, the windows' card
/// buttons).
pub unsafe fn draw_glyph_frame(dc: &ID2D1DeviceContext, g: crate::glyphs::Glyph, fr: &crate::glyphs::Frame, color: D2D1_COLOR_F) -> Result<()> {
    crate::glyphs::draw_device(dc, g, fr, color)
}

/// The halo `h` (DIP) as a compositor shadow of a visual's own content
/// (InheritFromVisualContent): the top bar's clear cells, whose content
/// is shown in their own ink through a mask and holds glyphs as well as
/// text (topbar::look::Halo). Its blur in the compositor's measure
/// (theme::composition_blur), its strength following the content's alpha.
pub fn halo_shadow(c: &windows::UI::Composition::Compositor, h: crate::theme::Halo) -> Result<windows::UI::Composition::DropShadow> {
    let d = c.CreateDropShadow()?;
    d.SetSourcePolicy(windows::UI::Composition::CompositionDropShadowSourcePolicy::InheritFromVisualContent)?;
    d.SetBlurRadius(crate::theme::composition_blur(h.blur))?;
    d.SetOffset(windows_numerics::Vector3 { X: 0.0, Y: h.dy, Z: 0.0 })?;
    d.SetColor(windows::UI::Color { A: h.color.a, R: h.color.r, G: h.color.g, B: h.color.b })?;
    Ok(d)
}

/// Draws the name label `a` at (`ox`, `oy`) px: its body when `bg` is not
/// clear (corners `radius` DIP: the theme's tip radius), its text in `fg`
/// in the middle.
pub unsafe fn paint_label(dc: &ID2D1DeviceContext, ox: f32, oy: f32, a: &LabelArt, bg: crate::theme::Rgba, fg: crate::theme::Rgba, radius: f32) -> Result<()> {
    let c = |v: crate::theme::Rgba| D2D1_COLOR_F { r: v.r as f32 / 255.0, g: v.g as f32 / 255.0, b: v.b as f32 / 255.0, a: v.a as f32 / 255.0 };
    let (w, h, tw, th) = (a.w, a.h, a.tw, a.th);
    if bg.a > 0 {
        let r = radius.max(0.0).min(h / a.scale / 2.0) * a.scale;
        let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: ox, top: oy, right: ox + w, bottom: oy + h }, radiusX: r, radiusY: r };
        dc.FillRoundedRectangle(&rr, &dc.CreateSolidColorBrush(&c(bg), None)?);
    }
    let fgb = dc.CreateSolidColorBrush(&c(fg), None)?;
    // Whole pixels: the text stays crisp at every scale.
    dc.DrawTextLayout(Vector2 { X: ox + ((w - tw) / 2.0).round(), Y: oy + ((h - th) / 2.0).round() }, &a.layout, &fgb, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
    Ok(())
}

/// Every text format shared drawing makes: the UI language's locale (so
/// fallback and Han glyphs follow it), Hangul in Malgun Gothic (in a
/// family without it), weights through ui_weight (never Bold). The
/// built-in Pretendard comes from its own collection (fonts::bundled) on
/// Segoe UI's line box (fonts::segoe_line_box): every line as tall as
/// Segoe's and the letters' optical middle where Segoe's is, so rows,
/// paragraphs and the text centered in them keep their places whichever
/// font draws them.
/// `t` as text that wraps is laid out (every wrapping layout: the
/// settings' paragraphs, tips, panels, Launchpad's names): Korean breaks
/// between words, at its spaces, as Korean is set, never inside one
/// ("옮겼어 / 요."). DirectWrite allows a break between any two Hangul
/// syllables whatever the locale or DWRITE_WORD_WRAPPING_WHOLE_WORD say
/// (gfx test), so a word joiner (U+2060, drawn as nothing) goes between
/// two Hangul letters; a word longer than the line still breaks
/// (DWRITE_WORD_WRAPPING_WRAP's emergency break). Chinese and Japanese
/// break between characters, as they are set: left as they are, as is
/// every other script. A key combination ("Win+숫자", "Ctrl+Alt+N")
/// stays on one line too: no break at a "+" with no space around it, in
/// any language. Only for text that is shown, never for a field whose
/// layout's positions are the text's (a caret, a selection). Pure: tested.
pub fn keep_words(t: &str) -> std::borrow::Cow<'_, str> {
    // Syllables and jamo (ㅋㅋ, old ones): Korean letters.
    let hangul = |c: char| matches!(c, '\u{AC00}'..='\u{D7A3}' | '\u{1100}'..='\u{11FF}' | '\u{3131}'..='\u{318E}' | '\u{A960}'..='\u{A97F}' | '\u{D7B0}'..='\u{D7FF}');
    let chars: Vec<char> = t.chars().collect();
    // A "+" inside a key combination: a letter or digit on both sides.
    let combo_plus = |i: usize| chars[i] == '+' && i > 0 && i + 1 < chars.len() && !chars[i - 1].is_whitespace() && !chars[i + 1].is_whitespace();
    let join = |i: usize| i > 0 && ((hangul(chars[i - 1]) && hangul(chars[i])) || combo_plus(i) || combo_plus(i - 1));
    if !(1..chars.len()).any(join) {
        return t.into();
    }
    let mut out = String::with_capacity(t.len() + t.len() / 2);
    for (i, c) in chars.iter().enumerate() {
        if join(i) {
            out.push('\u{2060}');
        }
        out.push(*c);
    }
    out.into()
}

pub(crate) fn ui_format(dw: &IDWriteFactory, family: &windows::core::HSTRING, weight: DWRITE_FONT_WEIGHT, size: f32) -> Result<windows::Win32::Graphics::DirectWrite::IDWriteTextFormat> {
    use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat1;
    use windows::Win32::Graphics::DirectWrite::DWRITE_LINE_SPACING_METHOD_UNIFORM;
    let locale = windows::core::HSTRING::from(dwrite_locale(crate::i18n::current()));
    let named = family.to_string();
    let collection = crate::fonts::collection_for(&named);
    // The built-in font by its family in its own collection.
    let family = &windows::core::HSTRING::from(crate::fonts::dwrite_name(&named));
    unsafe {
        let format = dw.CreateTextFormat(family, collection.as_ref(), ui_weight(weight), DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, &locale)?;
        if collection.is_some() {
            let (line, baseline) = crate::fonts::bundled_line_box();
            format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line * size, baseline * size)?;
        }
        if let (Some(fb), Ok(f1)) = (hangul_fallback(dw), format.cast::<IDWriteTextFormat1>()) {
            let _ = f1.SetFontFallback(&fb);
        }
        Ok(format)
    }
}

/// Pure: where soft_shadows draws, in device pixels, for a rounded rect
/// `r` (corners `radius`) in units with transform `m` at `dpi` (0 = 96):
/// the rect (mirroring undone), the corner radius and px per unit. None if
/// the transform rotates or skews (no shadow then; NopBar never does).
pub fn shadow_place(m: &windows_numerics::Matrix3x2, dpi: f32, r: D2D_RECT_F, radius: f32) -> Option<(D2D_RECT_F, f32, f32)> {
    let d = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
    if m.M12 != 0.0 || m.M21 != 0.0 || m.M11 == 0.0 || (m.M11.abs() - m.M22.abs()).abs() > 1e-4 {
        return None;
    }
    let (x0, x1) = ((m.M11 * r.left + m.M31) * d, (m.M11 * r.right + m.M31) * d);
    let (y0, y1) = ((m.M22 * r.top + m.M32) * d, (m.M22 * r.bottom + m.M32) * d);
    let k = m.M11.abs() * d;
    Some((D2D_RECT_F { left: x0.min(x1), top: y0.min(y1), right: x0.max(x1), bottom: y0.max(y1) }, radius.max(0.0) * k, k))
}

/// Soft drop shadows under a rounded rectangle `r` (corners `radius`) in
/// `dc`'s units: real Gaussian blurs (Direct2D's Shadow effect, sigma =
/// blur / 2 as CSS's and Fluent's blur), not stacked fills. `layers` are
/// (x, y, blur, color) in the same units, as Theme::elevation gives them.
/// Drawn in device pixels; the shape itself is not filled (draw it on top).
pub unsafe fn soft_shadows(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, layers: &[(f32, f32, f32, crate::theme::Rgba)]) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::Common::D2D1_COMPOSITE_MODE_SOURCE_OVER;
    use windows::Win32::Graphics::Direct2D::{CLSID_D2D1Shadow, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_VECTOR4, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION, D2D1_SHADOW_PROP_COLOR};
    let layers: Vec<_> = layers.iter().filter(|l| l.3.a > 0).collect();
    if layers.is_empty() || r.right <= r.left || r.bottom <= r.top {
        return Ok(());
    }
    let mut m = windows_numerics::Matrix3x2::default();
    dc.GetTransform(&mut m);
    let (mut dpx, mut dpy) = (0.0f32, 0.0f32);
    dc.GetDpi(&mut dpx, &mut dpy);
    let Some((dr, drad, k)) = shadow_place(&m, dpx, r, radius) else { return Ok(()) };
    let d = if dpx > 0.0 { dpx / 96.0 } else { 1.0 };
    // Device pixels: undo the DPI scale (and any mirroring: a shadow falls
    // straight down either way; x offsets keep their reading side).
    let mirrored = m.M11 < 0.0;
    dc.SetTransform(&windows_numerics::Matrix3x2 { M11: 1.0 / d, M12: 0.0, M21: 0.0, M22: 1.0 / d, M31: 0.0, M32: 0.0 });
    let drawn = (|| -> Result<()> {
        // The shape once, in a command list, blurred per layer.
        let list = dc.CreateCommandList()?;
        let old = dc.GetTarget()?;
        dc.SetTarget(&list);
        let white = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None);
        if let Ok(white) = &white {
            dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: dr, radiusX: drad, radiusY: drad }, white);
        }
        dc.SetTarget(&old);
        list.Close()?;
        white?;
        for &&(x, y, blur, c) in &layers {
            let fx = dc.CreateEffect(&CLSID_D2D1Shadow)?;
            fx.SetInput(0, &list, true);
            let sigma = (blur.max(0.0) * k / 2.0).min(250.0);
            fx.SetValue(D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32, D2D1_PROPERTY_TYPE_FLOAT, &sigma.to_le_bytes())?;
            let rgba = [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, c.a as f32 / 255.0];
            let bytes: Vec<u8> = rgba.iter().flat_map(|v| v.to_le_bytes()).collect();
            fx.SetValue(D2D1_SHADOW_PROP_COLOR.0 as u32, D2D1_PROPERTY_TYPE_VECTOR4, &bytes)?;
            let off = Vector2 { X: if mirrored { -x } else { x } * k, Y: y * k };
            dc.DrawImage(&fx.GetOutput()?, Some(&off), None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
        }
        Ok(())
    })();
    dc.SetTransform(&m);
    drawn
}

/// Gfx::text_layout_line with factory `dw`. Right to left it reads so like
/// text_layout (read_rtl: the "…" lands at the reading end), still at most
/// `max_w` wide and drawn from its left edge.
pub(crate) fn line_layout(
    dw: &IDWriteFactory,
    text: &str,
    family: &str,
    size_px: f32,
    weight: DWRITE_FONT_WEIGHT,
    max_w: f32,
) -> Result<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32, f32)> {
    use windows::Win32::Graphics::DirectWrite::{DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP};
    unsafe {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let fam = windows::core::HSTRING::from(&*crate::fonts::map(family));
        let format = ui_format(dw, &fam, weight, size_px * crate::fonts::scale())?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let sign = dw.CreateEllipsisTrimmingSign(&format)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        format.SetTrimming(&trim, &sign)?;
        let room = max_w.floor().max(1.0);
        let layout = dw.CreateTextLayout(&wide, &format, room, 400.0)?;
        let rtl = read_rtl(&layout);
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m)?;
        let w = m.widthIncludingTrailingWhitespace.min(room).ceil();
        if rtl {
            // Its own width (never past the room): it still draws from its
            // left edge.
            let _ = layout.SetMaxWidth(w.max(1.0));
        }
        Ok((layout, w, m.height.ceil()))
    }
}

/// The baseline of `layout`'s first line, px below its top (most of the
/// line's `height` if it has none).
pub(crate) fn first_baseline(layout: &windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, height: f32) -> f32 {
    use windows::Win32::Graphics::DirectWrite::DWRITE_LINE_METRICS;
    let mut lines = [DWRITE_LINE_METRICS::default(); 1];
    let mut count = 0u32;
    match unsafe { layout.GetLineMetrics(Some(&mut lines), &mut count) } {
        Ok(()) if count >= 1 => lines[0].baseline,
        _ => height * 0.78,
    }
}

/// A text surface's height and where its layout draws (px from its top):
/// as tall as the text, from the top; on `line`, the line's box with the
/// first line on its baseline.
fn on_line(layout: &windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, height: f32, line: Option<TextLine>) -> (f32, f32) {
    match line {
        Some(l) => (l.box_h.max(1.0), crate::topbar::baseline::origin_y(l.baseline, first_baseline(layout, height))),
        None => (height.ceil().max(1.0), 0.0),
    }
}

/// Right-to-left languages (ar, fa, he): `layout` reads right to left on
/// one line (punctuation, numbers and the "…" of cut text land where those
/// readers expect). True if it did; the caller then sets the layout's width
/// to the measured one, so it still draws from its left edge.
pub(crate) fn read_rtl(layout: &windows::Win32::Graphics::DirectWrite::IDWriteTextLayout) -> bool {
    use windows::Win32::Graphics::DirectWrite::{DWRITE_READING_DIRECTION_RIGHT_TO_LEFT, DWRITE_WORD_WRAPPING_NO_WRAP};
    if !crate::i18n::rtl() {
        return false;
    }
    unsafe { layout.SetReadingDirection(DWRITE_READING_DIRECTION_RIGHT_TO_LEFT).is_ok() && layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).is_ok() }
}

/// Harmony v2.1's shared drawing: text in a ramp style, the focus ring,
/// wells, knobs, the scroll edge, the progress ring, quiet states.
// (The screens adopt these batch by batch: not all are called yet.)
#[allow(dead_code)]
#[path = "gfx_parts.rs"]
pub(crate) mod parts;
#[allow(unused_imports)]
pub use parts::{caret, CARET_W, focus_color, focus_ring, focus_ring_rect, knob, quiet_rows, scroll_alpha, scroll_fade, spinner, spinner_phase, tracked_runs, well, Quiet, SPINNER_TURN_MS};

/// What the promo pictures share (tests only).
#[cfg(test)]
#[path = "promo_kit.rs"]
pub(crate) mod promo_kit;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn korean_is_joined_inside_words_only() {
        assert!(matches!(keep_words("Open Space"), std::borrow::Cow::Borrowed(_)));
        assert_eq!(keep_words("골라 주세요."), "골\u{2060}라 주\u{2060}세\u{2060}요.");
        // Only between two Korean letters: not after a space, a quote or Latin.
        assert_eq!(keep_words("“검색 열기”에 NopBar가"), "“검\u{2060}색 열\u{2060}기”에 NopBar가");
        assert_eq!(keep_words("ㅋㅋ"), "ㅋ\u{2060}ㅋ");
        // A key combination holds together, in any language; a lone "+" not.
        assert_eq!(keep_words("Win+숫자"), "Win\u{2060}+\u{2060}숫\u{2060}자");
        assert_eq!(keep_words("Press Ctrl+Alt+N"), "Press Ctrl\u{2060}+\u{2060}Alt\u{2060}+\u{2060}N");
        assert!(matches!(keep_words("1 + 1"), std::borrow::Cow::Borrowed(_)));
        // Chinese, Japanese and the rest: untouched (no copy either).
        for t in ["このキーはすでに使われています。", "此快捷键已被占用。", "Hello world", "مرحبا"] {
            assert!(matches!(keep_words(t), std::borrow::Cow::Borrowed(_)), "{t}");
        }
    }

    /// Where a wrapping layout of `text` at `w` DIP breaks: its lines (the
    /// joiners taken out) and the widest line's width.
    fn wrapped_lines(dw: &IDWriteFactory, text: &str, w: f32) -> (Vec<String>, f32) {
        use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_WRAP};
        unsafe {
            let fam = windows::core::HSTRING::from(&*crate::fonts::map(crate::fonts::THEME_DEFAULT));
            let f = ui_format(dw, &fam, DWRITE_FONT_WEIGHT_NORMAL, 13.0).unwrap();
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP).unwrap();
            let wide: Vec<u16> = text.encode_utf16().collect();
            let l = dw.CreateTextLayout(&wide, &f, w, 1000.0).unwrap();
            let mut n = 0u32;
            let _ = l.GetLineMetrics(None, &mut n);
            let mut lm = vec![DWRITE_LINE_METRICS::default(); n as usize];
            l.GetLineMetrics(Some(&mut lm), &mut n).unwrap();
            let mut at = 0usize;
            let lines = lm
                .iter()
                .map(|m| {
                    let s = String::from_utf16_lossy(&wide[at..at + m.length as usize]).replace('\u{2060}', "");
                    at += m.length as usize;
                    s
                })
                .collect();
            let mut tm = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut tm).unwrap();
            (lines, tm.width)
        }
    }

    /// DirectWrite itself breaks Korean inside words (whatever the locale
    /// or word wrapping mode: tried); keep_words makes every line end on a
    /// space, at any width; a word too long for the line still breaks, and
    /// Japanese / Chinese still wrap between characters.
    #[test]
    fn wrapped_korean_breaks_at_spaces_cjk_between_characters() {
        use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, DWRITE_FACTORY_TYPE_SHARED};
        let _l = crate::i18n::test_lock("ko");
        let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
        let ko = "NopBar가 이미 다른 키를 쓰고 있어요. 다른 키를 골라 주세요. 파일을 휴지통으로 옮겼어요.";
        let mut split_inside = false;
        for w in (90..=330).step_by(6).map(|w| w as f32) {
            let (plain, _) = wrapped_lines(&dw, ko, w);
            split_inside |= plain.iter().rev().skip(1).any(|l| !l.ends_with(' '));
            let (lines, _) = wrapped_lines(&dw, &keep_words(ko), w);
            for l in lines.iter().rev().skip(1) {
                assert!(l.ends_with(' '), "{w}: {lines:?} breaks inside a word");
            }
            assert_eq!(lines.concat(), ko);
        }
        assert!(split_inside, "DirectWrite alone would break Korean inside words (else this test proves nothing)");
        // One word wider than the line: broken, never past it.
        let long = "가나다라마바사아자차카타파하가나다라마바사";
        let (lines, width) = wrapped_lines(&dw, &keep_words(long), 120.0);
        assert!(lines.len() > 1 && width <= 120.5, "{lines:?} {width}");
        // Japanese and Chinese: between characters, as before.
        for t in ["このキーはすでに別の操作で使われています。別のキーを選んでください。", "此快捷键已被另一个操作使用。请选择其他按键。"] {
            let (lines, _) = wrapped_lines(&dw, &keep_words(t), 150.0);
            assert_eq!(lines, wrapped_lines(&dw, t, 150.0).0);
            assert!(lines.len() > 1 && lines.iter().rev().skip(1).any(|l| !l.ends_with(' ')), "{lines:?}");
        }
    }

    /// Every layout that wraps goes through keep_words: a file that makes
    /// a wrapping layout (outside its tests) uses it.
    #[test]
    fn every_wrapping_layout_keeps_korean_words_whole() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut bad = Vec::new();
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            let code = src.find("#[cfg(test)]").map_or(src.as_str(), |i| &src[..i]);
            // (Its callers - para, text_layout_wrapped - need nothing.)
            if code.contains("DWRITE_WORD_WRAPPING_WRAP") && !code.contains("keep_words(") {
                bad.push(f.display().to_string());
            }
        }
        assert!(bad.is_empty(), "wrapping without keep_words: {bad:?}");
    }

    /// `paint` on an `n` px transparent target of a WARP device context at
    /// `dpi`: its BGRA pixels.
    fn warp_pixels(n: u32, dpi: f32, paint: impl FnOnce(&ID2D1DeviceContext)) -> Vec<u8> {
        use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_TARGET, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_MAP_OPTIONS_READ};
        unsafe {
            let mut dev = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None).unwrap();
            let dxgi: IDXGIDevice = dev.unwrap().cast().unwrap();
            let f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
            let dc = f.CreateDevice(&dxgi).unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap();
            let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
            let size = D2D_SIZE_U { width: n, height: n };
            let props = |o| D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: dpi, dpiY: dpi, bitmapOptions: o, ..Default::default() };
            let target = dc.CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_TARGET)).unwrap();
            dc.SetTarget(&target);
            dc.SetDpi(dpi, dpi);
            dc.BeginDraw();
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            paint(&dc);
            dc.EndDraw(None, None).unwrap();
            let read = dc.CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW)).unwrap();
            read.CopyFromBitmap(None, &target, None).unwrap();
            let m = read.Map(D2D1_MAP_OPTIONS_READ).unwrap();
            let mut out = Vec::with_capacity((n * n * 4) as usize);
            for y in 0..n {
                out.extend_from_slice(std::slice::from_raw_parts(m.bits.add((y * m.pitch) as usize), (n * 4) as usize));
            }
            read.Unmap().unwrap();
            out
        }
    }

    #[test]
    fn shadows_are_real_blurs_where_the_shape_is() {
        use windows_numerics::Matrix3x2;
        let black = crate::theme::Rgba { r: 0, g: 0, b: 0, a: 255 };
        let n = 64u32;
        let alpha = |px: &[u8], x: u32, y: u32| px[((y * n + x) * 4 + 3) as usize];
        // A 32 x 24 card at (16, 16), its shadow 4 down, blur 8 (sigma 4).
        let shape = D2D_RECT_F { left: 16.0, top: 16.0, right: 48.0, bottom: 40.0 };
        let px = warp_pixels(n, 96.0, |dc| unsafe { soft_shadows(dc, shape, 4.0, &[(0.0, 4.0, 8.0, black)]).unwrap() });
        assert!(alpha(&px, 32, 28) > 250, "solid under the middle: {}", alpha(&px, 32, 28));
        assert_eq!((alpha(&px, 2, 2), alpha(&px, 32, 62)), (0, 0), "nothing far away");
        // Below the card: falls off smoothly (many levels, never back up),
        // about half at the offset edge (y 44).
        let col: Vec<u8> = (40..60).map(|y| alpha(&px, 32, y)).collect();
        assert!(col.windows(2).all(|w| w[1] <= w[0]), "{col:?}");
        let mut levels = col.clone();
        levels.dedup();
        assert!(levels.len() >= 12, "no stair steps: {col:?}");
        assert!((100..=160).contains(&alpha(&px, 32, 44)), "{}", alpha(&px, 32, 44));
        // Falls down: more below the card than above it.
        assert!(alpha(&px, 32, 44) > alpha(&px, 32, 12));
        // The same card at 200 % (scaled DIPs, mirrored too) lands on the
        // same device pixels.
        let half = D2D_RECT_F { left: 8.0, top: 8.0, right: 24.0, bottom: 20.0 };
        for m in [Matrix3x2 { M11: 2.0, M12: 0.0, M21: 0.0, M22: 2.0, M31: 0.0, M32: 0.0 }, Matrix3x2 { M11: -2.0, M12: 0.0, M21: 0.0, M22: 2.0, M31: 64.0, M32: 0.0 }] {
            let px2 = warp_pixels(n, 96.0, |dc| unsafe {
                dc.SetTransform(&m);
                soft_shadows(dc, half, 2.0, &[(0.0, 2.0, 4.0, black)]).unwrap();
            });
            for (x, y) in [(32, 28), (32, 44), (32, 50), (12, 28), (52, 28)] {
                assert!((alpha(&px, x, y) as i32 - alpha(&px2, x, y) as i32).abs() <= 3, "{m:?} at ({x}, {y}): {} vs {}", alpha(&px, x, y), alpha(&px2, x, y));
            }
        }
        // Nothing for no layers or an empty rect; a rotated transform skips.
        assert!(shadow_place(&Matrix3x2 { M11: 0.0, M12: 1.0, M21: -1.0, M22: 0.0, M31: 0.0, M32: 0.0 }, 96.0, shape, 4.0).is_none());
        let none = warp_pixels(n, 96.0, |dc| unsafe { soft_shadows(dc, shape, 4.0, &[]).unwrap() });
        assert!(none.chunks(4).all(|p| p[3] == 0));
        // At 144 DPI the DIPs are 1.5 px.
        let (r, rad, k) = shadow_place(&Matrix3x2::identity(), 144.0, shape, 4.0).unwrap();
        assert_eq!((r.left, r.top, r.right, r.bottom, rad, k), (24.0, 24.0, 72.0, 60.0, 6.0, 1.5));
    }

    #[test]
    fn locales_follow_the_ui_language() {
        assert_eq!(dwrite_locale("ko"), "ko-KR");
        assert_eq!(dwrite_locale("ja"), "ja-JP");
        assert_eq!(dwrite_locale("zh-cn"), "zh-CN");
        assert_eq!(dwrite_locale("zh-tw"), "zh-TW");
        assert_eq!(dwrite_locale("pt-br"), "pt-BR");
        assert_eq!(dwrite_locale("de"), "de");
        assert_eq!((dwrite_locale("en"), dwrite_locale("")), ("en-US".to_string(), "en-US".to_string()));
    }

    #[test]
    fn bold_is_never_asked_for() {
        use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_BLACK, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_EXTRA_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_WEIGHT_SEMI_LIGHT};
        for w in [DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_EXTRA_BOLD, DWRITE_FONT_WEIGHT_BLACK, DWRITE_FONT_WEIGHT(650)] {
            assert_eq!(ui_weight(w), DWRITE_FONT_WEIGHT_SEMI_BOLD, "{w:?}");
        }
        for w in [DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_WEIGHT_SEMI_LIGHT] {
            assert_eq!(ui_weight(w), w);
        }
        // Every format here goes through ui_format (one CreateTextFormat),
        // none with the empty locale.
        let src = include_str!("gfx.rs");
        let code = src.find("#[cfg(test)]").map_or(src, |i| &src[..i]);
        assert_eq!(code.matches("CreateTextFormat(").count(), 1);
        assert!(!code.contains(concat!("w!(", "\"\"", ")")), "a format with no locale");
    }

    #[test]
    fn formats_carry_the_locale_and_hangul_fallback() {
        use windows::Win32::Graphics::DirectWrite::{IDWriteTextFormat1, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_SEMI_BOLD};
        let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
        let fam = windows::core::HSTRING::from("Segoe UI");
        for (tag, want) in [("ko", "ko-KR"), ("en", "en-US"), ("ja", "ja-JP")] {
            let _l = crate::i18n::test_lock(tag);
            let f = ui_format(&dw, &fam, DWRITE_FONT_WEIGHT_BOLD, 13.0).unwrap();
            unsafe {
                let mut buf = vec![0u16; f.GetLocaleNameLength() as usize + 1];
                f.GetLocaleName(&mut buf).unwrap();
                let got = String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]);
                assert!(got.eq_ignore_ascii_case(want), "{tag}: {got}");
                assert_eq!(f.GetFontWeight(), DWRITE_FONT_WEIGHT_SEMI_BOLD, "Bold asked, 600 made");
                if crate::fonts::installed("Malgun Gothic") {
                    assert!(f.cast::<IDWriteTextFormat1>().unwrap().GetFontFallback().is_ok(), "Hangul -> Malgun Gothic");
                }
            }
        }
    }

    #[test]
    fn a_cut_line_keeps_the_rtl_reading_direction() {
        use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, DWRITE_READING_DIRECTION_RIGHT_TO_LEFT};
        let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
        // A long Arabic label with digits, brackets and Latin in it.
        let text = "مستند (2026) Report v3.1 — نسخة نهائية للمراجعة";
        let room = 80.0;
        for (tag, dir) in [("ar", DWRITE_READING_DIRECTION_RIGHT_TO_LEFT), ("he", DWRITE_READING_DIRECTION_RIGHT_TO_LEFT), ("en", DWRITE_READING_DIRECTION_LEFT_TO_RIGHT)] {
            let _l = crate::i18n::test_lock(tag);
            let (whole, full_w, _) = line_layout(&dw, text, "Segoe UI", 14.0, DWRITE_FONT_WEIGHT_NORMAL, 4000.0).unwrap();
            let (cut, w, _) = line_layout(&dw, text, "Segoe UI", 14.0, DWRITE_FONT_WEIGHT_NORMAL, room).unwrap();
            assert!(full_w > room, "{tag}: the test text must be cut");
            unsafe {
                assert_eq!(whole.GetReadingDirection(), dir, "{tag}: whole");
                assert_eq!(cut.GetReadingDirection(), dir, "{tag}: cut, as when whole");
                assert!(w <= room && cut.GetMaxWidth() <= room, "{tag}: still within the room ({w}, {})", cut.GetMaxWidth());
            }
        }
    }

    #[test]
    fn device_lost_codes() {
        for c in [0x887A_0005u32, 0x887A_0006, 0x887A_0007, 0x887A_0020, 0x8899_000C] {
            assert!(is_device_lost(HRESULT(c as i32)), "{c:#x}");
        }
        assert!(!is_device_lost(E_INVALIDARG));
        assert!(!is_device_lost(E_FAIL));
        assert!(!is_device_lost(HRESULT(0)));
    }

    #[test]
    fn bgra_sizes_are_checked() {
        assert_eq!(bgra_len(2, 3), Some(24));
        assert_eq!(bgra_len(0, 3), None);
        assert_eq!(bgra_len(3, 0), None);
        assert_eq!(bgra_len(MAX_SURFACE_SIDE, MAX_SURFACE_SIDE), Some(16_384 * 16_384 * 4));
        assert_eq!(bgra_len(MAX_SURFACE_SIDE + 1, 1), None);
        assert_eq!(bgra_len(1, u32::MAX), None);
    }

    #[test]
    fn recovery_backs_off_then_gives_up() {
        let mut p = RecoveryPlan::default();
        assert!(p.lost(0));
        let mut waits = Vec::new();
        while let Some(ms) = p.next_delay() {
            // Hardware only at first; the software device later.
            assert_eq!(p.allow_warp(), waits.len() as u32 >= HW_ONLY_ATTEMPTS);
            waits.push(ms);
            p.failed();
        }
        assert_eq!(waits, RETRY_MS.to_vec());
        assert!(waits.windows(2).all(|w| w[0] <= w[1]), "never shorter than before");
        // A success starts the next incident from the beginning.
        p.succeeded();
        assert_eq!(p.next_delay(), Some(0));
        assert!(!p.allow_warp());
    }

    #[test]
    fn too_many_losses_in_a_row_give_up() {
        let mut p = RecoveryPlan::default();
        for i in 0..LOSS_MAX as u64 {
            assert!(p.lost(i * 1_000));
        }
        assert!(!p.lost(LOSS_MAX as u64 * 1_000), "one more within the window is too many");
        // Spread out over time they are fine.
        let mut q = RecoveryPlan::default();
        for i in 0..(LOSS_MAX as u64 * 3) {
            assert!(q.lost(i * LOSS_WINDOW_MS));
        }
    }

    #[test]
    fn software_choice_wins_at_start_and_in_recovery() {
        // Start (WARP allowed) and the hardware-only recovery tries alike.
        assert_eq!(device_order(true, true), &[true, false]);
        assert_eq!(device_order(true, false), &[true, false]);
        assert_eq!(device_order(false, true), &[false, true]);
        assert_eq!(device_order(false, false), &[false]);
    }
}

/// Pure: the runs of ASCII digits in `text` as (start, length) in UTF-16
/// units (DirectWrite's text positions).
pub fn digit_runs(text: &str) -> Vec<(u32, u32)> {
    let mut runs = Vec::new();
    let (mut at, mut run): (u32, Option<(u32, u32)>) = (0, None);
    for c in text.chars() {
        if c.is_ascii_digit() {
            run = Some(match run { Some((s, n)) => (s, n + 1), None => (at, 1) });
        } else if let Some(r) = run.take() {
            runs.push(r);
        }
        at += c.len_utf16() as u32;
    }
    runs.extend(run);
    runs
}

#[cfg(test)]
mod digit_tests {
    #[test]
    fn digit_runs_in_utf16() {
        assert_eq!(super::digit_runs("오후 3:05:07"), vec![(3, 1), (5, 2), (8, 2)]);
        assert_eq!(super::digit_runs("12"), vec![(0, 2)]);
        assert_eq!(super::digit_runs("AM"), vec![]);
        // A surrogate pair counts two.
        assert_eq!(super::digit_runs("\u{1F600}9"), vec![(2, 1)]);
    }

    /// Pretendard's tabular figures are Latin only: after Hangul the clock
    /// still must not change width as its digits do.
    #[test]
    fn tabular_clock_keeps_its_width_after_hangul() {
        let _stage = crate::gfx::promo_kit::stage();
        let g = super::Gfx::new().unwrap();
        let normal = windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
        for fam in [crate::fonts::BUNDLED, "Segoe UI", "Malgun Gothic"] {
            for size in [13.0, 16.25, 19.5] {
                let w = |t: &str| g.text_layout_tabular(t, fam, size, normal).unwrap().1;
                let base = w("오후 3:05:07");
                for t in ["오후 3:11:11", "오후 3:00:00", "오후 3:59:58", "오후 1:47:14"] {
                    assert_eq!(w(t), base, "{fam} {size}: {t}");
                }
                assert_eq!(w("3:11:11 PM"), w("3:00:08 PM"), "{fam} {size}: latin");
            }
        }
    }
}
