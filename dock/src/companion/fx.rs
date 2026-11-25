//! Staged cartoon emotes. No ballistic velocity, gravity or random rotation.
use super::{anchors::{Anchors,Point}, model::State, motion::Pose};
use std::f32::consts::{PI,TAU};
pub const LIMIT:usize=8; // notify/error composites plus two physical dust puffs
#[cfg(test)]
pub const NAMES:[&str;10]=["drop","tick","z","bang","question","sparkle","note","steam","dust","dots"];
pub trait Random {fn unit(&mut self)->f32;}
pub struct Rng(pub u64);
impl Random for Rng {fn unit(&mut self)->f32 {self.0=self.0.wrapping_mul(6364136223846793005).wrapping_add(1);(self.0>>40) as f32/16777216.}}
#[derive(Clone,Copy,Debug,PartialEq,Eq,PartialOrd,Ord)]
enum Emotion {Life,Sleep,Focus,Wait,Done,Error,Notify}
#[derive(Clone,Copy,Debug)]
enum Anchor {Tip,Front,Back,Left,Right,HatFace,Head,Crown,Feet,Glove}
// Artwork-space stroke and neutral contour ink.
pub const BODY_STROKE:f32=4.;
pub const CONTOUR_INK:f32=41./255.;
// Requested DIP dimensions at the default companion scale; resize with its hat.
pub fn symbol_scale(s:f32)->f32 {s.max(0.56*0.75)}
pub fn stroke_width(s:f32,dpi:f32)->f32 {(BODY_STROKE*0.6*s).max(1./dpi.max(0.1))}
const GLOOM_LENGTH:f32=22./0.56;
const GLOOM_GAP:f32=7./0.56;
pub struct GloomLine {pub from:Point,pub to:Point,pub fade_end:Point,pub width:f32,pub opacity:f32}
#[derive(Clone,Debug)]
pub struct Particle {
 pub name:&'static str,pub t:f32,pub life:f32,pub follow:bool,
 anchor:Anchor,offset:Point,at:Point,world:Point,dir:f32,scale:f32,angle:f32,delay:f32,emotion:Emotion,persistent:bool,reduced:bool,
}
impl Particle {
 fn new(name:&'static str,anchor:Anchor,emotion:Emotion,life:f32)->Self {Self{name,t:0.,life,follow:true,anchor,offset:(0.,0.),at:(0.,0.),world:(0.,0.),dir:1.,scale:1.,angle:0.,delay:0.,emotion,persistent:false,reduced:false}}
 pub fn gloom_line(&self,gx:f32,gy:f32,s:f32)->Option<GloomLine> {
  if self.name!="gloom" {return None;}
  let (x,y,sc,_,opacity)=self.visual(gx,gy,s);
  let extra=symbol_scale(s)-s;
  let (x,y)=(x+self.offset.0*self.dir*extra,y-GLOOM_LENGTH*extra);
  Some(GloomLine{from:(x,y),to:(x,y+GLOOM_LENGTH*sc*if self.reduced{1.}else{((self.t-self.delay)/0.2).clamp(0.,1.)}),fade_end:(x,y+4./0.56*sc),width:BODY_STROKE*0.6*s,opacity})
 }
 pub fn visual(&self,gx:f32,gy:f32,s:f32)->(f32,f32,f32,f32,f32) {
  let t=(self.t-self.delay).max(0.);let u=(t/super::timing::POP).clamp(0.,1.);
  let mut sc=(0.6+0.4*u)*(1.+0.18*(PI*u).sin());let mut dx=0.;let mut dy=0.;let mut angle=if self.name=="tick"{self.angle*self.dir}else{self.angle};
  match self.name {
   "question"=>{dx=self.dir*1.5*(t*TAU/1.6).sin();angle+=6.*(t*TAU/1.6).sin();}
   "bang"=>{if t<0.8 {dy=-(t*TAU/0.4).sin().abs();}}
   "drop"=>dy=if self.emotion==Emotion::Error {6.*(t/2.).min(1.)}else{4.*(t/0.8).min(1.)},
   "gloom"=>sc=1., // draw-on length supplies the entrance, never pop or tremble
   "z"=>{dx=self.dir*12.*t;dy=-18.*t;sc=0.75+0.25*(t/self.life).min(1.);}
   "sparkle"=>{let q=(t/self.life).clamp(0.,1.);sc=if q<0.25{q/0.25}else if q<0.65{1.-(q-0.25)}else{0.6*(1.-q)/0.35};}
   "dust"=>{dx=self.dir*6.*(t/self.life).min(1.);sc*=1.-(t/self.life).min(1.);}
   _=>{}
  }
  if self.reduced {sc=if self.name=="z"{0.75}else{1.};dx=0.;dy=0.;angle=if self.name=="tick"{self.angle*self.dir}else{self.angle};}
  let (x,y)=if self.follow {(gx+self.at.0*s,gy+self.at.1*s)}else{self.world};
  let alpha=if self.t<self.delay {0.}else{(if self.name=="gloom"{0.6*if self.reduced{(t/super::timing::POP).min(1.)}else{1.}}else{(t/super::timing::POP).min(1.)})*((self.life-t)/super::timing::FADE).clamp(0.,1.)};
  (x+dx*s,y+dy*s,sc*self.scale*if self.name=="dust"{s}else{symbol_scale(s)},angle,alpha)
 }
}
pub struct Engine {pub parts:Vec<Particle>,state:Option<State>,frame:String,clock:f32,next:f32,pub revision:u64,pub created:u64,anchors:Option<Anchors>,last_dir:f32,known_direction:bool,breath:Option<f32>,scale:f32,body:Point,reduced:bool}
impl Default for Engine {fn default()->Self{Self{parts:Vec::new(),state:None,frame:String::new(),clock:0.,next:0.,revision:0,created:0,anchors:None,last_dir:1.,known_direction:false,breath:None,scale:0.56,body:(0.,0.),reduced:false}}}
impl Engine {
 pub fn yield_tick(&mut self,dt:f32,animated:bool) {
  if !animated {self.clear();return;}
  self.state=None;self.frame.clear();
  for p in &mut self.parts {
   p.persistent=false;
   p.life=p.life.min((p.t-p.delay).max(0.)+super::timing::FADE);
   p.delay=p.delay.min(p.t);
   p.t+=dt.clamp(0.,0.25);
  }
  self.parts.retain(|p|p.t<p.life+p.delay);
  if !self.parts.is_empty(){self.revision+=1;}
 }
 pub fn motion(&mut self,enabled:bool){self.reduced=!enabled;for p in &mut self.parts{p.reduced=self.reduced;}}
 pub fn clear(&mut self){if !self.parts.is_empty(){self.revision+=1;}self.parts.clear();self.state=None;self.frame.clear();}
 pub fn pose(&mut self,p:&Pose,gx:f32,gy:f32,scale:f32) {
  let a=Anchors::load(&p.frame);
  if a.direction!=0. {self.known_direction=true;self.last_dir=a.direction*if p.flip{-1.}else{1.};}
  else if p.frame.starts_with("walk_")||p.frame.starts_with("carry_"){self.known_direction=true;self.last_dir=if p.flip{-1.}else{1.};}
  let a=a.posed(p,if self.known_direction{self.last_dir*if p.flip{-1.}else{1.}}else{1.});self.anchors=Some(a);self.scale=scale;self.body=(gx,gy);
  for part in &mut self.parts {if part.follow {place(part,a);}}
 }
 pub fn breath_phase(&mut self,seconds:f32){self.breath=Some(seconds.rem_euclid(4.2));}
 fn emit(&mut self,mut p:Particle) {
  p.reduced=self.reduced;
  if p.name!="dust" {
   if self.parts.iter().any(|q|q.name!="dust"&&q.emotion>p.emotion){return;}
   self.parts.retain(|q|q.name=="dust"||q.emotion==p.emotion);
   if ["drop","question","bang"].contains(&p.name)&&self.parts.iter().any(|q|q.name==p.name){return;}
   if ["z","sparkle"].contains(&p.name)&&self.parts.iter().filter(|q|q.name==p.name).count()>=3{return;}
  }
  if self.parts.len()>=LIMIT{return;}
  if let Some(a)=self.anchors {place(&mut p,a);}
  p.world=(self.body.0+p.at.0*self.scale,self.body.1+p.at.1*self.scale);
  self.parts.push(p);self.revision+=1;self.created+=1;
 }
 pub fn burst(&mut self,name:&str,_rng:&mut impl Random,gx:f32,gy:f32,_night:bool) {
  self.body=(gx,gy);
  let make=|n,a,e,l|Particle::new(n,a,e,l);
  match name {
   "bang"=>{
    let mut p=make("bang",Anchor::Tip,Emotion::Notify,f32::INFINITY);p.offset=(10.,-14.);p.persistent=true;self.emit(p);
    // Keep the fan above/behind the punctuation, including its pop overshoot.
    for angle in [-150f32,-100.,-60.] {let mut p=make("tick",Anchor::Tip,Emotion::Notify,0.35);p.offset=(angle.to_radians().cos()*65.,angle.to_radians().sin()*65.);p.angle=angle+90.;p.scale=0.55;self.emit(p);}
   }
   "question"=>{let mut p=make("question",Anchor::Crown,Emotion::Wait,f32::INFINITY);p.offset=(14.,-14.);p.persistent=true;self.emit(p);}
   "drop"|"drop_big"=>{let error=name=="drop_big";let mut p=make("drop",if error{Anchor::Front}else{Anchor::Back},if error{Emotion::Error}else{Emotion::Life},if error{f32::INFINITY}else{1.07});p.offset=(if error{16.}else{0.},-10.);p.scale=if error{1.}else{0.7};p.persistent=error;self.emit(p);}
   "error"=>{self.burst("drop_big",_rng,gx,gy,false);for x in [-GLOOM_GAP,0.,GLOOM_GAP] {let mut p=make("gloom",Anchor::HatFace,Emotion::Error,f32::INFINITY);p.offset=(x,-GLOOM_LENGTH);p.persistent=true;self.emit(p);}}
   "sparkle"|"morning"=>{let count=if name=="morning"{1}else{3};for i in 0..count {let mut p=make("sparkle",if name=="morning"{Anchor::Glove}else{[Anchor::Front,Anchor::Back,Anchor::Tip][i]},if name=="morning"{Emotion::Life}else{Emotion::Done},0.9);p.offset=if name=="morning"{(16.,15.)}else if i==1{(-14.,-20.)}else{(18.,-20.)};p.delay=i as f32*0.1;p.scale=0.85;self.emit(p);}}
   "z"=>{let mut p=make("z",Anchor::Head,Emotion::Sleep,6.);p.offset=(18.,-26.);p.follow=false;self.emit(p);}
   "ticks_left"|"ticks_right"=>{for angle in [-35f32,0.,35.] {let mut p=make("tick",if name=="ticks_left"{Anchor::Left}else{Anchor::Right},Emotion::Focus,0.25);p.offset=(32.*angle.to_radians().cos(),32.*angle.to_radians().sin());p.angle=angle+90.;p.scale=0.55;self.emit(p);}}
   "dust"=>{self.parts.retain(|p|p.name!="dust");for side in [-1.,1.] {let mut p=make("dust",Anchor::Feet,Emotion::Life,0.5);p.offset=(side*25.,-3.);p.dir=side;p.follow=false;p.scale=0.9;self.emit(p);}}
   // steam/note/dots intentionally remain assets only, per art review.
   _=>{}
  }
 }
 pub fn tick(&mut self,dt:f32,state:State,frame:&str,gx:f32,gy:f32,rng:&mut impl Random,enabled:bool,night:bool) {
  if !enabled {self.clear();return;}
  let dt=dt.clamp(0.,0.25);
  if self.state!=Some(state) {
   self.state=Some(state);self.clock=0.;self.next=if state==State::Sleep{(1.05-self.breath.unwrap_or(0.)).rem_euclid(4.2)}else{0.};
   if matches!(state,State::Notify|State::Wait|State::Error|State::Done|State::Sleep) {self.parts.retain(|p|p.name=="dust");}
   for p in &mut self.parts {if p.persistent {p.persistent=false;p.life=p.t+super::timing::FADE;}}
   match state {State::Notify=>self.burst("bang",rng,gx,gy,night),State::Wait=>self.burst("question",rng,gx,gy,night),State::Done=>self.burst("sparkle",rng,gx,gy,night),State::Error=>self.burst("error",rng,gx,gy,night),_=>{}}
  }
  if self.frame!=frame {
   if matches!(frame,"sit_3"|"jump_5"){self.burst("dust",rng,gx,gy,night);}
   self.frame=frame.into();
  }
  self.clock+=dt;
  if self.clock>=self.next && state==State::Sleep {self.burst("z",rng,gx,gy,night);self.next=self.clock+4.2;}
  for p in &mut self.parts {p.t+=dt;}
  self.parts.retain(|p|p.t<p.life+p.delay);
  if !self.parts.is_empty(){self.revision+=1;}
 }
}
fn place(p:&mut Particle,a:Anchors) {
 // The side brim metadata can sit below the overlapping nose. Keep centered
 // hat marks above its upper edge instead of drawing through the nose.
 let q=match p.anchor {Anchor::Tip=>a.tip,Anchor::Front=>a.front,Anchor::Back=>a.back,Anchor::Left=>if a.front.0<a.back.0{a.front}else{a.back},Anchor::Right=>if a.front.0>a.back.0{a.front}else{a.back},Anchor::HatFace=>(a.nose.0,((a.front.1+a.back.1)/2.-4.).min(a.nose.1-12.)),Anchor::Head=>(if (a.tip.0-a.feet.0).abs()<(a.crown.0-a.feet.0).abs(){a.tip.0}else{a.crown.0},a.tip.1.min(a.crown.1)),Anchor::Crown=>a.crown,Anchor::Feet=>a.feet,Anchor::Glove=>(a.front.0,a.front.1+25.)};
 let dir=match p.anchor{Anchor::Left=>-1.,Anchor::Right=>1.,_=>a.direction};
 if p.name!="dust" {p.dir=dir;}
 let foot=if p.name=="dust"{p.dir*(a.front.0-a.back.0).abs()*0.45}else{0.};
 p.at=(q.0+p.offset.0*dir+foot,q.1+p.offset.1);
}
pub fn interval(base:Option<u32>,parts:&[Particle],battery:bool)->Option<u32>{let fps=parts.iter().map(|p|if ["z","question"].contains(&p.name){12u32}else{30}).max();base.map(|ms|fps.map_or(ms,|fps|ms.min((if battery{2000u32}else{1000}).div_ceil(fps))))}
#[cfg(test)] mod tests {
 use super::*;struct Fixed;impl Random for Fixed {fn unit(&mut self)->f32{0.5}}
 #[test] fn yielding_fades_existing_emotes_without_creating_any() {
  let mut e=Engine::default();e.burst("question",&mut Fixed,0.,0.,false);
  let created=e.created;
  for _ in 0..10 {e.yield_tick(0.034,true);assert_eq!(e.created,created);}
  assert!(e.parts.is_empty());
  e.burst("bang",&mut Fixed,0.,0.,false);e.yield_tick(0.,false);assert!(e.parts.is_empty());
 }
 fn bounds(p:&Particle)->[f32;4] {
  if let Some(line)=p.gloom_line(0.,0.,1.) {return [line.from.0-line.width/2.,line.from.1,line.to.0+line.width/2.,line.to.1];}
  let im=crate::icons::decode_bytes_fit(super::super::clips::fx_png(p.name).unwrap(),4096).unwrap();
  let (x,y,s,a,_)=p.visual(0.,0.,1.);let a=a.to_radians();
  let w=im.width as f32*s*0.25;let h=im.height as f32*s*0.25;
  let dx=w*a.cos().abs()+h*a.sin().abs();let dy=w*a.sin().abs()+h*a.cos().abs();
  [x-dx,y-dy,x+dx,y+dy]
 }
 fn overlap(a:[f32;4],b:[f32;4])->bool {a[0]<b[2]&&b[0]<a[2]&&a[1]<b[3]&&b[1]<a[3]}
 fn posed(frame:&str,flip:bool)->Engine {
  unsafe{let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
  let mut p=super::super::Motion::new(0.).pose();p.frame=frame.into();p.flip=flip;
  let mut e=Engine::default();e.pose(&p,0.,0.,1.);e
 }
 #[test] fn visual_review_focus_has_three_separate_outward_strokes(){
  for side in ["ticks_left","ticks_right"] {let mut e=posed("peek_0",false);e.burst(side,&mut Fixed,0.,0.,false);assert_eq!(e.parts.len(),3);
   for p in &mut e.parts{p.t=0.09;}
   for (i,p) in e.parts.iter().enumerate(){let anchor=if side=="ticks_left"{e.anchors.unwrap().front.0.min(e.anchors.unwrap().back.0)}else{e.anchors.unwrap().front.0.max(e.anchors.unwrap().back.0)};let b=bounds(p);assert!(if side=="ticks_left"{b[2]<=anchor-6.}else{b[0]>=anchor+6.});for q in &e.parts[i+1..]{assert!(!overlap(b,bounds(q)));assert!((p.angle-q.angle).abs()>=35.);}}
  }
 }
 #[test] fn visual_review_notify_and_error_bounds_do_not_overlap(){
  for flip in [false,true] {for (frame,burst,mark,other) in [("notify_1","bang","bang","tick"),("error_1","error","drop","gloom")] {
   let mut e=posed(frame,flip);e.burst(burst,&mut Fixed,0.,0.,false);
   for t in [0.03,0.06,0.09,0.12,0.2,0.34,0.7,2.] {for p in &mut e.parts{p.t=t;}let b=bounds(e.parts.iter().find(|p|p.name==mark).unwrap());for p in e.parts.iter().filter(|p|p.name==other){assert!(!overlap(b,bounds(p)),"{frame} flip={flip} t={t}: {b:?} {:?}",bounds(p));if other=="gloom"{assert_eq!(p.scale,1.);}}}
  }}
 }
 #[test] fn visual_review_sleep_origin_is_above_the_head(){
  for frame in ["sleep_0","sleep_1","sleep_2","sleep_3"] {for flip in [false,true] {let mut e=posed(frame,flip);e.burst("z",&mut Fixed,0.,0.,false);let p=&e.parts[0];let a=e.anchors.unwrap();let b=bounds(p);assert!(p.visual(0.,0.,1.).2>=0.75);assert!(b[3]<a.tip.1.min(a.crown.1),"{frame}: {b:?} {a:?}");let mut later=p.clone();later.t=1.;let v=later.visual(0.,0.,1.);assert!(v.1<p.world.1&&(v.0-p.world.0)*a.direction>0.);}}
 }
 #[test] fn error_uses_three_straight_gloom_lines_instead_of_squiggles(){
  for frame in ["error_0","error_1","error_2","error_3"] {for flip in [false,true] {let mut e=posed(frame,flip);e.burst("error",&mut Fixed,0.,0.,false);
   assert!(!e.parts.iter().any(|p|p.name=="squiggle"));let a=e.anchors.unwrap();
   let lines:Vec<_>=e.parts.iter().filter(|p|p.name=="gloom").collect();assert_eq!(lines.len(),3);
   let mut xs=Vec::new();for p in lines {let mut p=p.clone();
    let mut previous=None;
    for (t,len) in [(0.,0.),(0.1,11.),(0.2,22.),(1.,22.)] {p.t=t;let l=p.gloom_line(0.,0.,0.56).unwrap();
     assert_eq!(l.from.0,l.to.0);assert!((l.to.1-l.from.1-len).abs()<0.001);
     assert!((l.width-BODY_STROKE*0.6*0.56).abs()<0.001);assert_eq!(l.opacity,0.6);
     if let Some(top)=previous {assert_eq!(top,l.from);}previous=Some(l.from);
     if t>=0.2 {assert!(l.to.1<a.nose.1*0.56);assert!(l.to.1<((a.front.1+a.back.1)/2.)*0.56);assert!(l.from.1>a.tip.1*0.56);}
    }xs.push(p.gloom_line(0.,0.,0.56).unwrap().from.0);
   }xs.sort_by(f32::total_cmp);assert!((xs[1]-a.nose.0*0.56).abs()<0.001);assert!((xs[1]-xs[0]-7.).abs()<0.001);assert!((xs[2]-xs[1]-7.).abs()<0.001);
  }}
 }
 #[test] fn wait_is_created_once_and_idle_has_no_work_drops() {
  for (state,expected) in [(State::Wait,1),(State::Idle,0)] {let mut e=Engine::default();for _ in 0..100 {e.tick(0.1,state,"",0.,0.,&mut Fixed,true,false);assert!(!e.parts.iter().any(|p|p.name=="tick"));}assert_eq!(e.created,expected);}
 }
 #[test] fn framed_emotes_mirror_positions_but_not_glyphs(){
  unsafe{let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
  for (frame,burst) in [("wait_2","question"),("sleep_2","z"),("error_1","error"),("jump_3","drop")] {
   let mut p=super::super::Motion::new(0.).pose();p.frame=frame.into();let mut right=Engine::default();right.pose(&p,0.,0.,1.);right.burst(burst,&mut Fixed,0.,0.,false);
   p.flip=true;let mut left=Engine::default();left.pose(&p,0.,0.,1.);left.burst(burst,&mut Fixed,0.,0.,false);
   for (r,l) in right.parts.iter().zip(left.parts.iter()){let rv=r.visual(0.,0.,1.);let lv=l.visual(0.,0.,1.);assert!((rv.0+lv.0).abs()<0.001,"{frame} {:?} {:?}",rv,lv);assert_eq!(rv.1,lv.1);assert_eq!(rv.2,lv.2);assert_eq!(rv.3,lv.3);}
  }
 }
 #[test] fn drops_do_not_stack_and_sleep_starts_on_the_exhale(){let mut e=Engine::default();for _ in 0..5{e.burst("drop",&mut Fixed,0.,0.,false);}assert_eq!(e.parts.len(),1);e.clear();e.breath_phase(0.);for _ in 0..10{e.tick(0.1,State::Sleep,"sleep_0",0.,0.,&mut Fixed,true,false);}assert_eq!(e.created,1);e.tick(0.1,State::Sleep,"sleep_0",0.,0.,&mut Fixed,true,false);assert_eq!(e.created,2);}
 #[test] fn priority_and_unused_emotes() {
  let mut e=Engine::default();e.burst("bang",&mut Fixed,0.,0.,false);e.burst("question",&mut Fixed,0.,0.,false);assert_eq!(e.parts.len(),4);assert!(!e.parts.iter().any(|p|p.name=="question"));
  e.clear();e.burst("drop",&mut Fixed,0.,0.,false);e.burst("error",&mut Fixed,0.,0.,false);e.burst("drop",&mut Fixed,0.,0.,false);assert_eq!(e.parts.len(),4);assert!(e.parts.iter().all(|p|p.emotion==Emotion::Error));
  e.clear();for n in ["steam","note","dots","pet_heart","heart","squiggle","poke"] {e.burst(n,&mut Fixed,0.,0.,false);}assert!(e.parts.is_empty());
 }
 #[test] fn notify_wait_end_fade_and_dust_exception() {let mut e=Engine::default();e.tick(0.1,State::Wait,"",0.,0.,&mut Fixed,true,false);e.burst("dust",&mut Fixed,0.,0.,false);assert_eq!(e.parts.len(),3);e.tick(0.2,State::Idle,"",0.,0.,&mut Fixed,true,false);assert!(!e.parts.iter().any(|p|p.name=="question"));}
}

