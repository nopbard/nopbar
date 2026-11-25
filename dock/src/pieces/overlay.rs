//! The snip overlay (the "snip" shortcut, crate::shortcuts): a still
//! picture of the monitor under the pointer, taken before the overlay
//! exists (capture.rs), shown over that monitor, dimmed; a crosshair; a
//! drag selects a box (shown undimmed, with a thin edge outside it), the
//! button going up (or Enter) pins it, Esc, the right button or another
//! window taking over cancels. The box becomes an image piece at that spot
//! on the screen (crate::pieces::create_snip), cut from the same picture.
//!
//! Its own window, on the dock's thread, alive only while selecting: the
//! picture is drawn once; the dim and the edge are eight plain composition
//! rectangles moved as the pointer moves. Kept out of captures and screen
//! sharing. Nothing is written until a box is chosen; the picture is
//! dropped when the overlay goes.
//!
//! A chosen box: the overlay shrinks at once to the box, showing its part
//! of the still picture right where it was (nothing seems to stop), and
//! gives the keyboard back; a helper thread makes the PNG; when it comes
//! back the piece is saved in one place, synchronously
//! (crate::pieces::create_snip), and the held picture goes a moment
//! later, under the piece drawn by then.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

use windows::core::{w, Interface, Result, HSTRING};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionColorBrush, CompositionStretch, SpriteVisual};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_BRUSH_PROPERTIES, D2D1_EXTEND_MODE_CLAMP,
    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use super::capture::{Capturer, Screen};
use super::snip::{self, Ev, Frame, State};
use crate::gfx::Gfx;

const CLASS: windows::core::PCWSTR = w!("NopBar.SnipOverlay");
/// The shortcut's RegisterHotKey id (on the dock window).
pub const HOTKEY_ID: i32 = 0x534E;
/// Posted to the overlay: the PNG of the held box is ready (ENCODED).
const WM_ENCODED: u32 = WM_APP + 91;
/// The held picture goes this long after the piece is made (ms): the
/// piece is drawn over it by then.
const GONE_MS: u32 = 150;
const GONE_TIMER: usize = 1;

/// The helper thread's PNG for the overlay window it was made for.
static ENCODED: Mutex<Option<(isize, Option<Vec<u8>>)>> = Mutex::new(None);

struct Overlay {
    hwnd: HWND,
    frame: Frame,
    /// The monitor's top left on screen (px).
    origin: (i32, i32),
    line: i32,
    state: State,
    /// The window in front before (it gets the focus back).
    prev: HWND,
    /// Had the keyboard once (losing it after that cancels).
    active: bool,
    /// Holding a chosen box (screen px) while its PNG is made: nothing
    /// else is taken in.
    held: Option<RECT>,
    _target: DesktopWindowTarget,
    image: SpriteVisual,
    dims: Vec<SpriteVisual>,
    lines: Vec<SpriteVisual>,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Overlay) -> R) -> Option<R> {
    OVERLAY.with(|o| o.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Selecting now?
pub fn is_open() -> bool {
    with(|_| ()).is_some()
}

/// The shortcut: starts a snip on the monitor under the pointer, or
/// cancels the one under way.
pub fn on_hotkey(gfx: &Rc<Gfx>) {
    if is_open() {
        on(Ev::Lost);
    } else {
        start(gfx, &Screen);
    }
}

/// Pure: the monitor's picture and its top left, taken by `cap` (before
/// anything of ours shows).
pub fn snap(cap: &dyn Capturer, mon: RECT) -> Option<(Frame, (i32, i32))> {
    let f = cap.grab(mon)?;
    let want = ((mon.right - mon.left) as u32, (mon.bottom - mon.top) as u32);
    ((f.w, f.h) == want).then_some((f, (mon.left, mon.top)))
}

fn start(gfx: &Rc<Gfx>, cap: &dyn Capturer) {
    // The pieces are off, or their list does not read: nothing to pin to.
    if !super::can_make() {
        crate::log!("pieces: snip not started (pieces off or their list did not read)");
        return;
    }
    let Some(m) = crate::monitors::under_cursor() else { return };
    let prev = unsafe { GetForegroundWindow() };
    let Some((frame, origin)) = snap(cap, m.rect) else {
        crate::log!("pieces: the screen was not captured");
        return;
    };
    match unsafe { build(gfx, m.rect, frame, origin, snip::line_px(m.scale()), prev) } {
        Ok(o) => {
            let hwnd = o.hwnd;
            OVERLAY.with(|c| *c.borrow_mut() = Some(o));
            with(|o| o.layout());
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));
            }
        }
        Err(e) => crate::log!("pieces: no snip overlay ({e})"),
    }
}

