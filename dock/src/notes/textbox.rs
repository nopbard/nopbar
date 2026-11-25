//! A windowless RichEdit (msftedit.dll's ITextServices) typed in right on
//! a note's card: no window of its own. The card's window (the host)
//! hands it the keyboard, the IME and the mouse (`send_raw`), and the
//! card's Composition surface draws it over the card's own glass, like the
//! rest of the card. The caret is ours: RichEdit says where it is
//! (TxCreateCaret / TxSetCaretPos / TxShowCaret) and the card draws it
//! (`caret`).
//!
//! Drawing: TxDrawD2D cannot be used for the card's font. NopBar's
//! built-in Pretendard is a GDI font of this process only, which
//! DirectWrite (and so RichEdit's Direct2D mode) never sees: Hangul came
//! out as empty boxes. So RichEdit lays out and draws with GDI, as the
//! RichEdit window did (the same metrics, the same title nudge), twice
//! offscreen (over black and over white, `unmix`), and the card draws the
//! result as a premultiplied bitmap: the glass, static or live, shows
//! around every letter. `d2d` (TxDrawD2D) is kept for a font DirectWrite
//! can see.
//!
//! Everything here is in the card window's client px; RichEdit turns its
//! twips into px with the screen DC's LOGPIXELSY (as a RichEdit window
//! does), so the formats the note sets are the same as before.
//!
//! The host object (ITextHost2) is a plain vtable over `Shared`: the
//! `windows` crate's RichEdit interfaces carry no IIDs, so QueryInterface
//! is answered here with msftedit's own.

#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use windows::core::{s, w, IUnknown, IUnknown_Vtbl, Interface, BOOL, GUID, HRESULT};
use windows::Win32::Foundation::{COLORREF, E_NOINTERFACE, E_NOTIMPL, E_POINTER, HWND, LPARAM, LRESULT, POINT, RECT, RECTL, SIZE, S_FALSE, S_OK, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1RenderTarget;
use windows::Win32::Graphics::Gdi::{
    GetDC, GetSysColor, InvalidateRect, ReleaseDC, ScreenToClient, ClientToScreen, HBITMAP, HDC, HPALETTE, HRGN, SYS_COLOR_INDEX, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW,
    COLOR_WINDOWTEXT, COLOR_GRAYTEXT,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::RichEdit::{
    CHARFORMATW, ITextHost2_Vtbl, ITextHost_Vtbl, ITextServices2_Vtbl, PARAFORMAT, TXTBACKSTYLE, TXTBACK_TRANSPARENT, TXTBIT_CLIENTRECTCHANGE, TXTBIT_D2DDWRITE, TXTBIT_MULTILINE,
    TXTBIT_RICHTEXT, TXTBIT_WORDWRAP, TXTVIEW_ACTIVE,
};
use windows::Win32::UI::Input::Ime::{ImmGetContext, ImmReleaseContext, HIMC};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateCaret, DestroyCaret, KillTimer, SetCaretPos, SetCursor, SetTimer, HCURSOR, SCROLLBAR_CONSTANTS, SCROLL_WINDOW_FLAGS,
};

use super::richedit as re;

/// RichEdit's timers on the card window: their ids from here up (the
/// card's own timers stay below).
pub const TIMER_BASE: usize = 0x5245_0000;

/// msftedit's interface ids (riched20's headers differ; these are the ones
/// msftedit.dll exports).
const IID_ITEXTSERVICES2: GUID = GUID::from_u128(0x8d33f741_cf58_11ce_a89d_00aa006cadc5);
const IID_ITEXTHOST: GUID = GUID::from_u128(0x13e670f4_1a5a_11cf_abeb_00aa00b65ea1);
const IID_ITEXTHOST2: GUID = GUID::from_u128(0x13e670f5_1a5a_11cf_abeb_00aa00b65ea1);

/// An IID msftedit exports as data (`name`), else `fallback`.
fn exported_iid(name: windows::core::PCSTR, fallback: GUID) -> GUID {
    unsafe {
        let Ok(dll) = LoadLibraryW(w!("msftedit.dll")) else { return fallback };
        match GetProcAddress(dll, name) {
            Some(p) => *(p as *const GUID),
            None => fallback,
        }
    }
}

/// The caret as RichEdit last placed it (client px).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caret {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub shown: bool,
}

