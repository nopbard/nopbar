//! Per-frame artwork landmarks, decoded once. Source PNGs remain untouched.
use super::{clips, motion::Pose, render};
pub type Point = (f32, f32);
#[derive(Clone, Copy, Debug, Default)]
pub struct Anchors { pub tip:Point, pub front:Point, pub back:Point, pub nose:Point, pub nose_bottom:f32, pub crown:Point, pub feet:Point, pub direction:f32, pub edge:Option<f32> }
impl Anchors {
    /// Bubbles follow the body, never the delayed cone tip.
    pub fn body_tip(p:&Pose)->Point {
        let mut body=p.clone();body.hat_tilt=0.;
        Self::load(&p.frame).posed(&body,1.).tip
    }
    /// Preserve the approved spinner's artwork location, then apply exactly
    /// the same upper-cone and body transforms as the rendered hat.
    pub fn spinner_foot(p:&Pose,x:f32,y:f32,scale:f32)->Point {
        let f=&clips::catalog().frames[&p.frame];
        let (mut px,mut py)=(f.ax+1.2,f.ground-206.7);
        if py<render::hat_hinge(p) {let h=render::hat_matrix(p);(px,py)=(px*h.M11+py*h.M21+h.M31,px*h.M12+py*h.M22+h.M32);}
        let m=render::body_matrix(p,x,y,scale);
        (px*m.M11+py*m.M21+m.M31-1.2*scale,px*m.M12+py*m.M22+m.M32+206.7*scale)
    }
    pub fn load(name:&str)->Self {
        clips::catalog().anchors.get(name).copied().unwrap_or_default()
    }
    pub(super) fn calculate(f:&clips::Frame,bytes:Option<&[u8]>)->Self {
        let clamp=|p:Point|(p.0.clamp(0.,f.w.saturating_sub(1) as f32),p.1.clamp(0.,f.h.saturating_sub(1) as f32));
        let fallback=Self{tip:clamp((f.hx,0.)),front:clamp((f.hx+f.hatw/2.,f.hy-6.)),back:clamp((f.hx-f.hatw/2.,f.hy-6.)),nose:clamp((f.hx,f.hy)),nose_bottom:f.hy,crown:clamp((f.hx+f.hatw/4.,f.hy/2.)),feet:(f.ax,f.ground),direction:0.,edge:f.edge};
        let Some(image)=bytes.and_then(|b|crate::icons::decode_bytes_fit(b,4096)) else{return fallback;};
        if image.width!=f.w || image.height!=f.h {return fallback;}
            let mut top=(f.ax,f.ground); let mut nose=(0.,0.,0.); let mut nose_bottom=0f32;
            for (i,p) in image.pixels.chunks_exact(4).enumerate() {
                let (x,y)=((i%image.width as usize) as f32,(i/image.width as usize) as f32);
                if p[3]>120 && y<top.1 {top=(x,y);}
                // Approved peach nose (254,185,146), not the red hat or grey gloves.
                if p[3]>220 && p[2]>235 && (160..210).contains(&p[1]) && (125..170).contains(&p[0]) {nose.0+=x;nose.1+=y;nose.2+=1.;nose_bottom=nose_bottom.max(y);}
            }
            let nose=if nose.2>0. {(nose.0/nose.2,nose.1/nose.2)}else{(f.hx,f.hy)};
            let direction=if (nose.0-f.hx).abs()>f.hatw*0.08 {(nose.0-f.hx).signum()}else{0.};
            let d=if direction==0. {1.}else{direction};
            let clamp=|p:Point|(p.0.clamp(0.,f.w as f32-1.),p.1.clamp(0.,f.h as f32-1.));
            let front=clamp((f.hx+d*f.hatw/2.,f.hy-6.));
            let back=clamp((f.hx-d*f.hatw/2.,f.hy-6.));
            let cy=(top.1+front.1)/2.;
            // Outermost opaque hat silhouette at the crown's height.
            let row=cy as usize*image.width as usize;
            let edge=(0..image.width as usize).filter(|&x|image.pixels[(row+x)*4+3]>120);
            let cx=if d>0. {edge.last()}else{edge.into_iter().next()}.unwrap_or(f.hx as usize) as f32;
            Self{tip:top,front,back,nose,nose_bottom:if nose_bottom>0.{nose_bottom}else{f.hy},crown:clamp((cx,cy)),feet:(f.ax,f.ground),direction,edge:f.edge}
    }
    pub fn posed(mut self,p:&Pose,last:f32)->Self {
        if self.direction==0. && last<0. {std::mem::swap(&mut self.front,&mut self.back);self.crown.0=2.*self.feet.0-self.crown.0;}
        let m=render::body_matrix(p,0.,0.,1.);
        let tr=|q:Point|(q.0*m.M11+q.1*m.M21+m.M31,q.0*m.M12+q.1*m.M22+m.M32);
        if p.hat_tilt!=0. {
            let hat=render::hat_matrix(p);
            let bend=|q:Point| if q.1<render::hat_hinge(p) {(q.0*hat.M11+q.1*hat.M21+hat.M31,q.0*hat.M12+q.1*hat.M22+hat.M32)} else {q};
            self.tip=bend(self.tip);self.front=bend(self.front);self.back=bend(self.back);self.crown=bend(self.crown);
        }
        let d=if self.direction==0. {last}else{self.direction};
        Self{tip:tr(self.tip),front:tr(self.front),back:tr(self.back),nose:tr(self.nose),nose_bottom:self.nose_bottom,crown:tr(self.crown),feet:tr(self.feet),direction:d*if p.flip{-1.}else{1.},edge:self.edge}
    }
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn catalog_precomputes_landmarks_and_decode_failure_uses_metadata(){
  let c=clips::catalog();assert_eq!(c.anchors.len(),c.frames.len());let f=&c.frames["idle_0"];
  for bytes in [None,Some(&b"bad png"[..])] {let a=Anchors::calculate(f,bytes);assert_eq!(a.feet,(f.ax,f.ground));assert_eq!(a.nose,(f.hx,f.hy));assert!(a.front.0>a.back.0);}
  assert_eq!(Anchors::load("missing").direction,0.);
 }
 #[test] fn landmarks_are_inside_each_posture_and_mirror_positions_only() {
  unsafe {let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
  for name in ["idle_0","sit_3","sleep_2","read_4","walk_1"] {
   let a=Anchors::load(name);let f=&clips::catalog().frames[name];
   for (x,y) in [a.tip,a.front,a.back,a.nose,a.crown] {assert!(x>=0.&&x<f.w as f32&&y>=0.&&y<f.h as f32,"{name}: {x},{y}");}
   if a.direction!=0. {assert!((a.front.0-a.back.0)*a.direction>0.);}
   let mut p=super::super::Motion::new(0.).pose();p.frame=name.into();let r=a.posed(&p,1.);p.flip=true;let l=a.posed(&p,1.);
   assert!((r.nose.0+l.nose.0).abs()<0.001);assert_eq!(r.nose.1,l.nose.1);assert_eq!(r.direction,-l.direction);
  }
 }
}