unsafe fn build(gfx: &Rc<Gfx>, r: RECT, frame: Frame, origin: (i32, i32), line: i32, prev: HWND) -> Result<Overlay> {
    let hinst = GetModuleHandleW(None)?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: CLASS,
        hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassExW(&wc);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
        CLASS,
        &HSTRING::from(crate::i18n::t("pieces.snip")),
        WS_POPUP,
        r.left,
        r.top,
        w,
        h,
        None,
        None,
        Some(hinst.into()),
        None,
    )?;
    // Out of screen sharing and captures, always.
    crate::desklayer::privacy::register(hwnd, true);
    let built = (|| -> Result<Overlay> {
        let c = gfx.compositor.clone();
        let interop: ICompositorDesktopInterop = c.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
        let root = c.CreateContainerVisual()?;
        target.SetRoot(&root)?;
        // The still picture, drawn once at its own px.
        let image = c.CreateSpriteVisual()?;
        let surface = gfx.draw(frame.w, frame.h, |dc, ox, oy| paint_frame(dc, &frame, ox, oy))?;
        let brush = c.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::None)?;
        image.SetBrush(&brush)?;
        image.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;
        root.Children()?.InsertAtTop(&image)?;
        let solid = |a: f32, v: u8| -> Result<CompositionColorBrush> { c.CreateColorBrushWithColor(windows::UI::Color { A: (a * 255.0).round() as u8, R: v, G: v, B: v }) };
        let (dim, edge) = (solid(snip::DIM_ALPHA, 0)?, solid(snip::LINE_ALPHA, 255)?);
        let mut dims = Vec::new();
        let mut lines = Vec::new();
        for (list, b) in [(&mut dims, &dim), (&mut lines, &edge)] {
            for _ in 0..4 {
                let v = c.CreateSpriteVisual()?;
                v.SetBrush(b)?;
                root.Children()?.InsertAtTop(&v)?;
                list.push(v);
            }
        }
        Ok(Overlay { hwnd, frame, origin, line, state: State::Ready, prev, active: false, held: None, _target: target, image, dims, lines })
    })();
    if built.is_err() {
        crate::desklayer::privacy::forget(hwnd);
        let _ = DestroyWindow(hwnd);
    }
    built
}

/// Draws `f` at its own px at (`ox`, `oy`).
pub(super) unsafe fn paint_frame(dc: &ID2D1DeviceContext, f: &Frame, ox: f32, oy: f32) -> Result<()> {
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    let bmp = dc.CreateBitmap(D2D_SIZE_U { width: f.w, height: f.h }, Some(f.px.as_ptr() as _), f.w * 4, &props)?;
    let bprops = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR };
    let brush_props = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: ox, M32: oy } };
    let brush = dc.CreateBitmapBrush(&bmp, Some(&bprops), Some(&brush_props))?;
    dc.FillRectangle(&D2D_RECT_F { left: ox, top: oy, right: ox + f.w as f32, bottom: oy + f.h as f32 }, &brush);
    Ok(())
}

