//! Custom layered input/answer bubbles. No child controls or input hooks.
use crate::desklayer::{backdrop, card as glass};
use super::{bubbles::{Bubbles,Phase},history::History,history_view,input::Editor,paint::{self,Input,Item},state::{Conversation,Key},transport::{self,Update}};
use super::super::{render,window};
use std::{cell::{Cell,RefCell},rc::Rc,sync::mpsc,time::Instant};
use windows::{core::{w,Result},Win32::{Foundation::*,Graphics::{Direct2D::Common::D2D_RECT_F,Gdi::*},UI::{Input::{Ime::*,KeyboardAndMouse::*},WindowsAndMessaging::*}}};
#[derive(Clone,Copy)]
struct Anchor {work:RECT,dpi:f32,tip:(f32,f32),foot:(f32,f32),scale:f32,edge:Option<super::super::peek::Edge>}
thread_local! {
    static ENGINE:RefCell<Option<Engine>>=const{RefCell::new(None)};
    static ANCHOR:Cell<Option<Anchor>>=const{Cell::new(None)};
    static ACTIVE:Cell<bool>=const{Cell::new(false)};
}
struct Overlay {surface:window::Surface,size:(u32,u32),origin:(f32,f32),timer:Option<u32>}
impl Drop for Overlay {
    fn drop(&mut self) {
        backdrop::unlisten(self.surface.window.hwnd);
    }
}
impl Overlay {
    unsafe fn arm(&mut self,interval:Option<u32>) {
        if self.timer==interval{return;}
        if let Some(ms)=interval {SetTimer(Some(self.surface.window.hwnd),1,ms,None);}else{let _=KillTimer(Some(self.surface.window.hwnd),1);}
        self.timer=interval;
    }
}
struct Engine {
    gfx:Rc<crate::gfx::Gfx>,bubbles:Bubbles,editor:Editor,history_editor:Editor,chat:Conversation,history:History,
    keep:bool,account:Option<String>,overlay:Option<Overlay>,history_view:Option<history_view::View>,items:Vec<Item>,
    rx:mpsc::Receiver<Update>,tx:mpsc::Sender<Update>,cancel:Option<tokio::sync::oneshot::Sender<()>>,
    started:Instant,motion:bool,previous:HWND,focused:Option<HWND>,allowed:bool,
    say_queue:super::say_queue::SayQueue,
    placement:super::super::panels::ChatPlacement,
    panels_hidden:bool,
}
impl Engine {
    fn new(gfx:Rc<crate::gfx::Gfx>)->Self {
        let cfg=crate::config::Config::load();let keep=cfg.chat.keep_history;
        let history=if keep{History::read(&super::history::path())}else{History::default()};
        Self::build(gfx,keep,history,super::active_key(),cfg.companion.animations&&crate::effects::motion())
    }
    fn build(gfx:Rc<crate::gfx::Gfx>,keep:bool,history:History,account:Option<String>,motion:bool)->Self {
        let(tx,rx)=mpsc::channel();
        // Saved history is an archive; provider context stays isolated to this session/account.
        let chat=Conversation::default();
        Self{gfx,bubbles:Bubbles::default(),editor:Editor::default(),history_editor:Editor::default(),chat,account,history,keep,overlay:None,history_view:None,items:Vec::new(),rx,tx,cancel:None,started:Instant::now(),motion,previous:HWND::default(),focused:None,allowed:true,say_queue:Default::default(),placement:Default::default(),panels_hidden:false}
    }
    fn now(&self)->f64 {self.started.elapsed().as_secs_f64()}
    fn save(&self) {let _=self.history.save(&super::history::path(),self.keep);}
    fn sync_account(&mut self) {
        self.change_account(super::active_key());
    }
    fn change_account(&mut self,account:Option<String>) {
        if self.account!=account {
            self.cancel();self.chat.clear();if !self.bubbles.agent{self.bubbles.cancel();}self.account=account;
        }
    }
    fn editor_mut(&mut self,history:bool)->&mut Editor {if history{&mut self.history_editor}else{&mut self.editor}}
    fn history_focused(&self)->bool {self.history_view.as_ref().is_some_and(|v|self.focused==Some(v.hwnd))}
    fn cancel(&mut self) {if let Some(c)=self.cancel.take(){let _=c.send(());}self.chat.cancel();self.panels_hidden=false;}
    fn close_panels(&mut self) {
        self.panels_hidden=self.chat.pending.is_some();
        if !self.panels_hidden {self.bubbles.cancel();}
        self.history_view=None;
        self.items.clear();
        self.placement.reset();
        // Keep the hidden overlay's timer: it drains provider updates.
        if let Some(o)=&self.overlay {o.surface.window.hide();}
    }
    fn resume_input(&mut self) {
        let reopen=self.panels_hidden || self.bubbles.phase!=Phase::Input;
        self.panels_hidden=false;
        if reopen {self.placement.reset();}
        if self.chat.pending.is_none() && self.bubbles.phase!=Phase::Input {self.bubbles.input(self.now());}
    }
    fn active(&self)->bool {self.bubbles.phase==Phase::Input || self.chat.pending.is_some() || self.history_focused() || self.bubbles.phase==Phase::Reply && !self.bubbles.agent}
    unsafe fn overlay(&mut self)->Result<()> {
        if let Some(o)=&mut self.overlay {o.arm(Some(33));return Ok(());}
        let mut surface=window::Surface {raster:render::Raster::new(self.gfx.offscreen_context()?,1,1)?,window:window::Window::with_proc(1,1,false,w!("NopBarCompanionChat"),Some(proc))?};
        surface.window.follow_capture_setting();
        let hwnd=surface.window.hwnd;backdrop::listen(hwnd);super::session::subscribe(hwnd);SetTimer(Some(hwnd),1,33,None);
        self.overlay=Some(Overlay{surface,size:(1,1),origin:(0.,0.),timer:Some(33)});Ok(())
    }
    unsafe fn focus(&mut self,h:HWND) {
        if self.focused.is_none(){let previous=GetForegroundWindow();if previous!=h{self.previous=previous;}}self.focused=Some(h);
        let ex=GetWindowLongPtrW(h,GWL_EXSTYLE);SetWindowLongPtrW(h,GWL_EXSTYLE,ex&!(WS_EX_NOACTIVATE.0 as isize));
        let _=SetForegroundWindow(h);let _=SetFocus(Some(h));
    }
    unsafe fn restore(&mut self) {
        if let Some(h)=self.focused.take(){
            if self.overlay.as_ref().is_some_and(|o|o.surface.window.hwnd==h) {let ex=GetWindowLongPtrW(h,GWL_EXSTYLE);SetWindowLongPtrW(h,GWL_EXSTYLE,ex|WS_EX_NOACTIVATE.0 as isize);}
            if GetForegroundWindow()==h && IsWindow(Some(self.previous)).as_bool(){let _=SetForegroundWindow(self.previous);}
        }
    }
    unsafe fn open(&mut self)->Result<()> {
        self.sync_account();self.overlay()?;
        self.resume_input();
        self.draw()?;let h=self.overlay.as_ref().unwrap().surface.window.hwnd;self.focus(h);ACTIVE.with(|a|a.set(self.active()));Ok(())
    }
    unsafe fn send(&mut self,history:bool) {
        if self.chat.pending.is_some() || !super::connected(){return;}
        self.sync_account();let text=self.editor_mut(history).field.text();if !self.chat.begin(text.clone()){return;}
        self.bubbles.submit(text,self.now());*self.editor_mut(history)=Editor::default();let cfg=crate::config::Config::load();
        self.cancel=Some(transport::start(self.chat.generation,cfg.chat.provider,cfg.chat.active_model().to_string(),self.chat.input(),cfg.chat.daily_limit,self.tx.clone()));
        if !history{self.restore();}
    }
    fn receive(&mut self) {
        while let Ok(u)=self.rx.try_recv(){match u {
            Update::Delta(g,s) if g==self.chat.generation && self.chat.pending.is_some()=>{
                let before=self.chat.answer.len();self.chat.delta(g,&s);if self.chat.answer.len()>before {self.bubbles.delta(&s,self.now());}
            }
            Update::Finished(g,e) if g==self.chat.generation && self.chat.pending.is_some()=>{
                let user=self.chat.pending.clone().unwrap();self.chat.finish(g,e);self.cancel=None;
                self.panels_hidden=false;
                if let Some(e)=e {self.bubbles.delta(e,self.now());}
                let answer=self.bubbles.current.as_ref().map_or_else(String::new,|m|m.text.clone());
                self.history.push(user,answer);
                self.history.link_last(self.bubbles.previous.front().map(|m|m.id),self.bubbles.current.as_ref().map(|m|m.id));self.save();
                self.bubbles.finish(self.now());
            }
            _=>{}
        }}
    }
    fn drain_say(&mut self) {
        let ready=self.allowed && !self.active() && self.bubbles.phase==Phase::Hidden;
        if let Some(text)=self.say_queue.take(ready) {
            self.placement.reset();self.bubbles.say(text.clone(),self.now());
            self.history.push(String::new(),text);self.history.link_last(None,self.bubbles.current.as_ref().map(|m|m.id));self.save();
        }
    }
    unsafe fn draw(&mut self)->Result<()> {
        if self.bubbles.phase==Phase::Hidden {self.placement.reset();}
        let quiet = super::super::notifications_quiet() && !self.active();
        if !quiet { self.drain_say(); }
        ACTIVE.with(|a|a.set(self.active()));let Some(a)=ANCHOR.with(Cell::get)else{return Ok(());};let now=self.now();
        let display=self.editor.displayed();let input=Input{text:&display,caret:self.editor.caret(),selection:self.editor.selection(),composition:self.editor.comp_range()};
        let work=paint::rect(a.work.left as f32/a.dpi,a.work.top as f32/a.dpi,(a.work.right-a.work.left) as f32/a.dpi,(a.work.bottom-a.work.top) as f32/a.dpi);
        self.items=paint::arrange(&self.gfx,&self.bubbles,&input,a.tip,work,now,self.motion,a.dpi)?;
        paint::place_edge(&mut self.items,a.tip,work,a.edge);
        self.placement.apply(&mut self.items,a.work,a.dpi,self.bubbles.phase==Phase::Input,false);
        let spinner_foot=paint::spinner_foot(a.foot,a.tip,a.scale,work,a.edge);
        if let Some(o)=&mut self.overlay {
            let thinking=self.bubbles.phase==Phase::Thinking;
            if quiet || self.panels_hidden || (self.items.is_empty() && !thinking) || !self.allowed {o.surface.window.hide();}
            else {
                o.surface.window.front(true);
                let mut bounds=self.items.iter().fold(None,|r:Option<D2D_RECT_F>,i|Some(match r{None=>i.bounds,Some(r)=>D2D_RECT_F{left:r.left.min(i.bounds.left),top:r.top.min(i.bounds.top),right:r.right.max(i.bounds.right),bottom:r.bottom.max(i.bounds.bottom)}})).unwrap_or(paint::rect(a.tip.0,a.tip.1,1.,1.));
                if thinking {let(p,r,d)=super::bubbles::spinner(spinner_foot,a.scale);bounds.left=bounds.left.min(p.0-r-d);bounds.right=bounds.right.max(p.0+r+d);bounds.top=bounds.top.min(p.1-r-d);bounds.bottom=bounds.bottom.max(p.1+r+d);}
                let x=((bounds.left-4.)*a.dpi).floor() as i32;let y=((bounds.top-4.)*a.dpi).floor() as i32;
                let size=((((bounds.right+4.)*a.dpi).ceil() as i32-x).max(1) as u32,(((bounds.bottom+4.)*a.dpi).ceil() as i32-y).max(1) as u32);
                if size!=o.size {o.surface.resize(size.0,size.1)?;o.size=size;}
                o.origin=(x as f32/a.dpi,y as f32/a.dpi);
                let requested = crate::theme::Theme {is_light: crate::syslook::current().light, ..crate::theme::Theme::graphite()};
                let under = backdrop::under_for(o.surface.window.hwnd, RECT {left:x,top:y,right:x+size.0 as i32,bottom:y+size.1 as i32});
                let theme = glass::theme_for(&requested, under.as_ref());
                let caret_visible=blink(now)&&self.focused==Some(o.surface.window.hwnd);
                o.surface.raster.custom(a.dpi,o.surface.window.pixels_mut(),|dc|{
                    dc.SetTransform(&(render::translate(-o.origin.0,-o.origin.1)*render::scale(a.dpi,a.dpi)));
                    for item in &self.items {paint::item(dc,item,(item.depth==0 && self.bubbles.phase==Phase::Input).then_some(&input),caret_visible,theme)?;}
                    if thinking {paint::spinner(dc,spinner_foot,a.scale,now-self.bubbles.entered,self.motion,theme)?;}Ok(())
                })?;o.surface.window.present(x,y,true)?;
                if self.bubbles.phase==Phase::Input && self.focused==Some(o.surface.window.hwnd) {
                    if let Some(it)=self.items.first(){let(x,y)=it.text.caret(input.caret);super::input::place_ime(o.surface.window.hwnd,it.bounds.left+super::bubbles::PAD_X+x-o.origin.0,it.bounds.top+super::bubbles::PAD_Y+y-o.origin.1,a.dpi);}
                }
            }
        }
        if let Some(h)=&mut self.history_view {
            h.display_changed(a.work,a.dpi)?;
            h.motion=self.motion;
            h.pending_ids=(self.bubbles.previous.front().map(|m|m.id),self.bubbles.current.as_ref().map(|m|m.id));
            let text=self.history_editor.displayed();let input=Input{text:&text,caret:self.history_editor.caret(),selection:self.history_editor.selection(),composition:self.history_editor.comp_range()};
            let focused=self.focused==Some(h.hwnd);h.draw(&self.history,self.chat.pending.as_deref().map(|u|(u,self.chat.answer.as_str())),self.account.is_some().then_some(&input),blink(now)&&focused,focused)?;
            if let Some(field) = h.input.as_ref().filter(|_| self.focused == Some(h.hwnd)) {
                let (x,y)=field.text.caret(input.caret);
                super::input::place_ime(h.hwnd,history_view::MARGIN+field.bounds.left+super::bubbles::PAD_X+x,history_view::MARGIN+field.bounds.top+super::bubbles::PAD_Y+y,h.dpi);
            }
        }
        if let Some(o)=&mut self.overlay {
            if (self.panels_hidden || self.bubbles.phase==Phase::Hidden) && self.history_view.is_none() && self.chat.pending.is_none(){o.arm(None);}
            else {let interval=if self.motion && (self.bubbles.scrolling(now) || self.history_view.as_ref().is_some_and(|h|h.scrolling())) {16}else if self.chat.pending.is_some() || (self.motion && (now-self.bubbles.entered<0.25 || self.bubbles.finished.is_some())) {33}else{250};o.arm(Some(interval));}
        }
        Ok(())
    }
    unsafe fn history(&mut self)->Result<()> {
        self.panels_hidden=false;
        self.sync_account();self.overlay()?;
        if self.history_view.is_none(){
            let Some(a)=ANCHOR.with(Cell::get)else{return Ok(());};
            let theme=crate::dock::companion_theme().unwrap_or_else(crate::theme::Theme::graphite);
            self.history_view=Some(history_view::View::new(self.gfx.clone(),theme,a.dpi,a.work,Some(proc))?);
        }
        let h=self.history_view.as_ref().unwrap().hwnd;self.draw()?;let _=ShowWindow(h,SW_SHOWNORMAL);self.focus(h);ACTIVE.with(|a|a.set(self.active()));Ok(())
    }
}

