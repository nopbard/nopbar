//! The history window shares NopBar's composition glass and bubble text painter.
use super::{bubbles::{Message,Scroll},history::History,paint::{self,Input,Item,Text}};
use crate::{gfx::Gfx,theme::Theme};
use std::rc::Rc;
use windows::{core::{w,Interface,Result},UI::Composition::{ContainerVisual,SpriteVisual,Desktop::DesktopWindowTarget},Win32::{Foundation::*,Graphics::{Direct2D::*,DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD,Dwm::*},System::{LibraryLoader::GetModuleHandleW,WinRT::Composition::ICompositorDesktopInterop},UI::WindowsAndMessaging::*}};
use windows_numerics::{Matrix3x2,Vector2,Vector3};
pub const WIDTH:f32=380.;
pub const HEIGHT:f32=540.;
pub const MARGIN:f32=24.;
pub struct View {
    pub hwnd:HWND,
    pub dpi:f32,
    work:RECT,
    pub scroll:Scroll,
    max_scroll:f32,
    pub pending_ids:(Option<u64>,Option<u64>),
    reveal:Option<Message>,
    extra:Vec<Item>,
    pub motion:bool,
    clock:std::time::Instant,
    pub input:Option<Item>,
    pub theme:Theme,
    pub root:ContainerVisual,
    rows:Vec<Item>,
    pending_rows:Vec<Item>,
    revision:Option<u64>,
    pending_text:Option<(String,String)>,