/// Pure: what the overlay draws over the picture for `s`: the dim
/// rectangles and the edge's (px).
pub fn shapes(s: State, w: i32, h: i32, line: i32) -> ([RECT; 4], [RECT; 4]) {
    let sel = snip::shown(s, w, h);
    let none = RECT::default();
    (snip::mask(sel, w, h), sel.map(|r| snip::border(r, line)).unwrap_or([none; 4]))
}

impl Overlay {
    fn size(&self) -> (i32, i32) {
        (self.frame.w as i32, self.frame.h as i32)
    }

    /// The dim and the edge follow the state.
    fn layout(&self) {
        let (w, h) = self.size();
        let (dims, lines) = shapes(self.state, w, h, self.line);
        for (v, r) in self.dims.iter().zip(dims.iter()).chain(self.lines.iter().zip(lines.iter())) {
            let _ = v.SetOffset(Vector3 { X: r.left as f32, Y: r.top as f32, Z: 0.0 });
            let _ = v.SetSize(Vector2 { X: (r.right - r.left).max(0) as f32, Y: (r.bottom - r.top).max(0) as f32 });
        }
    }
}

fn on(ev: Ev) {
    let Some(s) = with(|o| {
        if o.held.is_some() {
            return State::Ready;
        }
        let (w, h) = o.size();
        let s = snip::next(o.state, ev, w, h);
        if s != o.state {
            o.state = s;
            o.layout();
        }
        s
    }) else {
        return;
    };
    match s {
        State::Done(r) => keep(r),
        State::Cancelled => close(Some(ev)),
        _ => {}
    }
}

fn give_back(prev: HWND) {
    unsafe {
        if !prev.is_invalid() && IsWindow(Some(prev)).as_bool() {
            let _ = SetForegroundWindow(prev);
        }
    }
}

/// The overlay goes (its picture with it). `cancel`: cancelled by that;
/// the keyboard goes back to the window that had it only when the overlay
/// still has it and the user did not go elsewhere (snip::gives_back).
fn close(cancel: Option<Ev>) {
    let Some(o) = OVERLAY.with(|c| c.borrow_mut().take()) else { return };
    let (hwnd, prev) = (o.hwnd, o.prev);
    drop(o);
    let front = unsafe { GetForegroundWindow() } == hwnd;
    unsafe {
        let _ = ReleaseCapture();
        crate::desklayer::privacy::forget(hwnd);
        let _ = DestroyWindow(hwnd);
    }
    if cancel.is_some_and(|ev| snip::gives_back(ev, front)) {
        give_back(prev);
    }
}

/// Makes `cut`'s PNG on a helper thread; `done` gets it there (None: it
/// could not be made). False when no thread could be started.
pub fn spawn_encode(cut: Frame, done: impl FnOnce(Option<Vec<u8>>) + Send + 'static) -> bool {
    std::thread::Builder::new()
        .name("snip-png".into())
        .spawn(move || {
            let _c = crate::desklayer::load::charge(crate::desklayer::load::Kind::Pieces);
            let com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) }.is_ok();
            let png = snip::png_of(&cut);
            drop(cut);
            if com {
                unsafe { windows::Win32::System::Com::CoUninitialize() };
            }
            done(png);
        })
        .is_ok()
}

/// A box was chosen: held where it was (the overlay shrunk to it, the
/// keyboard given back) while a helper thread makes its PNG (on_encoded).
fn keep(sel: RECT) {
    let Some((hwnd, cut, prev)) = with(|o| {
        let c = snip::cut(&o.frame, sel, o.origin)?;
        let (_, (dx, dy)) = snip::hold(sel, o.origin);
        o.held = Some(c.rect);
        // Only the box's part of the still picture: no dim, no edge.
        let _ = o.image.SetOffset(Vector3 { X: dx, Y: dy, Z: 0.0 });
        for v in o.dims.iter().chain(o.lines.iter()) {
            let _ = v.SetSize(Vector2 { X: 0.0, Y: 0.0 });
        }
        // The monitor's pixels are no longer needed here (the drawing
        // device holds what shows).
        o.frame = Frame { w: 0, h: 0, px: Vec::new() };
        Some((o.hwnd, c, o.prev))
    })
    .flatten() else {
        crate::log!("pieces: the snip was not pinned");
        close(Some(Ev::Lost));
        return;
    };
    let r = cut.rect;
    unsafe {
        let _ = ReleaseCapture();
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE);
    }
    give_back(prev);
    let target = hwnd.0 as isize;
    let started = spawn_encode(cut.frame, move |png| {
        if let Ok(mut e) = ENCODED.lock() {
            *e = Some((target, png));
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_ENCODED, WPARAM(0), LPARAM(0));
        }
    });
    if !started {
        crate::log!("pieces: the snip was not pinned (no helper thread)");
        close(None);
    }
}

