//! Every card's shadow, in a window of its own that every click passes
//! through (`WS_EX_LAYERED | WS_EX_TRANSPARENT`): a card window is only as
//! big as its card, so a click beside it (a desktop icon at the next cell,
//! a double click on the desktop) goes to Explorer whatever thread or
//! process is under it. The 0.1.92 card windows were 32 DIP larger than
//! their cards for the shadow, and HTTRANSPARENT passes clicks to the
//! same thread's windows only.
//!
//! The shadow window sits right under its card in the z-order and moves,
//! hides and shows with it (a subclass of the card window follows
//! WM_WINDOWPOSCHANGED). Its picture (card::shadow: the two shadows and
//! the hairline) is drawn again only when the card's size, scale, theme or
//! lift changes; a move only moves the window. Card and shadow together
//! are card::plate, the picture every snapshot shows.
use super::card;
use std::{cell::RefCell, collections::HashMap};
use windows::{
    core::{w, Result},
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
};

/// Room round the card for its shadow, the lifted one included (y 18,
/// blur 40) (DIP).
pub const MARGIN: f32 = 80.;
const SUBCLASS: usize = 0x5348;

/// The shadow window's extended styles: layered and transparent (clicks
/// pass to whatever is under it), never activated, not on the taskbar.
pub const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOREDIRECTIONBITMAP.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0);

/// What the picture depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub card: (i32, i32),
    pub dpi: f32,
    pub light: bool,
    pub adaptive: bool,
    pub lift: bool,
}

struct Shade {
    hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    size: (i32, i32),
    key: Option<Key>,
    light: bool,
    adaptive: bool,
    lift: bool,
}

impl Drop for Shade {
    fn drop(&mut self) {
        unsafe {
            super::privacy::forget(self.hwnd);
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

thread_local! {
    /// Card window → its shadow.
    static SHADES: RefCell<HashMap<isize, Shade>> = RefCell::new(HashMap::new());
}

unsafe extern "system" fn proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(h, m, wp, lp),
    }
}

unsafe fn make() -> Result<Shade> {
    let inst = GetModuleHandleW(None)?;
    let class = w!("NopBar.WidgetShade");
    RegisterClassExW(&WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(proc), hInstance: inst.into(), lpszClassName: class, ..Default::default() });
    let hwnd = CreateWindowExW(EX_STYLE, class, None, WS_POPUP, 0, 0, 1, 1, None, None, Some(inst.into()), None)?;
    super::privacy::register(hwnd, false);
    let dc = CreateCompatibleDC(None);
    let bmp = CreateBitmap(1, 1, 1, 32, None);
    let old = SelectObject(dc, bmp.into());
    Ok(Shade { hwnd, dc, bmp, old, size: (1, 1), key: None, light: false, adaptive: false, lift: false })
}

/// Pure: the shadow window's rectangle for card `card` at `dpi`.
pub fn window_for(card: RECT, dpi: f32) -> RECT {
    let m = (MARGIN * dpi).round() as i32;
    RECT { left: card.left - m, top: card.top - m, right: card.right + m, bottom: card.bottom + m }
}

/// The picture of key `k` (premultiplied BGRA, window size).
pub fn picture(k: Key) -> Option<(i32, i32, Vec<u8>)> {
    let m = (MARGIN * k.dpi).round() as i32;
    let (w, h) = (k.card.0 + 2 * m, k.card.1 + 2 * m);
    let (cw, ch) = (k.card.0 as f32 / k.dpi, k.card.1 as f32 / k.dpi);
    let mdip = m as f32 / k.dpi;
    // Lifted (dragged): the drag hint draws the card and its deep shadow
    // (crate::desklayer::drag_hint); nothing shows here.
    if k.lift {
        return Some((w, h, vec![0; (w * h * 4) as usize]));
    }
    let px = super::surface::render(w, h, k.dpi, |dc, _| unsafe {
        let mut t = windows_numerics::Matrix3x2::identity();
        dc.GetTransform(&mut t);
        dc.SetTransform(&(crate::companion::render::translate(mdip, mdip) * t));
        card::shadow(dc, k.light, k.adaptive, cw, ch)
    })?;
    Some((w, h, px))
}