/// What the host knows: shared by the host object (RichEdit's side) and
/// the TextBox (the note's side).
struct Shared {
    hwnd: Cell<HWND>,
    /// Where the text lies (client px).
    rect: Cell<RECT>,
    caret: Cell<Caret>,
    /// The card window has the keyboard (the caret shows only then).
    focus: Cell<bool>,
    /// RichEdit holds the mouse (a selection being dragged).
    capture: Cell<bool>,
    /// Drawn with Direct2D (TXTBIT_D2DDWRITE).
    d2d: bool,
    /// Being drawn: invalidations from inside the draw are dropped.
    drawing: Cell<bool>,
    /// New text since the box was last drawn (or the caret moved).
    dirty: Cell<bool>,
    ink: Cell<COLORREF>,
    selection: Cell<COLORREF>,
    /// The default formats RichEdit asks for (CHARFORMAT2W, PARAFORMAT2).
    cf: RefCell<Box<re::CharFormat2>>,
    pf: RefCell<Box<re::ParaFormat2>>,
    /// Told of RichEdit's notifications (EN_CHANGE, ...): (card, code).
    notify: Cell<Option<fn(HWND, u32)>>,
    /// Why drawing failed (the card falls back).
    failed: RefCell<Option<String>>,
    /// The text's premultiplied pixels as last drawn (w, h, BGRA).
    pixels: RefCell<Option<(i32, i32, Vec<u8>)>>,
}

impl Shared {
    fn invalidate(&self) {
        self.dirty.set(true);
        if self.drawing.get() {
            return;
        }
        let h = self.hwnd.get();
        if !h.is_invalid() {
            unsafe {
                let _ = InvalidateRect(Some(h), None, false);
            }
        }
    }
}

#[repr(C)]
struct HostObj {
    vtbl: *const ITextHost2_Vtbl,
    refs: Cell<u32>,
    sh: Rc<Shared>,
}

unsafe fn host<'a>(this: *mut c_void) -> &'a HostObj {
    &*(this as *const HostObj)
}

