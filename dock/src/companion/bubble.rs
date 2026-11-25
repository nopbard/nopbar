//! Nonactivating compact fan. The only hook observes outside mouse clicks and
//! always forwards them. Transparent holes are absent from both paint and hit.
use super::{convenience::{self,Action},fan::{self,Point,Rect,Layout,Id,Hit,Expanded},grab::Kind,render,timing,window};
use crate::{gfx::Gfx,theme::Theme,glyphs::{self,Glyph}};
use std::{rc::Rc,sync::{Mutex,OnceLock,atomic::{AtomicIsize,Ordering}},collections::HashMap};
use windows::{core::{w,Result},Win32::{Foundation::*,Graphics::Direct2D::{Common::*,*},System::LibraryLoader::GetModuleHandleW,UI::{WindowsAndMessaging::*,Input::KeyboardAndMouse::*}}};
use windows_numerics::Vector2;
use windows::Win32::{Graphics::Gdi::ClientToScreen,UI::Input::Pointer::*};
const OUTSIDE:u32=WM_APP+2;
const MOUSE_LEAVE:u32=0x02a3;
static TARGET:AtomicIsize=AtomicIsize::new(0);
#[derive(Clone)] struct Regions {layout:Layout,body:Vec<Rect>,dpi:f32}
static REGIONS:Mutex<Option<Regions>>=Mutex::new(None);
fn screen_rect(r:RECT,dpi:f32)->Rect {Rect{left:r.left as f32/dpi,top:r.top as f32/dpi,right:r.right as f32/dpi,bottom:r.bottom as f32/dpi}}
fn native_rect(r:Rect,dpi:f32)->RECT {RECT{left:(r.left*dpi).floor() as i32,top:(r.top*dpi).floor() as i32,right:(r.right*dpi).ceil() as i32,bottom:(r.bottom*dpi).ceil() as i32}}
fn in_visit(r:&Regions,p:POINT)->bool {let p=Point{x:p.x as f32/r.dpi,y:p.y as f32/r.dpi};r.body.iter().any(|r|r.contains(p))||r.layout.hit(p)!=Hit::Outside}
/// Exact alpha runs, cached in artwork coordinates; only their transform changes.
pub(super) fn silhouette(p:&super::motion::Pose,x:f32,y:f32,scale:f32)->Vec<Rect> {
 static CACHE:OnceLock<Mutex<HashMap<String,Vec<Rect>>>>=OnceLock::new();
 let mut cache=CACHE.get_or_init(Default::default).lock().unwrap();
 let runs=cache.entry(p.frame.clone()).or_insert_with(||{
  let image=crate::icons::decode_bytes_fit(super::clips::png(&p.frame,"full").expect("validated frame"),4096).expect("validated companion image");
  let mut runs=Vec::new();for y in 0..image.height {let mut start=None;
   for x in 0..=image.width {let solid=x<image.width&&image.pixels[((y*image.width+x)*4+3) as usize]>0;
    if solid&&start.is_none(){start=Some(x);}else if !solid {if let Some(left)=start.take(){runs.push(Rect{left:left as f32,top:y as f32,right:x as f32,bottom:y as f32+1.});}}
   }
  }runs
 });
 let m=render::body_matrix(p,x,y,scale);
 runs.iter().map(|r|{let corners=[(r.left,r.top),(r.right,r.top),(r.left,r.bottom),(r.right,r.bottom)];let mut out=Rect{left:f32::INFINITY,top:f32::INFINITY,right:f32::NEG_INFINITY,bottom:f32::NEG_INFINITY};
  for (x,y) in corners {let x1=x*m.M11+y*m.M21+m.M31;let y1=x*m.M12+y*m.M22+m.M32;out.left=out.left.min(x1);out.top=out.top.min(y1);out.right=out.right.max(x1);out.bottom=out.bottom.max(y1);}out
 }).collect()
}
#[derive(Clone,PartialEq)] struct Key {notes:bool,edge:bool,scale:f32,dpi:f32,work:Rect,kind:Kind,expanded:Expanded}
struct Opening {tip:Point,shape:Vec<Rect>,scale:f32,work:Rect,dpi:f32}
pub struct Bubble {opening:Option<Opening>,shown:bool,at:f64,closing:Option<f64>,outside:Option<f64>,pointer:fan::Pointer,expanded:Expanded,kind:Kind,native:Option<Native>,key:Option<Key>,layout:Option<Layout>,shape:Vec<Rect>,active:Option<u32>,dirty:bool,was_animating:bool,tip:Option<Id>,failed_tips:Vec<Id>,last_input:f64,cursor:Option<POINT>,hover_kind:Kind}
impl Default for Bubble {fn default()->Self{Self{opening:None,shown:false,at:0.,closing:None,outside:None,pointer:Default::default(),expanded:Expanded::None,kind:Kind::Mouse,native:None,key:None,layout:None,shape:Vec::new(),active:None,dirty:false,was_animating:false,tip:None,failed_tips:Vec::new(),last_input:0.,cursor:None,hover_kind:Kind::Mouse}}}
impl Bubble {
 pub fn open(&self)->bool{self.shown}
 pub fn visible(&self)->bool{self.shown||self.closing.is_some()}
 pub fn set_kind(&mut self,kind:Kind){self.kind=kind;self.hover_kind=kind;}
 fn hide_tip(&mut self){if self.tip.take().is_some(){crate::topbar::hide_tip();}}
 pub fn show(&mut self,now:f64){self.opening=None;self.hide_tip();self.native=None;self.shown=true;self.at=now;self.last_input=now;self.cursor=None;self.failed_tips.clear();self.closing=None;self.outside=None;self.pointer=Default::default();self.active=None;self.expanded=Expanded::None;self.key=None;self.layout=None;self.dirty=true;}
 pub fn close(&mut self,now:f64){if self.shown {self.hide_tip();self.shown=false;self.closing=Some(now);self.pointer.cancel();self.active=None;if let Some(n)=&mut self.native{n.unhook();}}}
 pub fn dismiss(&mut self){self.opening=None;self.hide_tip();self.shown=false;self.closing=None;self.native=None;self.key=None;self.layout=None;}
 pub fn visit(&mut self,now:f64,inside:bool){if self.shown {
  if now-self.last_input>=if self.kind==Kind::Mouse{20.}else{6.}{self.close(now);return;}
  if self.kind==Kind::Mouse {if inside{self.outside=None;}else if now-*self.outside.get_or_insert(now)>=4.{self.close(now);}}
 }}
 pub fn interval(&self,now:f64,animations:bool)->u32 {if self.animating(now,animations){17}else{250}}
 fn animating(&self,now:f64,animations:bool)->bool {animations&&(now-self.at<(timing::FAN_OPEN+4.*timing::FAN_STAGGER) as f64||self.closing.is_some()||self.pointer.focus.is_some()&&now-self.pointer.hover_at<timing::FAN_HOVER as f64)}
 fn needs_layout(&self,key:&Key,forbidden:&[Rect])->bool {self.key.as_ref()!=Some(key)||self.layout.as_ref().is_some_and(|p|p.obstructed(forbidden))}
 fn activate(&mut self,id:Id,now:f64){
  if matches!(id,Id::Size|Id::Rest){self.expanded=fan::expanded(self.expanded,id);self.key=None;self.hide_tip();self.dirty=true;return;}
  let action=match id{Id::Hide=>Action::Hide,Id::Detach=>Action::Detach,Id::Settings=>Action::Settings,Id::NewNote=>Action::NewNote,Id::Smaller=>Action::Step(-1),Id::Larger=>Action::Step(1),Id::Rest30=>Action::Rest(convenience::now()+1800),Id::Rest60=>Action::Rest(convenience::now()+3600),Id::Morning=>Action::Rest(convenience::tomorrow(convenience::now())),_=>return};
  convenience::request(action);
  if !matches!(id,Id::Smaller|Id::Larger){self.close(now);}else{self.key=None;self.dirty=true;}
 }
 #[allow(clippy::too_many_arguments)]
 pub fn update(&mut self,gfx:&Rc<Gfx>,theme:&Theme,p:&super::motion::Pose,x:f32,y:f32,scale:f32,tip:Point,_body:RECT,work:RECT,forbidden:&[RECT],now:f64,dpi:f32,animations:bool,edge:bool)->Result<()> {
  if !self.visible(){return Ok(());}
  if self.closing.is_some_and(|at|!animations||now-at>=timing::FAN_CLOSE as f64){self.dismiss();return Ok(());}
  let work=screen_rect(work,dpi);
  if self.shown {
   let forbidden:Vec<_>=forbidden.iter().copied().filter_map(|r|screen_rect(r,dpi).normalized()).collect();
   self.layout_at((p,x,y,scale),tip,work,&forbidden,dpi,edge);
  }
  let Some(layout)=self.layout.clone()else{self.dismiss();return Ok(());};
  let mut cursor=POINT::default();unsafe{let _=GetCursorPos(&mut cursor);}
  let elapsed=if animations{(now-self.at).max(0.) as f32}else{1.};let closing=self.closing.map(|at|(now-at) as f32);
  let hover=if animations{1.+0.08*(((now-self.pointer.hover_at) as f32/timing::FAN_HOVER).clamp(0.,1.))}else{1.};
  let presented=layout.presentation(elapsed,closing,animations,self.pointer.focus,hover);
  let body_hit=if scale<=0.4 {fan::dilate(&self.shape,28.)}else{self.shape.clone()};
  let mut regions=Regions{layout:presented.clone(),body:body_hit,dpi};
  let moved=self.cursor.is_some_and(|old|old!=cursor);self.cursor=Some(cursor);
  if moved {self.hover_kind=Kind::Mouse;if self.kind==Kind::Mouse{self.last_input=now;}}
  self.visit(now,in_visit(&regions,cursor));
  if self.shown&&self.active.is_none_or(|id|id==0)&&(self.kind==Kind::Mouse||moved||self.hover_kind==Kind::Mouse) {let old=self.pointer.focus;self.pointer.hover(presented.hit(Point{x:cursor.x as f32/dpi,y:cursor.y as f32/dpi}),now);self.dirty|=old!=self.pointer.focus;}
  // Polling may have changed hover or closed the fan. Paint and hit use the
  // same final presentation, rather than disagreeing for one frame.
  let closing=self.closing.map(|at|(now-at) as f32);
  let hover=if animations{1.+0.08*(((now-self.pointer.hover_at) as f32/timing::FAN_HOVER).clamp(0.,1.))}else{1.};
  regions.layout=layout.presentation(elapsed,closing,animations,self.pointer.focus,hover);
  if self.native.as_ref().is_some_and(|n|n.dpi!=dpi){self.native=None;}
  if self.native.is_none(){self.native=Some(unsafe{Native::new(gfx,&layout,dpi,self.shown)?});self.dirty=true;}
  *REGIONS.lock().unwrap()=Some(regions);
  let animate=self.animating(now,animations);
  let n=self.native.as_mut().unwrap();let r=native_rect(layout.bounds,dpi);
  let dimensions=((r.right-r.left).max(1) as u32,(r.bottom-r.top).max(1) as u32);
  if n.size!=dimensions {unsafe{n.surface.resize(dimensions.0,dimensions.1)?;}n.size=dimensions;self.dirty=true;}
  let repaint=self.dirty||animate||self.was_animating;self.was_animating=animate;
  if repaint {let focus=self.pointer.focus;
   unsafe{n.surface.raster.custom(dpi,n.surface.window.pixels_mut(),|dc|{
    dc.SetTransform(&(render::translate(-r.left as f32/dpi,-r.top as f32/dpi)*render::scale(dpi,dpi)));
    paint(dc,&layout,theme,focus,hover,elapsed,closing,animations,dpi)
   })?;}self.dirty=false;
  }
  unsafe{n.surface.window.present(r.left,r.top,repaint)?;}
  let tip_id=if self.shown{self.pointer.tooltip(now,self.hover_kind)}else{None};
  if self.tip!=tip_id {self.hide_tip();if let Some(id)=tip_id.filter(|id|!self.failed_tips.contains(id)) {self.failed_tips.push(id);if let Some(button)=layout.buttons.iter().find(|b|b.id==id){
   let text=tooltip(button.id);
   if let Some((w,h))=crate::topbar::wrapped_tip_size(gfx,theme,dpi,&text,200.*dpi) {
    if let Some(r)=layout.tooltip_rect(id,w as f32/dpi,h as f32/dpi,work) {
     let at=POINT{x:((r.left+r.right)*0.5*dpi).round() as i32,y:(r.top*dpi).round() as i32};
     crate::topbar::show_tip_wrapped(gfx,theme,dpi,&text,at,false,native_rect(work.inset(8.),dpi),200.*dpi);self.tip=Some(id);self.failed_tips.retain(|failed|*failed!=id);
    }
   }
  }}}
  Ok(())
 }
 // Freeze menu geometry too: resizing or dragging the pet cannot move its menu.
 fn layout_at(&mut self,drawing:(&super::motion::Pose,f32,f32,f32),tip:Point,work:Rect,forbidden:&[Rect],dpi:f32,edge:bool) {
  let (p,x,y,scale)=drawing;
  if self.opening.as_ref().is_none_or(|a|a.work!=work||a.dpi!=dpi) {
   self.opening=Some(Opening{tip,shape:silhouette(p,x,y,scale),scale,work,dpi});
  }
  let scale=self.opening.as_ref().unwrap().scale;
  let key=Key{notes:crate::notes::running(),edge,scale,dpi,work,kind:self.kind,expanded:self.expanded};
  if self.needs_layout(&key,forbidden){self.hide_tip();self.failed_tips.clear();let opening=self.opening.as_ref().unwrap();let shape=opening.shape.clone();self.layout=fan::place_tools(opening.tip,&shape,scale<=0.4,work,forbidden,self.kind,self.expanded,key.notes,key.edge);self.shape=shape;self.key=Some(key);self.dirty=true;}
 }
}
pub(super) fn tooltip(id:Id)->String {use crate::i18n::t;match id{
 Id::Hide=>t("companion.menu_hide"),Id::Rest=>t("companion.rest"),Id::Size=>t("companion.size"),Id::Detach=>t("companion.detach"),Id::Settings=>t("companion.settings"),Id::NewNote=>t("notes.new"),Id::Smaller=>return format!("{} −",t("companion.size")),Id::Larger=>return format!("{} +",t("companion.size")),Id::Rest30=>t("companion.rest30"),Id::Rest60=>t("companion.rest60"),Id::Morning=>t(convenience::morning_label(convenience::now()))}.into()}