/// Card window `card` gets a shadow that follows it.
pub fn attach(card: HWND) {
    SHADE_LOOK.with(|l| { l.borrow_mut().entry(card.0 as isize).or_insert((false, false, false)); });
    unsafe {
        let _ = SetWindowSubclass(card, Some(follow_proc), SUBCLASS, 0);
    }
}

/// Layered uploads can bypass the card's positioning messages. Called after
/// every companion presentation; unregistered sprite/bubble windows are ignored.
pub fn sync(card: HWND) {
    if SHADES.with(|s| s.borrow().contains_key(&(card.0 as isize)))
        || SHADE_LOOK.with(|s| s.borrow().contains_key(&(card.0 as isize))) {
        follow(card);
    }
}

/// Card `card`'s look: theme and lifted (being dragged).
pub fn set(card: HWND, light: bool, adaptive: bool, lift: bool) {
    let changed = SHADES.with(|s| {
        let mut s = s.borrow_mut();
        let Some(x) = s.get_mut(&(card.0 as isize)) else { return true };
        let c = (x.light, x.adaptive, x.lift) != (light, adaptive, lift);
        x.light = light;
        x.adaptive = adaptive;
        x.lift = lift;
        c
    });
    if changed {
        if SHADES.with(|s| !s.borrow().contains_key(&(card.0 as isize))) {
            SHADE_LOOK.with(|l| l.borrow_mut().insert(card.0 as isize, (light, adaptive, lift)));
        }
        follow(card);
    }
}

thread_local! {
    /// Looks asked for before the shadow window exists.
    static SHADE_LOOK: RefCell<HashMap<isize, ShadeLook>> = RefCell::new(HashMap::new());
}

type ShadeLook = (bool, bool, bool);

/// The shadow window of card `card` (tests).
#[cfg(test)]
pub fn of(card: HWND) -> Option<HWND> {
    SHADES.with(|s| s.borrow().get(&(card.0 as isize)).map(|x| x.hwnd))
}

