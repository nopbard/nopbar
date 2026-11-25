//! GPU Composition editor overlay. The gallery panel stays solid; its card
//! previews share live::Layer with widgets when a live wallpaper is present.
use crate::{gfx::Gfx, monitors::Monitor, theme::Theme};
use std::rc::Rc;
use windows::{
    core::{w, Interface, Result},
    Win32::{
        Foundation::*,
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::WindowsAndMessaging::*,
    },
    UI::Composition::{CompositionDrawingSurface, ContainerVisual, Desktop::DesktopWindowTarget},
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};
/// The gallery panel's shadow margin (DIP).
const PANEL_M: f32 = 40.;
pub struct Surface {
    pub hwnd: HWND,
    pub _root: ContainerVisual,
    _target: DesktopWindowTarget,
    gfx: Rc<Gfx>,
    main: CompositionDrawingSurface,
    live: super::live::Layer,
    panel_live: super::live::Layer,
    panel: CompositionDrawingSurface,
    panel_root: ContainerVisual,
    monitor: Monitor,
}
impl Surface {
    pub unsafe fn new(
        gfx: Rc<Gfx>,
        _theme: &Theme,
        monitor: Monitor,
        handler: WNDPROC,
    ) -> Result<Self> {
        let inst = GetModuleHandleW(None)?;
        let class = w!("NopBar.WidgetEditor");
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: handler,
            hInstance: inst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        });
        let w = monitor.work.right - monitor.work.left;
        let h = monitor.work.bottom - monitor.work.top;
        let dpi = monitor.scale();
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            None,
            WS_POPUP,
            monitor.work.left,
            monitor.work.top,
            w,
            h,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        let result = (|| -> Result<Self> {
            let c = &gfx.compositor;
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, false)?;
            let root = c.CreateContainerVisual()?;
            root.SetSize(Vector2 {
                X: w as f32,
                Y: h as f32,
            })?;
            target.SetRoot(&root)?;
            let main = gfx.draw(w as u32, h as u32, |_, _, _| Ok(()))?;
            let sprite = c.CreateSpriteVisual()?;
            sprite.SetSize(Vector2 {
                X: w as f32,
                Y: h as f32,
            })?;
            sprite.SetBrush(&c.CreateSurfaceBrushWithSurface(&main)?)?;
            root.Children()?.InsertAtTop(&sprite)?;
            let r = super::edit_view::panel(monitor);
            let pw = r.right - r.left;
            let ph = r.bottom - r.top;
            // The panel and its shadow (PANEL_M round it) in one surface.
            let m = PANEL_M;
            let panel_root = c.CreateContainerVisual()?;
            panel_root.SetOffset(Vector3 {
                X: ((r.left - m) * dpi).round(),
                Y: ((r.top - m) * dpi).round(),
                Z: 0.,
            })?;
            root.Children()?.InsertAtTop(&panel_root)?;
            let (sw, sh) = (((pw + 2. * m) * dpi).ceil(), ((ph + 2. * m) * dpi).ceil());
            let panel = gfx.draw(sw as u32, sh as u32, |_, _, _| Ok(()))?;
            let sprite = c.CreateSpriteVisual()?;
            sprite.SetSize(Vector2 { X: sw, Y: sh })?;
            sprite.SetBrush(&c.CreateSurfaceBrushWithSurface(&panel)?)?;
            panel_root.Children()?.InsertAtTop(&sprite)?;
            panel_root.SetIsVisible(false)?;
            let live = super::live::Layer::new(c, &root, Some(hwnd))?;
            let panel_live = super::live::Layer::new(c, &panel_root, Some(hwnd))?;
            Ok(Self {
                live, panel_live,
                hwnd,
                _root: root,
                _target: target,
                gfx: gfx.clone(),
                main,
                panel,
                panel_root,
                monitor,
            })
        })();
        if result.is_err() {
            let _ = DestroyWindow(hwnd);
        } else {
            super::privacy::register(hwnd, false);
            // The gallery's previews are glass: drawn again when the
            // wallpaper's picture is ready or changes (review 9).
            super::backdrop::listen(hwnd);
        }
        result
    }
    pub fn draw(&self, theme: &Theme, scene: &mut super::edit_view::Scene<'_>) -> Result<()> {
        let gallery = scene.gallery;
        scene.gallery = false;
        let dpi = self.monitor.scale();
        let result = self.gfx.redraw(&self.main, |dc, ox, oy| unsafe {
            dc.SetTransform(&Matrix3x2 {
                M11: dpi,
                M22: dpi,
                M31: ox,
                M32: oy,
                ..Matrix3x2::identity()
            });
            self.live.draw((ox, oy), || super::edit_view::paint(dc, &self.gfx, theme, scene))
        });
        scene.gallery = gallery;
        result?;
        if gallery {
            let r = super::edit_view::panel(self.monitor);
            self.gfx.redraw(&self.panel, |dc, ox, oy| unsafe {
                let m = PANEL_M;
                dc.SetTransform(&Matrix3x2 {
                    M11: dpi,
                    M22: dpi,
                    M31: ox - ((r.left - m) * dpi).round() + 0.,
                    M32: oy - ((r.top - m) * dpi).round() + 0.,
                    ..Matrix3x2::identity()
                });
                self.panel_live.draw((ox, oy), || super::edit_view::paint_panel(dc, &self.gfx, theme, scene, true))
            })?;
        }
        self.panel_root.SetIsVisible(gallery)?;
        Ok(())
    }
    pub fn show(&self) {
        super::win::to_desktop(self.hwnd, super::win::shell_host());
        super::win::placing(|| unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        });
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        super::backdrop::unlisten(self.hwnd);
        super::privacy::forget(self.hwnd);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "system" fn plain(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        DefWindowProcW(h, m, w, l)
    }
    /// Review 9: the edit overlay (its gallery previews the glass) is told
    /// when the wallpaper's picture is ready or changed.
    #[test]
    fn review_09_the_editor_hears_about_new_pictures() {
        let stage = crate::gfx::promo_kit::stage();
        let r = RECT { left: -30000, top: -30000, right: -30000 + 800, bottom: -30000 + 600 };
        let m = crate::monitors::Monitor { handle: 0, rect: r, work: r, primary: false, dpi: 96 };
        let s = unsafe { Surface::new(stage.gfx.clone(), &crate::theme::Theme::graphite(), m, Some(plain)).unwrap() };
        assert!(super::super::backdrop::listening(s.hwnd));
        let h = s.hwnd;
        drop(s);
        assert!(!super::super::backdrop::listening(h));
    }
}
