//! Identical drawing and hit geometry for HWNDs and offscreen inspection.
use super::bubbles::{self, Bubbles, Phase, BASE, LINE, PAD_X, PAD_Y, RADIUS, WIDTH};
use crate::{desklayer::card as glass, gfx::Gfx, theme::Theme};
use windows::{core::Result, Win32::Graphics::{Direct2D::{Common::*, *}, DirectWrite::*}};
use windows_numerics::{Matrix3x2, Vector2};

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left:x, top:y, right:x+w, bottom:y+h }
}
pub fn contains(r: D2D_RECT_F, x:f32, y:f32) -> bool { x>=r.left && x<r.right && y>=r.top && y<r.bottom }
pub fn color(v:f32, a:f32) -> D2D1_COLOR_F { D2D1_COLOR_F {r:v,g:v,b:v,a} }

pub struct Text {
    pub layout: IDWriteTextLayout,
    pub width: f32,
    pub height: f32,
    pub scroll: f32,
    pub max_scroll: f32,
}
impl Text {
    pub fn new(gfx:&Gfx, text:&str, max:f32, lines:u32, caret:Option<u32>) -> Result<Self> {
        unsafe {
            let (layout, _, _) = gfx.text_layout(text, crate::fonts::THEME_DEFAULT, 14., DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
            // Words stay whole; only a word longer than the line breaks.
            layout.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
            layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            layout.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, LINE, BASE)?;
            layout.SetMaxWidth((max-2.*PAD_X).max(14.))?;
            layout.SetMaxHeight(1_000_000.)?;
            let mut m = DWRITE_TEXT_METRICS::default(); layout.GetMetrics(&mut m)?;
            // Equal insets left and right; an empty input is just its caret.
            let width = (m.widthIncludingTrailingWhitespace.ceil()+2.*PAD_X).clamp(2.*PAD_X+2.,max);
            let height = (m.lineCount.max(1).min(lines) as f32 * LINE)+2.*PAD_Y;
            let max_scroll = (m.height-(height-2.*PAD_Y)).max(0.);
            let mut scroll = max_scroll;
            if let Some(at) = caret {
                let (mut x,mut y,mut hit)=(0.,0.,DWRITE_HIT_TEST_METRICS::default());
                layout.HitTestTextPosition(at,false,&mut x,&mut y,&mut hit)?;
                scroll = (y+LINE-(height-2.*PAD_Y)).max(0.);
            }
            Ok(Self {layout,width,height,scroll,max_scroll})
        }
    }
    pub fn caret(&self, at:u32) -> (f32,f32) { unsafe {
        let(mut x,mut y,mut m)=(0.,0.,DWRITE_HIT_TEST_METRICS::default());
        let _=self.layout.HitTestTextPosition(at,false,&mut x,&mut y,&mut m);
        (x,y-self.scroll)
    }}
    pub fn hit(&self,x:f32,y:f32)->u32 { unsafe {
        let(mut trailing,mut inside,mut m)=(Default::default(),Default::default(),DWRITE_HIT_TEST_METRICS::default());
        let _=self.layout.HitTestPoint(x,y+self.scroll,&mut trailing,&mut inside,&mut m);
        m.textPosition+if trailing.as_bool(){m.length}else{0}
    }}
}
pub struct Item {
    pub message_id: Option<u64>,
    pub overflow_done: bool,
    pub text: Text,
    pub bounds: D2D_RECT_F,
    pub user: bool,
    pub alpha: f32,
    pub scale: f32,
    pub depth: usize,
    pub placeholder: bool,
}
pub struct Input<'a> {pub text:&'a str,pub caret:u32,pub selection:(u32,u32),pub composition:Option<(u32,u32)>}
pub fn hit_item(it:&Item,x:f32,y:f32)->bool {
    if it.alpha<=0.004 || it.depth>=3 || !contains(it.bounds,x,y) {return false;}
    let r=RADIUS*it.scale;
    let cx=x.clamp(it.bounds.left+r,it.bounds.right-r);
    let cy=y.clamp(it.bounds.top+r,it.bounds.bottom-r);
    (x-cx).hypot(y-cy)<=r
}