/// The shadow where its card is: hidden with it, right under it in the
/// z-order, its picture drawn again if the card changed.
pub fn follow(card: HWND) {
    unsafe {
        let visible = IsWindowVisible(card).as_bool();
        let has = SHADES.with(|s| s.borrow().contains_key(&(card.0 as isize)));
        if !visible {
            if has {
                SHADES.with(|s| {
                    if let Some(x) = s.borrow().get(&(card.0 as isize)) {
                        if IsWindowVisible(x.hwnd).as_bool() {
                            let _ = ShowWindow(x.hwnd, SW_HIDE);
                        }
                    }
                });
            }
            return;
        }
        if !has {
            match make() {
                Ok(mut x) => {
                    if let Some((light, adaptive, lift)) = SHADE_LOOK.with(|l| l.borrow_mut().remove(&(card.0 as isize))) {
                        x.light = light;
                        x.adaptive = adaptive;
                        x.lift = lift;
                    }
                    SHADES.with(|s| s.borrow_mut().insert(card.0 as isize, x));
                }
                Err(e) => {
                    crate::log!("widgets: no shadow window ({e})");
                    return;
                }
            }
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(card, &mut r);
        let dpi = super::surface::scale(card);
        let wr = window_for(r, dpi);
        let Some((hwnd, key_now, light, adaptive, lift)) = SHADES.with(|s| s.borrow().get(&(card.0 as isize)).map(|x| (x.hwnd, x.key, x.light, x.adaptive, x.lift))) else { return };
        let key = Key { card: (r.right - r.left, r.bottom - r.top), dpi, light, adaptive, lift };
        if key_now != Some(key) {
            if let Some((w, h, px)) = picture(key) {
                SHADES.with(|s| {
                    if let Some(x) = s.borrow_mut().get_mut(&(card.0 as isize)) {
                        x.upload(w, h, &px, (wr.left, wr.top));
                        x.key = Some(key);
                    }
                });
            }
        }
        // Right under its card; where it is.
        let _ = SetWindowPos(hwnd, Some(card), wr.left, wr.top, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW);
    }
}

pub fn opacity(card: HWND, alpha: u8) {
    SHADES.with(|s| {
        if let Some(shade) = s.borrow().get(&(card.0 as isize)) {
            unsafe { shade.present(None, alpha); }
        }
    });
}

impl Shade {
    unsafe fn upload(&mut self, w: i32, h: i32, px: &[u8], at: (i32, i32)) {
        if self.size != (w, h) {
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let Ok(bmp) = CreateDIBSection(Some(self.dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else { return };
            SelectObject(self.dc, bmp.into());
            let _ = DeleteObject(self.bmp.into());
            self.bmp = bmp;
            self.size = (w, h);
            std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len().min((w * h * 4) as usize));
        } else {
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            SetDIBits(Some(self.dc), self.bmp, 0, h as u32, px.as_ptr() as *const _, &bi, DIB_RGB_COLORS);
        }
        self.present(Some(POINT { x: at.0, y: at.1 }), 255);
    }

    unsafe fn present(&self, at: Option<POINT>, alpha: u8) {
        let (w, h) = self.size;
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
        if let Err(e) = UpdateLayeredWindow(self.hwnd, None, at.as_ref().map(std::ptr::from_ref), Some(&SIZE { cx: w, cy: h }), Some(self.dc), Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA) {
            crate::log!("widgets: shadow not shown ({e})");
        }
    }
}

unsafe extern "system" fn follow_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM, _: usize, _: usize) -> LRESULT {
    let r = DefSubclassProc(h, m, wp, lp);
    match m {
        WM_WINDOWPOSCHANGED => {
            let p = &*(lp.0 as *const WINDOWPOS);
            if !(p.flags.contains(SWP_NOMOVE) && p.flags.contains(SWP_NOSIZE) && p.flags.contains(SWP_NOZORDER)) || p.flags.contains(SWP_SHOWWINDOW) || p.flags.contains(SWP_HIDEWINDOW) {
                follow(h);
            }
        }
        WM_DPICHANGED => follow(h),
        WM_NCDESTROY => {
            SHADES.with(|s| s.borrow_mut().remove(&(h.0 as isize)));
            SHADE_LOOK.with(|l| l.borrow_mut().remove(&(h.0 as isize)));
            let _ = RemoveWindowSubclass(h, Some(follow_proc), SUBCLASS);
        }
        _ => {}
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layered_card_upload_move_resize_hide_and_drop_keep_shadow_in_sync() {
        super::super::surface::set_test_scale(Some(1.));
        unsafe {
            let mut card = crate::companion::window::Window::with_proc(
                288, 176, false, w!("NopBar.LayeredShadeTest"), Some(plain),
            ).unwrap();
            attach(card.hwnd);
            set(card.hwnd, false, false, false);
            card.pixels_mut().fill(255);
            card.present(-30000, -30000, true).unwrap();
            let shade = of(card.hwnd).unwrap();
            for (x,y,dirty) in [(-29800,-29900,true),(-29600,-29800,false)] {
                card.present(x,y,dirty).unwrap();
                let (mut a,mut b)=(RECT::default(),RECT::default());
                GetWindowRect(card.hwnd,&mut a).unwrap();GetWindowRect(shade,&mut b).unwrap();
                assert_eq!(b,window_for(a,1.));
            }
            card.resize(320,220).unwrap();
            card.pixels_mut().fill(255);
            card.present(-29500,-29700,true).unwrap();
            let (mut a,mut b)=(RECT::default(),RECT::default());
            GetWindowRect(card.hwnd,&mut a).unwrap();GetWindowRect(shade,&mut b).unwrap();
            assert_eq!(b,window_for(a,1.));
            card.hide();assert!(!IsWindowVisible(shade).as_bool());
            card.present(-29500,-29700,false).unwrap();assert!(IsWindowVisible(shade).as_bool());
            drop(card);assert!(!IsWindow(Some(shade)).as_bool());
        }
        super::super::surface::set_test_scale(None);
    }
    unsafe extern "system" fn plain(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        DefWindowProcW(h, m, w, l)
    }
    fn card_window(r: RECT) -> HWND {
        unsafe {
            let inst = GetModuleHandleW(None).unwrap();
            RegisterClassExW(&WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(plain), hInstance: inst.into(), lpszClassName: w!("NopBar.ShadeTestCard"), ..Default::default() });
            CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, w!("NopBar.ShadeTestCard"), None, WS_POPUP, r.left, r.top, r.right - r.left, r.bottom - r.top, None, None, Some(inst.into()), None).unwrap()
        }
    }

    /// The shadow lets every click through, sits right under its card,
    /// and keeps its place round the card when the card moves or hides.
    #[test]
    fn the_shadow_is_a_click_through_window_right_under_its_card() {
        super::super::surface::set_test_scale(Some(1.5));
        let r = RECT { left: -30000, top: -30000, right: -30000 + 432, bottom: -30000 + 264 };
        let card = card_window(r);
        attach(card);
        set(card, false, false, false);
        unsafe {
            let _ = ShowWindow(card, SW_SHOWNOACTIVATE);
            let shade = of(card).expect("a shadow window");
            let ex = GetWindowLongPtrW(shade, GWL_EXSTYLE) as u32;
            for (f, name) in [(WS_EX_LAYERED, "layered"), (WS_EX_TRANSPARENT, "transparent"), (WS_EX_NOREDIRECTIONBITMAP, "no redirection"), (WS_EX_TOOLWINDOW, "tool"), (WS_EX_NOACTIVATE, "no activate")] {
                assert!(ex & f.0 != 0, "{name}");
            }
            assert!(IsWindowVisible(shade).as_bool());
            // Right under the card.
            assert_eq!(GetWindow(card, GW_HWNDNEXT).ok(), Some(shade));
            let around = |c: HWND, s: HWND| {
                let (mut a, mut b) = (RECT::default(), RECT::default());
                let _ = GetWindowRect(c, &mut a);
                let _ = GetWindowRect(s, &mut b);
                (a, b)
            };
            let (a, b) = around(card, shade);
            assert_eq!(b, window_for(a, 1.5));
            // Moved: still round it, still right under it.
            let _ = SetWindowPos(card, None, -29000, -29500, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            let (a, b) = around(card, shade);
            assert_eq!(a.left, -29000);
            assert_eq!(b, window_for(a, 1.5));
            assert_eq!(GetWindow(card, GW_HWNDNEXT).ok(), Some(shade));
            // Resized: drawn again for the new size.
            let _ = SetWindowPos(card, None, 0, 0, 204, 264, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            let (a, b) = around(card, shade);
            assert_eq!(b, window_for(a, 1.5));
            // Hidden with it.
            let _ = ShowWindow(card, SW_HIDE);
            assert!(!IsWindowVisible(shade).as_bool());
            let _ = DestroyWindow(card);
            assert!(!IsWindow(Some(shade)).as_bool(), "gone with its card");
        }
        super::super::surface::set_test_scale(None);
    }

    /// Card and shadow in two windows make the same picture as one plate
    /// (the snapshots): the shadow's pixels under the card's glass.
    #[test]
    fn card_and_shadow_make_one_plate() {
        let theme = crate::theme::Theme::graphite();
        let k = Key { card: (288, 176), dpi: 1., light: false, adaptive: false, lift: false };
        let (w, h, sh) = picture(k).unwrap();
        let m = (MARGIN * k.dpi).round() as i32;
        let body = super::super::surface::render(288, 176, 1., |dc, _| unsafe { card::body(dc, &theme, 288., 176., None) }).unwrap();
        let plate = super::super::surface::render(w, h, 1., |dc, _| unsafe {
            dc.SetTransform(&crate::companion::render::translate(m as f32, m as f32));
            card::plate(dc, &theme, 288., 176., false, None)
        })
        .unwrap();
        let mut worst = 0;
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let mut px = [sh[i], sh[i + 1], sh[i + 2], sh[i + 3]];
                let (cx, cy) = (x - m, y - m);
                if (0..288).contains(&cx) && (0..176).contains(&cy) {
                    let j = ((cy * 288 + cx) * 4) as usize;
                    let a = body[j + 3] as i32;
                    for c in 0..4 {
                        px[c] = (body[j + c] as i32 + px[c] as i32 * (255 - a) / 255).min(255) as u8;
                    }
                }
                for c in 0..4 {
                    worst = worst.max((px[c] as i32 - plate[i + c] as i32).abs());
                }
            }
        }
        assert!(worst <= 2, "card + shadow differ from the plate by {worst}");
    }
}