unsafe extern "system" fn qi(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    let iid = *iid;
    if iid == IUnknown::IID || iid == IID_ITEXTHOST || iid == IID_ITEXTHOST2 {
        add_ref(this);
        *out = this;
        S_OK
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let h = host(this);
    h.refs.set(h.refs.get() + 1);
    h.refs.get()
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let n = {
        let h = host(this);
        h.refs.set(h.refs.get().saturating_sub(1));
        h.refs.get()
    };
    if n == 0 {
        drop(Box::from_raw(this as *mut HostObj));
    }
    n
}

unsafe extern "system" fn get_dc(this: *mut c_void) -> HDC {
    let h = host(this).sh.hwnd.get();
    GetDC((!h.is_invalid()).then_some(h))
}
unsafe extern "system" fn release_dc(this: *mut c_void, dc: HDC) -> i32 {
    let h = host(this).sh.hwnd.get();
    ReleaseDC((!h.is_invalid()).then_some(h), dc)
}
unsafe extern "system" fn show_scroll_bar(_: *mut c_void, _: i32, _: BOOL) -> BOOL {
    BOOL(1)
}
unsafe extern "system" fn enable_scroll_bar(_: *mut c_void, _: SCROLLBAR_CONSTANTS, _: i32) -> BOOL {
    BOOL(1)
}
unsafe extern "system" fn set_scroll_range(_: *mut c_void, _: i32, _: i32, _: i32, _: BOOL) -> BOOL {
    BOOL(1)
}
unsafe extern "system" fn set_scroll_pos(this: *mut c_void, _: i32, _: i32, _: BOOL) -> BOOL {
    host(this).sh.invalidate();
    BOOL(1)
}
unsafe extern "system" fn invalidate_rect(this: *mut c_void, _: *mut RECT, _: BOOL) {
    host(this).sh.invalidate();
}
unsafe extern "system" fn view_change(this: *mut c_void, _: BOOL) {
    host(this).sh.invalidate();
}
unsafe extern "system" fn create_caret(this: *mut c_void, _: HBITMAP, w: i32, h: i32) -> BOOL {
    let sh = &host(this).sh;
    sh.caret.set(Caret { w: w.max(1), h, ..sh.caret.get() });
    // The system's caret too, never seen (the window has no redirection
    // bitmap): magnifiers and the IME follow it.
    let hw = sh.hwnd.get();
    if !hw.is_invalid() && sh.focus.get() {
        let _ = CreateCaret(hw, None, w.max(1), h);
    }
    BOOL(1)
}
unsafe extern "system" fn show_caret(this: *mut c_void, show: BOOL) -> BOOL {
    let sh = &host(this).sh;
    sh.caret.set(Caret { shown: show.as_bool(), ..sh.caret.get() });
    sh.invalidate();
    BOOL(1)
}
unsafe extern "system" fn set_caret_pos(this: *mut c_void, x: i32, y: i32) -> BOOL {
    let sh = &host(this).sh;
    sh.caret.set(Caret { x, y, ..sh.caret.get() });
    if sh.focus.get() {
        let _ = SetCaretPos(x, y);
    }
    sh.invalidate();
    BOOL(1)
}
unsafe extern "system" fn set_timer(this: *mut c_void, id: u32, ms: u32) -> BOOL {
    let h = host(this).sh.hwnd.get();
    BOOL((!h.is_invalid() && SetTimer(Some(h), TIMER_BASE + id as usize, ms, None) != 0) as i32)
}
unsafe extern "system" fn kill_timer(this: *mut c_void, id: u32) {
    let h = host(this).sh.hwnd.get();
    if !h.is_invalid() {
        let _ = KillTimer(Some(h), TIMER_BASE + id as usize);
    }
}
unsafe extern "system" fn scroll_window_ex(this: *mut c_void, _: i32, _: i32, _: *mut RECT, _: *mut RECT, _: HRGN, _: *mut RECT, _: SCROLL_WINDOW_FLAGS) {
    host(this).sh.invalidate();
}
unsafe extern "system" fn set_capture(this: *mut c_void, on: BOOL) {
    let sh = &host(this).sh;
    sh.capture.set(on.as_bool());
    let h = sh.hwnd.get();
    if h.is_invalid() {
        return;
    }
    if on.as_bool() {
        SetCapture(h);
    } else {
        let _ = ReleaseCapture();
    }
}
unsafe extern "system" fn set_focus(this: *mut c_void) {
    let h = host(this).sh.hwnd.get();
    if !h.is_invalid() {
        let _ = SetFocus(Some(h));
    }
}
unsafe extern "system" fn set_cursor(_: *mut c_void, cur: HCURSOR, _: BOOL) {
    SetCursor(Some(cur));
}
unsafe extern "system" fn screen_to_client(this: *mut c_void, p: *mut POINT) -> BOOL {
    let h = host(this).sh.hwnd.get();
    BOOL((!h.is_invalid() && !p.is_null() && ScreenToClient(h, &mut *p).as_bool()) as i32)
}
unsafe extern "system" fn client_to_screen(this: *mut c_void, p: *mut POINT) -> BOOL {
    let h = host(this).sh.hwnd.get();
    BOOL((!h.is_invalid() && !p.is_null() && ClientToScreen(h, &mut *p).as_bool()) as i32)
}
unsafe extern "system" fn activate(_: *mut c_void, old: *mut i32) -> HRESULT {
    if !old.is_null() {
        *old = 0;
    }
    S_OK
}
unsafe extern "system" fn deactivate(_: *mut c_void, _: i32) -> HRESULT {
    S_OK
}
unsafe extern "system" fn get_client_rect(this: *mut c_void, r: *mut RECT) -> HRESULT {
    if r.is_null() {
        return E_POINTER;
    }
    *r = host(this).sh.rect.get();
    S_OK
}
unsafe extern "system" fn get_view_inset(_: *mut c_void, r: *mut RECT) -> HRESULT {
    if !r.is_null() {
        *r = RECT::default();
    }
    S_OK
}
unsafe extern "system" fn get_char_format(this: *mut c_void, out: *const *const CHARFORMATW) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let p: *const re::CharFormat2 = &**host(this).sh.cf.as_ptr();
    *(out as *mut *const CHARFORMATW) = p as *const CHARFORMATW;
    S_OK
}
unsafe extern "system" fn get_para_format(this: *mut c_void, out: *const *const PARAFORMAT) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let p: *const re::ParaFormat2 = &**host(this).sh.pf.as_ptr();
    *(out as *mut *const PARAFORMAT) = p as *const PARAFORMAT;
    S_OK
}
unsafe extern "system" fn get_sys_color(this: *mut c_void, i: SYS_COLOR_INDEX) -> COLORREF {
    let sh = &host(this).sh;
    match i {
        COLOR_WINDOWTEXT | COLOR_HIGHLIGHTTEXT => sh.ink.get(),
        COLOR_HIGHLIGHT => sh.selection.get(),
        COLOR_GRAYTEXT => sh.ink.get(),
        COLOR_WINDOW => COLORREF(0x00FF_FFFF),
        _ => COLORREF(GetSysColor(i)),
    }
}
unsafe extern "system" fn get_back_style(_: *mut c_void, s: *mut TXTBACKSTYLE) -> HRESULT {
    if !s.is_null() {
        *s = TXTBACK_TRANSPARENT;
    }
    S_OK
}
unsafe extern "system" fn get_max_length(_: *mut c_void, n: *mut u32) -> HRESULT {
    if !n.is_null() {
        *n = i32::MAX as u32;
    }
    S_OK
}
unsafe extern "system" fn get_scroll_bars(_: *mut c_void, bars: *mut u32) -> HRESULT {
    if !bars.is_null() {
        // ES_AUTOVSCROLL: scrolled as typed, no bar.
        *bars = 0x0040;
    }
    S_OK
}
unsafe extern "system" fn get_password_char(_: *mut c_void, c: *mut i8) -> HRESULT {
    if !c.is_null() {
        *c = 0;
    }
    S_FALSE
}
unsafe extern "system" fn get_accelerator_pos(_: *mut c_void, cp: *mut i32) -> HRESULT {
    if !cp.is_null() {
        *cp = -1;
    }
    S_OK
}
unsafe extern "system" fn get_extent(_: *mut c_void, _: *mut SIZE) -> HRESULT {
    E_NOTIMPL
}
unsafe extern "system" fn char_format_change(_: *mut c_void, _: *const CHARFORMATW) -> HRESULT {
    S_OK
}
unsafe extern "system" fn para_format_change(_: *mut c_void, _: *const PARAFORMAT) -> HRESULT {
    S_OK
}
unsafe extern "system" fn get_property_bits(this: *mut c_void, mask: u32, bits: *mut u32) -> HRESULT {
    if bits.is_null() {
        return E_POINTER;
    }
    let mut b = TXTBIT_RICHTEXT | TXTBIT_MULTILINE | TXTBIT_WORDWRAP;
    if host(this).sh.d2d {
        b |= TXTBIT_D2DDWRITE;
    }
    *bits = b & mask;
    S_OK
}
unsafe extern "system" fn notify(this: *mut c_void, code: u32, _: *mut c_void) -> HRESULT {
    let sh = &host(this).sh;
    if let Some(f) = sh.notify.get() {
        f(sh.hwnd.get(), code);
    }
    S_OK
}
unsafe extern "system" fn imm_get_context(this: *mut c_void) -> HIMC {
    let h = host(this).sh.hwnd.get();
    if h.is_invalid() {
        HIMC::default()
    } else {
        ImmGetContext(h)
    }
}
unsafe extern "system" fn imm_release_context(this: *mut c_void, himc: HIMC) {
    let h = host(this).sh.hwnd.get();
    if !h.is_invalid() {
        let _ = ImmReleaseContext(h, himc);
    }
}
unsafe extern "system" fn get_selection_bar_width(_: *mut c_void, w: *mut i32) -> HRESULT {
    if !w.is_null() {
        *w = 0;
    }
    S_OK
}
unsafe extern "system" fn is_double_click_pending(_: *mut c_void) -> BOOL {
    BOOL(0)
}
unsafe extern "system" fn get_window(this: *mut c_void, out: *mut HWND) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    *out = host(this).sh.hwnd.get();
    S_OK
}
unsafe extern "system" fn set_foreground_window(_: *mut c_void) -> HRESULT {
    S_OK
}
unsafe extern "system" fn get_palette(_: *mut c_void) -> HPALETTE {
    HPALETTE::default()
}
unsafe extern "system" fn get_east_asian_flags(_: *mut c_void, f: *mut i32) -> HRESULT {
    if !f.is_null() {
        *f = 0;
    }
    S_OK
}
unsafe extern "system" fn set_cursor2(_: *mut c_void, cur: HCURSOR, _: BOOL) -> HCURSOR {
    SetCursor(Some(cur))
}
unsafe extern "system" fn free_notification(_: *mut c_void) {}
unsafe extern "system" fn get_edit_style(_: *mut c_void, _: u32, data: *mut u32) -> HRESULT {
    if !data.is_null() {
        *data = 0;
    }
    S_OK
}
unsafe extern "system" fn get_window_styles(_: *mut c_void, style: *mut u32, ex: *mut u32) -> HRESULT {
    if !style.is_null() {
        // ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN
        *style = 0x0004 | 0x0040 | 0x1000;
    }
    if !ex.is_null() {
        *ex = 0;
    }
    S_OK
}
unsafe extern "system" fn show_drop_caret(_: *mut c_void, _: BOOL, _: HDC, _: *mut RECT) -> HRESULT {
    S_OK
}
unsafe extern "system" fn destroy_caret(this: *mut c_void) -> HRESULT {
    let sh = &host(this).sh;
    sh.caret.set(Caret { shown: false, ..sh.caret.get() });
    if sh.focus.get() {
        let _ = DestroyCaret();
    }
    sh.invalidate();
    S_OK
}
unsafe extern "system" fn get_horz_extent(_: *mut c_void, n: *mut i32) -> HRESULT {
    if !n.is_null() {
        *n = 0;
    }
    S_OK
}

