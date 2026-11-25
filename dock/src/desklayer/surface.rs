//! One Composition surface per widget window: the card's glass, content and
//! hover buttons drawn in one Direct2D pass on the GPU.
//!
//! A card window (note, inbox, battery) is a `WS_EX_NOREDIRECTIONBITMAP`
//! popup exactly as big as its card (its rounded corners transparent): a
//! click anywhere outside the card's rectangle goes to the desktop. The
//! shadow is a window of its own that every click passes through
//! (crate::desklayer::shade). No window region, no DWM corners.
use crate::gfx::Gfx;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use windows::{
    core::{Interface, Result},
    Win32::{
        Foundation::HWND,
        Graphics::Direct2D::ID2D1DeviceContext,
        System::WinRT::Composition::ICompositorDesktopInterop,
        UI::HiDpi::GetDpiForWindow,
    },
    UI::Composition::{CompositionDrawingSurface, ContainerVisual, Desktop::DesktopWindowTarget},
};
use windows_numerics::{Matrix3x2, Vector2};

thread_local! {
    #[cfg(not(test))]
    static GFX: RefCell<Option<Rc<Gfx>>> = const { RefCell::new(None) };
    static RASTER: RefCell<Option<crate::companion::render::Raster>> = const { RefCell::new(None) };
    /// Pictures at a given scale, whatever display the test runs on.
    static TEST_SCALE: Cell<Option<f32>> = const { Cell::new(None) };
}

/// Card windows of this thread count as being at scale `s` (tests only).
#[cfg(test)]
pub fn set_test_scale(s: Option<f32>) {
    TEST_SCALE.with(|t| t.set(s));
}

/// The test scale, if any (always None outside tests).
pub fn test_scale() -> Option<f32> {
    TEST_SCALE.with(|t| t.get())
}

/// Window `hwnd`'s scale (px per DIP).
pub fn scale(hwnd: HWND) -> f32 {
    if let Some(s) = test_scale() {
        return s;
    }
    unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0
}

/// The Gfx of this thread (made once).
pub fn gfx() -> Option<Rc<Gfx>> {
    #[cfg(test)]
    {
        Some(crate::gfx::promo_kit::stage().gfx.clone())
    }
    #[cfg(not(test))]
    GFX.with(|g| {
        let mut g = g.borrow_mut();
        if g.is_none() {
            *g = Gfx::new().map_err(|e| crate::log!("widgets: no Direct2D ({e})")).ok().map(Rc::new);
        }
        g.clone()
    })
}

/// Draws offscreen a `w` x `h` px picture at scale `s` (premultiplied BGRA).
pub fn render(w: i32, h: i32, s: f32, paint: impl FnOnce(&ID2D1DeviceContext, &Gfx) -> Result<()>) -> Option<Vec<u8>> {
    let gfx = gfx()?;
    if w <= 0 || h <= 0 {
        return None;
    }
    let draw = |raster: &mut crate::companion::render::Raster| -> Result<Vec<u8>> {
        unsafe {
            raster.resize(w as u32, h as u32)?;
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            raster.custom(s, &mut pixels, |dc| paint(dc, &gfx))?;
            Ok(pixels)
        }
    };
    RASTER.with(|slot| {
        // A picture drawn while another is being drawn (a card's glass
        // made inside its picture) gets a raster of its own.
        let r = match slot.try_borrow_mut() {
            Ok(mut slot) => (|| -> Result<Vec<u8>> {
                if slot.is_none() {
                    *slot = Some(unsafe { crate::companion::render::Raster::new(gfx.offscreen_context()?, w as u32, h as u32)? });
                }
                draw(slot.as_mut().unwrap())
            })(),
            Err(_) => gfx.offscreen_context().and_then(|c| unsafe { crate::companion::render::Raster::new(c, w as u32, h as u32) }).and_then(|mut r| draw(&mut r)),
        };
        r.map_err(|e| crate::log!("widgets: offscreen picture failed ({e})")).ok()
    })
}

pub struct Glass {
    /// Its visual tree (offscreen pictures read it back).
    #[cfg_attr(not(test), allow(dead_code))]
    pub root: ContainerVisual,
    /// Kept: the window shows the tree while it lives.
    _target: Option<DesktopWindowTarget>,
    gfx: Rc<Gfx>,
    surface: CompositionDrawingSurface,
    live: super::live::Layer,
    /// The card's size (px): the window's.
    pub card: (i32, i32),
    pub dpi: f32,
}

impl Glass {
    /// The surface of a card `card` px big at `dpi`, shown in `hwnd` (None:
    /// a detached visual, for offscreen pictures).
    pub fn new(hwnd: Option<HWND>, gfx: Rc<Gfx>, card: (i32, i32), dpi: f32) -> Result<Self> {
        let (w, h) = card;
        let root = gfx.compositor.CreateContainerVisual()?;
        root.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;
        let target = match hwnd {
            Some(hwnd) => {
                let interop: ICompositorDesktopInterop = gfx.compositor.cast()?;
                let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, false)? };
                target.SetRoot(&root)?;
                Some(target)
            }
            None => None,
        };
        let surface = gfx.draw(w.max(1) as u32, h.max(1) as u32, |_, _, _| Ok(()))?;
        let sprite = gfx.compositor.CreateSpriteVisual()?;
        sprite.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;
        sprite.SetBrush(&gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
        root.Children()?.InsertAtTop(&sprite)?;
        let live = super::live::Layer::new(&gfx.compositor, &root, hwnd)?;
        Ok(Self { root, _target: target, gfx, surface, live, card, dpi })
    }

    /// The card is `card` px big at `dpi` now: a new surface in the same
    /// visual tree (its window and target stay; a window takes one target).
    pub fn resize(&mut self, card: (i32, i32), dpi: f32) -> Result<()> {
        if self.card == card && self.dpi == dpi {
            return Ok(());
        }
        let (w, h) = card;
        let surface = self.gfx.draw(w.max(1) as u32, h.max(1) as u32, |_, _, _| Ok(()))?;
        let sprite = self.gfx.compositor.CreateSpriteVisual()?;
        sprite.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;
        sprite.SetBrush(&self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
        self.root.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;
        let children = self.root.Children()?;
        children.RemoveAll()?;
        children.InsertAtTop(&sprite)?;
        self.live.attach(&self.root)?;
        self.surface = surface;
        self.card = card;
        self.dpi = dpi;
        Ok(())
    }

    /// Draws the whole card again: `paint` gets the context scaled to DIP
    /// with the card's top left at (0, 0).
    pub fn draw(&self, paint: impl FnOnce(&ID2D1DeviceContext, &Gfx) -> Result<()>) -> Result<()> {
        self.gfx.redraw(&self.surface, |dc, ox, oy| unsafe {
            dc.SetTransform(&Matrix3x2 { M11: self.dpi, M22: self.dpi, M31: ox, M32: oy, ..Matrix3x2::identity() });
            self.live.draw((ox, oy), || paint(dc, &self.gfx))
        })
    }

    /// Detaches the visual from its window (offscreen pictures).
    #[cfg(test)]
    pub fn detach(&self) {
        if let Some(t) = &self._target {
            let _ = t.SetRoot(None::<&windows::UI::Composition::Visual>);
        }
    }
}