/// The held box's PNG came back: the piece is saved and shown (one place,
/// synchronous); the held picture goes a moment later.
fn on_encoded(hwnd: HWND) {
    let png = ENCODED.lock().ok().and_then(|mut e| e.take()).filter(|(h, _)| *h == hwnd.0 as isize).and_then(|(_, p)| p);
    let Some((rect, prev)) = with(|o| o.held.map(|r| (r, o.prev))).flatten() else { return };
    let made = png.is_some_and(|png| super::create_snip(&png, rect, prev));
    if !made {
        crate::log!("pieces: the snip was not pinned");
        close(None);
        return;
    }
    unsafe {
        if SetTimer(Some(hwnd), GONE_TIMER, GONE_MS, None) == 0 {
            close(None);
        }
    }
}

/// NopBar is quitting: a snip under way goes.
pub fn shutdown() {
    close(None);
}

fn lp_point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
            let (x, y) = lp_point(lp);
            on(Ev::Down(x, y));
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = lp_point(lp);
            on(Ev::Move(x, y));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // Decided first: letting the capture go after it is no longer
            // a drag that WM_CAPTURECHANGED would cancel.
            let (x, y) = lp_point(lp);
            on(Ev::Up(x, y));
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            on(Ev::Right);
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u16;
            if vk == VK_ESCAPE.0 {
                on(Ev::Esc);
            } else if vk == VK_RETURN.0 {
                on(Ev::Enter);
            } else if msg == WM_SYSKEYDOWN {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
                // Another window took the keyboard after we had it (not
                // the one given it back while a box is held).
                if with(|o| o.active && o.held.is_none()).unwrap_or(false) {
                    on(Ev::Away);
                }
            } else {
                with(|o| o.active = true);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CAPTURECHANGED => {
            // The capture taken away mid-drag (not by our own button-up).
            if HWND(lp.0 as *mut _) != hwnd && with(|o| matches!(o.state, State::Dragging { .. })).unwrap_or(false) {
                on(Ev::Lost);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            on(Ev::Lost);
            LRESULT(0)
        }
        WM_ENCODED => {
            on_encoded(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == GONE_TIMER => {
            let _ = KillTimer(Some(hwnd), GONE_TIMER);
            close(None);
            LRESULT(0)
        }
        // While a box is held: clicks on it do nothing.
        WM_MOUSEACTIVATE if with(|o| o.held.is_some()).unwrap_or(false) => LRESULT(MA_NOACTIVATE as isize),
        // Over its own monitor at its own px: nothing to rescale.
        WM_DPICHANGED => LRESULT(0),
        WM_CLOSE => {
            on(Ev::Lost);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The shortcut ("snip" in crate::shortcuts), registered on the dock
/// window; "none" registers nothing.
pub fn apply_hotkey(owner: HWND, spec: &str) {
    use crate::hotkeys::{record, Status, SNIP};
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
    static CURRENT: std::sync::Mutex<Option<(isize, String, bool)>> = std::sync::Mutex::new(None);
    let Ok(mut cur) = CURRENT.lock() else { return };
    let me = owner.0 as isize;
    if cur.as_ref().is_some_and(|(o, s, ok)| *o == me && s == spec && *ok) {
        return;
    }
    if let Some((old, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), HOTKEY_ID);
        }
    }
    let ok = match crate::search::hotkey::parse(spec) {
        Ok(None) => {
            record(SNIP, spec, Status::Off);
            false
        }
        Ok(Some((mods, key))) => match unsafe { RegisterHotKey(Some(owner), HOTKEY_ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, key) } {
            Ok(()) => {
                crate::log!("pieces: snip hotkey {spec}");
                record(SNIP, spec, Status::Ok);
                true
            }
            Err(e) => {
                crate::log!("pieces: snip hotkey {spec} not registered ({e})");
                record(SNIP, spec, Status::from_error(&e));
                false
            }
        },
        Err(e) => {
            crate::log!("pieces: bad snip hotkey {e}");
            record(SNIP, spec, Status::Invalid);
            false
        }
    };
    *cur = Some((me, spec.to_string(), ok));
}

#[cfg(test)]
mod render;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pieces::capture::fake::Painted;

    fn rc(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: r, bottom: b }
    }

    #[test]
    fn the_monitor_is_captured_once_where_it_is() {
        let cap = Painted { asked: RefCell::new(Vec::new()) };
        // A monitor left of and above the primary.
        let mon = rc(-1920, -200, 0, 880);
        let (f, origin) = snap(&cap, mon).expect("captured");
        assert_eq!(*cap.asked.borrow(), vec![mon], "the whole monitor, once, in screen px");
        assert_eq!((f.w, f.h, origin), (1920, 1080, (-1920, -200)));
        // A capturer that gives the wrong size: no snip.
        struct Short;
        impl Capturer for Short {
            fn grab(&self, _: RECT) -> Option<Frame> {
                Some(Frame { w: 1, h: 1, px: vec![0; 4] })
            }
        }
        assert!(snap(&Short, mon).is_none());
    }

    /// From an injected capture to a saved piece: the cut is the box of the
    /// captured picture, placed at that box's screen px.
    #[test]
    fn a_chosen_box_becomes_a_saved_piece_at_its_spot() {
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
        let cap = Painted { asked: RefCell::new(Vec::new()) };
        let mon_rect = rc(-1920, -200, 0, 880);
        let (frame, origin) = snap(&cap, mon_rect).unwrap();
        // Dragged from (300, 150) up-left to (100, 50).
        let (w, h) = (frame.w as i32, frame.h as i32);
        let s = snip::next(snip::next(snip::next(State::Ready, Ev::Down(300, 150), w, h), Ev::Move(200, 90), w, h), Ev::Up(100, 50), w, h);
        let State::Done(sel) = s else { panic!("{s:?}") };
        let c = snip::cut(&frame, sel, origin).expect("cut");
        assert_eq!(c.rect, rc(-1820, -150, -1620, -50));
        assert_eq!((c.frame.w, c.frame.h), (200, 100));
        // The cut's first pixel is the capture's (100, 50).
        assert_eq!(&c.frame.px[..4], &[100, 50, 99, 255]);
        // Its PNG made on a helper thread, handed back once.
        let ui = std::thread::current().id();
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(spawn_encode(c.frame.clone(), move |png| {
            let _ = tx.send((std::thread::current().id(), png));
        }));
        let (on, png) = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("encoded");
        assert_ne!(on, ui, "not on the caller's thread");
        let png = png.expect("a PNG");
        assert_eq!(crate::pieces::snip::png_size(&png[..24]), Some((200, 100)));
        let m = (png, c.rect);
        // Saved like any piece: its picture, then its line at that spot.
        let d = std::env::temp_dir().join(format!("nopbar-snip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        crate::desklayer::disk::set_test_dir(Some(d.clone()));
        let mon = crate::monitors::Monitor { handle: 1, rect: mon_rect, work: rc(-1920, -200, 0, 840), primary: false, dpi: 144 };
        let p = crate::desklayer::place::from_px(m.1, &[mon]).unwrap();
        let mut st = crate::pieces::store::open();
        assert!(crate::pieces::store::save_new(&mut st, "p1-1", &m.0, p, true));
        assert_eq!(std::fs::read(crate::pieces::store::picture("p1-1")).unwrap(), m.0);
        assert!(crate::pieces::store::open().data.pieces[0].screen, "listed as cut from the screen");
        assert_eq!(super::super::px_of(&p, true, &[mon]), Some(m.1), "comes back at the same px");
        crate::desklayer::disk::set_test_dir(None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_dim_and_edge_follow_the_drag() {
        let (d, l) = shapes(State::Ready, 100, 80, 1);
        assert_eq!(d[0], rc(0, 0, 100, 80));
        assert!(l.iter().all(|r| r.right == r.left), "no edge before a drag");
        let (d, l) = shapes(State::Dragging { from: (60, 50), to: (10, 20) }, 100, 80, 2);
        assert_eq!(d[0], rc(0, 0, 100, 20));
        assert_eq!(l[0], rc(8, 18, 62, 20));
        assert_eq!(shapes(State::Cancelled, 100, 80, 1).0[0], rc(0, 0, 100, 80));
    }

    #[test]
    fn the_shortcut_is_in_the_registry() {
        let s = crate::shortcuts::find(crate::shortcuts::SNIP).expect("snip shortcut");
        assert_eq!((s.kind, s.scope, s.how), (crate::shortcuts::Kind::Chord, crate::shortcuts::Scope::Global, crate::shortcuts::How::Hotkey));
        assert!(s.apply.is_some());
        assert_eq!(s.default, "win+alt+s");
        let c = crate::shortcuts::parse(s.default).unwrap().expect("on by default");
        // Not another shortcut's default.
        for o in crate::shortcuts::ALL.iter().filter(|o| o.id != s.id) {
            assert_ne!(crate::shortcuts::parse(o.default).ok().flatten(), Some(c), "{} has it", o.id);
        }
        assert_eq!(crate::shortcuts::row_key(s.id), crate::hotkeys::SNIP);
        assert!(crate::shortcuts::duplicates(&crate::config::Config::default()).is_empty());
    }

    /// Registered on the caller's window and given back on "none" (a key
    /// no one uses, on message-only windows).
    #[test]
    fn the_shortcut_registers_and_lets_go() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
        let mk = || unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), None, WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None) }.unwrap();
        let (dock, other) = (mk(), mk());
        let spec = "ctrl+alt+shift+f23";
        apply_hotkey(dock, spec);
        let st = crate::hotkeys::parse(&crate::hotkeys::snapshot());
        let mine = st.iter().find(|(k, s, _)| k == crate::hotkeys::SNIP && s == spec).cloned();
        assert!(mine.is_some(), "{st:?}");
        apply_hotkey(dock, "none");
        let st = crate::hotkeys::parse(&crate::hotkeys::snapshot());
        assert!(st.iter().any(|(k, s, x)| k == crate::hotkeys::SNIP && s == "none" && *x == crate::hotkeys::Status::Off));
        if mine.is_some_and(|m| m.2 == crate::hotkeys::Status::Ok) {
            let c = crate::shortcuts::parse(spec).unwrap().unwrap();
            assert!(unsafe { RegisterHotKey(Some(other), 0x77, HOT_KEY_MODIFIERS(c.mods) | MOD_NOREPEAT, c.vk) }.is_ok(), "the key is free again");
            unsafe {
                let _ = UnregisterHotKey(Some(other), 0x77);
            }
        }
        unsafe {
            let _ = DestroyWindow(dock);
            let _ = DestroyWindow(other);
        }
    }

    #[test]
    fn the_strings_are_there() {
        for key in ["pieces.snip", "shortcuts.snip", "shortcuts.snip.hint"] {
            assert!(crate::i18n::get(key).is_some(), "{key}");
        }
    }
}
