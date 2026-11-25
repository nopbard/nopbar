//! Typing on a card. A card window has no redirection bitmap, so a child
//! edit control in it never shows: text is typed in an owned popup over
//! the part of the card where the text goes, its background that part of
//! the card (the same card::body, cut out), right above its card in the
//! z-order and never above other programs. The note's editor
//! (crate::notes::note) and the battery's rename field use this.
use super::card;
use std::{cell::RefCell, collections::HashMap};
use windows::{
    core::w,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
};

/// The card's pixels under a field.
pub struct Bg {
    pub bmp: HBITMAP,
    pub w: i32,
    pub h: i32,
}

impl Drop for Bg {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.bmp.into());
        }
    }
}

/// A top-down 32-bit DIB holding `pixels`.
pub unsafe fn dib(pixels: &[u8], w: i32, h: i32) -> Option<HBITMAP> {
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(bmp.into());
        return None;
    }
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len().min((w * h * 4) as usize));
    Some(bmp)
}

/// The piece `cut` (px, from the card's top left) of card `card` (screen
/// px) at scale `s`: its glass over the wallpaper there.
pub fn piece(card: RECT, s: f32, theme: &crate::theme::Theme, cut: RECT, under: Option<&super::backdrop::Under>) -> Option<Bg> {
    let size = (card.right - card.left, card.bottom - card.top);
    let (w, h) = (size.0 as f32 / s, size.1 as f32 / s);
    let pixels = super::surface::render(size.0, size.1, s, |dc, _| unsafe { card::body(dc, theme, w, h, under) })?;
    let (bw, bh) = ((cut.right - cut.left).min(size.0 - cut.left), (cut.bottom - cut.top).min(size.1 - cut.top));
    if bw <= 0 || bh <= 0 || cut.left < 0 || cut.top < 0 {
        return None;
    }
    let mut out = vec![0u8; (bw * bh * 4) as usize];
    for y in 0..bh {
        let from = (((cut.top + y) * size.0 + cut.left) * 4) as usize;
        let to = (y * bw * 4) as usize;
        out[to..to + (bw * 4) as usize].copy_from_slice(&pixels[from..from + (bw * 4) as usize]);
    }
    unsafe { dib(&out, bw, bh) }.map(|bmp| Bg { bmp, w: bw, h: bh })
}

/// Paints `bg` (or the plain card colour without one) at `dc`'s origin.
pub unsafe fn paint(dc: HDC, bg: Option<(HBITMAP, i32, i32)>, light: bool) {
    match bg {
        Some((bmp, w, h)) => {
            let mem = CreateCompatibleDC(Some(dc));
            let old = SelectObject(mem, bmp.into());
            let _ = BitBlt(dc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteDC(mem);
        }
        None => {
            let c = card::surface(light);
            let b = CreateSolidBrush(COLORREF(c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16));
            FillRect(dc, &RECT { left: 0, top: 0, right: 4000, bottom: 4000 }, b);
            let _ = DeleteObject(b.into());
        }
    }
}

/// Pure: where a field goes in the z-order, given the window right above
/// its card (`prev`): None when it already is that window (right above the
/// card), else after `prev` (the top with no window above).
pub fn slot(prev: Option<isize>, pop: isize) -> Option<Option<isize>> {
    match prev {
        Some(p) if p == pop => None,
        Some(p) => Some(Some(p)),
        None => Some(None),
    }
}

/// Puts field `pop` right above card `owner`.
pub fn above(owner: HWND, pop: HWND) {
    let prev = unsafe { GetWindow(owner, GW_HWNDPREV) }.ok().filter(|h| !h.is_invalid()).map(|h| h.0 as isize);
    if let Some(slot) = slot(prev, pop.0 as isize) {
        let after = slot.map_or(HWND_TOP, |h| HWND(h as *mut _));
        unsafe {
            let _ = SetWindowPos(pop, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
        }
    }
}

/// WM_WINDOWPOSCHANGING of field `pop` on card `owner`: an activation's
/// "to the top" puts it right above its card, never above other programs.
pub fn pin(owner: HWND, pop: HWND, p: &mut WINDOWPOS) {
    if super::win::is_placing() || p.flags.contains(SWP_NOZORDER) {
        return;
    }
    let prev = unsafe { GetWindow(owner, GW_HWNDPREV) }.ok().filter(|x| !x.is_invalid()).map(|x| x.0 as isize);
    match slot(prev, pop.0 as isize) {
        None => p.flags |= SWP_NOZORDER,
        Some(Some(after)) => p.hwndInsertAfter = HWND(after as *mut _),
        Some(None) => {}
    }
}

struct Field {
    bg: Option<Bg>,
    light: bool,
    font: HFONT,
    ink: COLORREF,
    brush: Option<HBRUSH>,
    /// The EDIT's top in the field (px): where the pattern starts for it.
    top: i32,
}

impl Drop for Field {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font.into());
            }
            if let Some(b) = self.brush.take() {
                let _ = DeleteObject(b.into());
            }
        }
    }
}