fn blink(now:f64)->bool {((now*2.) as u64).is_multiple_of(2)}
fn with_engine(f:impl FnOnce(&mut Engine)) {
    ENGINE.with(|e|{let Ok(mut e)=e.try_borrow_mut()else{return;};if e.is_none(){if let Some(gfx)=crate::gfx::shared(){*e=Some(Engine::new(gfx));}}
        if let Some(e)=e.as_mut(){f(e);}});
}
fn fallback_anchor() {
    if ANCHOR.with(Cell::get).is_some(){return;}
    if let Some(m)=crate::monitors::primary(){let dpi=m.scale();ANCHOR.with(|a|a.set(Some(Anchor{work:m.work,dpi,tip:((m.work.left+m.work.right) as f32/(2.*dpi),m.work.bottom as f32/dpi-130.),foot:((m.work.left+m.work.right) as f32/(2.*dpi),m.work.bottom as f32/dpi),scale:0.28,edge:None})));}
}
pub fn is_open()->bool {ACTIVE.with(Cell::get)}
pub fn above_sprite(sprite:HWND) {
    if super::super::ui_paused(){return;}
    ENGINE.with(|cell|{if let Ok(e)=cell.try_borrow(){if let Some(e)=e.as_ref().filter(|e|e.bubbles.phase==Phase::Thinking){if let Some(o)=&e.overlay {unsafe {
        let h=o.surface.window.hwnd;
        if IsWindowVisible(h).as_bool() && GetWindowLongPtrW(sprite,GWL_EXSTYLE)&WS_EX_TOPMOST.0 as isize!=0 {
            let before=GetWindow(sprite,GW_HWNDPREV).unwrap_or(HWND_TOPMOST);
            if before!=h {let _=SetWindowPos(h,Some(before),0,0,0,0,SWP_NOMOVE|SWP_NOSIZE|SWP_NOACTIVATE);}
        }
    }}}}});
}
pub fn open() {super::recheck_on_click();if !super::connected(){return;}fallback_anchor();with_engine(|e|unsafe{let _=e.open();});super::super::refresh();}
pub fn open_history() {fallback_anchor();with_engine(|e|unsafe{let _=e.history();});super::super::refresh();}
pub fn say(text: String) {
    fallback_anchor();
    with_engine(|e| unsafe {
        e.say_queue.push(text);
        if e.overlay().is_ok() { let _=e.draw(); }
    });
}
pub fn shutdown() {
    ENGINE.with(|e|{if let Ok(mut e)=e.try_borrow_mut(){if let Some(e)=e.as_mut(){unsafe{e.restore();}e.cancel();e.bubbles.cancel();e.history_view=None;e.overlay=None;}}});ACTIVE.with(|a|a.set(false));
}
/// Monitor relocation closes only presentation, preserving transport and drafts.
pub fn close_panels() {
    ENGINE.with(|e| {if let Ok(mut e)=e.try_borrow_mut(){if let Some(e)=e.as_mut(){unsafe{e.restore();}e.close_panels();}}});
    ACTIVE.with(|a|a.set(false));
}
pub(super) fn configure(keep:bool) {
    #[cfg(not(test))]
    ENGINE.with(|e|{if let Ok(mut e)=e.try_borrow_mut(){if let Some(e)=e.as_mut(){if e.keep!=keep{e.keep=keep;e.save();}}else {let path=super::history::path();let _=History::read(&path).save(&path,keep);}}});
    #[cfg(test)] let _=keep;
}
pub fn frame()->Option<String> {
    ENGINE.with(|e|{let e=e.try_borrow().ok()?;let e=e.as_ref()?;let phase=e.bubbles.phase;
        if phase==Phase::Reply && e.bubbles.agent || phase==Phase::Hidden && !e.history_focused(){return None;}
        let error=phase==Phase::Reply && e.chat.error.is_some() && e.bubbles.finished.is_some_and(|at|e.now()-at<super::super::clips::catalog().clip("error").duration() as f64);
        let clip=if error{"error"}else if phase==Phase::Thinking{"think"}else if phase==Phase::Reply{"wait"}else{return Some("look_4".into());};
        let elapsed=if e.motion{(e.now()-e.bubbles.entered) as f32}else{0.};Some(super::super::clips::catalog().clip(clip).frame(elapsed).to_string())
    })
}
pub fn anchor(work:RECT,dpi:f32,hat:(i32,i32),foot:(f32,f32),scale:f32,motion:bool,edge:Option<super::super::peek::Edge>) {
    ANCHOR.with(|a|a.set(Some(Anchor{work,dpi,tip:(hat.0 as f32/dpi,hat.1 as f32/dpi),foot,scale,edge})));
    ENGINE.with(|e|{if let Ok(mut e)=e.try_borrow_mut(){if let Some(e)=e.as_mut(){e.motion=motion;unsafe{let _=e.draw();}}}});
}
unsafe extern "system" fn proc(h:HWND,m:u32,w:WPARAM,l:LPARAM)->LRESULT {
    const FOCUS_CHANGED:u32=WM_APP+19;
    if matches!(m,WM_NCCREATE|WM_CREATE|WM_NCDESTROY|WM_DESTROY){return DefWindowProcW(h,m,w,l);}
    // These messages also arrive synchronously inside SetFocus/ShowWindow.
    if m==WM_IME_SETCONTEXT{return DefWindowProcW(h,m,w,LPARAM(l.0&!(ISC_SHOWUICOMPOSITIONWINDOW as isize)));}
    if m==WM_ACTIVATE {let _=PostMessageW(Some(h),FOCUS_CHANGED,w,l);return DefWindowProcW(h,m,w,l);}
    if m==WM_LBUTTONDOWN {
        let composing=ENGINE.with(|cell|cell.try_borrow().ok().and_then(|e|e.as_ref().map(|e|if e.history_view.as_ref().is_some_and(|v|v.hwnd==h){e.history_editor.composing}else{e.editor.composing})).unwrap_or(false));
        // Complete before borrowing Engine: IMM sends composition messages synchronously.
        if composing {let ctx=ImmGetContext(h);if !ctx.is_invalid(){let _=ImmNotifyIME(ctx,NI_COMPOSITIONSTR,CPS_COMPLETE,0);let _=ImmReleaseContext(h,ctx);}}
    }
    if m==WM_ERASEBKGND{return LRESULT(1);}let mut handled=false;
    let result=ENGINE.with(|cell|{
        let Ok(mut engine)=cell.try_borrow_mut()else{return None;};let e=engine.as_mut()?;
        let is_history=e.history_view.as_ref().is_some_and(|v|v.hwnd==h);let is_overlay=e.overlay.as_ref().is_some_and(|o|o.surface.window.hwnd==h);
        if !is_history && !is_overlay{return None;}let editing=e.account.is_some() && (is_history || e.bubbles.phase==Phase::Input);
        if m==WM_MOUSEACTIVATE{return Some(LRESULT(if editing{MA_ACTIVATE}else{MA_NOACTIVATE} as isize));}
        if m==WM_NCHITTEST && is_overlay {
            let dpi=ANCHOR.with(Cell::get)?.dpi;
            let x=(l.0&0xffff) as i16 as f32/dpi;let y=((l.0>>16)&0xffff) as i16 as f32/dpi;
            return Some(LRESULT(if e.items.iter().any(|i|paint::hit_item(i,x,y)){HTCLIENT as isize}else{HTTRANSPARENT as isize}));
        }
        if m==WM_NCHITTEST && is_history {
            let mut p=POINT{x:(l.0&0xffff) as i16 as i32,y:((l.0>>16)&0xffff) as i16 as i32};let _=ScreenToClient(h,&mut p);
            let dpi=e.history_view.as_ref().unwrap().dpi;let x=p.x as f32/dpi-history_view::MARGIN;let y=p.y as f32/dpi-history_view::MARGIN;
            return Some(LRESULT(if !(0. ..history_view::WIDTH).contains(&x)||!(0. ..history_view::HEIGHT).contains(&y){HTTRANSPARENT as isize}else if y<56. && x<280.{HTCAPTION as isize}else{HTCLIENT as isize}));
        }
        match m {
            FOCUS_CHANGED=>{
                if GetForegroundWindow()==h {
                    let previous=HWND(l.0 as _);
                    let ours=e.overlay.as_ref().is_some_and(|o|o.surface.window.hwnd==previous)||e.history_view.as_ref().is_some_and(|v|v.hwnd==previous);
                    if w.0&0xffff!=WA_INACTIVE as usize && !ours && IsWindow(Some(previous)).as_bool(){e.previous=previous;}
                    e.focused=Some(h);
                }
                else if e.focused==Some(h) {
                    e.focused=None;
                    if is_overlay {let ex=GetWindowLongPtrW(h,GWL_EXSTYLE);SetWindowLongPtrW(h,GWL_EXSTYLE,ex|WS_EX_NOACTIVATE.0 as isize);if e.bubbles.phase==Phase::Input {e.bubbles.cancel();}}
                }
            }
            WM_SETCURSOR if editing=>{
                let mut in_input=is_overlay;
                if let Some(v)=e.history_view.as_ref().filter(|_|is_history) {let mut p=POINT::default();let _=GetCursorPos(&mut p);let _=ScreenToClient(h,&mut p);in_input=v.input.as_ref().is_some_and(|input|paint::contains(input.bounds,p.x as f32/v.dpi-history_view::MARGIN,p.y as f32/v.dpi-history_view::MARGIN));}
                if in_input {SetCursor(Some(LoadCursorW(None,IDC_IBEAM).unwrap_or_default()));return Some(LRESULT(1));}
                return None;
            }
            WM_TIMER=>{
                e.sync_account();e.receive();e.allowed=crate::dock::companion_mode()==super::super::visibility::Mode::Run && super::super::visible() && !super::super::living::away();
                let mut p=POINT::default();let _=GetCursorPos(&mut p);
                if let Some(a)=ANCHOR.with(Cell::get){let inside=e.items.iter().any(|i|paint::contains(i.bounds,p.x as f32/a.dpi,p.y as f32/a.dpi));e.bubbles.hover(inside,e.now());}e.bubbles.tick(e.now(),e.motion);
            }
            backdrop::WM_BACKDROP if is_overlay=>{},
            super::session::CHANGED=>e.sync_account(),
            WM_CLOSE=>{e.restore();if is_history{e.history_view=None;}else{e.cancel();e.bubbles.cancel();}}
            WM_DPICHANGED if is_history=>{
                if let Some(v)=&mut e.history_view {
                    let _=v.set_dpi((w.0&0xffff) as f32/96.);
                    let r=&*(l.0 as *const RECT);let _=SetWindowPos(h,None,r.left,r.top,r.right-r.left,r.bottom-r.top,SWP_NOZORDER|SWP_NOACTIVATE);
                }
            }
            WM_IME_STARTCOMPOSITION if editing=>e.editor_mut(is_history).composing=true,
            WM_IME_COMPOSITION if editing=>e.editor_mut(is_history).compose(h,l.0 as u32),
            WM_IME_ENDCOMPOSITION if editing=>{let ed=e.editor_mut(is_history);ed.composing=false;ed.composition.clear();}
            WM_IME_CHAR if editing=>{},
            WM_KEYUP if w.0==13=>e.editor_mut(is_history).suppress_enter=false,
            WM_KEYDOWN if is_history && !editing && w.0==27=>{e.restore();e.history_view=None;}
            WM_KEYDOWN if editing=>{
                let shift=GetKeyState(VK_SHIFT.0 as i32)<0;let ctrl=GetKeyState(VK_CONTROL.0 as i32)<0;
                let ed=e.editor_mut(is_history);
                if w.0==13 && ed.composing {ed.suppress_enter=true;return None;}
                if w.0==13 && ed.suppress_enter {return Some(LRESULT(0));}
                match super::state::key(w.0 as u32,shift,ed.composing) {
                    Key::Send=>{e.send(is_history);e.editor_mut(is_history).suppress_enter=true;}
                    Key::Close=>{e.restore();if is_history{e.history_view=None;}else{e.bubbles.cancel();}}
                    Key::Native=>e.editor_mut(is_history).edit_key(h,w.0 as u32,ctrl,shift),Key::Newline=>e.editor_mut(is_history).suppress_enter=false,
                }
            }
            WM_CHAR if editing=>e.editor_mut(is_history).character(w.0 as u16),
            WM_LBUTTONDOWN=>{
                let dpi=if is_history{e.history_view.as_ref().unwrap().dpi}else{ANCHOR.with(Cell::get)?.dpi};
                let mut x=(l.0&0xffff) as i16 as f32/dpi;let mut y=((l.0>>16)&0xffff) as i16 as f32/dpi;
                if is_history {
                    x-=history_view::MARGIN;y-=history_view::MARGIN;
                    if y<56. && x>330. {e.restore();e.history_view=None;}
                    else if editing && y<56. && x>292. {e.cancel();e.chat.clear();e.bubbles.cancel();e.history_editor=Editor::default();}
                    else if let Some(field) = e.history_view.as_ref().and_then(|v|v.input.as_ref()).filter(|field|editing && paint::contains(field.bounds,x,y)) {
                        let at=field.text.hit(x-field.bounds.left-11.,y-field.bounds.top-6.);
                        e.history_editor.click(at,GetKeyState(VK_SHIFT.0 as i32)<0);e.focus(h);
                    }
                }else {
                    let o=e.overlay.as_ref().unwrap();x+=o.origin.0;y+=o.origin.1;

                    if let Some(it)=e.items.iter().find(|i|paint::hit_item(i,x,y)) {
                        if editing && it.depth==0 {
                            let at=it.text.hit((x-it.bounds.left)/it.scale-super::bubbles::PAD_X,(y-it.bounds.top)/it.scale-super::bubbles::PAD_Y);
                            e.editor.click(at,GetKeyState(VK_SHIFT.0 as i32)<0);e.focus(h);
                        }else if let Some(message)=it.message_id.and_then(|id|e.bubbles.message(id)).cloned() {
                            if e.history().is_ok(){if let Some(view)=&mut e.history_view {view.reveal(message);}}
                        }
                    }
                }
            }
            WM_RBUTTONUP if is_overlay=>{let _=e.history();}
            WM_MOUSEWHEEL if is_history=>{if let Some(v)=&mut e.history_view{let delta=((w.0>>16)&0xffff) as i16 as f32/120.;v.wheel(delta);}}
            WM_MOUSEWHEEL if is_overlay=>{
                let dpi=ANCHOR.with(Cell::get)?.dpi;
                let x=(l.0&0xffff) as i16 as f32/dpi;let y=((l.0>>16)&0xffff) as i16 as f32/dpi;
                let now=e.now();let delta=((w.0>>16)&0xffff) as i16 as f32/120.;
                let it=e.items.iter().find(|i|paint::hit_item(i,x,y))?;
                if let Some(message)=it.message_id.and_then(|id|e.bubbles.message_mut(id)) {message.wheel(delta,it.text.scroll,it.text.max_scroll,now,e.motion);}
                e.bubbles.hover(true,now);
            }
            _=>return None,
        }
        handled=true;let _=e.draw();Some(LRESULT(0))
    });
    if handled{super::super::refresh();}result.unwrap_or_else(||DefWindowProcW(h,m,w,l))
}