static VTBL: ITextHost2_Vtbl = ITextHost2_Vtbl {
    base__: ITextHost_Vtbl {
        base__: IUnknown_Vtbl { QueryInterface: qi, AddRef: add_ref, Release: release },
        TxGetDC: get_dc,
        TxReleaseDC: release_dc,
        TxShowScrollBar: show_scroll_bar,
        TxEnableScrollBar: enable_scroll_bar,
        TxSetScrollRange: set_scroll_range,
        TxSetScrollPos: set_scroll_pos,
        TxInvalidateRect: invalidate_rect,
        TxViewChange: view_change,
        TxCreateCaret: create_caret,
        TxShowCaret: show_caret,
        TxSetCaretPos: set_caret_pos,
        TxSetTimer: set_timer,
        TxKillTimer: kill_timer,
        TxScrollWindowEx: scroll_window_ex,
        TxSetCapture: set_capture,
        TxSetFocus: set_focus,
        TxSetCursor: set_cursor,
        TxScreenToClient: screen_to_client,
        TxClientToScreen: client_to_screen,
        TxActivate: activate,
        TxDeactivate: deactivate,
        TxGetClientRect: get_client_rect,
        TxGetViewInset: get_view_inset,
        TxGetCharFormat: get_char_format,
        TxGetParaFormat: get_para_format,
        TxGetSysColor: get_sys_color,
        TxGetBackStyle: get_back_style,
        TxGetMaxLength: get_max_length,
        TxGetScrollBars: get_scroll_bars,
        TxGetPasswordChar: get_password_char,
        TxGetAcceleratorPos: get_accelerator_pos,
        TxGetExtent: get_extent,
        OnTxCharFormatChange: char_format_change,
        OnTxParaFormatChange: para_format_change,
        TxGetPropertyBits: get_property_bits,
        TxNotify: notify,
        TxImmGetContext: imm_get_context,
        TxImmReleaseContext: imm_release_context,
        TxGetSelectionBarWidth: get_selection_bar_width,
    },
    TxIsDoubleClickPending: is_double_click_pending,
    TxGetWindow: get_window,
    TxSetForegroundWindow: set_foreground_window,
    TxGetPalette: get_palette,
    TxGetEastAsianFlags: get_east_asian_flags,
    TxSetCursor2: set_cursor2,
    TxFreeTextServicesNotification: free_notification,
    TxGetEditStyle: get_edit_style,
    TxGetWindowStyles: get_window_styles,
    TxShowDropCaret: show_drop_caret,
    TxDestroyCaret: destroy_caret,
    TxGetHorzExtent: get_horz_extent,
};