thread_local! {
    static FIELDS: RefCell<HashMap<isize, Field>> = RefCell::new(HashMap::new());
}

/// How a single-line field's text is set.
pub struct Look {
    pub size: f32,
    pub weight: i32,
    pub ink: crate::theme::Rgba,
    pub light: bool,
}

/// A one-line field over `at` (screen px) on card `owner`: an owned popup
/// holding an EDIT, its background `bg`, text in `look` at scale `s`.
/// Returns (popup, edit); the caller focuses the edit and destroys the
/// popup when done.
pub fn line(owner: HWND, at: RECT, bg: Option<Bg>, look: &Look, s: f32, text: &str) -> Option<(HWND, HWND)> {
    unsafe {
        let inst = GetModuleHandleW(None).ok()?;
        let class = w!("NopBar.WidgetField");
        RegisterClassExW(&WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(proc), hInstance: inst.into(), lpszClassName: class, hCursor: LoadCursorW(None, IDC_IBEAM).unwrap_or_default(), ..Default::default() });
        let (w, h) = (at.right - at.left, at.bottom - at.top);
        let pop = CreateWindowExW(WS_EX_TOOLWINDOW, class, None, WS_POPUP, at.left, at.top, w, h, Some(owner), None, Some(inst.into()), None).ok()?;
        // Kept out of captures with its card (review 5).
        super::privacy::register(pop, false);
        let family = crate::fonts::gdi_face(&crate::fonts::map(crate::fonts::THEME_DEFAULT));
        let font = CreateFontW(-((look.size * s).round() as i32), 0, 0, 0, look.weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, DEFAULT_QUALITY, 0, &windows::core::HSTRING::from(family));
        let ink = COLORREF(look.ink.r as u32 | (look.ink.g as u32) << 8 | (look.ink.b as u32) << 16);
        let brush = bg.as_ref().map(|b| CreatePatternBrush(b.bmp));
        // Its text box: one line, in the middle of the field's height.
        let line_h = ((look.size * s * 1.5).round() as i32).min(h);
        let top = ((h - line_h) / 2).max(0);
        FIELDS.with(|f| f.borrow_mut().insert(pop.0 as isize, Field { bg, light: look.light, font, ink, brush, top }));
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            &windows::core::HSTRING::from(text),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
            top,
            w,
            line_h,
            Some(pop),
            None,
            Some(inst.into()),
            None,
        );
        let Ok(edit) = edit else {
            let _ = DestroyWindow(pop);
            return None;
        };
        SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        // No inner margins (EM_SETMARGINS, EC_LEFTMARGIN | EC_RIGHTMARGIN).
        SendMessageW(edit, 0x00D3, Some(WPARAM(3)), Some(LPARAM(0)));
        super::win::placing(|| {
            let _ = ShowWindow(pop, SW_SHOWNOACTIVATE);
            above(owner, pop);
        });
        Some((pop, edit))
    }
}

