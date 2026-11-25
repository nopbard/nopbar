//! Capture keyboard policy and the real Composition panel.
use super::widgets::{self, Capture, WIDTH};
use crate::{gfx::Gfx, theme::Theme};
use std::rc::Rc;
use windows::{
    core::{w, Interface, Result},
    Win32::{
        Foundation::*,
        Graphics::Dwm::*,
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::WindowsAndMessaging::*,
    },
    UI::Composition::{ContainerVisual, Desktop::DesktopWindowTarget},
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};
pub const MARGIN: f32 = 48.;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Native,
    Send,
    NewNote,
    Close,
    Next,
    Previous,
    Recall,
}
pub fn key(vk: u32, shift: bool, composing: bool, suppressed: bool) -> Key {
    if composing || vk == 13 && suppressed {
        return Key::Native;
    }
    match vk {
        13 => {
            if shift {
                Key::NewNote
            } else {
                Key::Send
            }
        }
        27 => Key::Close,
        9 => {
            if shift {
                Key::Previous
            } else {
                Key::Next
            }
        }
        38 => Key::Recall,
        _ => Key::Native,
    }
}
pub fn fallback(open: bool, text: &str) -> bool {
    open && text.trim().is_empty()
}
#[derive(Default)]
pub struct Editing {
    pub last: Option<String>,
    pub id: Option<String>,
    pub time: Option<Option<i64>>,
}
impl Editing {
    pub fn reset(&mut self) {
        self.id = None;
        self.time = None;
    }
    pub fn remember_note(&mut self, _id: String) {
        self.last = None;
        self.reset();
    }
}
#[derive(Debug, PartialEq)]
pub enum HotkeyStep {
    Restore,
    Toggle,
    Open,
}
pub fn hotkey_steps(open: bool, text: &str, summoned: bool) -> Vec<HotkeyStep> {
    if summoned {
        vec![HotkeyStep::Toggle]
    } else if fallback(open, text) {
        vec![HotkeyStep::Restore, HotkeyStep::Toggle]
    } else {
        vec![HotkeyStep::Open]
    }
}
pub struct PendingClick {
    id: String,
    pub at: std::time::Instant,
}
impl PendingClick {
    pub fn new(rows: &[super::inbox::Row], index: usize) -> Self {
        Self {
            id: rows[index].id.clone(),
            at: std::time::Instant::now(),
        }
    }
    pub fn row<'a>(&self, rows: &'a mut [super::inbox::Row]) -> Option<&'a mut super::inbox::Row> {
        rows.iter_mut().find(|r| r.id == self.id)
    }
}
pub struct View {
    pub hwnd: HWND,
    pub dpi: f32,
    pub _root: ContainerVisual,
    pub origin: (i32, i32),
    pub outer: ContainerVisual,
    _target: DesktopWindowTarget,
    gfx: Rc<Gfx>,
    surface: windows::UI::Composition::CompositionDrawingSurface,
}
impl View {
    pub unsafe fn new(
        gfx: Rc<Gfx>,
        theme: &Theme,
        dpi: f32,
        work: RECT,
        handler: WNDPROC,
    ) -> Result<Self> {
        let inst = GetModuleHandleW(None)?;
        let class = w!("NopBarCapture");
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: handler,
            hInstance: inst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_IBEAM)?,
            ..Default::default()
        });
        let w = ((WIDTH + MARGIN * 2.) * dpi).ceil() as i32;
        let h = ((230. + MARGIN * 2.) * dpi).ceil() as i32;
        let x = (work.left + work.right - w) / 2;
        let y = work.top + ((work.bottom - work.top) as f32 * 0.18) as i32 - (MARGIN * dpi) as i32;
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!(""),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        let built = (|| -> Result<Self> {
            let yes = windows::core::BOOL(1);
            let _ =
                DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &yes as *const _ as _, 4);
            let c = &gfx.compositor;
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let root = c.CreateContainerVisual()?;
            root.SetSize(Vector2 {
                X: w as f32,
                Y: h as f32,
            })?;
            target.SetRoot(&root)?;
            let outer = c.CreateContainerVisual()?;
            outer.SetScale(Vector3 {
                X: dpi,
                Y: dpi,
                Z: 1.,
            })?;
            outer.SetOffset(Vector3 {
                X: MARGIN * dpi,
                Y: MARGIN * dpi,
                Z: 0.,
            })?;
            root.Children()?.InsertAtTop(&outer)?;
            crate::popup::chrome(&gfx, theme, &outer, WIDTH, widgets::HEIGHT, dpi, 20.)?;
            let content = c.CreateSpriteVisual()?;
            content.SetSize(Vector2 { X: WIDTH, Y: 230. })?;
            outer.Children()?.InsertAtTop(&content)?;
            let surface = gfx.draw(
                (WIDTH * dpi).ceil() as u32,
                (230. * dpi).ceil() as u32,
                |_, _, _| Ok(()),
            )?;
            content.SetBrush(&c.CreateSurfaceBrushWithSurface(&surface)?)?;
            crate::desklayer::privacy::register(hwnd, false);
            Ok(Self {
                hwnd,
                dpi,
                _root: root,
                origin: (x, y),
                outer,
                _target: target,
                gfx,
                surface,
            })
        })();
        if built.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        built
    }
    pub fn draw(&self, theme: &Theme, c: &Capture<'_>) -> Result<()> {
        self.gfx.redraw(&self.surface, |dc, ox, oy| unsafe {
            dc.SetTransform(&Matrix3x2 {
                M11: self.dpi,
                M22: self.dpi,
                M31: ox,
                M32: oy,
                ..Matrix3x2::identity()
            });
            widgets::capture(dc, &self.gfx, theme, c)
        })
    }
    pub fn animate(&self, scale: f32, alpha: f32, delta: (f32, f32)) -> Result<()> {
        self.outer.SetCenterPoint(Vector3 {
            X: WIDTH / 2.,
            Y: 28.,
            Z: 0.,
        })?;
        self.outer.SetScale(Vector3 {
            X: self.dpi * scale,
            Y: self.dpi * scale,
            Z: 1.,
        })?;
        self.outer.SetOffset(Vector3 {
            X: MARGIN * self.dpi + delta.0,
            Y: MARGIN * self.dpi + delta.1,
            Z: 0.,
        })?;
        self.outer.SetOpacity(alpha)?;
        Ok(())
    }
}
impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_edit_context_clears_on_empty_close_and_open() {
        for _event in ["empty", "escape", "deactivate", "open"] {
            let mut e = Editing {
                last: Some("last".into()),
                id: Some("row".into()),
                time: Some(Some(100)),
            };
            e.reset();
            assert!(e.id.is_none() && e.time.is_none());
            assert_eq!(e.last.as_deref(), Some("last"));
        }
    }
    #[test]
    fn review_new_note_is_not_a_whole_document_recall_target() {
        let mut e = Editing::default();
        e.remember_note("document".into());
        assert!(e.last.is_none());
    }
    #[test]
    fn review_empty_second_hotkey_restores_before_toggle() {
        assert_eq!(
            hotkey_steps(true, "", false),
            [HotkeyStep::Restore, HotkeyStep::Toggle]
        );
    }
    #[test]
    fn review_summoned_hotkey_dismisses_without_capture() {
        assert_eq!(hotkey_steps(false, "", true), [HotkeyStep::Toggle]);
    }
    #[test]
    fn review_pending_click_follows_id_after_rows_change() {
        let mut rows = vec![
            super::super::inbox::Row::new("a".into(), 0, None, 0, None),
            super::super::inbox::Row::new("b".into(), 0, None, 0, None),
        ];
        let click = PendingClick::new(&rows, 1);
        rows.swap(0, 1);
        assert_eq!(click.row(&mut rows).unwrap().text, "b");
        rows.remove(0);
        assert!(click.row(&mut rows).is_none());
    }
    #[test]
    fn shortcut_twice_empty_uses_legacy_summon() {
        assert!(fallback(true, ""));
        assert!(!fallback(false, ""));
        assert!(!fallback(true, "draft"));
    }
    #[test]
    fn ime_enter_is_not_submission() {
        assert_eq!(key(13, false, true, false), Key::Native);
        assert_eq!(key(13, false, false, true), Key::Native);
        assert_eq!(key(13, false, false, false), Key::Send);
        assert_eq!(key(13, true, false, false), Key::NewNote);
    }
}
