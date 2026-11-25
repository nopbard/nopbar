//! A card window that never takes the focus (battery, inbox): the shared
//! widget surface (crate::desklayer::surface) in a no-activate popup as big
//! as the card, its shadow in a click-through window (crate::desklayer::shade).
use super::{
    model::{Desktop, Device, Size},
    paint,
};
use crate::{desklayer::card, desklayer::surface::Glass, gfx::Gfx, theme::Theme};
use std::rc::Rc;
use windows::{
    core::{w, Result},
    Win32::{
        Foundation::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
};
pub struct View {
    pub hwnd: HWND,
    pub glass: Glass,
    /// Where its card is (screen px): the wallpaper under it.
    at: std::cell::Cell<Option<RECT>>,
    /// Being dragged: drawn a little larger.
    pub lifted: std::cell::Cell<bool>,
    pub dpi: f32,
    pub size: Size,
    pub dimensions: (f32, f32),
}
impl View {
    #[cfg(test)]
    pub unsafe fn new(gfx: Rc<Gfx>, theme: &Theme, size: Size, dpi: f32, handler: WNDPROC) -> Result<Self> {
        Self::sized(gfx, theme, size, size.dimensions(), dpi, handler)
    }
    pub unsafe fn sized(gfx: Rc<Gfx>, theme: &Theme, size: Size, dimensions: (f32, f32), dpi: f32, handler: WNDPROC) -> Result<Self> {
        Self::classed(gfx, theme, size, dimensions, dpi, handler, w!("NopBar.DeviceBattery"))
    }
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn classed(
        gfx: Rc<Gfx>,
        _theme: &Theme,
        size: Size,
        dimensions: (f32, f32),
        dpi: f32,
        handler: WNDPROC,
        class: windows::core::PCWSTR,
    ) -> Result<Self> {
        let inst = GetModuleHandleW(None)?;
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: handler,
            hInstance: inst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        let card = ((dimensions.0 * dpi).round() as i32, (dimensions.1 * dpi).round() as i32);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!(""),
            WS_POPUP,
            0,
            0,
            card.0,
            card.1,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        crate::desklayer::hover::attach(hwnd);
        // Its shadow: a click-through window right under it.
        crate::desklayer::shade::attach(hwnd);
        match Glass::new(Some(hwnd), gfx, card, dpi) {
            Ok(glass) => {
                crate::desklayer::backdrop::listen(hwnd);
                Ok(Self { hwnd, glass, at: std::cell::Cell::new(None), lifted: std::cell::Cell::new(false), dpi, size, dimensions })
            }
            Err(e) => {
                let _ = DestroyWindow(hwnd);
                Err(e)
            }
        }
    }
    /// Resized (its handles, its size menu, a DPI change): the same window,
    /// a surface of the new size. The window keeps its handle, so whatever
    /// holds it (edit mode's chosen card, a command being handled) stays
    /// valid.
    pub fn resize(&mut self, size: Size, dimensions: (f32, f32), dpi: f32) -> Result<()> {
        let card = ((dimensions.0 * dpi).round() as i32, (dimensions.1 * dpi).round() as i32);
        self.glass.resize(card, dpi)?;
        self.size = size;
        self.dimensions = dimensions;
        self.dpi = dpi;
        Ok(())
    }
    /// The card drawn again: plate, `content`, and the … button at the
    /// window's hover alpha.
    pub fn frame(&self, theme: &Theme, kind: card::Kind, content: impl FnOnce(&windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, &Gfx, &Theme) -> Result<()>) -> Result<()> {
        self.frame_at(theme, kind, crate::desklayer::hover::alpha(self.hwnd), content)
    }
    /// frame() with the buttons at `alpha`.
    pub fn frame_at(&self, theme: &Theme, kind: card::Kind, alpha: f32, content: impl FnOnce(&windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, &Gfx, &Theme) -> Result<()>) -> Result<()> {
        let (w, h) = self.dimensions;
        let under = self.at.get().or_else(|| crate::desklayer::icons::card(self.hwnd))
            .and_then(|r| crate::desklayer::backdrop::under_for(self.hwnd, r));
        let glass = card::glass_for(theme, under.as_ref());
        let lift = self.lifted.get();
        crate::desklayer::shade::set(self.hwnd, glass.map_or(theme.is_light, |g| g.light), glass.is_some(), lift);
        self.glass.draw(|dc, gfx| unsafe {
            // Dragged: the drag hint draws it (the same painter, lifted);
            // this window is clear meanwhile.
            if lift {
                return Ok(());
            }
            face(dc, gfx, theme, kind, (w, h), under.as_ref(), alpha, content)
        })
    }
    /// Dragged: the window clear, its shadow gone (the drag hint draws
    /// the card).
    pub fn clear(&self, light: bool) {
        self.lifted.set(true);
        crate::desklayer::shade::set(self.hwnd, light, false, true);
        let _ = self.glass.draw(|_, _| Ok(()));
    }
    /// The card will be at `r` (screen px): drawn over the wallpaper there.
    pub fn aim(&self, r: RECT) {
        self.at.set(Some(r));
    }
    pub fn hide(&self) {
        crate::desklayer::win::placing(|| unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        });
    }
    pub fn draw(&self, theme: &Theme, desktop: &Desktop, rows: &[&Device]) -> Result<()> {
        self.frame(theme, card::Kind::Battery, |dc, gfx, theme| unsafe { paint::content_sized(dc, gfx, theme, desktop, rows, self.dimensions) })
    }
    pub fn place(&self, r: RECT) {
        self.at.set(Some(r));
        unsafe {
            crate::desklayer::win::placing(|| {
                let _ = SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW);
            });
            crate::desklayer::icons::register_card(self.hwnd, 0);
            crate::desklayer::icons::commit(self.hwnd, r);
            crate::desklayer::win::to_desktop(self.hwnd, crate::desklayer::win::shell_host());
        }
    }
}
/// What a card window needs before it draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remake {
    /// None yet: a window is made.
    Make,
    /// Another size, DPI or shape: the same window, resized.
    Resize,
    /// As it is.
    Keep,
}