unsafe extern "system" fn proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let owner = GetWindow(h, GW_OWNER).unwrap_or_default();
    let with = |f: &dyn Fn(&Field) -> LRESULT| FIELDS.with(|x| x.borrow().get(&(h.0 as isize)).map(f));
    match m {
        WM_ERASEBKGND => {
            with(&|f| {
                paint(HDC(wp.0 as *mut _), f.bg.as_ref().map(|b| (b.bmp, b.w, b.h)), f.light);
                LRESULT(1)
            })
            .unwrap_or(LRESULT(1))
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(h, &mut ps);
            with(&|f| {
                paint(dc, f.bg.as_ref().map(|b| (b.bmp, b.w, b.h)), f.light);
                LRESULT(0)
            });
            let _ = EndPaint(h, &ps);
            LRESULT(0)
        }
        // The EDIT over the card's own pixels, its text in the card's ink.
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            let dc = HDC(wp.0 as *mut _);
            with(&|f| {
                SetTextColor(dc, f.ink);
                SetBkMode(dc, TRANSPARENT);
                let _ = SetBrushOrgEx(dc, 0, -f.top, None);
                match f.brush {
                    Some(b) => LRESULT(b.0 as isize),
                    None => {
                        let c = card::surface(f.light);
                        SetBkColor(dc, COLORREF(c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16));
                        LRESULT(GetStockObject(DC_BRUSH).0 as isize)
                    }
                }
            })
            .unwrap_or_else(|| DefWindowProcW(h, m, wp, lp))
        }
        WM_WINDOWPOSCHANGING if !owner.is_invalid() => {
            pin(owner, h, &mut *(lp.0 as *mut WINDOWPOS));
            DefWindowProcW(h, m, wp, lp)
        }
        WM_COMMAND if !owner.is_invalid() => {
            // The EDIT's text scrolled: its background again.
            if ((wp.0 >> 16) & 0xFFFF) as u32 == EN_UPDATE {
                let _ = InvalidateRect(Some(HWND(lp.0 as *mut _)), None, true);
            }
            SendMessageW(owner, m, Some(wp), Some(lp))
        }
        WM_NCDESTROY => {
            FIELDS.with(|x| x.borrow_mut().remove(&(h.0 as isize)));
            super::privacy::forget(h);
            DefWindowProcW(h, m, wp, lp)
        }
        _ => DefWindowProcW(h, m, wp, lp),
    }
}

/// Field `pop`'s background (tests).
#[cfg(test)]
pub fn has_bg(pop: HWND) -> bool {
    FIELDS.with(|x| x.borrow().get(&(pop.0 as isize)).is_some_and(|f| f.bg.is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "system" fn plain(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        DefWindowProcW(h, m, w, l)
    }
    /// The battery's rename field (review: a child EDIT in the card's
    /// window never showed): an owned popup holding the EDIT, over the
    /// card's own pixels, right above the card.
    #[test]
    fn a_line_field_is_an_owned_popup_over_the_card() {
        unsafe {
            let inst = GetModuleHandleW(None).unwrap();
            RegisterClassExW(&WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(plain), hInstance: inst.into(), lpszClassName: w!("NopBar.FieldTestCard"), ..Default::default() });
            let card = RECT { left: -30000, top: -30000, right: -30000 + 136, bottom: -30000 + 176 };
            let owner = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, w!("NopBar.FieldTestCard"), None, WS_POPUP, card.left, card.top, 136, 176, None, None, Some(inst.into()), None).unwrap();
            let _ = ShowWindow(owner, SW_SHOWNOACTIVATE);
            let theme = crate::theme::Theme::graphite();
            let cut = RECT { left: 34, top: 40, right: 120, bottom: 64 };
            let bg = piece(card, 1., &theme, cut, None);
            assert!(bg.as_ref().is_some_and(|b| (b.w, b.h) == (86, 24)), "the card's pixels there");
            let at = RECT { left: card.left + 34, top: card.top + 40, right: card.left + 120, bottom: card.top + 64 };
            let look = Look { size: 12., weight: 600, ink: card::ink(&theme, 1), light: false };
            // Review 4: a cut outside the card is no picture, never a panic.
            assert!(piece(card, 1., &theme, RECT { left: 140, top: 180, right: 141, bottom: 181 }, None).is_none());
            assert!(piece(card, 1., &theme, RECT { left: 120, top: 170, right: 300, bottom: 300 }, None).is_some_and(|b| (b.w, b.h) == (16, 6)), "cut to the card");
            let (pop, edit) = line(owner, at, bg, &look, 1., "Keyboard").expect("a field");
            // Review 5: kept out of captures like its card.
            assert!(super::super::privacy::registered(pop));
            assert_eq!(GetWindow(pop, GW_OWNER).ok(), Some(owner), "owned by the card");
            assert_eq!(GetParent(edit).ok(), Some(pop));
            assert!(has_bg(pop));
            let mut r = RECT::default();
            let _ = GetWindowRect(pop, &mut r);
            assert_eq!(r, at);
            assert!(IsWindowVisible(pop).as_bool());
            assert_eq!(GetWindow(owner, GW_HWNDPREV).ok(), Some(pop), "right above its card");
            let mut t = [0u16; 16];
            let n = GetWindowTextW(edit, &mut t);
            assert_eq!(String::from_utf16_lossy(&t[..n as usize]), "Keyboard");
            let _ = DestroyWindow(pop);
            let _ = DestroyWindow(owner);
        }
    }

    #[test]
    fn a_field_goes_right_above_its_card() {
        assert_eq!(slot(Some(7), 7), None);
        assert_eq!(slot(Some(9), 7), Some(Some(9)));
        assert_eq!(slot(None, 7), Some(None));
    }
}