type CreateTextServicesFn = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT;

/// A windowless RichEdit hosted by card window `hwnd`.
pub struct TextBox {
    /// ITextServices2 (one reference of ours).
    ts: *mut c_void,
    /// What CreateTextServices gave (its own reference).
    unk: Option<IUnknown>,
    host: *mut HostObj,
    sh: Rc<Shared>,
}

/// Why a TextBox could not be made (logged by the caller).
#[derive(Debug)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

thread_local! {
    /// Tests: pretend msftedit cannot make a windowless RichEdit.
    static REFUSE: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub fn set_test_refuse(on: bool) {
    REFUSE.with(|r| r.set(on));
}

impl TextBox {
    /// A windowless RichEdit on card window `hwnd` (may be invalid for
    /// offscreen use), drawn with Direct2D when `d2d`.
    pub fn new(hwnd: HWND, d2d: bool, notify: fn(HWND, u32)) -> Result<TextBox, Unavailable> {
        if REFUSE.with(|r| r.get()) {
            return Err(Unavailable("refused (test)".into()));
        }
        let sh = Rc::new(Shared {
            hwnd: Cell::new(hwnd),
            rect: Cell::new(RECT { left: 0, top: 0, right: 100, bottom: 100 }),
            caret: Cell::new(Caret::default()),
            focus: Cell::new(false),
            capture: Cell::new(false),
            d2d,
            drawing: Cell::new(false),
            dirty: Cell::new(true),
            ink: Cell::new(COLORREF(0)),
            selection: Cell::new(COLORREF(0x00C8_C8C8)),
            cf: RefCell::new(Box::new(base_char_format())),
            pf: RefCell::new(Box::new(base_para_format())),
            notify: Cell::new(Some(notify)),
            failed: RefCell::new(None),
            pixels: RefCell::new(None),
        });
        let host = Box::into_raw(Box::new(HostObj { vtbl: &VTBL, refs: Cell::new(1), sh: sh.clone() }));
        let fail = |why: String| {
            // SAFETY: our one reference.
            unsafe { release(host as *mut c_void) };
            Err(Unavailable(why))
        };
        unsafe {
            let dll = match LoadLibraryW(w!("msftedit.dll")) {
                Ok(d) => d,
                Err(e) => return fail(format!("no msftedit.dll ({e})")),
            };
            let Some(create) = GetProcAddress(dll, s!("CreateTextServices")) else { return fail("no CreateTextServices".into()) };
            let create: CreateTextServicesFn = std::mem::transmute(create);
            let mut unk: *mut c_void = std::ptr::null_mut();
            let hr = create(std::ptr::null_mut(), host as *mut c_void, &mut unk);
            if hr.is_err() || unk.is_null() {
                return fail(format!("CreateTextServices failed ({hr:?})"));
            }
            let unk = IUnknown::from_raw(unk);
            let iid = exported_iid(s!("IID_ITextServices2"), IID_ITEXTSERVICES2);
            let mut ts: *mut c_void = std::ptr::null_mut();
            let hr = unk.query(&iid, &mut ts);
            if hr.is_err() || ts.is_null() {
                drop(unk);
                return fail(format!("no ITextServices2 ({hr:?})"));
            }
            let tb = TextBox { ts, unk: Some(unk), host, sh };
            let _ = (tb.services().base__.OnTxInPlaceActivate)(tb.ts, std::ptr::null_mut());
            Ok(tb)
        }
    }

    fn services(&self) -> &ITextServices2_Vtbl {
        // SAFETY: `ts` is a live ITextServices2.
        unsafe { &**(self.ts as *const *const ITextServices2_Vtbl) }
    }

    /// Sends RichEdit message `msg`: (handled, result). Not handled
    /// (S_FALSE): the window's own default should run.
    pub fn send_raw(&self, msg: u32, wp: WPARAM, lp: LPARAM) -> (bool, LRESULT) {
        let mut r = LRESULT(0);
        let hr = unsafe { (self.services().base__.TxSendMessage)(self.ts, msg, wp, lp, &mut r) };
        (hr == S_OK, r)
    }

    pub fn hwnd(&self) -> HWND {
        self.sh.hwnd.get()
    }

    /// Where the text lies on the card (client px).
    pub fn rect(&self) -> RECT {
        self.sh.rect.get()
    }

    pub fn set_rect(&self, r: RECT) {
        if self.sh.rect.get() == r {
            return;
        }
        self.sh.rect.set(r);
        unsafe {
            let _ = (self.services().base__.OnTxPropertyBitsChange)(self.ts, TXTBIT_CLIENTRECTCHANGE, TXTBIT_CLIENTRECTCHANGE);
        }
        self.sh.invalidate();
    }

    /// The text's colour and the selection's (achromatic: a grey band).
    pub fn set_colors(&self, ink: COLORREF, selection: COLORREF) {
        self.sh.ink.set(ink);
        self.sh.selection.set(selection);
    }

    /// The default format RichEdit asks the host for.
    pub fn set_default_format(&self, cf: &re::CharFormat2) {
        let mut f = base_char_format();
        f.face = cf.face;
        f.height = cf.height;
        f.weight = cf.weight;
        f.color = cf.color;
        **self.sh.cf.borrow_mut() = f;
    }

    /// The card window got (`on`) or lost the keyboard.
    pub fn set_focus(&self, on: bool) {
        if self.sh.focus.replace(on) == on {
            return;
        }
        let msg = if on { 0x0007 } else { 0x0008 }; // WM_SETFOCUS / WM_KILLFOCUS
        self.send_raw(msg, WPARAM(0), LPARAM(0));
        if !on {
            let c = self.sh.caret.get();
            self.sh.caret.set(Caret { shown: false, ..c });
            unsafe {
                let _ = DestroyCaret();
            }
        }
        self.sh.invalidate();
    }

    pub fn has_focus(&self) -> bool {
        self.sh.focus.get()
    }

    /// RichEdit holds the mouse (a selection is being dragged).
    pub fn captured(&self) -> bool {
        self.sh.capture.get()
    }

    /// The capture went elsewhere.
    pub fn lose_capture(&self) {
        self.sh.capture.set(false);
    }

    /// The caret (client px), while it shows and the card has the keyboard.
    /// None while text is selected (the selection shows instead).
    pub fn caret(&self) -> Option<Caret> {
        let c = self.sh.caret.get();
        if !(c.shown && self.sh.focus.get() && c.h > 0) {
            return None;
        }
        let (a, b) = re::sel(self);
        (a == b).then_some(c)
    }

    /// Draws the text onto `rt` (its transform already maps client px
    /// to the target). Err: drawing failed (the caller falls back).
    pub fn draw(&self, rt: &ID2D1RenderTarget) -> windows::core::Result<()> {
        if self.sh.d2d {
            let r = self.sh.rect.get();
            let mut bounds = RECTL { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
            self.sh.drawing.set(true);
            let hr = unsafe { (self.services().TxDrawD2D)(self.ts, rt.as_raw(), &mut bounds, std::ptr::null_mut(), TXTVIEW_ACTIVE.0) };
            self.sh.drawing.set(false);
            self.sh.dirty.set(false);
            return hr.ok();
        }
        self.draw_gdi(rt)
    }

    /// RichEdit's GDI drawing (the card's font is a GDI font of this
    /// process: DirectWrite, and so TxDrawD2D, cannot see it) on the card:
    /// drawn twice, over black and over white, which gives every pixel's
    /// coverage and colour; the result goes on the card as a premultiplied
    /// bitmap, so the glass (static or live) shows around the letters. The
    /// pixels are kept until the text, the selection or the size changes.
    fn draw_gdi(&self, rt: &ID2D1RenderTarget) -> windows::core::Result<()> {
        use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
        use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_BITMAP_PROPERTIES};
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
        let r = self.sh.rect.get();
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || h <= 0 {
            return Ok(());
        }
        let fresh = !self.sh.dirty.get() && self.sh.pixels.borrow().as_ref().is_some_and(|(pw, ph, _)| (*pw, *ph) == (w, h));
        if !fresh {
            let black = self.raster(r, 0x00)?;
            let white = self.raster(r, 0xFF)?;
            *self.sh.pixels.borrow_mut() = Some((w, h, unmix(&black, &white)));
            self.sh.dirty.set(false);
        }
        let px = self.sh.pixels.borrow();
        let Some((_, _, px)) = px.as_ref() else { return Ok(()) };
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96., dpiY: 96. };
            let bmp = rt.CreateBitmap(D2D_SIZE_U { width: w as u32, height: h as u32 }, Some(px.as_ptr() as *const c_void), (w * 4) as u32, &props)?;
            let to = D2D_RECT_F { left: r.left as f32, top: r.top as f32, right: r.right as f32, bottom: r.bottom as f32 };
            rt.DrawBitmap(&bmp, Some(&to), 1., D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
        }
        Ok(())
    }

    /// The text over `bg` (0 black, 255 white): `r`'s px, top down BGRA.
    fn raster(&self, r: RECT, bg: u8) -> windows::core::Result<Vec<u8>> {
        use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, SetViewportOrgEx, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = match CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) if !bits.is_null() => b,
                Ok(b) => {
                    let _ = DeleteObject(b.into());
                    let _ = DeleteDC(dc);
                    return Err(windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY));
                }
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(e);
                }
            };
            std::ptr::write_bytes(bits as *mut u8, bg, (w * h * 4) as usize);
            let old = SelectObject(dc, bmp.into());
            // The card's client px onto the bitmap's origin.
            let _ = SetViewportOrgEx(dc, -r.left, -r.top, None);
            let mut bounds = RECTL { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
            self.sh.drawing.set(true);
            let hr = (self.services().base__.TxDraw)(
                self.ts,
                windows::Win32::System::Com::DVASPECT_CONTENT,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dc,
                HDC::default(),
                &mut bounds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                0,
                TXTVIEW_ACTIVE.0,
            );
            self.sh.drawing.set(false);
            let out = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(dc);
            hr.ok().map(|_| out)
        }
    }

    /// Drawing failed (`e`): the card falls back to the editor window.
    pub fn set_failed(&self, e: &windows::core::Error) {
        let how = if self.sh.d2d { "TxDrawD2D" } else { "TxDraw" };
        self.sh.failed.borrow_mut().get_or_insert_with(|| format!("{how} failed ({e})"));
    }

    /// Why drawing failed, if it did.
    pub fn failed(&self) -> Option<String> {
        self.sh.failed.borrow().clone()
    }

}