#[cfg(test)] mod polish_tests {
 use super::*;
 #[test] fn symbols_keep_three_quarters_of_default_at_small_sizes() {
  for name in ["question","bang","z","drop","gloom"] {for size in [0.28,0.34,0.56] {
   let mut p=Particle::new(name,Anchor::Tip,Emotion::Life,5.);p.t=0.5;
   let full=p.visual(0.,0.,0.56).2;let small=p.visual(0.,0.,size).2;
   assert!(small+1e-6>=full*0.75);assert!((small/full-symbol_scale(size)/0.56).abs()<1e-5);
  }}
 }
 #[test] fn procedural_strokes_keep_ratio_with_one_physical_pixel_floor() {
  for s in [0.28,0.34,0.56,0.8] {for dpi in [1.,1.5,2.] {
   let width=stroke_width(s,dpi);assert!(width*dpi>=1.);assert!((width-(BODY_STROKE*0.6*s).max(1./dpi)).abs()<1e-6);
  }}
 }
 #[test] fn reduced_emotes_are_fixed_and_only_fade() {
  for name in ["question","bang","drop","z","sparkle","gloom"] {
   let mut p=Particle::new(name,Anchor::Tip,Emotion::Life,1.);p.reduced=true;
   p.t=0.03;let first=p.visual(0.,0.,0.34);p.t=0.2;let middle=p.visual(0.,0.,0.34);p.t=0.95;let end=p.visual(0.,0.,0.34);
   assert_eq!((first.0,first.1,first.2,first.3),(middle.0,middle.1,middle.2,middle.3));
   assert_eq!((end.0,end.1,end.2,end.3),(middle.0,middle.1,middle.2,middle.3));assert!(first.4<middle.4&&end.4<middle.4);
  }
 }
}