/// Any local or provider bubble owns the companion conversation space.
pub fn busy()->bool{ENGINE.with(|e|e.try_borrow().map(|e|e.as_ref().is_some_and(|e|e.bubbles.phase!=Phase::Hidden||e.active())).unwrap_or(true))}

#[cfg(test)] mod tests {
    use super::*;
    #[test]
    fn review_monitor_move_preserves_request_and_both_drafts() {
        let stage=crate::gfx::promo_kit::stage();
        let mut e=Engine::build(stage.gfx.clone(),false,History::default(),None,false);
        e.editor.insert("입력 초안");e.history_editor.insert("기록 초안");
        e.chat.begin("질문".into());e.bubbles.submit("질문".into(),0.);
        let generation=e.chat.generation;
        let (cancel,mut cancelled)=tokio::sync::oneshot::channel();e.cancel=Some(cancel);
        e.close_panels();
        assert!(matches!(cancelled.try_recv(),Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert_eq!(e.chat.pending.as_deref(),Some("질문"));
        assert_eq!(e.editor.field.text(),"입력 초안");
        assert_eq!(e.history_editor.field.text(),"기록 초안");
        e.tx.send(Update::Delta(generation,"답변".into())).unwrap();
        e.tx.send(Update::Finished(generation,None)).unwrap();e.receive();
        assert_eq!(e.history.turns.back().unwrap().1,"답변");
        assert!(!e.panels_hidden);
        e.close_panels();e.resume_input();
        assert_eq!(e.bubbles.phase,Phase::Input);
        assert_eq!(e.editor.field.text(),"입력 초안");
        assert_eq!(e.history_editor.field.text(),"기록 초안");
        e.close_panels();
        assert!(!e.active(), "a closed draft must not block later notices");
        e.say_queue.push("새 알림".into());e.drain_say();
        assert_eq!(e.bubbles.phase,Phase::Reply);
        assert!(!e.panels_hidden);
        assert_eq!(e.editor.field.text(),"입력 초안");
    }
    #[test]
    fn panels_stay_put_in_the_actual_draw_path_and_reopen_at_the_new_tip() {
        let stage=crate::gfx::promo_kit::stage();
        let mut e=Engine::build(stage.gfx.clone(),false,History::default(),None,false);
        let a=Anchor{work:RECT{left:-30000,top:-30000,right:-28000,bottom:-28000},dpi:1.,tip:(-29000.,-28500.),foot:(-29000.,-28300.),scale:0.28,edge:None};
        ANCHOR.with(|c|c.set(Some(a)));
        e.bubbles.input(0.);e.editor.insert("짧은 입력도 고정");
        unsafe{e.draw().unwrap();}
        let original=e.items[0].bounds;
        ANCHOR.with(|c|c.set(Some(Anchor{tip:(-29500.,-28600.),..a})));
        e.resume_input();
        unsafe{e.draw().unwrap();}
        assert_eq!(e.items[0].bounds,original);
        e.editor.insert("\n두 번째\n세 번째");
        unsafe{e.draw().unwrap();}
        assert_eq!((e.items[0].bounds.left,e.items[0].bounds.top),(original.left,original.top));
        e.bubbles.cancel();unsafe{e.draw().unwrap();}
        e.bubbles.input(0.);unsafe{e.draw().unwrap();}
        assert_ne!(e.items[0].bounds.left,original.left);
        e.bubbles.cancel();unsafe{e.draw().unwrap();}
        e.bubbles.say("안녕!".into(),0.);unsafe{e.draw().unwrap();}
        let small=e.items[0].bounds;
        ANCHOR.with(|c|c.set(Some(a)));unsafe{e.draw().unwrap();}
        assert_ne!(e.items[0].bounds,small);
        ANCHOR.with(|c|c.set(None));ACTIVE.with(|c|c.set(false));
    }
    #[test] fn account_changes_keep_archive_but_clear_provider_context() {
        let stage=crate::gfx::promo_kit::stage();
        let mut e=Engine::build(stage.gfx.clone(),false,History::default(),None,false);
        e.history=History::default();e.history.push("archive".into(),"answer".into());
        e.account=Some("first".into());e.chat.begin("private".into());let generation=e.chat.generation;
        e.change_account(None);
        assert_eq!(e.history.turns.len(),1);assert!(e.chat.pending.is_none());assert!(e.chat.turns.is_empty());
        e.chat.delta(generation,"late");assert!(e.chat.answer.is_empty());
        e.change_account(Some("second".into()));assert_eq!(e.history.turns[0].0,"archive");
        e.chat.clear();assert_eq!(e.history.turns.len(),1);
        e.editor.insert("overhead draft");e.history_editor.insert("history draft");
        assert_eq!(e.editor.field.text(),"overhead draft");
        e.bubbles.say("agent".into(),0.);assert!(!e.active());
        e.change_account(Some("third".into()));assert!(e.bubbles.agent);assert_eq!(e.bubbles.phase,Phase::Reply);
        e.bubbles.input(1.);assert!(e.active());
        e.bubbles.submit("user".into(),2.);e.bubbles.delta("answer",3.);assert!(e.active());
    }
}
