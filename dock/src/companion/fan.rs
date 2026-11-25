//! Pure compact fan geometry and pointer semantics. All coordinates are DIPs.
use super::{grab::Kind, timing};
#[derive(Clone,Copy,Debug,Default,PartialEq)]
pub struct Point {pub x:f32,pub y:f32}
impl Point {pub fn distance(self,p:Self)->f32 {(self.x-p.x).hypot(self.y-p.y)}}
#[derive(Clone,Copy,Debug,Default,PartialEq)]
pub struct Rect {pub left:f32,pub top:f32,pub right:f32,pub bottom:f32}
impl Rect {
 pub fn normalized(self)->Option<Self> { [self.left,self.top,self.right,self.bottom].iter().all(|v|v.is_finite()).then_some(Self{left:self.left.min(self.right),top:self.top.min(self.bottom),right:self.left.max(self.right),bottom:self.top.max(self.bottom)}) }
 pub fn overlaps(self,r:Self)->bool {self.left<=r.right&&self.right>=r.left&&self.top<=r.bottom&&self.bottom>=r.top}
 pub fn contains(self,p:Point)->bool {p.x>=self.left&&p.x<=self.right&&p.y>=self.top&&p.y<=self.bottom}
 pub fn distance(self,p:Point)->f32 {let Some(r)=self.normalized()else{return f32::INFINITY;};p.distance(Point{x:p.x.clamp(r.left,r.right),y:p.y.clamp(r.top,r.bottom)})}
 pub fn inset(self,n:f32)->Self {Self{left:self.left+n,top:self.top+n,right:self.right-n,bottom:self.bottom-n}}
 fn includes(self,r:Self)->bool {r.left>=self.left&&r.right<=self.right&&r.top>=self.top&&r.bottom<=self.bottom}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Id {Hide,Rest,Size,Detach,NewNote,Settings,Smaller,Larger,Rest30,Rest60,Morning}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub enum Expanded {#[default] None,Size,Rest}
#[derive(Clone,Debug,PartialEq)]
pub struct Button {pub id:Id,pub center:Point,pub radius:f32,pub angle:f32}
#[derive(Clone,Debug,PartialEq)]
pub struct Arc {pub center:Point,pub radius:f32,pub middle:f32,pub span:f32,pub width:f32}
fn polar(c:Point,r:f32,angle:f32)->Point {let a=angle.to_radians();Point{x:c.x+r*a.cos(),y:c.y-r*a.sin()}}
fn delta(a:f32,b:f32)->f32 {(a-b+180.).rem_euclid(360.)-180.}
impl Arc {
 pub fn points(&self)->Vec<Point> {let n=self.span.ceil() as usize;(0..=n).map(|i|polar(self.center,self.radius,self.middle-self.span/2.+self.span*i as f32/n as f32)).collect()}
 pub fn contains(&self,p:Point)->bool {
  let a=(self.center.y-p.y).atan2(p.x-self.center.x).to_degrees();
  let q=polar(self.center,self.radius,self.middle+delta(a,self.middle).clamp(-self.span/2.,self.span/2.));
  p.distance(q)<=self.width/2.+0.75
 }
 pub fn bounds(&self)->Rect {let mut r=Rect{left:f32::INFINITY,top:f32::INFINITY,right:f32::NEG_INFINITY,bottom:f32::NEG_INFINITY};
  for a in [self.middle-self.span/2.,self.middle+self.span/2.,0.,90.,180.,270.] {if a!=self.middle-self.span/2.&&a!=self.middle+self.span/2.&&delta(a,self.middle).abs()>self.span/2.{continue;}let p=polar(self.center,self.radius,a);r.left=r.left.min(p.x-self.width/2.);r.top=r.top.min(p.y-self.width/2.);r.right=r.right.max(p.x+self.width/2.);r.bottom=r.bottom.max(p.y+self.width/2.);}r
 }
}
#[derive(Clone,Debug,PartialEq)]
pub struct Layout {pub tip:Point,pub center:Point,pub radius:f32,pub buttons:Vec<Button>,pub arcs:Vec<Arc>,pub bounds:Rect}
#[derive(Clone,Copy,Debug,PartialEq)]
pub enum Hit {Button(Id),Arc,Outside}
fn shape_bounds(shape:&[Rect])->Rect {shape.iter().fold(Rect{left:f32::INFINITY,top:f32::INFINITY,right:f32::NEG_INFINITY,bottom:f32::NEG_INFINITY},|a,b|Rect{left:a.left.min(b.left),top:a.top.min(b.top),right:a.right.max(b.right),bottom:a.bottom.max(b.bottom)})}
/// Expand alpha runs only along axes smaller than the minimum input target.
pub fn dilate(shape:&[Rect],minimum:f32)->Vec<Rect> {
 let bounds=shape.iter().fold(Rect{left:f32::INFINITY,top:f32::INFINITY,right:f32::NEG_INFINITY,bottom:f32::NEG_INFINITY},|a,b|Rect{left:a.left.min(b.left),top:a.top.min(b.top),right:a.right.max(b.right),bottom:a.bottom.max(b.bottom)});
 let dx=((minimum-(bounds.right-bounds.left))/2.).max(0.);
 let dy=((minimum-(bounds.bottom-bounds.top))/2.).max(0.);
 shape.iter().map(|r|Rect{left:r.left-dx,top:r.top-dy,right:r.right+dx,bottom:r.bottom+dy}).collect()
}
impl Layout {
 /// Translate the fan, not its animation origin or the character silhouette.
 fn shifted(&self,dx:f32)->Self {
  let mut p=self.clone();p.center.x+=dx;p.bounds.left+=dx;p.bounds.right+=dx;
  for b in &mut p.buttons{b.center.x+=dx;}for a in &mut p.arcs{a.center.x+=dx;}p
 }
 pub fn hit(&self,p:Point)->Hit {
  if !self.bounds.inset(-0.75).contains(p){return Hit::Outside;}
  if let Some(b)=self.buttons.iter().rev().find(|b|b.center.distance(p)<=b.radius+0.75){return Hit::Button(b.id);}
  if self.arcs.iter().any(|a|a.contains(p)){Hit::Arc}else{Hit::Outside}
 }
 pub fn presentation(&self,t:f32,closing:Option<f32>,enabled:bool,focus:Option<Id>,hover:f32)->Self {
  let mut out=self.clone();out.buttons=self.buttons.iter().enumerate().filter_map(|(i,b)|{
   let (progress,alpha)=animation(t,i.min(4),closing,enabled);if alpha<=0.{return None;}
   let mut b=b.clone();b.center=Point{x:self.tip.x+(b.center.x-self.tip.x)*progress,y:self.tip.y+(b.center.y-self.tip.y)*progress};
   if focus==Some(b.id){b.radius*=hover;}Some(b)
  }).collect();out
 }
 /// Place the measured common tooltip outside every button, including at edges.
 pub fn tooltip_rect(&self,id:Id,w:f32,h:f32,work:Rect)->Option<Rect> {
  let b=self.buttons.iter().find(|b|b.id==id)?;let work=work.inset(8.);
  if w>work.right-work.left||h>work.bottom-work.top{return None;}
  // An expanded satellite arc can block the nearest label positions. Move
  // only the tooltip outward; menu radii and button spacing remain unchanged.
  for extra in 0..=32 {for angle in [b.angle,90.,270.,180.,0.,45.,135.,225.,315.] {
   let a=angle.to_radians();let dx=a.cos();let dy=-a.sin();
   let distance=b.radius*1.08+8.+dx.abs()*w/2.+dy.abs()*h/2.+extra as f32*8.;
   let left=(b.center.x+dx*distance-w/2.).clamp(work.left,work.right-w);
   let top=(b.center.y+dy*distance-h/2.).clamp(work.top,work.bottom-h);
   let r=Rect{left,top,right:left+w,bottom:top+h};
   if self.buttons.iter().all(|b|r.distance(b.center)>=b.radius*1.08+4.){return Some(r);}
  }}None
 }
 #[cfg(test)]
 fn valid(&self,work:Rect,forbidden:&[Rect],silhouette:&[Rect])->bool {self.valid_bounds(work,forbidden,silhouette,shape_bounds(silhouette))}
 fn valid_bounds(&self,work:Rect,forbidden:&[Rect],silhouette:&[Rect],shape_box:Rect)->bool {
  let work=work.inset(8.);
  for (i,b) in self.buttons.iter().enumerate(){
   let r=Rect{left:b.center.x-b.radius,top:b.center.y-b.radius,right:b.center.x+b.radius,bottom:b.center.y+b.radius};
   if !work.includes(r)||forbidden.iter().any(|a|a.overlaps(r.inset(-1.))&&a.distance(b.center)<b.radius+1.)||shape_box.overlaps(r.inset(-6.))&&silhouette.iter().any(|a|a.overlaps(r.inset(-6.))&&a.distance(b.center)<b.radius+6.) {return false;}
   if self.buttons[..i].iter().any(|a|a.center.distance(b.center)+0.001<a.radius+b.radius+10.){return false;}
  }
  for arc in &self.arcs {
   if !work.includes(arc.bounds()){return false;}
   // A one-degree sample plus 1 DIP covers the small chord error, including caps.
   let bounds=arc.bounds().inset(-1.);
   let shape=if shape_box.overlaps(bounds){silhouette}else{&[]};
   let obstacles:Vec<_>=forbidden.iter().chain(shape).filter(|r|r.overlaps(bounds)).collect();
   // Horizontal translation can put the rounded arc cap over the hat even
   // when its button clears the silhouette. Keep that painted cap off it too.
   if !obstacles.is_empty()&&arc.points().iter().any(|&p|obstacles.iter().any(|r|r.distance(p)<=arc.width/2.+1.)){return false;}
  }true
 }
 pub fn obstructed(&self,forbidden:&[Rect])->bool {forbidden.iter().filter_map(|r|r.normalized()).any(|r| {
  self.bounds.inset(-1.).overlaps(r)&&(self.buttons.iter().any(|b|r.distance(b.center)<b.radius+1.)||self.arcs.iter().any(|a|a.bounds().inset(-1.).overlaps(r)&&a.points().iter().any(|&p|r.distance(p)<=a.width/2.+1.)))
 })}
}
#[allow(clippy::too_many_arguments)]
fn candidate(tip:Point,small:bool,diameter:f32,radius:f32,rotation:f32,step:f32,expanded:Expanded,satellite_distance:f32,notes:bool,edge:bool)->Layout {
 let center=Point{x:tip.x,y:tip.y+if small{24.}else{40.}};
 let mut buttons=Vec::new();
 let mut ids=vec![Id::Hide,Id::Rest,Id::Size];
 if edge {ids.push(Id::Detach);}
 if notes {ids.push(Id::NewNote);}
 ids.push(Id::Settings);
 let span = (ids.len()-1) as f32*step;
 for (i,&id) in ids.iter().enumerate(){let angle=90.+span/2.-i as f32*step+rotation;
  buttons.push(Button{id,center:polar(center,radius,angle),radius:diameter/2.,angle});}
 let mut arcs=vec![Arc{center,radius,middle:90.+rotation,span:span+28.,width:diameter+8.}];
 if expanded!=Expanded::None {
  let (parent,ids,angles,diam)=if expanded==Expanded::Size {(2,vec![Id::Smaller,Id::Larger],vec![12.,-12.],30.)}
    else {(1,vec![Id::Rest30,Id::Rest60,Id::Morning],vec![20.,0.,-20.],28.)};
  // Owner resolution: both satellite groups use +44 for mouse, +48 for touch/pen.
  // Keep the 10 DIP validation unchanged; no parent/child exception.
  let middle=buttons[parent].angle;let outer=radius+satellite_distance;
  let span=angles[0]*2.+28.;
  for (id,a) in ids.into_iter().zip(angles) {let angle=middle+a;buttons.push(Button{id,center:polar(center,outer,angle),radius:diam/2.,angle});}
  arcs.push(Arc{center,radius:outer,middle,span,width:diam+8.});
 }
 let mut bounds=arcs[0].bounds();for arc in &arcs[1..]{let r=arc.bounds();bounds.left=bounds.left.min(r.left);bounds.top=bounds.top.min(r.top);bounds.right=bounds.right.max(r.right);bounds.bottom=bounds.bottom.max(r.bottom);}
 Layout{tip,center,radius,buttons,arcs,bounds}
}
/// The alpha runs are the actual transformed silhouette, not its bounding box.
/// No solution is preferable to overlapping the dock or stealing outside input.
#[cfg(test)]
pub fn place(tip:Point,silhouette:&[Rect],small:bool,work:Rect,forbidden:&[Rect],kind:Kind,expanded:Expanded)->Option<Layout> {
 place_tools(tip,silhouette,small,work,forbidden,kind,expanded,false,false)
}
/// The same geometry, including the notes entry only while its widget is enabled.
#[allow(clippy::too_many_arguments)]
pub fn place_tools(tip:Point,silhouette:&[Rect],small:bool,work:Rect,forbidden:&[Rect],kind:Kind,expanded:Expanded,notes:bool,edge:bool)->Option<Layout> {
 let work=work.normalized()?;
 let forbidden:Vec<_>=forbidden.iter().filter_map(|r|r.normalized()).collect();
 let (base,diam)=if kind==Kind::Mouse{(80.,34.)}else{(92.,40.)};
 place_diameter(tip,silhouette,small,work,&forbidden,base,diam,expanded,if kind==Kind::Mouse{44.}else{48.},notes,edge)
}
/// One placement search's wall-clock budget on the UI thread. Unoptimized
/// geometry is much slower; release builds enforce the real budget.
fn ui_budget()->std::time::Duration{std::time::Duration::from_millis(if cfg!(debug_assertions){128}else{8})}
#[cfg(not(test))]
fn search_budget()->std::time::Duration{ui_budget()}
// Tests check placements, not wall time: on a loaded machine (shared CI
// runners) the UI budget would turn a valid layout into "no placement".
// The test that measures the budget asks for the real one.
#[cfg(test)]
thread_local!{static TEST_BUDGET:std::cell::Cell<Option<std::time::Duration>>=const{std::cell::Cell::new(None)};}
#[cfg(test)]
fn search_budget()->std::time::Duration{TEST_BUDGET.with(|b|b.get()).unwrap_or(std::time::Duration::from_secs(10))}
#[allow(clippy::too_many_arguments)]
fn place_diameter(tip:Point,shape:&[Rect],small:bool,work:Rect,forbidden:&[Rect],base:f32,diam:f32,expanded:Expanded,satellite_distance:f32,notes:bool,edge:bool)->Option<Layout> {
 // Bound impossible searches on the hook-owning UI thread. Never accept an
 // unsafe candidate when the budget expires; the caller dismisses the fan.
 let started=std::time::Instant::now();
 let budget=search_budget();
 let shape_box=shape_bounds(shape);
 let step=if notes{26.}else{32.};
 let base=base.max((diam+10.)/(2.*(step/2_f32).to_radians().sin()));
 let rotations=std::iter::once(0.).chain((1..=6).flat_map(|i|[-5.*i as f32,5.*i as f32])).collect::<Vec<_>>();
 let initial=candidate(tip,small,diam,base,0.,step,expanded,satellite_distance,notes,edge);
 let inside=work.inset(8.);let b=initial.bounds;
 // A horizontal edge alone must not turn the fan down the character's side.
 // Include the painted stroke and round caps, not just the arc centerline.
 let side_only=(b.left<inside.left||b.right>inside.right)&&b.top>=inside.top&&b.bottom<=inside.bottom
  &&!forbidden.iter().any(|r|r.left<b.right&&r.right>b.left&&r.top<b.bottom&&r.bottom>b.top);
 let direction=if tip.x<(work.left+work.right)/2.{1.}else{-1.};
 let fit=|radius:f32,step:f32|->Option<Layout>{
  if 2.*radius*(step/2.).to_radians().sin()+0.001<diam+10.{return None;}
  // Order: translate the upper fan, rotate at most 30 degrees, then lower fan.
  for lower in [0.,180.] {for &rotation in &rotations {
   let p=candidate(tip,small,diam,radius,lower+rotation,step,expanded,satellite_distance,notes,edge);
   if side_only&&p.arcs.iter().any(|a|a.bounds().bottom-tip.y>radius*0.3+0.001){continue;}
   let limit=radius*0.9;
   let needed=if direction>0.{(inside.left-p.bounds.left).max(0.)}else{(p.bounds.right-inside.right).max(0.)};
   let shifts=std::iter::once(0.).chain(std::iter::once(needed.min(limit)))
    .chain((1..=(limit/4.).ceil() as u32).map(|i|(i as f32*4.).min(limit)));
   for shift in shifts {if started.elapsed()>=budget{return None;}if shift+0.001<needed{continue;}let bounds=Rect{left:p.bounds.left+direction*shift,right:p.bounds.right+direction*shift,..p.bounds};if !inside.includes(bounds){continue;}let p=p.shifted(direction*shift);if p.valid_bounds(work,forbidden,shape,shape_box){return Some(p);}}
  }}None
 };
 if let Some(p)=fit(base,step){return Some(p);}
 // Radius growth is the last resort; retain the earlier compact-span fallback.
 let max_radius=if notes{160.}else{120.};
 for extra in 1..=20 {let radius=(base+extra as f32*4.).min(max_radius);
  if started.elapsed()>=budget{return None;}
  for step in [32.,30.,28.,26.,24.,22.,20.] {
   if notes && step>26. {continue;}
   if let Some(p)=fit(radius,step){return Some(p);}
  }
  if radius>=max_radius{break;}
 }None
}
#[derive(Clone,Copy)]
struct Press {id:Id,at:f64,kind:Kind}
#[derive(Default)]
pub struct Pointer {pub focus:Option<Id>,pub hover_at:f64,press:Option<Press>,held_tip:Option<Id>}
impl Pointer {
 pub fn hover(&mut self,hit:Hit,now:f64){let next=if let Hit::Button(id)=hit{Some(id)}else{None};if self.focus!=next{self.focus=next;self.hover_at=now;self.held_tip=None;}}
 pub fn down(&mut self,hit:Hit,now:f64,kind:Kind){self.hover(hit,now);self.press=self.focus.map(|id|Press{id,at:now,kind});self.held_tip=None;}
 pub fn up(&mut self,hit:Hit,now:f64)->Option<Id>{let p=self.press.take()?;
  if hit!=Hit::Button(p.id){return None;}
  if p.kind!=Kind::Mouse&&now-p.at>=timing::FAN_HOLD as f64 {self.held_tip=Some(p.id);return None;}Some(p.id)
 }
 pub fn cancel(&mut self){self.press=None;self.held_tip=None;self.focus=None;}
 pub fn tooltip(&self,now:f64,kind:Kind)->Option<Id>{
  if let Some(p)=self.press {return (p.kind!=Kind::Mouse&&self.focus==Some(p.id)&&now-p.at>=timing::FAN_HOLD as f64).then_some(p.id);}
  self.held_tip.or_else(||(kind==Kind::Mouse&&now-self.hover_at>=timing::FAN_TOOLTIP as f64).then_some(self.focus).flatten())
 }
}
pub fn expanded(current:Expanded,id:Id)->Expanded {match id{Id::Size=>if current==Expanded::Size{Expanded::None}else{Expanded::Size},Id::Rest=>if current==Expanded::Rest{Expanded::None}else{Expanded::Rest},_=>current}}
pub fn animation(t:f32,index:usize,closing:Option<f32>,enabled:bool)->(f32,f32) {
 if let Some(t)=closing {let u=(t/timing::FAN_CLOSE).clamp(0.,1.);return (if enabled{1.-u}else{1.},1.-u);}
 let t=t-index as f32*timing::FAN_STAGGER;let u=(t/timing::FAN_OPEN).clamp(0.,1.);
 let pos=if enabled {let q=u-1.;1.+2.1*q*q*q+1.1*q*q}else{1.};
 (pos,(t/timing::POP).clamp(0.,1.))
}
#[cfg(test)] mod tests {
 use super::*;
 #[test]
 fn optional_note_and_detach_buttons_keep_clearance() {
  let work=rect(8.,30.,472.,412.);
  for x in [240.,55.,425.] {
   for kind in [Kind::Mouse,Kind::Touch,Kind::Pen] {
    for enabled in [false,true] {
     for edge in [false,true] {
      let tip=Point{x,y:260.};
      let shape=hat(tip,0.28);
      let p=place_tools(tip,&shape,true,work,&[],kind,Expanded::None,enabled,edge)
       .unwrap_or_else(|| panic!("x={x}, kind={kind:?}, enabled={enabled}, edge={edge}"));
      assert_eq!(p.buttons.len(),4+usize::from(enabled)+usize::from(edge));
      assert_eq!(p.buttons.iter().any(|b|b.id==Id::NewNote),enabled);
      assert_eq!(p.buttons.iter().any(|b|b.id==Id::Detach),edge);
      assert!(p.valid(work,&[],&shape));
     }
    }
   }
  }
 }
 fn rect(l:f32,t:f32,r:f32,b:f32)->Rect{Rect{left:l,top:t,right:r,bottom:b}}
 #[test] fn malformed_obstacles_are_normalized_or_discarded() {
  assert_eq!(rect(20.,20.,10.,10.).distance(Point{x:15.,y:15.}),0.);
  assert_eq!(rect(f32::NAN,0.,1.,1.).distance(Point::default()),f32::INFINITY);
  let tip=Point{x:400.,y:400.};let work=rect(0.,0.,900.,800.);
  let clean=place(tip,&[],false,work,&[rect(0.,0.,900.,30.)],Kind::Mouse,Expanded::None);
  assert_eq!(clean,place(tip,&[],false,work,&[rect(900.,30.,0.,0.),rect(f32::NAN,0.,1.,1.)],Kind::Mouse,Expanded::None));
 }
 #[test] fn dilation_preserves_holes_and_does_not_pad_large_axes() {
  let shape=[rect(0.,0.,5.,60.),rect(20.,0.,25.,5.)];let p=dilate(&shape,28.);
  assert_eq!(p[0],rect(-1.5,0.,6.5,60.));assert!(!p.iter().any(|r|r.contains(Point{x:15.,y:30.})));
  assert_eq!(dilate(&[rect(0.,0.,40.,50.)],28.),[rect(0.,0.,40.,50.)]);
 }
 #[test] fn antialiased_edges_have_three_quarter_dip_hit_slop() {
  let p=candidate(Point{x:300.,y:300.},false,34.,80.,0.,32.,Expanded::None,44.,false,false);let b=&p.buttons[2];
  assert_eq!(p.hit(Point{x:b.center.x,y:b.center.y-b.radius-0.5}),Hit::Button(b.id));
  let a=&p.arcs[0];assert!(a.contains(polar(a.center,a.radius+a.width/2.+0.5,a.middle)));
 }
 #[test] #[ignore="wall-clock UI search budget; run alone"]
 fn placement_worst_case_is_bounded() {
  let work=rect(0.,0.,1000.,800.);let shape=vec![rect(0.,0.,1000.,800.);1000];
  TEST_BUDGET.with(|b|b.set(Some(ui_budget())));
  let started=std::time::Instant::now();assert!(place(Point{x:500.,y:400.},&shape,false,work,&[work],Kind::Touch,Expanded::Rest).is_none());
  let elapsed=started.elapsed();eprintln!("fan impossible search: {elapsed:?}");assert!(elapsed<std::time::Duration::from_millis(if cfg!(debug_assertions){140}else{12}));
 }
 fn hat(tip:Point,size:f32)->Vec<Rect>{(0..140).map(|i|{let y=i as f32*size;rect(tip.x-y*0.35,tip.y+y,tip.x+y*0.35,tip.y+y+size)}).collect()}
 #[test] fn compact_default_and_alpha_clearance_instead_of_box(){
  let tip=Point{x:400.,y:380.};let shape=hat(tip,0.8);let p=place(tip,&shape,false,rect(0.,0.,800.,700.),&[],Kind::Mouse,Expanded::None).unwrap();
  assert_eq!(p.radius,80.);assert_eq!(p.center.y,420.);assert_eq!(p.buttons.iter().map(|b|b.angle).collect::<Vec<_>>(),[138.,106.,74.,42.]);assert!(p.valid(rect(0.,0.,800.,700.),&[],&shape));
 }
 #[test] fn edges_obstacles_sizes_and_touch_do_not_overlap(){
  let work=rect(0.,0.,1000.,800.);
  for size in [0.28,0.8] {for tip in [Point{x:500.,y:500.},Point{x:35.,y:500.},Point{x:965.,y:500.},Point{x:500.,y:90.}] {for kind in [Kind::Mouse,Kind::Touch,Kind::Pen] {
   let shape=hat(tip,size);let forbidden=[rect(0.,0.,1000.,32.),rect(380.,740.,620.,800.),rect(460.,32.,540.,60.)];
   let p=place(tip,&shape,size<=0.4,work,&forbidden,kind,Expanded::None).unwrap_or_else(||panic!("no placement: tip={tip:?}, size={size}, kind={kind:?}"));assert!(p.valid(work,&forbidden,&shape));assert_eq!(p.center.y-tip.y,if size<=0.4{24.}else{40.});
  }}}
  let tip=Point{x:500.,y:500.};for mode in [Expanded::None,Expanded::Size,Expanded::Rest] {assert!(place_diameter(tip,&hat(tip,0.8),false,work,&[],92.,48.,mode,48.,false,false).is_some());}
 }
 #[test] fn side_edges_keep_the_upper_arc_shallow(){
  let work=rect(8.,30.,472.,412.);
  for x in [55.,425.] {for kind in [Kind::Mouse,Kind::Touch,Kind::Pen] {
   let tip=Point{x,y:227.};let p=place(tip,&hat(tip,0.56),false,work,&[],kind,Expanded::None).unwrap_or_else(||panic!("no shallow placement: tip={tip:?}, kind={kind:?}"));
   assert!((p.center.x-tip.x).abs()<=0.9*p.radius+0.001);
   assert!((p.center.x-tip.x)*(240.-tip.x)>0.,"must shift inward: {p:?}");
   for arc in &p.arcs {assert!(arc.bounds().bottom-tip.y<=p.radius*0.3+0.001,"arc drops along body: {p:?}");}
  }}
 }
 #[test] fn satellites_keep_owner_approved_distances_angles_and_minimum_gap(){
  for kind in [Kind::Mouse,Kind::Touch,Kind::Pen] {for mode in [Expanded::Size,Expanded::Rest] {
   let p=place(Point{x:500.,y:500.},&[],false,rect(0.,0.,1000.,800.),&[],kind,mode).unwrap();
   assert_eq!(p.radius,if kind==Kind::Mouse{80.}else{92.});
   assert_eq!(p.arcs[1].radius,p.radius+if kind==Kind::Mouse{44.}else{48.});
   for (i,b) in p.buttons.iter().enumerate(){for a in &p.buttons[..i]{assert!(a.center.distance(b.center)-a.radius-b.radius>=10.,"{:?}/{:?}",a.id,b.id);}}assert!(p.valid(rect(0.,0.,1000.,800.),&[],&[]));
   let parent=if mode==Expanded::Size{2}else{1};let offsets=p.buttons[4..].iter().map(|b|b.angle-p.buttons[parent].angle).collect::<Vec<_>>();
   assert_eq!(offsets,if mode==Expanded::Size{vec![12.,-12.]}else{vec![20.,0.,-20.]});
  }}
  assert_eq!(expanded(Expanded::None,Id::Rest),Expanded::Rest);assert_eq!(expanded(Expanded::Rest,Id::Rest),Expanded::None);
 }
 #[test] fn hit_regions_have_a_transparent_hole_and_empty_arc_does_nothing(){
  let p=candidate(Point{x:300.,y:300.},false,34.,80.,0.,32.,Expanded::None,44.,false,false);
  assert_eq!(p.hit(p.buttons[2].center),Hit::Button(Id::Size));assert_eq!(p.hit(p.center),Hit::Outside);
  assert_eq!(p.hit(polar(p.center,p.radius,90.)),Hit::Arc);
  let mut pointer=Pointer::default();pointer.down(Hit::Arc,0.,Kind::Mouse);assert_eq!(pointer.up(Hit::Arc,0.1),None);
 }
 #[test] fn hover_and_long_press_tooltips_never_activate_on_hold_release(){
  let mut p=Pointer::default();p.hover(Hit::Button(Id::Size),0.);assert_eq!(p.tooltip(0.39,Kind::Mouse),None);assert_eq!(p.tooltip(0.41,Kind::Mouse),Some(Id::Size));
  for kind in [Kind::Touch,Kind::Pen] {p.down(Hit::Button(Id::Rest),1.,kind);assert_eq!(p.tooltip(1.49,kind),None);assert_eq!(p.tooltip(1.51,kind),Some(Id::Rest));assert_eq!(p.up(Hit::Button(Id::Rest),1.6),None);
   p.down(Hit::Button(Id::Size),2.,kind);assert_eq!(p.up(Hit::Button(Id::Size),2.1),Some(Id::Size));}
 }
 #[test] fn reduced_motion_only_fades_and_native_motion_returns_to_tip(){
  assert_eq!(animation(0.05,0,None,false).0,1.);assert_eq!(animation(0.,0,Some(0.05),false).0,1.);
  assert!(animation(0.1,0,None,true).0>1.);assert_eq!(animation(0.,0,Some(0.1),true),(0.,0.));
 }
 #[test] fn presented_buttons_and_hover_use_the_painted_hit_geometry() {
  let p=candidate(Point{x:300.,y:300.},false,34.,80.,0.,32.,Expanded::None,44.,false,false);
  let hover=p.presentation(1.,None,true,Some(Id::Size),1.08);
  let b=&hover.buttons[2];assert_eq!(hover.hit(Point{x:b.center.x+b.radius-0.1,y:b.center.y}),Hit::Button(Id::Size));
  let opening=p.presentation(0.08,None,true,None,1.);
  for b in &opening.buttons {if opening.bounds.contains(b.center) {
   let top=opening.buttons.iter().rev().find(|a|a.center.distance(b.center)<=a.radius).unwrap();assert_eq!(opening.hit(b.center),Hit::Button(top.id));
  }}
  assert_eq!(p.presentation(0.,None,true,None,1.).buttons.len(),0);
 }
 #[test] fn no_space_returns_none_and_size_changes_relayout(){
  assert!(place(Point{x:10.,y:10.},&[],true,rect(0.,0.,40.,40.),&[],Kind::Mouse,Expanded::None).is_none());
  let a=place(Point{x:400.,y:400.},&[],true,rect(0.,0.,900.,800.),&[],Kind::Mouse,Expanded::Size).unwrap();
  let b=place(Point{x:400.,y:300.},&hat(Point{x:400.,y:300.},0.8),false,rect(0.,0.,900.,800.),&[],Kind::Mouse,Expanded::Size).unwrap();assert_ne!(a.buttons[2].center,b.buttons[2].center);
 }
 #[test] fn measured_tooltips_stay_outside_buttons_after_edge_clamping(){
  let work=rect(0.,0.,1000.,800.);
  for tip in [Point{x:500.,y:500.},Point{x:35.,y:500.},Point{x:965.,y:500.},Point{x:500.,y:90.}] {
   let p=place(tip,&[],false,work,&[],Kind::Mouse,Expanded::Size).unwrap();
   for b in &p.buttons {for (w,h) in [(70.,28.),(200.,56.)] {
    let r=p.tooltip_rect(b.id,w,h,work).unwrap_or_else(||panic!("tip={tip:?}, button={b:?}, text={w}x{h}, layout={p:?}"));assert!(work.inset(8.).includes(r));
    assert!(p.buttons.iter().all(|b|r.distance(b.center)>=b.radius*1.08+4.));
   }}
  }
 }
}