/// Pure: what view `now` (its size, DPI, DIP dimensions; None: none yet)
/// needs to show `want`. A window is never made again for a new size: its
/// handle stays the card's.
pub fn remake(now: Option<(Size, f32, (f32, f32))>, want: (Size, f32, (f32, f32))) -> Remake {
    match now {
        None => Remake::Make,
        Some((size, dpi, dims)) if size == want.0 && (dpi - want.1).abs() <= 0.01 && dims == want.2 => Remake::Keep,
        Some(_) => Remake::Resize,
    }
}

impl View {
    /// remake()'s input for this view.
    pub fn shape(&self) -> (Size, f32, (f32, f32)) {
        (self.size, self.dpi, self.dimensions)
    }
}

/// A battery's or the inbox's card as its window draws it: the glass over
/// `under`, `content`, the … button at `alpha`.
#[allow(clippy::too_many_arguments)]
pub unsafe fn face(
    dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext,
    gfx: &Gfx,
    theme: &Theme,
    kind: card::Kind,
    (w, h): (f32, f32),
    under: Option<&crate::desklayer::backdrop::Under>,
    alpha: f32,
    content: impl FnOnce(&windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, &Gfx, &Theme) -> Result<()>,
) -> Result<()> {
    card::body(dc, theme, w, h, under)?;
    let theme = card::theme_for(theme, under);
    content(dc, gfx, theme)?;
    card::controls(dc, theme, kind, w, alpha)
}
impl Drop for View {
    fn drop(&mut self) {
        crate::desklayer::backdrop::unlisten(self.hwnd);
        crate::vdesk::unlisten(self.hwnd);
        crate::desklayer::privacy::forget(self.hwnd);
        crate::desklayer::icons::forget(self.hwnd);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