impl re::Target for TextBox {
    fn send(&self, msg: u32, wp: usize, lp: isize) -> isize {
        self.send_raw(msg, WPARAM(wp), LPARAM(lp)).1 .0
    }
}

impl Drop for TextBox {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.services().base__.OnTxInPlaceDeactivate)(self.ts);
            if self.sh.capture.get() {
                let _ = ReleaseCapture();
            }
        }
        self.sh.notify.set(None);
        self.sh.hwnd.set(HWND::default());
        // Ours first, then CreateTextServices' (the text services go),
        // then the host's own reference.
        unsafe {
            drop(IUnknown::from_raw(self.ts));
        }
        drop(self.unk.take());
        unsafe {
            release(self.host as *mut c_void);
        }
    }
}

fn base_char_format() -> re::CharFormat2 {
    re::CharFormat2 {
        // CFM_ALL (no effects set) with the weight.
        mask: 0xF800_003F | re::CFM_WEIGHT,
        charset: 1, // DEFAULT_CHARSET
        weight: 400,
        ..Default::default()
    }
}

fn base_para_format() -> re::ParaFormat2 {
    let mut p = re::ParaFormat2 { mask: 0x8000_003F, alignment: 1, tab_count: 1, ..Default::default() };
    p.tabs[0] = 720;
    p
}

