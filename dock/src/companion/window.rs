//! Per-pixel layered windows, as displays::identify uses. Windows itself
//! passes zero-alpha pixels to other processes (HTTRANSPARENT cannot).
//! The ground shadow is a separate, entirely input-transparent window.
use super::{
    grab::{Input, Kind, Point},
    render::Raster,
};
use std::cell::Cell;
use windows::core::{w, Result};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::{
    Foundation::*, Graphics::Gdi::*, System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::*,
};

static DESKTOP_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub(super) fn desktop_changed() { DESKTOP_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }

struct HitData {
    bits: *mut u8,
    w: u32,
    h: u32,
    active: Option<(u32, Kind)>,
    wheel: i32,
    ignore: Cell<bool>,
    ctrl_timer: Cell<bool>,
    right_armed: Cell<bool>,
}
/// Layered HWNDs never receive input over zero-alpha pixels, regardless of
/// WM_NCHITTEST. Reserve the least nonzero alpha only in the invisible target
/// padding; native input reads this same buffer.
fn pad_input(pixels: &mut [u8], w: u32, h: u32, dpi: f32) -> Option<RECT> {
    let mut bounds = (w, h, 0, 0);
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        if px[3] > 1 { let x=i as u32%w;let y=i as u32/w;
            bounds=(bounds.0.min(x),bounds.1.min(y),bounds.2.max(x+1),bounds.3.max(y+1)); }
    }
    if bounds.2==0 {return None;}
    let dx=(((28.*dpi).ceil() as i32-(bounds.2-bounds.0) as i32).max(0)+1)/2;
    let dy=(((28.*dpi).ceil() as i32-(bounds.3-bounds.1) as i32).max(0)+1)/2;
    let left=(bounds.0 as i32-dx).max(0);let right=(bounds.2 as i32+dx).min(w as i32);
    let top=(bounds.1 as i32-dy).max(0);let bottom=(bounds.3 as i32+dy).min(h as i32);
    if dx!=0||dy!=0 {
        let solid:Vec<_>=pixels.chunks_exact(4).enumerate().filter(|(_,p)|p[3]>1).map(|(i,_)|(i as i32%w as i32,i as i32/w as i32)).collect();
        for (sx,sy) in solid {for y in (sy-dy).max(0)..=(sy+dy).min(h as i32-1) {for x in (sx-dx).max(0)..=(sx+dx).min(w as i32-1) {
            let px=&mut pixels[((y as u32*w+x as u32)*4) as usize..][..4];
            if px[3]==0 {px.copy_from_slice(&[0,0,0,1]);}
        }}}
    }
    Some(RECT{left,top,right,bottom})
}
impl HitData {
    unsafe fn hit(&self, x: i32, y: i32) -> bool {
        x >= 0
            && y >= 0
            && x < self.w as i32
            && y < self.h as i32
            && *self
                .bits
                .add(((y as u32 * self.w + x as u32) * 4 + 3) as usize)
                > 0
    }
}
pub struct Window {
    hit: Box<HitData>,
    pub hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: u32,
    h: u32,
    shown: Cell<bool>,
    position: Cell<Option<(i32, i32)>>,
    fractional: Cell<(f32,f32)>,
    opacity: Cell<u8>,
    presented_opacity: Cell<u8>,
    resized: Cell<bool>,
    faded: Cell<bool>,
    front: Cell<bool>,
    desktop_generation: Cell<u64>,
    capture_registered: bool,
}
/// None means no z-order call, including the first frame while yielding.
fn z_after(shown: bool, resized: bool, yielding: bool, overlay: Option<HWND>) -> Option<HWND> {
    (!yielding && (!shown || resized)).then_some(overlay.unwrap_or(HWND_NOTOPMOST))
}
fn fade_plan(shown:bool,faded:bool,alpha:f32)->Option<(bool,bool)> {
    (shown||faded).then_some((alpha>0.,alpha<1. && (faded||alpha==0.)))
}
/// EnumWindows walks front to back: inserting after the lowest visible NopBar
/// overlay keeps *all* of them above us, even with nested menus.
unsafe fn overlay_below() -> Option<HWND> {
    struct Overlays { label: Option<HWND>, last: Option<HWND> }
    unsafe extern "system" fn visit(h: HWND, lp: LPARAM) -> windows::core::BOOL {
        let overlays=&mut *(lp.0 as *mut Overlays);
        let mut pid=0;GetWindowThreadProcessId(h,Some(&mut pid));
        if pid==std::process::id() && IsWindowVisible(h).as_bool() {
            let mut name=[0u16;128];let n=GetClassNameW(h,&mut name);
            if overlays.label==Some(h) || matches!(String::from_utf16_lossy(&name[..n.max(0) as usize]).as_str(),
                "NopBarPopup"|"NopBarPreview"|"NopBarPreviewShade"|"NopBarStack"|"NopBarSearch"|"NopBarTip") {
                overlays.last=Some(h);
            }
        }
        windows::core::BOOL(1)
    }
    // Dock labels live in the dock's composition window, not a separate HWND.
    let mut overlays=Overlays{label:crate::dock::companion_label_window(),last:None};
    let _=EnumWindows(Some(visit),LPARAM(&mut overlays as *mut Overlays as isize));
    overlays.last
}
fn presentation_flags(shown: bool) -> SET_WINDOW_POS_FLAGS {
    SWP_NOMOVE
        | SWP_NOSIZE
        | SWP_NOACTIVATE
        | SWP_SHOWWINDOW
        | if shown {
            SWP_NOZORDER
        } else {
            SET_WINDOW_POS_FLAGS(0)
        }
}
fn input_style(old:isize,ignore:bool,ctrl:bool)->isize {let flag=WS_EX_TRANSPARENT.0 as isize;if ignore&&!ctrl{old|flag}else{old&!flag}}
fn right_click(armed:&Cell<bool>,message:u32,allowed:bool)->bool {
    match message {
        WM_RBUTTONDOWN=>{armed.set(allowed);false},
        WM_RBUTTONUP=>armed.replace(false) && allowed,
        _=>false,
    }
}
const CTRL_TIMER:usize=0x434a;
unsafe fn ctrl_down()->bool{windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL.0 as i32)<0}
unsafe fn set_input_style(hwnd:HWND,ignore:bool,ctrl:bool){let old=GetWindowLongPtrW(hwnd,GWL_EXSTYLE);let next=input_style(old,ignore,ctrl);if old!=next{SetWindowLongPtrW(hwnd,GWL_EXSTYLE,next);}}
impl Window {
    pub(super) fn follow_capture_setting(&mut self) {
        crate::desklayer::privacy::register(self.hwnd, false);
        self.capture_registered = true;
    }
    pub fn front(&self, front: bool) {
        if self.front.replace(front) != front { self.resized.set(true); }
    }
    pub fn in_front(&self) -> bool { self.front.get() }
    pub fn ignore(&self,ignore:bool){unsafe{
        if self.hit.ignore.replace(ignore)==ignore&&self.hit.ctrl_timer.get()==ignore{return;}
        if ignore {SetTimer(Some(self.hwnd),CTRL_TIMER,50,None);}else{let _=KillTimer(Some(self.hwnd),CTRL_TIMER);}
        self.hit.ctrl_timer.set(ignore);
        set_input_style(self.hwnd,ignore,ignore&&ctrl_down());
    }}
    pub unsafe fn new(w: u32, h: u32, shadow: bool) -> Result<Self> {
        Self::with_proc(w,h,shadow,w!("NopBarCompanion"),Some(proc))
    }
    pub(crate) unsafe fn with_proc(w:u32,h:u32,shadow:bool,class:windows::core::PCWSTR,handler:WNDPROC)->Result<Self> {
        let inst = GetModuleHandleW(None)?;
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: handler,
            hInstance: inst.into(),
            hCursor: LoadCursorW(None, IDC_HAND)?,
            lpszClassName: class,
            ..Default::default()
        });
        let flags = WS_EX_LAYERED
            | WS_EX_TOOLWINDOW
            | WS_EX_NOACTIVATE
            | if shadow {
                WS_EX_TRANSPARENT
            } else {
                WINDOW_EX_STYLE(0)
            };
        let hwnd = CreateWindowExW(
            flags,
            class,
            w!(""),
            WS_POPUP,
            0,
            0,
            w as i32,
            h as i32,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        let dc = CreateCompatibleDC(None);
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
        let mut bits = std::ptr::null_mut();
        let bmp = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(b) => b,
            Err(e) => {
                let _ = DeleteDC(dc);
                let _ = DestroyWindow(hwnd);
                return Err(e);
            }
        };
        let old = SelectObject(dc, bmp.into());
        let mut hit = Box::new(HitData {
            bits: bits as _,
            w,
            h,
            active: None,
            wheel: 0,
            ignore: Cell::new(false),
            ctrl_timer: Cell::new(false),
            right_armed: Cell::new(false),
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut *hit as *mut HitData as isize);
        if !shadow {
            disable_feedback(hwnd);
        }
        Ok(Self {
            hit,
            hwnd,
            dc,
            bmp,
            old,
            bits: bits as _,
            shown: Cell::new(false),
            position: Cell::new(None),
            fractional: Cell::new((0.,0.)),
            opacity: Cell::new(255),
            presented_opacity: Cell::new(255),
            resized: Cell::new(true),
            faded: Cell::new(false),
            front: Cell::new(false),
            desktop_generation: Cell::new(0),
            capture_registered: false,
            w,
            h,
        })
    }
    /// Resize the DIB without replacing HWND: mouse and pointer capture survive.
    pub unsafe fn resize(&mut self, w: u32, h: u32) -> Result<()> {
        if self.w == w && self.h == h {
            return Ok(());
        }
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
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        SelectObject(self.dc, bmp.into());
        let _ = DeleteObject(self.bmp.into());
        self.bmp = bmp;
        self.bits = bits as _;
        self.w = w;
        self.h = h;
        self.hit.bits = self.bits;
        self.hit.w = w;
        self.hit.h = h;
        self.resized.set(true);
        Ok(())
    }
    pub unsafe fn pixels_mut(&mut self) -> &mut [u8] {
        std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize)
    }
    pub unsafe fn pad_input(&mut self,dpi:f32) {
        let (w,h)=(self.w,self.h);pad_input(self.pixels_mut(),w,h,dpi);
    }
    pub unsafe fn present(&self, x: i32, y: i32, dirty: bool) -> Result<()> {
        self.fractional.set((0.,0.));
        let generation = DESKTOP_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
        let dirty=dirty || self.presented_opacity.get()!=self.opacity.get() || self.resized.get() || self.desktop_generation.get()!=generation;
        if !dirty && self.shown.get() {
            if self.position.get() != Some((x, y)) {
                SetWindowPos(
                    self.hwnd,
                    None,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW,
                )?;
                self.position.set(Some((x, y)));
                crate::desklayer::shade::sync(self.hwnd);
            }
            return Ok(());
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: self.opacity.get(),
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(
            self.hwnd,
            None,
            Some(&POINT { x, y }),
            Some(&SIZE {
                cx: self.w as i32,
                cy: self.h as i32,
            }),
            Some(self.dc),
            Some(&POINT::default()),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )?;
        let yielding=super::ui_paused() || crate::dock::companion_mode()!=super::visibility::Mode::Run;
        let restack = self.resized.get() || self.desktop_generation.get() != generation;
        let needs_z=!yielding && (!self.shown.get() || restack);
        let after=z_after(self.shown.get(),restack,yielding,if needs_z && self.front.get() {Some(overlay_below().unwrap_or(HWND_TOPMOST))}else{None});
        SetWindowPos(
            self.hwnd,
            after,
            0,
            0,
            0,
            0,
            presentation_flags(after.is_none()),
        )?;
        if !yielding && !self.front.get() && needs_z {
            crate::desklayer::win::to_desktop(self.hwnd, crate::desklayer::win::shell_host());
        }
        self.shown.set(true);
        if !yielding { self.resized.set(false); self.desktop_generation.set(generation); }
        self.presented_opacity.set(self.opacity.get());
        self.position.set(Some((x, y)));
        crate::desklayer::shade::sync(self.hwnd);
        Ok(())
    }
    pub fn opacity(&self, alpha: f32) { self.opacity.set((alpha.clamp(0.,1.)*255.).round() as u8); }
    pub fn can_restore(&self) -> bool {self.shown.get()||self.faded.get()}
    pub unsafe fn moved_frame(&self, dx:f32,dy:f32,screens:&[RECT]) -> super::visibility::Frame {
        let (rx,ry)=self.fractional.get();
        self.position.get().map_or_default(|(x,y)|super::visibility::Frame::from_pixels(
            std::slice::from_raw_parts(self.bits,(self.w*self.h*4) as usize),self.w,
            (x+(dx+rx).round() as i32,y+(dy+ry).round() as i32),screens,false))
    }
    #[cfg(test)]
    pub fn visibility_state(&self) -> (bool,u8) {(self.shown.get(),self.opacity.get())}
    /// Upload cached pixels only; neither animation nor location advances.
    pub unsafe fn cached(&self) -> Result<()> {
        if let Some((x,y))=self.position.get() { self.present(x,y,false)?; }
        Ok(())
    }
    pub unsafe fn fade_cached(&self, alpha:f32) -> Result<()> {
        let Some((show,faded))=fade_plan(self.shown.get(),self.faded.get(),alpha) else{return Ok(());};
        self.faded.set(faded);
        self.opacity(alpha);
        if !show {
            if self.shown.replace(false) {let _=ShowWindow(self.hwnd,SW_HIDE);}
            crate::desklayer::shade::sync(self.hwnd);
        } else {
            self.cached()?;
        }
        Ok(())
    }
    pub unsafe fn body_bounds(&self) -> Option<RECT> {
        let (x,y)=self.position.get()?;
        let mut r=RECT{left:self.w as i32,top:self.h as i32,right:0,bottom:0};
        for (i,p) in std::slice::from_raw_parts(self.bits,(self.w*self.h*4) as usize).as_chunks::<4>().0.iter().enumerate() {
            if p[3]>1 {let (px,py)=(i as i32%self.w as i32,i as i32/self.w as i32);
                r.left=r.left.min(px);r.top=r.top.min(py);r.right=r.right.max(px+1);r.bottom=r.bottom.max(py+1);}
        }
        (r.right>r.left).then_some(RECT{left:r.left+x,top:r.top+y,right:r.right+x,bottom:r.bottom+y})
    }
    pub fn cancel_input(&mut self) {
        let active=self.hit.active.take();
        finish_cancel(active, || unsafe { let _=ReleaseCapture(); }, super::input);
    }
    /// Translate only an already-visible cached surface; never upload pixels.
    pub unsafe fn move_by(&self,dx:f32,dy:f32)->Result<()> {
        if !self.shown.get(){return Ok(());}
        let Some((x,y))=self.position.get()else{return Ok(());};
        let (rx,ry)=self.fractional.get();let dx=dx+rx;let dy=dy+ry;
        let (ix,iy)=(dx.round() as i32,dy.round() as i32);
        self.present(x+ix,y+iy,false)?;
        self.fractional.set((dx-ix as f32,dy-iy as f32));Ok(())
    }
    pub fn hide(&self) {
        self.faded.set(false);
        if self.hit.ctrl_timer.replace(false){unsafe{let _=KillTimer(Some(self.hwnd),CTRL_TIMER);}}
        if self.shown.replace(false) { unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }}
        crate::desklayer::shade::sync(self.hwnd);
    }
}
// No HitData reference survives an OS call or synchronous input/render dispatch.
fn deliver_mouse_down(capture:impl FnOnce(),input:impl FnOnce(),active:impl FnOnce()->bool,release:impl FnOnce()) {
    capture();input();if !active(){release();}
}
fn finish_cancel(active:Option<(u32,Kind)>, release:impl FnOnce(), cancel:impl FnOnce(Input)) {
    if let Some((id,kind))=active {
        if kind==Kind::Mouse { release(); }
        cancel(Input::Cancel{id});
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        if self.capture_registered {
            crate::desklayer::privacy::forget(self.hwnd);
        }
        unsafe {
            if let Some((id, _)) = self.hit.active.take() {
                super::input(Input::Cancel { id });
            }
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
// https://learn.microsoft.com/windows/win32/tablet/wm-tablet-querysystemgesturestatus-message
const TABLET_GESTURES: u32 = 0x02CC;
const TABLET_FLAGS: isize = 0x00000001 | 0x00000008 | 0x00000010 | 0x00010000 | 0x00000200;
fn promoted_mouse(extra: usize) -> bool {
    extra & 0xffffff00 == 0xff515700
}
unsafe fn disable_feedback(hwnd: HWND) {
    use windows::Win32::UI::Controls::*;
    let off = windows::core::BOOL(0);
    for kind in [
        FEEDBACK_TOUCH_CONTACTVISUALIZATION,
        FEEDBACK_PEN_BARRELVISUALIZATION,
        FEEDBACK_PEN_PRESSANDHOLD,
        FEEDBACK_TOUCH_PRESSANDHOLD,
        FEEDBACK_TOUCH_TAP,
        FEEDBACK_PEN_TAP,
        FEEDBACK_TOUCH_RIGHTTAP,
        FEEDBACK_PEN_RIGHTTAP,
        FEEDBACK_TOUCH_DOUBLETAP,
        FEEDBACK_PEN_DOUBLETAP,
    ] {
        let _ = SetWindowFeedbackSetting(
            hwnd,
            kind,
            0,
            std::mem::size_of_val(&off) as u32,
            Some(&off as *const _ as _),
        );
    }
}
unsafe fn client_point(h: HWND, l: LPARAM) -> Point {
    let mut p = POINT {
        x: (l.0 & 0xffff) as i16 as i32,
        y: ((l.0 >> 16) & 0xffff) as i16 as i32,
    };
    let _ = ClientToScreen(h, &mut p);
    Point::new(p.x as f32, p.y as f32)
}
unsafe fn hit_screen(h: HWND, data: &HitData, p: Point) -> bool {
    let mut pt = POINT {
        x: p.x as i32,
        y: p.y as i32,
    };
    let _ = ScreenToClient(h, &mut pt);
    data.hit(pt.x, pt.y)
}
unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    use windows::Win32::UI::Input::Pointer::*;
    if m == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    if m == WM_POINTERACTIVATE {
        return LRESULT(PA_NOACTIVATE as isize);
    }
    if m == TABLET_GESTURES {
        return LRESULT(TABLET_FLAGS);
    }
    if matches!(m, WM_CONTEXTMENU | WM_RBUTTONDOWN | WM_RBUTTONUP) {
        let ptr=GetWindowLongPtrW(h,GWLP_USERDATA) as *mut HitData;
        if !ptr.is_null() {
            let allowed=!super::input_blocked() && super::APP.with(|a|a.try_borrow().is_ok_and(|a|a.as_ref().is_some_and(|a|!a.cfg.ignore_mouse || ctrl_down())));
            if right_click(&(*ptr).right_armed,m,allowed) {super::convenience::request(super::convenience::Action::Fan);}
        }
        return LRESULT(0);
    }
    let ptr = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut HitData;
    if ptr.is_null() {
        return DefWindowProcW(h, m, w, l);
    }
    if m==WM_TIMER&&w.0==CTRL_TIMER {
        if (*ptr).ignore.get(){set_input_style(h,true,ctrl_down());}
        return LRESULT(0);
    }
    if matches!(m,WM_LBUTTONDOWN|WM_POINTERDOWN)&&(*ptr).ignore.get()&&!ctrl_down(){
        // Ctrl may have been released between low-frequency polls. Restore
        // transparency immediately so the next click reaches the window below.
        set_input_style(h,true,false);return LRESULT(0);
    }
    if matches!(m, WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP) {
        let id = (w.0 & 0xffff) as u32;
        let mut info = POINTER_INFO::default();
        if GetPointerInfo(id, &mut info).is_err() {
            return LRESULT(0);
        }
        if info.pointerType == PT_MOUSE {
            return DefWindowProcW(h, m, w, l);
        }
        let kind = if info.pointerType == PT_PEN {
            Kind::Pen
        } else {
            Kind::Touch
        };
        let p = Point::new(info.ptPixelLocation.x as f32, info.ptPixelLocation.y as f32);
        if info.pointerFlags.0 & POINTER_FLAG_CANCELED.0 != 0 {
            if (*ptr).active.is_some_and(|a| a.0 == id) {
                (*ptr).active = None;
                super::input(Input::Cancel { id });
            }
            return LRESULT(0);
        }
        // WM_POINTERDOWN implicitly captures the pointer until UP/cancel.
        // https://learn.microsoft.com/windows/win32/inputmsg/wm-pointerdown
        match m {
            WM_POINTERDOWN if (*ptr).active.is_none() && hit_screen(h, &*ptr, p) => {
                (*ptr).active = Some((id, kind));
                super::input(Input::Down { id, kind, point: p });
            }
            WM_POINTERUPDATE if (*ptr).active.is_some_and(|a| a.0 == id) => {
                super::input(Input::Move { id, point: p })
            }
            WM_POINTERUP if (*ptr).active.is_some_and(|a| a.0 == id) => {
                (*ptr).active = None;
                super::input(Input::Up { id, point: p });
            }
            _ => {}
        }
        return LRESULT(0);
    }
    if matches!(m, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE)
        && promoted_mouse(GetMessageExtraInfo().0 as usize)
    {
        return LRESULT(0);
    }
    match m {
        WM_MOUSEWHEEL if w.0&0x0008!=0 => {
            (*ptr).wheel+=((w.0>>16)&0xffff) as i16 as i32;
            let steps=(*ptr).wheel/120;(*ptr).wheel%=120;
            if steps!=0 {super::input(Input::Wheel{steps});}
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if (*ptr).active == Some((0, Kind::Mouse)) {
                super::input(Input::Move {
                    id: 0,
                    point: client_point(h, l),
                });
            } else {
                use windows::Win32::UI::Input::KeyboardAndMouse::{
                    TrackMouseEvent, TME_HOVER, TME_LEAVE, TRACKMOUSEEVENT,
                };
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_HOVER | TME_LEAVE,
                    hwndTrack: h,
                    dwHoverTime: 600,
                };
                let _ = TrackMouseEvent(&mut track);
            }
            LRESULT(0)
        }
        0x02A1 => {
            if (*ptr).active.is_none() {
                super::show_tooltip(h);
            }
            LRESULT(0)
        }
        0x02A3 => {
            crate::topbar::hide_tip();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let p = client_point(h, l);
            if (*ptr).active.is_none() && hit_screen(h, &*ptr, p) {
                (*ptr).active = Some((0, Kind::Mouse));
                crate::topbar::hide_tip();
                deliver_mouse_down(
                    || { SetCapture(h); },
                    || super::input(Input::Down {id:0,kind:Kind::Mouse,point:p}),
                    || { let current=GetWindowLongPtrW(h,GWLP_USERDATA) as *const HitData; !current.is_null()&&(*current).active.is_some() },
                    || { let _=ReleaseCapture(); },
                );
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if (*ptr).active == Some((0, Kind::Mouse)) {
                (*ptr).active = None;
                super::input(Input::Up {
                    id: 0,
                    point: client_point(h, l),
                });
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_POINTERCAPTURECHANGED if (*ptr).active.is_none_or(|a| a.0 != (w.0 & 0xffff) as u32) => {
            LRESULT(0)
        }
        WM_CAPTURECHANGED if (*ptr).active.is_some_and(|a| a.1 != Kind::Mouse) => LRESULT(0),
        WM_CANCELMODE => {
            let active=(*ptr).active.take();
            finish_cancel(active, || {let _=ReleaseCapture();}, super::input);
            LRESULT(0)
        }
        WM_CAPTURECHANGED | WM_POINTERCAPTURECHANGED => {
            if let Some((id, _)) = (*ptr).active.take() {
                super::input(Input::Cancel { id });
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(h, m, w, l),
    }
}
pub struct Surface {
    pub window: Window,
    pub raster: Raster,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn companion_capture_registration_is_scoped_to_its_window() {
        let mut window = unsafe { Window::new(1, 1, true).unwrap() };
        let hwnd = window.hwnd;
        window.follow_capture_setting();
        let registered = crate::desklayer::privacy::registered(hwnd);
        drop(window);
        assert!(registered);
        assert!(!crate::desklayer::privacy::registered(hwnd));
    }

    #[test] fn cover_fades_restore_only_surfaces_that_were_visible() {
        for alpha in [1.,0.5,0.,0.5,1.] {assert_eq!(fade_plan(false,false,alpha),None);}
        let (mut shown,mut faded)=(true,false);
        for alpha in [1.,0.5,0.,0.,0.5,1.] {
            (shown,faded)=fade_plan(shown,faded,alpha).unwrap();
            assert_eq!(shown,alpha>0.);
        }
        assert!(!faded);
        // An effect ending normally calls hide(), clearing both flags.
        assert_eq!(fade_plan(false,false,0.5),None);
    }
    #[test] fn a_popup_dismissal_release_cannot_open_a_companion_menu() {
        let armed=Cell::new(false);
        assert!(!right_click(&armed,WM_RBUTTONUP,true));
        assert!(!right_click(&armed,WM_RBUTTONDOWN,false));
        assert!(!right_click(&armed,WM_RBUTTONUP,true));
        assert!(!right_click(&armed,WM_RBUTTONDOWN,true));
        assert!(right_click(&armed,WM_RBUTTONUP,true));
        assert!(!right_click(&armed,WM_RBUTTONUP,true));
        right_click(&armed,WM_RBUTTONDOWN,true);
        assert!(!right_click(&armed,WM_RBUTTONUP,false));
    }
    #[test] fn z_order_only_changes_on_show_or_resize_and_never_while_yielding() {
        let menu=HWND(123 as *mut _);
        assert_eq!(z_after(false,false,false,None),Some(HWND_NOTOPMOST));
        assert_eq!(z_after(false,false,false,Some(menu)),Some(menu));
        assert_eq!(z_after(true,true,false,Some(menu)),Some(menu));
        assert_eq!(z_after(true,false,false,Some(menu)),None);
        for shown in [false,true] {for resized in [false,true] {
            assert_eq!(z_after(shown,resized,true,Some(menu)),None);
        }}
    }
    #[test] fn down_captures_before_render_and_releases_after_stop_or_failure(){
        use std::cell::RefCell;
        for stopped in [false,true] {
            let active=Cell::new(Some((0,Kind::Mouse)));let events=RefCell::new(Vec::new());
            deliver_mouse_down(||events.borrow_mut().push("capture"),||{
                events.borrow_mut().push("render");
                // Both paused draw and failed draw call stop/cancel_input here.
                if stopped {active.set(None);}
            },||active.get().is_some(),||events.borrow_mut().push("release"));
            assert_eq!(*events.borrow(),if stopped{vec!["capture","render","release"]}else{vec!["capture","render"]});
            assert_eq!(active.get().is_none(),stopped);
        }
    }
    #[test] fn ignore_mouse_toggles_only_transparency_and_ctrl_temporarily_restores_grabbing(){
        let base=(WS_EX_NOACTIVATE|WS_EX_LAYERED|WS_EX_TOOLWINDOW).0 as isize;
        let ignored=input_style(base,true,false);assert_ne!(ignored&WS_EX_TRANSPARENT.0 as isize,0);
        assert_eq!(input_style(ignored,true,true),base);assert_eq!(input_style(ignored,false,false),base);
    }
    #[test]
    fn review_cancel_releases_mouse_once_and_delivers_fall() {
        use std::cell::RefCell;
        for kind in [Kind::Mouse,Kind::Touch,Kind::Pen] {
            let mut active=Some((9,kind));
            let events=RefCell::new(Vec::new());
            finish_cancel(active.take(),||events.borrow_mut().push("release"),|event|{
                assert!(matches!(event,Input::Cancel{id:9}));events.borrow_mut().push("cancel");
            });
            assert!(active.is_none());
            finish_cancel(active.take(),||panic!("double release"),|_|panic!("double cancel"));
            assert_eq!(*events.borrow(),if kind==Kind::Mouse{vec!["release","cancel"]}else{vec!["cancel"]});
        }
    }
    #[test]
    fn touch_never_promotes_a_context_menu_or_second_click() {
        unsafe {
            for msg in [WM_CONTEXTMENU, WM_RBUTTONDOWN, WM_RBUTTONUP] {
                assert_eq!(proc(HWND::default(), msg, WPARAM(0), LPARAM(0)), LRESULT(0));
            }
            assert_eq!(
                proc(HWND::default(), WM_POINTERACTIVATE, WPARAM(0), LPARAM(0)),
                LRESULT(PA_NOACTIVATE as isize)
            );
            assert_ne!(
                proc(HWND::default(), TABLET_GESTURES, WPARAM(0), LPARAM(0)).0 & 1,
                0
            );
        }
        assert!(promoted_mouse(0xff515780));
        assert!(!promoted_mouse(0));
    }
    #[test]
    fn only_body_alpha_accepts_pointer_down() {
        let mut px = vec![0u8; 16];
        px[7] = 255;
        let data = HitData {
            bits: px.as_mut_ptr(),
            w: 2,
            h: 2,
            active: None,
            wheel: 0,
            ignore: Cell::new(false),
            ctrl_timer: Cell::new(false),
            right_armed: Cell::new(false),
        };
        unsafe {
            assert!(!data.hit(0, 0));
            assert!(data.hit(1, 0));
            assert!(!data.hit(-1, 0));
            assert!(!data.hit(2, 0));
        }
    }
    #[test]
    fn frame_updates_preserve_z_order_and_never_activate() {
        assert_eq!(presentation_flags(false).0 & SWP_NOZORDER.0, 0);
        assert_ne!(presentation_flags(true).0 & SWP_NOZORDER.0, 0);
        for shown in [false, true] {
            assert_ne!(presentation_flags(shown).0 & SWP_NOACTIVATE.0, 0);
        }
    }
}
impl Surface {
    pub unsafe fn resize(&mut self, w: u32, h: u32) -> Result<()> {
        self.raster.resize(w,h)?;
        self.window.resize(w, h)?;
        Ok(())
    }
    pub unsafe fn new(gfx: &crate::gfx::Gfx, w: u32, h: u32, shadow: bool) -> Result<Self> {
        // Build graphics before creating any window. A failed device cannot
        // leave an empty topmost window behind.
        let raster = Raster::new(gfx.offscreen_context()?, w, h)?;
        let mut window = Window::new(w, h, shadow)?;
        window.follow_capture_setting();
        Ok(Self { window, raster })
    }
}

#[cfg(test)] mod polish_tests {
 use super::*;
 #[test] fn large_silhouette_does_not_claim_empty_bounding_box() {
  let w=80;let mut pixels=vec![0;w*w*4];
  for y in 10..60 {for x in 10..15 {pixels[(y*w+x)*4+3]=255;}}
  for x in 10..60 {pixels[(10*w+x)*4+3]=255;}
  let before=pixels.clone();pad_input(&mut pixels,w as u32,w as u32,1.);assert_eq!(pixels,before);
  assert_eq!(pixels[(40*w+40)*4+3],0);
 }
 #[test] fn small_target_is_shared_by_press_grab_and_pet_at_every_dpi() {
  for dpi in [1.,1.5,2.] {for size in [0.28,0.34,0.4] {
   let w=160;let mut pixels=vec![0;w*w*4];let side=(40.*size*dpi) as usize;
   for y in 70..70+side {for x in 70..70+side {pixels[(y*w+x)*4..][..4].copy_from_slice(&[20,30,40,255]);}}
   let rect=pad_input(&mut pixels,w as u32,w as u32,dpi).unwrap();
   assert!((rect.right-rect.left) as f32>=28.*dpi);assert!((rect.bottom-rect.top) as f32>=28.*dpi);
   let hit=HitData{bits:pixels.as_mut_ptr(),w:w as u32,h:w as u32,active:None,wheel:0,ignore:Cell::new(false),ctrl_timer:Cell::new(false),right_armed:Cell::new(false)};
   unsafe {assert!(hit.hit(rect.left,rect.top));assert!(hit.hit(70,70));assert!(!hit.hit(rect.left-1,rect.top));}
   assert_eq!(&pixels[(70*w+70)*4..][..4],&[20,30,40,255]);
   assert_eq!(pixels[((rect.top as usize*w)+rect.left as usize)*4+3],1);
  }}
  assert!(pad_input(&mut vec![0;64*64*4],64,64,1.).is_none());
 }
}