    outer:ContainerVisual,
    content:SpriteVisual,
    _target:DesktopWindowTarget,
    gfx:Rc<Gfx>,
    surface:windows::UI::Composition::CompositionDrawingSurface,
}
impl View {
    pub unsafe fn new(gfx:Rc<Gfx>,theme:Theme,dpi:f32,work:RECT,handler:WNDPROC)->Result<Self> {
        let inst=GetModuleHandleW(None)?;
        RegisterClassExW(&WNDCLASSEXW{cbSize:std::mem::size_of::<WNDCLASSEXW>() as u32,lpfnWndProc:handler,hInstance:inst.into(),lpszClassName:w!("NopBarChatHistory"),hCursor:LoadCursorW(None,IDC_ARROW)?,..Default::default()});
        let w=((WIDTH+2.*MARGIN)*dpi).ceil() as i32;let h=((HEIGHT+2.*MARGIN)*dpi).ceil() as i32;
        let hwnd=CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP|WS_EX_TOOLWINDOW|WS_EX_TOPMOST,w!("NopBarChatHistory"),&windows::core::HSTRING::from(crate::i18n::t("chat.history")),WS_POPUP,(work.left+work.right-w)/2,(work.top+work.bottom-h)/2,w,h,None,None,Some(inst.into()),None)?;
        let built=(||->Result<Self>{
            let enabled=windows::core::BOOL(1);let _=DwmSetWindowAttribute(hwnd,DWMWA_USE_HOSTBACKDROPBRUSH,&enabled as *const _ as _,4);
            let c=&gfx.compositor;let interop:ICompositorDesktopInterop=c.cast()?;
            let target=interop.CreateDesktopWindowTarget(hwnd,true)?;
            let host=c.CreateContainerVisual()?;host.SetSize(Vector2{X:(WIDTH+48.)*dpi,Y:(HEIGHT+48.)*dpi})?;target.SetRoot(&host)?;
            let outer=c.CreateContainerVisual()?;outer.SetScale(Vector3{X:dpi,Y:dpi,Z:1.})?;outer.SetOffset(Vector3{X:MARGIN*dpi,Y:MARGIN*dpi,Z:0.})?;host.Children()?.InsertAtTop(&outer)?;
            let(root,border)=crate::popup::chrome(&gfx,&theme,&outer,WIDTH,HEIGHT,dpi,16.)?;
            let content=c.CreateSpriteVisual()?;content.SetSize(Vector2{X:WIDTH,Y:HEIGHT})?;root.Children()?.InsertBelow(&content,&border)?;
            let surface=gfx.draw((WIDTH*dpi).ceil() as u32,(HEIGHT*dpi).ceil() as u32,|_,_,_|Ok(()))?;
            let brush=c.CreateSurfaceBrushWithSurface(&surface)?;content.SetBrush(&brush)?;
            Ok(Self{hwnd,dpi,work,scroll:Scroll::default(),max_scroll:0.,pending_ids:(None,None),reveal:None,extra:Vec::new(),motion:true,clock:std::time::Instant::now(),input:None,theme,root:host,rows:Vec::new(),pending_rows:Vec::new(),revision:None,pending_text:None,outer,content,_target:target,gfx,surface})
        })();if built.is_err(){let _=DestroyWindow(hwnd);}built
    }
    pub fn draw(&mut self,history:&History,pending:Option<(&str,&str)>,input:Option<&Input<'_>>,blink:bool,focused:bool)->Result<()> {
        self.input=input.map(|input|input_item(&self.gfx,input,focused)).transpose()?;
        if self.revision!=Some(history.revision) {
            self.rows=rows(&self.gfx,history.turns.iter().map(|(u,a)|(u.as_str(),a.as_str())))?;
            let mut row=0;
            for (index,(u,a)) in history.turns.iter().enumerate() {for (s,user) in [(u,true),(a,false)] {
                if s.is_empty(){continue;}
                self.rows[row].message_id=history.links.iter().find_map(|(id,key)|(*key==(index,user)).then_some(*id));row+=1;
            }}
            self.revision=Some(history.revision);
        }
        if self.pending_text.as_ref().map(|(u,a)|(u.as_str(),a.as_str()))!=pending {
            self.pending_rows=rows(&self.gfx,pending.into_iter())?;
            self.pending_text=pending.map(|(u,a)|(u.to_owned(),a.to_owned()));
        }
        for row in &mut self.pending_rows {row.message_id=if row.user{self.pending_ids.0}else{self.pending_ids.1};}
        if let Some(message)=&self.reveal {
            self.extra.clear();
            if !self.rows.iter().chain(&self.pending_rows).any(|r|r.message_id==Some(message.id)) {
                self.extra=rows(&self.gfx,std::iter::once(if message.user{(message.text.as_str(),"")}else{("",message.text.as_str())}))?;
                for row in &mut self.extra {row.message_id=Some(message.id);}
            }
        }
        let all=||self.rows.iter().chain(&self.pending_rows).chain(&self.extra);
        let total=content_height(all());
        let max_scroll=(total-(list_bottom(self.input.as_ref())-72.)).max(0.);
        let now=self.clock.elapsed().as_secs_f64();
        if let Some(message)=self.reveal.take() {
            let mut top=0.;let mut previous=None;
            for row in all() {
                top+=row_gap(previous,row.user);previous=Some(row.user);
                if row.message_id==Some(message.id) {self.scroll.jump((max_scroll-top).clamp(0.,max_scroll));break;}
                top+=row.text.height;
            }
        }
        self.max_scroll=max_scroll;self.scroll.clamp(max_scroll);
        let scroll=self.scroll.value(now,self.motion);
        let dpi=self.dpi;
        self.gfx.redraw(&self.surface,|dc,ox,oy|unsafe{
            dc.SetTransform(&Matrix3x2{M11:dpi,M22:dpi,M31:ox,M32:oy,..Matrix3x2::identity()});
            paint_content(dc,&self.gfx,&self.theme,self.rows.iter_mut().chain(&mut self.pending_rows).chain(&mut self.extra),self.input.as_ref().zip(input),blink,focused,scroll,max_scroll)
        })
    }
    pub fn reveal(&mut self,message:Message) {self.reveal=Some(message);}
    pub fn wheel(&mut self,delta:f32) {
        let now=self.clock.elapsed().as_secs_f64();let current=self.scroll.value(now,self.motion);
        self.scroll.wheel(delta*60.,current,self.max_scroll,now,self.motion);
    }
    pub fn scrolling(&self)->bool {self.motion && self.scroll.active(self.clock.elapsed().as_secs_f64())}
    pub fn set_dpi(&mut self,dpi:f32)->Result<()> {
        if self.dpi==dpi{return Ok(());}
        self.dpi=dpi;
        self.root.SetSize(Vector2{X:(WIDTH+48.)*dpi,Y:(HEIGHT+48.)*dpi})?;
        self.outer.SetScale(Vector3{X:dpi,Y:dpi,Z:1.})?;
        self.outer.SetOffset(Vector3{X:MARGIN*dpi,Y:MARGIN*dpi,Z:0.})?;
        self.outer.Children()?.RemoveAll()?;
        let (root,border)=crate::popup::chrome(&self.gfx,&self.theme,&self.outer,WIDTH,HEIGHT,dpi,16.)?;
        root.Children()?.InsertBelow(&self.content,&border)?;
        self.surface=self.gfx.draw((WIDTH*dpi).ceil() as u32,(HEIGHT*dpi).ceil() as u32,|_,_,_|Ok(()))?;
        self.content.SetBrush(&self.gfx.compositor.CreateSurfaceBrushWithSurface(&self.surface)?)
    }
    pub unsafe fn display_changed(&mut self,work:RECT,dpi:f32)->Result<()> {
        if self.work==work && self.dpi==dpi {return Ok(());}
        let mut old=RECT::default();GetWindowRect(self.hwnd,&mut old)?;
        self.set_dpi(dpi)?;
        let size=(((WIDTH+2.*MARGIN)*dpi).ceil() as i32,((HEIGHT+2.*MARGIN)*dpi).ceil() as i32);
        let r=super::super::panels::clamp(work,size,(old.left,old.top));
        SetWindowPos(self.hwnd,None,r.left,r.top,size.0,size.1,SWP_NOZORDER|SWP_NOACTIVATE)?;
        self.work=work;Ok(())
    }
}
impl Drop for View {fn drop(&mut self){unsafe{let _=DestroyWindow(self.hwnd);}}}
/// The field under the list: the hint shows only while it is unfocused and
/// empty, so the caret never sits on top of it.
pub fn input_item(gfx:&Gfx,input:&Input<'_>,focused:bool)->Result<Item> {
    let hint=input.text.is_empty() && !focused;
    let mut text=Text::new(gfx,if hint{crate::i18n::t("chat.input_placeholder")}else{input.text},FIELD_W,4,Some(input.caret))?;
    text.width=FIELD_W;text.height+=4.;
    let h=text.height;Ok(Item{message_id:None,overflow_done:false,text,bounds:paint::rect(16.,HEIGHT-16.-h,FIELD_W,h),user:true,alpha:1.,scale:1.,depth:0,placeholder:hint})
}
const FIELD_W:f32=WIDTH-32.;
fn list_bottom(input: Option<&Item>) -> f32 {
    input.map_or(HEIGHT - 16., |item| item.bounds.top - 16.)
}
#[allow(clippy::too_many_arguments)]
unsafe fn paint_content<'a>(dc:&ID2D1DeviceContext,gfx:&Gfx,theme:&Theme,rows:impl Iterator<Item=&'a mut Item>,editing:Option<(&Item,&Input<'_>)>,blink:bool,focused:bool,scroll:f32,max_scroll:f32)->Result<()> {
    let light=theme.is_light;
    let ink=paint::color(if light{0.12}else{0.94},1.);
    let brush=dc.CreateSolidColorBrush(&ink,None)?;
    let(title,_,_)=gfx.text_layout(crate::i18n::t("chat.history"),crate::fonts::THEME_DEFAULT,14.,DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
    dc.DrawTextLayout(Vector2{X:16.,Y:18.},&title,&brush,D2D1_DRAW_TEXT_OPTIONS_NONE);
    if editing.is_some() {crate::glyphs::draw(dc,crate::glyphs::Glyph::Plus,WIDTH-62.,28.,16.,ink)?;}
    crate::glyphs::draw(dc,crate::glyphs::Glyph::Close,WIDTH-28.,28.,14.,ink)?;
    let separator=dc.CreateSolidColorBrush(&paint::color(if light{0.}else{1.},0.07),None)?;
    dc.DrawLine(Vector2{X:0.,Y:56.},Vector2{X:WIDTH,Y:56.},&separator,1.,None);
    let bottom=list_bottom(editing.map(|(item,_)|item));
    let mut y=72.-max_scroll+scroll;
    dc.PushAxisAlignedClip(&paint::rect(0.,57.,WIDTH,(bottom-57.).max(0.)),D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    let mut previous:Option<bool>=None;
    let outline=dc.CreateSolidColorBrush(&paint::color(if light{0.}else{1.},0.13),None)?;
    let radius=super::bubbles::RADIUS-0.5;
    let result=(||->Result<()> {
        for row in rows {
            // Same speaker: close together; a new speaker: a little air.
            y+=row_gap(previous,row.user);
            previous=Some(row.user);
            row.bounds.top=y;row.bounds.bottom=y+row.text.height;
            if row.bounds.bottom>57. && y<bottom {
                paint::item(dc,row,None,false,theme)?;
                let b=row.bounds;
                dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT {
                    rect: paint::rect(b.left+0.5,b.top+0.5,b.right-b.left-1.,b.bottom-b.top-1.),
                    radiusX: radius,
                    radiusY: radius,
                },&outline,1.,None);
            }
            y+=row.text.height;
        }Ok(())
    })();dc.PopAxisAlignedClip();result?;
    let Some((input_item,input)) = editing else {return Ok(());};
    // A quiet field in the window's own tone, not a white box.
    let field=input_item.bounds;
    paint::rounded(dc,field,10.,paint::color(if light{0.}else{1.},if light{0.05}else{0.08}))?;
    if focused {
        let ring=dc.CreateSolidColorBrush(&paint::color(if light{0.}else{1.},0.22),None)?;
        dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT{rect:paint::rect(field.left+0.5,field.top+0.5,field.right-field.left-1.,field.bottom-field.top-1.),radiusX:9.5,radiusY:9.5},&ring,1.,None);
    }
    let mut old=Matrix3x2::identity();dc.GetTransform(&mut old);
    dc.SetTransform(&(Matrix3x2::translation(field.left,field.top+2.)*old));
    let text_ink=if input_item.placeholder{paint::color(if light{0.12}else{0.94},0.45)}else{ink};
    let drawn=paint::body(dc,input_item,Some(input).filter(|_|focused),blink,text_ink,true);
    dc.SetTransform(&old);drawn
}