/// Pure: the same text drawn over black and over white (BGRA, opaque) as
/// premultiplied BGRA: each pixel's coverage is how much white still shows
/// (the channels averaged, so ClearType's colour fringes become grey
/// edges); its colour is what was drawn over black. Grey, as the card's
/// ink and selection are.
pub fn unmix(black: &[u8], white: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; black.len().min(white.len())];
    for ((o, b), w) in out.chunks_exact_mut(4).zip(black.chunks_exact(4)).zip(white.chunks_exact(4)) {
        let seen = (0..3).map(|i| w[i] as i32 - b[i] as i32).sum::<i32>() / 3;
        let a = (255 - seen).clamp(0, 255);
        let c = ((b[0] as i32 + b[1] as i32 + b[2] as i32) / 3).min(a);
        o[0] = c as u8;
        o[1] = c as u8;
        o[2] = c as u8;
        o[3] = a as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_and_colour_come_back_from_two_backgrounds() {
        // Nothing drawn: clear.
        assert_eq!(unmix(&[0, 0, 0, 255], &[255, 255, 255, 255]), vec![0, 0, 0, 0]);
        // Solid white ink: white, opaque.
        assert_eq!(unmix(&[255, 255, 255, 255], &[255, 255, 255, 255]), vec![255, 255, 255, 255]);
        // Solid black ink: black, opaque.
        assert_eq!(unmix(&[0, 0, 0, 255], &[0, 0, 0, 255]), vec![0, 0, 0, 255]);
        // Half-covered mid grey: half alpha, premultiplied.
        let px = unmix(&[100, 100, 100, 255], &[228, 228, 228, 255]);
        assert_eq!(px[3], 127);
        assert_eq!(px[0], 100);
        // Never brighter than its coverage (premultiplied stays valid).
        let px = unmix(&[200, 200, 200, 255], &[250, 250, 250, 255]);
        assert!(px[0] <= px[3]);
    }
}