// Shared by the HWND and harmony; no permanent captions are painted.
thread_local! {
 static STROKE:std::cell::RefCell<Option<(ID2D1Factory,ID2D1StrokeStyle)>>=const{std::cell::RefCell::new(None)};
 static TEXT_FACTORY:std::cell::OnceCell<windows::Win32::Graphics::DirectWrite::IDWriteFactory>=const{std::cell::OnceCell::new()};
}
unsafe fn stroke(factory:&ID2D1Factory)->Result<ID2D1StrokeStyle> {
 STROKE.with(|cache|{let mut cache=cache.borrow_mut();if cache.as_ref().is_none_or(|(f,_)|f!=factory) {
  let style=factory.CreateStrokeStyle(&D2D1_STROKE_STYLE_PROPERTIES{startCap:D2D1_CAP_STYLE_ROUND,endCap:D2D1_CAP_STYLE_ROUND,lineJoin:D2D1_LINE_JOIN_ROUND,..Default::default()},None)?;
  *cache=Some((factory.clone(),style));
 }Ok(cache.as_ref().unwrap().1.clone())})
}
unsafe fn duration_layout(text:&str,width:f32)->Result<windows::Win32::Graphics::DirectWrite::IDWriteTextLayout> {
 use windows::Win32::Graphics::DirectWrite::*;
 TEXT_FACTORY.with(|cache|{
  if cache.get().is_none() {
   let factory:IDWriteFactory=DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
   let _=cache.set(factory);
  }
  let size=(10.*crate::fonts::scale()).min(12.);
  let factory=cache.get().unwrap();
  let format=crate::gfx::ui_format(factory,&windows::core::HSTRING::from(crate::fonts::ui_family()),DWRITE_FONT_WEIGHT_NORMAL,size)?;
  format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
  format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
  format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
  let text: Vec<_> = text.encode_utf16().collect();
  let layout = factory.CreateTextLayout(&text, &format, 400., 24.)?;
  let mut metrics = DWRITE_TEXT_METRICS::default();
  layout.GetMetrics(&mut metrics)?;
  if metrics.width > width {
   layout.SetFontSize(size * width / metrics.width, DWRITE_TEXT_RANGE {startPosition: 0, length: text.len() as u32})?;
  }
  layout.SetMaxWidth(width)?;
  Ok(layout)
 })
}
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn paint(dc:&ID2D1DeviceContext,layout:&Layout,theme:&Theme,focus:Option<Id>,hover:f32,t:f32,closing:Option<f32>,animations:bool,dpi:f32)->Result<()> {
 let ink=theme.ink();let light=ink.r<128;let color=|v:f32,a:f32|D2D1_COLOR_F{r:v,g:v,b:v,a};
 let fade=closing.map_or((t/timing::POP).clamp(0.,1.),|v|1.-(v/timing::FAN_CLOSE).clamp(0.,1.));
 let factory=dc.GetFactory()?;let style=stroke(&factory)?;
 for arc in &layout.arcs {
  let path=factory.CreatePathGeometry()?;let sink=path.Open()?;let pts=arc.points();sink.BeginFigure(Vector2{X:pts[0].x,Y:pts[0].y},D2D1_FIGURE_BEGIN_HOLLOW);
  for p in &pts[1..]{sink.AddLine(Vector2{X:p.x,Y:p.y});}sink.EndFigure(D2D1_FIGURE_END_OPEN);sink.Close()?;
  let edge=dc.CreateSolidColorBrush(&color(if light{0.1}else{0.9},0.08*fade),None)?;
  dc.DrawGeometry(&path,&edge,arc.width,&style);
  let fill=dc.CreateSolidColorBrush(&color(if light{0.94}else{0.16},0.72*fade),None)?;dc.DrawGeometry(&path,&fill,(arc.width-2./dpi).max(1.),&style);
 }
 for (i,b) in layout.buttons.iter().enumerate(){
  let (progress,alpha)=fan::animation(t,i.min(4),closing,animations);
  let center=Point{x:layout.tip.x+(b.center.x-layout.tip.x)*progress,y:layout.tip.y+(b.center.y-layout.tip.y)*progress};
  let r=b.radius*if focus==Some(b.id){hover}else{1.};
  let fill=dc.CreateSolidColorBrush(&color(if light{1.}else{0.22},0.98*alpha),None)?;
  dc.FillEllipse(&D2D1_ELLIPSE{point:Vector2{X:center.x,Y:center.y},radiusX:r,radiusY:r},&fill);
  let c=color(if light{0.2}else{0.94},alpha);
  icon(dc,b.id,center,r,c)?;
 }Ok(())
}
unsafe fn icon(dc:&ID2D1DeviceContext,id:Id,p:Point,radius:f32,c:D2D1_COLOR_F)->Result<()> {
 let brush=dc.CreateSolidColorBrush(&c,None)?;let pt=|x,y|Vector2{X:p.x+x,Y:p.y+y};let line=|a:(f32,f32),b:(f32,f32)|dc.DrawLine(pt(a.0,a.1),pt(b.0,b.1),&brush,1.5,None);
 match id {
  Id::Hide=>{for (a,b) in [((-7.,3.),(0.,-10.)),((0.,-10.),(7.,3.)),((-8.,3.),(8.,3.)),((0.,4.),(0.,10.)),((-3.,7.),(0.,10.)),((0.,10.),(3.,7.))]{line(a,b);}}
  Id::Rest=>{for (x,y,s) in [(-8.,0.,8.),(2.,-7.,6.)]{line((x,y),(x+s,y));line((x+s,y),(x,y+s));line((x,y+s),(x+s,y+s));}}
  Id::Size=>{for (a,b) in [((-7.,7.),(7.,-7.)),((-7.,0.),(-7.,7.)),((-7.,7.),(0.,7.)),((0.,-7.),(7.,-7.)),((7.,-7.),(7.,0.))]{line(a,b);}}
  Id::Detach=>glyphs::draw(dc,Glyph::Pinned,p.x,p.y,20.,c)?,
  Id::NewNote=>glyphs::draw(dc,Glyph::Document,p.x,p.y,20.,c)?,
  Id::Settings=>glyphs::draw(dc,Glyph::Gear,p.x,p.y,20.,c)?,
  Id::Smaller=>glyphs::draw(dc,Glyph::Minus,p.x,p.y,18.,c)?,Id::Larger=>glyphs::draw(dc,Glyph::Plus,p.x,p.y,18.,c)?,
  Id::Morning=>glyphs::draw(dc,Glyph::Sun,p.x,p.y,17.,c)?,
  Id::Rest30|Id::Rest60=>{
   let text=crate::i18n::t(if id==Id::Rest30{"companion.rest30_short"}else{"companion.rest60_short"});
   let width = 2. * radius - 6.;
   let layout = duration_layout(text, width)?;
   dc.DrawTextLayout(pt(-width / 2., -12.), &layout, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
  }
 }Ok(())
}
struct Native {surface:window::Surface,dpi:f32,size:(u32,u32),mouse:Option<HHOOK>}
impl Native {
 unsafe fn new(gfx:&Rc<Gfx>,layout:&Layout,dpi:f32,interactive:bool)->Result<Self> {
  let r=native_rect(layout.bounds,dpi);let size=((r.right-r.left).max(1) as u32,(r.bottom-r.top).max(1) as u32);
  let raster=render::Raster::new(gfx.offscreen_context()?,size.0,size.1)?;
  let mut window=window::Window::with_proc(size.0,size.1,false,w!("NopBarCompanionFan"),Some(proc))?;
  window.follow_capture_setting();
  let mut n=Self{surface:window::Surface{raster,window},dpi,size,mouse:None};
  if interactive {let h=n.surface.window.hwnd;TARGET.store(h.0 as isize,Ordering::SeqCst);n.mouse=Some(SetWindowsHookExW(WH_MOUSE_LL,Some(mouse_hook),Some(GetModuleHandleW(None)?.into()),0)?);}
  Ok(n)
 }
 fn unhook(&mut self){let h=self.surface.window.hwnd;let _=TARGET.compare_exchange(h.0 as isize,0,Ordering::SeqCst,Ordering::SeqCst);unsafe{if let Some(h)=self.mouse.take(){let _=UnhookWindowsHookEx(h);}}}
}
impl Drop for Native {fn drop(&mut self){self.unhook();}}
unsafe extern "system" fn mouse_hook(code:i32,w:WPARAM,l:LPARAM)->LRESULT {
 if code>=0&&matches!(w.0 as u32,WM_LBUTTONDOWN|WM_RBUTTONDOWN|WM_MBUTTONDOWN){let p=(*(l.0 as *const MSLLHOOKSTRUCT)).pt;
  let outside=REGIONS.lock().unwrap().as_ref().is_some_and(|r|!in_visit(r,p));
  if outside {let h=TARGET.load(Ordering::SeqCst);if h!=0{let _=PostMessageW(Some(HWND(h as _)),OUTSIDE,WPARAM(0),LPARAM(0));}}
 }CallNextHookEx(None,code,w,l)
}
fn hit_screen(p:POINT)->Hit {REGIONS.lock().unwrap().as_ref().map_or(Hit::Outside,|r|r.layout.hit(Point{x:p.x as f32/r.dpi,y:p.y as f32/r.dpi}))}
unsafe extern "system" fn proc(h:HWND,m:u32,w:WPARAM,l:LPARAM)->LRESULT {
 if m==WM_MOUSEACTIVATE{return LRESULT(MA_NOACTIVATE as isize);}if m==WM_POINTERACTIVATE{return LRESULT(PA_NOACTIVATE as isize);}
 if m==WM_NCHITTEST {let p=POINT{x:(l.0&0xffff) as i16 as i32,y:((l.0>>16)&0xffff) as i16 as i32};return LRESULT(if hit_screen(p)==Hit::Outside{HTTRANSPARENT as isize}else{HTCLIENT as isize});}
 if m==0x02cc{return LRESULT(0x00000001|0x00000008|0x00000010|0x00010000|0x00000200);}
 let pointer=matches!(m,WM_POINTERDOWN|WM_POINTERUPDATE|WM_POINTERUP|WM_POINTERLEAVE);
 if matches!(m,WM_LBUTTONDOWN|WM_LBUTTONUP|WM_MOUSEMOVE)&&GetMessageExtraInfo().0 as usize&0xffffff00==0xff515700{return LRESULT(0);}
 if pointer||matches!(m,OUTSIDE|WM_MOUSEMOVE|WM_LBUTTONDOWN|WM_LBUTTONUP|MOUSE_LEAVE|WM_CAPTURECHANGED|WM_POINTERCAPTURECHANGED){
  let mut point=POINT{x:(l.0&0xffff) as i16 as i32,y:((l.0>>16)&0xffff) as i16 as i32};let mut kind=Kind::Mouse;let mut cancelled=false;
  if pointer&&m!=WM_POINTERLEAVE {let mut info=POINTER_INFO::default();if GetPointerInfo((w.0&0xffff) as u32,&mut info).is_err(){return LRESULT(0);}if info.pointerType==PT_MOUSE{return DefWindowProcW(h,m,w,l);}point=info.ptPixelLocation;kind=if info.pointerType==PT_PEN{Kind::Pen}else{Kind::Touch};cancelled=info.pointerFlags.0&POINTER_FLAG_CANCELED.0!=0;}
  else{let _=ClientToScreen(h,&mut point);}
  let hit=hit_screen(point);let id=if pointer||m==WM_POINTERCAPTURECHANGED{(w.0&0xffff) as u32}else{0};
  // Capture precedes dispatch: dispatch can synchronously redraw/resize windows.
  let capture=m==WM_LBUTTONDOWN&&super::APP.with(|a|a.try_borrow().ok().is_some_and(|a|a.as_ref().is_some_and(|a|a.bubble.open()&&a.bubble.active.is_none())));
  if capture {SetCapture(h);}
  super::APP.with(|a|{let Ok(mut a)=a.try_borrow_mut()else{return;};let Some(a)=a.as_mut()else{return;};let now=a.start.elapsed().as_secs_f64();let b=&mut a.bubble;
   if m==OUTSIDE{b.close(now);return;}if !b.open(){return;}
   if matches!(m,WM_MOUSEMOVE|WM_LBUTTONDOWN|WM_LBUTTONUP|WM_POINTERDOWN|WM_POINTERUPDATE|WM_POINTERUP){b.last_input=now;b.hover_kind=kind;}
   if (cancelled||matches!(m,WM_CAPTURECHANGED|WM_POINTERCAPTURECHANGED))&&b.active==Some(id){b.pointer.cancel();b.active=None;b.hide_tip();b.dirty=true;return;}
   if matches!(m,WM_MOUSEMOVE|WM_POINTERUPDATE)&&(b.active.is_none()||b.active==Some(id)){b.pointer.hover(hit,now);b.dirty=true;}
   if matches!(m,MOUSE_LEAVE|WM_POINTERLEAVE)&&b.active.is_none(){b.pointer.hover(Hit::Outside,now);b.hide_tip();b.dirty=true;}
   if matches!(m,WM_LBUTTONDOWN|WM_POINTERDOWN)&&b.active.is_none(){b.active=Some(id);b.pointer.down(hit,now,kind);b.dirty=true;}
   if matches!(m,WM_LBUTTONUP|WM_POINTERUP)&&b.active==Some(id){b.active=None;if let Some(id)=b.pointer.up(hit,now){b.activate(id,now);}b.dirty=true;}
  });
  if m==WM_MOUSEMOVE{let mut track=TRACKMOUSEEVENT{cbSize:std::mem::size_of::<TRACKMOUSEEVENT>() as u32,dwFlags:TME_LEAVE,hwndTrack:h,..Default::default()};let _=TrackMouseEvent(&mut track);}
  if m==WM_LBUTTONUP&&GetCapture()==h{let _=ReleaseCapture();}
  super::refresh();return LRESULT(0);
 }if m==WM_ERASEBKGND{return LRESULT(1);}DefWindowProcW(h,m,w,l)
}
#[cfg(test)] mod tests {
 #[test] fn menu_geometry_survives_pet_motion_resize_and_expansion() {
  let _stage=crate::gfx::promo_kit::stage();
  let mut b=Bubble::default();b.show(0.);
  let mut p=super::super::motion::Motion::new(0.).pose();
  let work=Rect{left:0.,top:0.,right:1400.,bottom:1000.};
  let tip=Point{x:600.,y:500.};
  b.layout_at((&p,600.,650.,0.28),tip,work,&[],1.,false);
  let initial=b.layout.clone().unwrap();
  p.frame="read_2".into();
  b.layout_at((&p,900.,700.,0.56),Point{x:900.,y:550.},work,&[],1.,false);
  assert_eq!(b.layout.as_ref().unwrap().bounds,initial.bounds);
  b.expanded=Expanded::Rest;b.key=None;
  b.layout_at((&p,900.,700.,0.56),Point{x:900.,y:550.},work,&[],1.,false);
  assert_eq!(b.layout.as_ref().unwrap().center,initial.center);
  b.show(1.);
  b.layout_at((&p,900.,700.,0.56),Point{x:900.,y:550.},work,&[],1.,false);
  assert_ne!(b.layout.as_ref().unwrap().center,initial.center);
 }
 use super::*;
 #[test] fn stationary_fan_sleeps_and_each_input_kind_expires() {
  for kind in [Kind::Mouse,Kind::Touch,Kind::Pen] {let mut b=Bubble::default();b.set_kind(kind);b.show(0.);
   assert_eq!(b.interval(0.1,true),17);assert_eq!(b.interval(1.,true),250);assert_eq!(b.interval(0.1,false),250);
   let timeout=if kind==Kind::Mouse{20.}else{6.};b.visit(timeout-0.01,true);assert!(b.open());b.visit(timeout,true);assert!(!b.open());
  }
  let mut b=Bubble::default();b.set_kind(Kind::Touch);b.show(0.);b.visit(0.,false);b.last_input=5.;b.visit(6.,false);assert!(b.open());b.visit(11.,false);assert!(!b.open());
 }
 #[test] fn only_geometry_settings_or_actual_obstacles_relayout() {
  let mut b=Bubble::default();b.show(0.);let work=Rect{left:0.,top:0.,right:900.,bottom:700.};
  let key=Key{notes:false,edge:false,scale:0.56,dpi:1.,work,kind:Kind::Mouse,expanded:Expanded::None};
  b.layout=fan::place(Point{x:400.,y:300.},&[],false,work,&[],Kind::Mouse,Expanded::None);b.key=Some(key.clone());
  assert!(!b.needs_layout(&key,&[Rect{left:0.,top:0.,right:10.,bottom:10.}]));
  let button=&b.layout.as_ref().unwrap().buttons[0];let p=button.center;
  assert!(b.needs_layout(&key,&[Rect{left:p.x-1.,right:p.x+1.,top:p.y-1.,bottom:p.y+1.}]));
  for changed in [Key{scale:0.4,..key.clone()},Key{dpi:1.5,..key.clone()},Key{kind:Kind::Pen,..key.clone()},Key{expanded:Expanded::Rest,..key.clone()}] {assert!(b.needs_layout(&changed,&[]));}
 }
 #[test] fn cursor_departure_and_outside_hole_close_without_consuming_click(){
  let mut b=Bubble::default();b.show(0.);b.visit(1.,false);b.visit(4.9,false);assert!(b.open());b.visit(5.,true);b.visit(6.,false);b.visit(10.,false);assert!(!b.open());
  let layout=fan::place(Point{x:400.,y:300.},&[],false,Rect{left:0.,top:0.,right:900.,bottom:700.},&[],Kind::Mouse,Expanded::None).unwrap();
  let r=Regions{layout,body:vec![Rect{left:380.,top:300.,right:420.,bottom:500.}],dpi:1.};
  assert!(in_visit(&r,POINT{x:400,y:400}));assert!(!in_visit(&r,POINT{x:200,y:200}));
  let source=include_str!("bubble.rs");let hook=source.split("unsafe extern \"system\" fn mouse_hook").nth(1).unwrap().split("fn hit_screen").next().unwrap();assert!(hook.contains("CallNextHookEx"));assert!(!hook.contains("LRESULT(1)"));
 }
 #[test] fn native_layout_clears_the_actual_png_silhouette_at_all_sizes(){
  unsafe{let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
  for scale in [0.28,0.4,0.56,0.8] {for kind in [Kind::Mouse,Kind::Touch,Kind::Pen] {
   let mut p=super::super::Motion::new(800.).pose();p.frame="look_4".into();let a=super::super::anchors::Anchors::load(&p.frame).posed(&p,1.);
   let tip=Point{x:800.+a.tip.0*scale,y:900.+a.tip.1*scale};let shape=silhouette(&p,800.,900.,scale);assert!(!shape.is_empty());
   for mode in [Expanded::Size,Expanded::Rest] {
   let layout=fan::place(tip,&shape,scale<=0.4,Rect{left:0.,top:0.,right:1600.,bottom:1200.},&[],kind,mode).unwrap();
   for b in &layout.buttons {assert!(shape.iter().all(|r|r.distance(b.center)+0.001>=b.radius+6.));}
   }
  }}
 }
 #[test] fn no_activation_and_input_kind_survives_open(){let mut b=Bubble::default();b.set_kind(Kind::Pen);b.show(1.);assert_eq!(b.kind,Kind::Pen);b.close(1.2);assert!(!b.open()&&b.visible());b.dismiss();assert!(!b.visible());unsafe{assert_eq!(proc(HWND::default(),WM_MOUSEACTIVATE,WPARAM(0),LPARAM(0)),LRESULT(MA_NOACTIVATE as isize));}}
}