fn rows<'a>(gfx:&Gfx,turns:impl Iterator<Item=(&'a str,&'a str)>)->Result<Vec<Item>> {
    let mut rows=Vec::new();
    for (u,a) in turns {
        for (s,user) in [(u,true),(a,false)] {
            if s.is_empty(){continue;}
            let text=Text::new(gfx,s,272.,u32::MAX,None)?;
            let x=if user{WIDTH-16.-text.width}else{16.};
            let h=text.height;let w=text.width;
            rows.push(Item{message_id:None,overflow_done:false,text,bounds:paint::rect(x,0.,w,h),user,alpha:1.,scale:1.,depth:0,placeholder:false});
        }
    }
    Ok(rows)
}
fn row_gap(previous:Option<bool>,user:bool)->f32 {previous.map_or(0.,|p|if p==user{6.}else{12.})}
fn content_height<'a>(rows:impl Iterator<Item=&'a Item>)->f32 {
    let mut previous=None;
    rows.map(|r|{let gap=row_gap(previous,r.user);previous=Some(r.user);r.text.height+gap}).sum()
}

#[cfg(test)] mod tests {
    use super::*;
    unsafe extern "system" fn hidden_proc(h:HWND,m:u32,w:WPARAM,l:LPARAM)->LRESULT {DefWindowProcW(h,m,w,l)}
    #[test]
    fn reveal_ids_and_scroll_both_ends_in_hidden_history() {
        let stage=crate::gfx::promo_kit::stage();let gfx=stage.gfx.clone();
        let mut view=unsafe{View::new(gfx,Theme::graphite(),1.,RECT{left:0,top:0,right:1000,bottom:1000},Some(hidden_proc)).unwrap()};
        view.motion=false;
        let input=Input{text:"",caret:0,selection:(0,0),composition:None};
        let mut b=super::super::bubbles::Bubbles::default();let mut h=History::default();
        b.say("같은 답".into(),0.);let first=b.current.clone().unwrap();
        h.push("question".into(),first.text.clone());h.link_last(None,Some(first.id));
        for _ in 0..20 {h.push("question".into(),"answer".into());}
        b.say("같은 답".into(),1.);let last=b.current.clone().unwrap();
        h.push("question".into(),last.text.clone());h.link_last(None,Some(last.id));
        view.reveal(first.clone());view.draw(&h,None,Some(&input),false,false).unwrap();
        assert_eq!(view.rows.iter().find(|r|r.message_id==Some(first.id)).unwrap().bounds.top,72.);
        view.reveal(last.clone());view.draw(&h,None,Some(&input),false,false).unwrap();
        let row=view.rows.iter().find(|r|r.message_id==Some(last.id)).unwrap();
        assert!(row.bounds.top>=57. && row.bounds.bottom<=list_bottom(view.input.as_ref()));
        view.wheel(10000.);view.draw(&h,None,Some(&input),false,false).unwrap();assert_eq!(view.rows[0].bounds.top,72.);
        view.wheel(10000.);view.wheel(-1.);view.draw(&h,None,Some(&input),false,false).unwrap();assert_eq!(view.rows[0].bounds.top,12.);
        view.wheel(-10000.);view.draw(&h,None,Some(&input),false,false).unwrap();
        assert_eq!(view.rows.last().unwrap().bounds.bottom,list_bottom(view.input.as_ref()));
        b.say("긴 메시지\n".repeat(80),2.);let long=b.current.clone().unwrap();
        h.push(String::new(),long.text.clone());h.link_last(None,Some(long.id));
        view.reveal(long.clone());view.draw(&h,None,Some(&input),false,false).unwrap();
        assert_eq!(view.rows.last().unwrap().bounds.top,72.);
        view.wheel(-10000.);view.draw(&h,None,Some(&input),false,false).unwrap();
        assert_eq!(view.rows.last().unwrap().bounds.bottom,list_bottom(view.input.as_ref()));
        // Cancelled/unpersisted messages are still readable without mutating saved history.
        b.say("취소된 답".into(),3.);view.reveal(b.current.clone().unwrap());
        view.draw(&h,None,Some(&input),false,false).unwrap();assert_eq!(view.extra.len(),1);
    }
}