/// Bubble layout at `now`: the input or answer just above the hat tip,
/// older ones stacked over it. Motion (all ease-out):
/// - a new bubble pops up from 6 DIP lower, 0.96 to 1 scale, fading in;
/// - when the stack moves a level, older bubbles slide into their new place
///   and shrink and fade to their new level's style;
/// - the bubble pushed past the last level drifts up and fades away;
/// - at the end the whole stack drifts up and fades, topmost first.
/// Every edge lands on a device pixel (`dpi`) so text stays crisp.
#[allow(clippy::too_many_arguments)]
pub fn arrange(gfx:&Gfx,b:&Bubbles,input:&Input<'_>,tip:(f32,f32),work:D2D_RECT_F,now:f64,motion:bool,dpi:f32)->Result<Vec<Item>> {
    if b.phase==Phase::Hidden {return Ok(Vec::new());}
    let snap=|v:f32|(v*dpi).round()/dpi;
    let max=WIDTH.min((work.right-work.left-8.).max(2.*PAD_X+2.));
    // Older bubbles make room first; the new one pops in a beat later.
    let slide=if motion {bubbles::ease((now-b.entered)/0.22)} else {1.};
    let pop=if motion {bubbles::ease((now-b.entered-0.06)/0.18)} else {1.};
    let level=if motion {bubbles::ease((now-b.shifted)/0.26)} else {1.};
    let current=if b.phase==Phase::Input {Some((input.text,true,true,None))}
        else {b.current.as_ref().map(|m|(m.text.as_str(),m.user,false,Some(m)))};
    let has_current=current.is_some();
    let mut rows:Vec<_>=current.into_iter().map(|(t,u,e,m)|(0,t,u,e,m)).collect();
    rows.extend(b.previous.iter().enumerate().map(|(i,m)|(i+1,m.text.as_str(),m.user,false,Some(m))));
    if let Some((m,_))=&b.evicted {rows.push((3,m.text.as_str(),m.user,false,Some(m)));}
    // The exit: per bubble progress, topmost first.
    let deepest=rows.iter().map(|r|r.0).filter(|d|*d<3).max().unwrap_or(0);
    let exit=|depth:usize|->f32 {
        let Some(deadline)=b.deadline().filter(|_|!b.hovered) else {return 0.};
        if !motion {return f32::from(now>=deadline);}
        let delay=(deepest.saturating_sub(depth)) as f64*bubbles::STAGGER;
        bubbles::ease((now-deadline-delay)/bubbles::FADE_ONE)
    };
    let mut items=Vec::new();
    let mut bottom=tip.1-bubbles::GAP;
    if b.phase==Phase::Thinking {bottom-=8.*if motion{bubbles::ease((now-b.entered)/0.26)}else{1.};}
    let mut current_h=0.;
    for (depth,s,user,editing,message) in rows {
        let mut text=Text::new(gfx,s,max,4,editing.then_some(input.caret))?;
        if b.reminder {unsafe {
            let range=DWRITE_TEXT_RANGE{startPosition:0,length:s.encode_utf16().count() as u32};
            text.layout.SetFontSize(15.,range)?;text.layout.SetFontWeight(DWRITE_FONT_WEIGHT_NORMAL,range)?;
            let mut m=DWRITE_TEXT_METRICS::default();text.layout.GetMetrics(&mut m)?;
            text.width=(m.widthIncludingTrailingWhitespace.ceil()+2.*PAD_X).clamp(2.*PAD_X+2.,max);
            text.height=m.lineCount.clamp(1, 4) as f32*LINE+2.*PAD_Y;
        }}
        if let Some(scroll)=message.and_then(|m|m.scroll.as_ref()) {text.scroll=scroll.value(now,motion).clamp(0.,text.max_scroll);}
        let overflow_done=!editing && text.max_scroll>0. && (depth>0 || b.finished.is_some());
        let (scale,a,lift);
        if depth==0 {
            scale=0.96+0.04*pop; a=pop; lift=6.*(1.-pop);
        } else if depth==3 {
            let t=if motion{bubbles::ease((now-b.evicted.as_ref().map_or(now,|e|e.1))/0.34)}else{1.};
            let (s0,a0)=bubbles::stack_style(2);
            scale=s0-0.06*t; a=a0*(1.-t); lift=-14.*t;
        } else {
            let (s0,a0)=bubbles::stack_style(depth-1);let (s1,a1)=bubbles::stack_style(depth);
            scale=s0+(s1-s0)*level; a=a0+(a1-a0)*level;
            // Older bubbles start where the new one now stands and slide up into place.
            lift=if has_current {(1.-slide)*(current_h+7.)} else {0.};
        }
        let out=exit(depth.min(2));
        let w=text.width*scale; let h=text.height*scale;
        let x=snap((tip.0-w/2.).clamp(work.left+4.,(work.right-w-4.).max(work.left+4.)));
        let y=snap(bottom-h+lift-10.*out);
        if depth==0 {current_h=h;}
        items.push(Item{message_id:message.map(|m|m.id),overflow_done,text,bounds:rect(x,y,w,h),user,alpha:a*(1.-out),scale,depth,placeholder:editing&&s.is_empty()});
        if depth<3 {bottom=bottom-h-if depth==0{7.}else{6.};}
    }
    // Clamp the stack as one group so its older messages never overlap at the top edge.
    if let Some(top)=items.iter().map(|i|i.bounds.top).reduce(f32::min) {
        let shift=snap((work.top+4.-top).max(0.));
        for item in &mut items {item.bounds.top+=shift;item.bounds.bottom+=shift;}
    }
    Ok(items)
}
pub unsafe fn rounded(dc:&ID2D1DeviceContext,r:D2D_RECT_F,radius:f32,c:D2D1_COLOR_F)->Result<()> {
    let brush=dc.CreateSolidColorBrush(&c,None)?;
    dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT{rect:r,radiusX:radius,radiusY:radius},&brush); Ok(())
}
pub unsafe fn item(dc:&ID2D1DeviceContext,it:&Item,input:Option<&Input<'_>>,blink:bool,theme:&Theme)->Result<()> {
    if it.alpha<=0.004 {return Ok(());}
    let mut old=Matrix3x2::identity();dc.GetTransform(&mut old);
    let transform=super::super::render::scale(it.scale,it.scale)*super::super::render::translate(it.bounds.left,it.bounds.top)*old;
    dc.SetTransform(&transform);
    let (w,h)=(it.text.width,it.text.height);
    let result=(||->Result<()> {
        // Soft two-layer drop shadow: a tight contact shade and a wide ambient one.
        rounded(dc,rect(-3.,-1.,w+6.,h+6.),RADIUS+3.,color(0.,0.045*it.alpha))?;
        rounded(dc,rect(-1.,0.5,w+2.,h+2.),RADIUS+1.,color(0.,0.10*it.alpha))?;
        let mut fill=glass::rgba(glass::surface(theme.is_light));
        fill.a*=it.alpha;
        rounded(dc,rect(0.,0.,w,h),RADIUS,fill)?;
        let mut ink=glass::rgba(glass::ink(theme,if it.user {1} else {2}));
        ink.a*=it.alpha;
        body(dc,it,input,blink,ink,!it.placeholder)?;
        if input.is_none() && it.text.max_scroll>0. {
            // Fade only the edge with hidden text. The solid end meets the inset.
            for (hidden,y,reverse) in [(it.text.scroll>0.01,PAD_Y,false),(it.text.scroll<it.text.max_scroll-0.01,h-PAD_Y-10.,true)] {
                if !hidden {continue;}
                let mut clear=fill;clear.a=0.;
                let stops=[D2D1_GRADIENT_STOP{position:0.,color:if reverse{clear}else{fill}},D2D1_GRADIENT_STOP{position:1.,color:if reverse{fill}else{clear}}];
                let target:ID2D1RenderTarget=windows::core::Interface::cast(dc)?;
                let stops=target.CreateGradientStopCollection(&stops,D2D1_GAMMA_2_2,D2D1_EXTEND_MODE_CLAMP)?;
                let brush=dc.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES{startPoint:Vector2{X:0.,Y:y},endPoint:Vector2{X:0.,Y:y+10.}},None,&stops)?;
                dc.FillRectangle(&rect(PAD_X-2.,y,w-2.*PAD_X+4.,10.),&brush);
            }
        }
        if it.overflow_done {
            let mut dot=ink;dot.a*=0.4;let brush=dc.CreateSolidColorBrush(&dot,None)?;
            for dx in [-5.,0.,5.] {dc.FillEllipse(&D2D1_ELLIPSE{point:Vector2{X:w/2.+dx,Y:h-PAD_Y/2.},radiusX:1.25,radiusY:1.25},&brush);}
        }
        Ok(())
    })();dc.SetTransform(&old);result
}
/// The text, selection, IME underline and caret inside a bubble or field
/// whose top left is the current origin.
pub unsafe fn body(dc:&ID2D1DeviceContext,it:&Item,input:Option<&Input<'_>>,blink:bool,ink:D2D1_COLOR_F,text:bool)->Result<()> {
    let (w,h)=(it.text.width,it.text.height);
    dc.PushAxisAlignedClip(&rect(PAD_X-2.,PAD_Y,w-2.*PAD_X+4.,h-2.*PAD_Y),D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    let drawn=(||->Result<()> {
        let ink=dc.CreateSolidColorBrush(&ink,None)?;
        if let Some(input)=input {
            let selection=if input.composition.is_some(){(0,0)}else{input.selection};
            for (range,composition) in [(selection,false),(input.composition.unwrap_or((0,0)),true)] {
                if range.1>range.0 {
                    let mut count=0;
                    let _=it.text.layout.HitTestTextRange(range.0,range.1-range.0,PAD_X,PAD_Y-it.text.scroll,None,&mut count);
                    let mut boxes=vec![DWRITE_HIT_TEST_METRICS::default();count as usize];
                    it.text.layout.HitTestTextRange(range.0,range.1-range.0,PAD_X,PAD_Y-it.text.scroll,Some(&mut boxes),&mut count)?;
                    for r in boxes {if composition {
                        dc.DrawLine(Vector2{X:r.left,Y:r.top+LINE-2.},Vector2{X:r.left+r.width,Y:r.top+LINE-2.},&ink,1.,None);
                    }else {rounded(dc,rect(r.left,r.top+1.,r.width,LINE-2.),2.,color(0.5,0.25*it.alpha))?;}}
                }
            }
        }
        if text {
            dc.DrawTextLayout(Vector2{X:PAD_X,Y:PAD_Y-it.text.scroll},&it.text.layout,&ink,D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
        if let Some(input)=input.filter(|_|blink) {
            let(x,y)=it.text.caret(input.caret);
            // Same height as the glyphs, centred on the line.
            dc.FillRectangle(&rect(PAD_X+x-0.5,PAD_Y+y+3.,1.5,LINE-6.),&ink);
        }
        Ok(())
    })();dc.PopAxisAlignedClip();drawn
}
pub unsafe fn spinner(dc:&ID2D1DeviceContext,foot:(f32,f32),scale:f32,now:f64,motion:bool,theme:&Theme)->Result<()> {
    let(p,r,dot)=bubbles::spinner(foot,scale);
    let head=if motion{(now/0.9*8.).floor() as usize%8}else{0};
    let ink=glass::rgba(glass::ink(theme,1));
    for i in 0..8 {
        let angle=i as f32*std::f32::consts::TAU/8.-std::f32::consts::FRAC_PI_2;
        let trail=(head+8-i)%8;
        let brush=dc.CreateSolidColorBrush(&D2D1_COLOR_F{a:1.-0.78*trail as f32/7.,..ink},None)?;
        dc.FillEllipse(&D2D1_ELLIPSE{point:Vector2{X:p.0+angle.cos()*r,Y:p.1+angle.sin()*r},radiusX:dot,radiusY:dot},&brush);
    }Ok(())
}

/// Position the entire chat stack inward from a docked hat tip.
pub(crate) fn place_edge(items:&mut [Item],tip:(f32,f32),work:D2D_RECT_F,edge:Option<super::super::peek::Edge>) {
    use super::super::peek::Edge;
    let Some(edge)=edge else{return;};
    let Some(first)=items.first() else{return;};
    let dx=match edge {Edge::Bottom=>0.,Edge::Left=>tip.0+bubbles::GAP-first.bounds.left,Edge::Right=>tip.0-bubbles::GAP-first.bounds.right};
    let dy=if edge==Edge::Bottom {0.}else{tip.1-(first.bounds.top+first.bounds.bottom)/2.};
    for it in items.iter_mut() {it.bounds.left+=dx;it.bounds.right+=dx;it.bounds.top+=dy;it.bounds.bottom+=dy;}
    let Some(top)=items.iter().map(|i|i.bounds.top).reduce(f32::min)else{return;};
    let bottom=items.iter().map(|i|i.bounds.bottom).reduce(f32::max).unwrap();
    let shift=(work.top+4.-top).max(0.)-(bottom-work.bottom+4.).max(0.);
    for it in items {let dx=(work.left+4.-it.bounds.left).max(0.)-(it.bounds.right-work.right+4.).max(0.);it.bounds.left+=dx;it.bounds.right+=dx;it.bounds.top+=shift;it.bounds.bottom+=shift;}
}

/// Keep the existing spinner painter inside the work area for a docked gnome.
pub(crate) fn spinner_foot(foot:(f32,f32),tip:(f32,f32),scale:f32,work:D2D_RECT_F,edge:Option<super::super::peek::Edge>)->(f32,f32) {
    let Some(edge)=edge else{return foot;};
    let (nx,ny)=edge.inward();let (_,r,d)=bubbles::spinner((0.,0.),scale);let inset=r+d+4.;
    let x=(tip.0+nx*(inset+8.)).clamp(work.left+inset,(work.right-inset).max(work.left+inset));
    let y=(tip.1+ny*(inset+8.)).clamp(work.top+inset,(work.bottom-inset).max(work.top+inset));
    (x-1.2*scale,y+206.7*scale)
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn overflow_edges_manual_follow_and_hit_geometry() {
        let stage=crate::gfx::promo_kit::stage();let gfx=&stage.gfx;
        let mut b=Bubbles::default();b.delta(&"가나다 ABC\n".repeat(11),0.);
        let input=Input{text:"",caret:0,selection:(0,0),composition:None};
        let arrange=|b:&Bubbles,now:f64|super::arrange(gfx,b,&input,(200.,300.),rect(0.,0.,400.,480.),now,true,1.).unwrap();
        let items=arrange(&b,1.);let it=&items[0];
        assert_eq!(it.text.max_scroll,160.);assert_eq!(it.text.scroll,160.);assert!(!it.overflow_done);
        assert!(!hit_item(it,it.bounds.left,it.bounds.top));
        assert!(hit_item(it,it.bounds.left+20.,it.bounds.top+20.));
        b.current.as_mut().unwrap().wheel(4.,160.,160.,1.,false);
        b.delta("new bottom",2.);let items=arrange(&b,2.);assert_eq!(items[0].text.scroll,80.);
        b.finish(3.);let items=arrange(&b,3.);assert!(items[0].overflow_done);
        b.input(4.);let items=arrange(&b,5.);assert_eq!(items[1].text.scroll,80.);
    }
    #[test] fn actual_font_width_wrap_and_tip_gap() {
        unsafe {let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
        let stage=crate::gfx::promo_kit::stage();let gfx=stage.gfx.clone();
        let a=Text::new(&gfx,"가",WIDTH,4,None).unwrap();let b=Text::new(&gfx,"가나다",WIDTH,4,None).unwrap();assert!(a.width<b.width);
        let long=Text::new(&gfx,&"긴 입력 문자열 ".repeat(40),WIDTH,4,None).unwrap();
        assert!(long.width<=280.);assert_eq!(long.height,LINE*4.+2.*PAD_Y);assert!(long.scroll>0.);
        let mut bubbles=Bubbles::default();bubbles.input(0.);
        let input=Input{text:"오늘 뭐 할까",caret:0,selection:(0,0),composition:None};
        let items=arrange(&gfx,&bubbles,&input,(498.,300.),rect(0.,0.,500.,600.),1.,true,1.).unwrap();
        assert_eq!(items[0].bounds.bottom,292.);assert!(items[0].bounds.right<=496.);
        bubbles.say("older".into(),1.);bubbles.input(2.);bubbles.submit("question".into(),3.);bubbles.delta("answer",4.);
        let items=arrange(&gfx,&bubbles,&input,(-498.,-190.),rect(-800.,-200.,800.,600.),4.5,true,1.).unwrap();
        assert_eq!(items.len(),3);assert_eq!(items[1].alpha,0.55);assert_eq!(items[2].alpha,0.28);
        assert!(items.iter().all(|i|i.bounds.top>=-196.));
        assert!(items[2].bounds.bottom<items[1].bounds.top && items[1].bounds.bottom<items[0].bounds.top);
    }
}
