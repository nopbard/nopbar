//! Shared body/effect visibility policy and reversible native presentation.
use windows::Win32::Foundation::RECT;
use super::window::Window;

/// One decision for the body and its desktop layers, after raster clipping.
/// Alpha 1 is invisible input padding, not visible body ink.
#[derive(Clone, Copy, Debug, Default)]
pub struct Frame { pub body: bool, pub effects: bool }
impl Frame {
    pub fn from_pixels(pixels: &[u8], width: u32, origin: (i32,i32), screens: &[RECT], quiet: bool) -> Self {
        let (mut total,mut visible)=(0usize,0usize);
        if width==0 {return Self::default();}
        for (i,p) in pixels.as_chunks::<4>().0.iter().enumerate() {
            if p[3]<=1 {continue;}
            total+=1;
            let (x,y)=(origin.0+i as i32%width as i32,origin.1+i as i32/width as i32);
            if screens.iter().any(|r|x>=r.left&&x<r.right&&y>=r.top&&y<r.bottom) {visible+=1;}
        }
        Self {body:visible>0,effects:!quiet&&visible>0&&visible*2>=total}
    }
    fn layers(self) -> [bool;3] { [self.body,self.body,self.body&&self.effects] }
    pub fn hide(windows: [Option<&Window>;3]) {
        for w in windows.into_iter().flatten() {w.opacity(0.);w.hide();}
    }
    /// Preserve shadow/body/effect stacking. Any failure hides the entire group.
    pub unsafe fn present(self, windows: [Option<&Window>;3], updates: [Option<(i32,i32,bool)>;3], alpha:f32) -> windows::core::Result<()> {
        if !self.body||windows[0].is_none()||updates[0].is_none()||alpha<=0. {Self::hide(windows);return Ok(());}
        let layers=self.layers();
        // Desktop insertion is immediately above the host, so place the
        // highest layer first. Topmost insertion has the opposite order.
        let order = if windows[0].is_some_and(|w| w.in_front()) { [1,0,2] } else { [2,0,1] };
        for i in order {
            let (w,update,show)=(windows[i],updates[i],layers[i]);
            let Some(w)=w else {continue;};
            if let Some((x,y,dirty))=update.filter(|_|show&&alpha>0.) {
                w.opacity(alpha);
                if let Err(e)=w.present(x,y,dirty) {Self::hide(windows);return Err(e);}
            } else {w.opacity(0.);w.hide();}
        }
        Ok(())
    }
    pub unsafe fn fade(self, windows: [Option<&Window>;3], alpha:f32) -> windows::core::Result<()> {
        if windows[0].is_none_or(|w|!w.can_restore()) {Self::hide(windows);return Ok(());}
        for (w,show) in windows.into_iter().zip(self.layers()) {
            let Some(w)=w else {continue;};
            if show {
                if let Err(e)=w.fade_cached(alpha) {Self::hide(windows);return Err(e);}
            } else {w.opacity(0.);w.hide();}
        }
        Ok(())
    }
    pub unsafe fn move_by(windows:[Option<&Window>;3],dx:f32,dy:f32,screens:&[RECT],alpha:f32)->windows::core::Result<Self> {
        let frame=windows[0].map_or_default(|w|w.moved_frame(dx,dy,screens));
        if !frame.body||windows[0].is_none_or(|w|!w.can_restore()) {Self::hide(windows);return Ok(frame);}
        // Moving particles is subject to the new body position before any HWND moves.
        for i in [0,1,2] {
            let Some(w)=windows[i] else {continue;};
            if i==1||i==2&&!frame.effects {w.opacity(0.);w.hide();continue;}
            if let Err(e)=w.move_by(dx,dy) {Self::hide(windows);return Err(e);}
        }
        frame.fade(windows,alpha)?;
        Ok(frame)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Run,
    Yield,
    Hide,
}

pub fn mode(launchpad: bool, idle: bool, popup: bool, preview: bool, stack: bool, search: bool, ui: bool) -> Mode {
    if launchpad || idle { Mode::Hide }
    else if popup || preview || stack || search || ui { Mode::Yield }
    else { Mode::Run }
}

pub fn rest_interval(animated: bool, battery: bool) -> u32 {
    if !animated { 500 } else if battery { 67 } else { 34 }
}

pub struct Fade {
    pub opacity: f32,
    from: f32,
    target: f32,
    elapsed: f32,
}
impl Default for Fade {
    fn default() -> Self { Self { opacity: 1., from: 1., target: 1., elapsed: 0. } }
}
impl Fade {
    pub fn step(&mut self, hidden: bool, dt: f32, animated: bool) -> f32 {
        let target = if hidden { 0. } else { 1. };
        if self.target != target {
            self.from = self.opacity;
            self.target = target;
            self.elapsed = 0.;
        } else { self.elapsed += dt; }
        let duration = if hidden { 0.120 } else { 0.180 };
        let t = if animated { (self.elapsed / duration).clamp(0., 1.) } else { 1. };
        self.opacity = self.from + (self.target - self.from) * t;
        self.opacity
    }
    pub fn moving(&self) -> bool { self.opacity != self.target }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wait_layers_follow_every_hidden_path_and_return() { unsafe {
        use crate::companion::{fx,model::State};
        let mut engine=fx::Engine::default();let mut rng=fx::Rng(1);
        engine.tick(0.25,State::Wait,"wait_0",0.,0.,&mut rng,true,false);
        assert!(engine.parts.iter().any(|p|p.name=="question"));
        let mut body=Window::new(8,8,false).unwrap();
        let mut effect=Window::new(8,8,true).unwrap();
        body.pixels_mut().fill(255);effect.pixels_mut().fill(255);
        let screen=RECT{left:-30000,top:-30000,right:-29992,bottom:-29992};
        let at=(screen.left,screen.top);
        for (name,ink,quiet,origin,alpha,expected) in [
            ("wait",255,false,at,1.,(true,true)),
            ("full peek",0,true,at,1.,(false,false)),
            ("partial peek",255,true,at,1.,(true,false)),
            ("rest journey",255,true,at,1.,(true,false)),
            ("home",0,true,at,1.,(false,false)),
            ("offscreen",255,false,(-30100,-30000),1.,(false,false)),
            ("mostly clipped",255,false,(-30007,-30000),1.,(true,false)),
            ("transparent padding",1,false,at,1.,(false,false)),
            ("fullscreen",255,false,at,0.,(false,false)),
            ("return",255,false,at,1.,(true,true)),
        ] {
            body.pixels_mut().fill(ink);
            let frame=Frame::from_pixels(body.pixels_mut(),8,origin,&[screen],quiet);
            frame.present([Some(&body),None,Some(&effect)],[Some((origin.0,origin.1,true)),None,Some((at.0,at.1,true))],alpha).unwrap();
            assert_eq!((body.visibility_state().0,effect.visibility_state().0),expected,"{name}");
            assert_eq!(body.visibility_state().1,if expected.0{(alpha*255.).round() as u8}else{0},"{name}");
            assert_eq!(effect.visibility_state().1,if expected.1{(alpha*255.).round() as u8}else{0},"{name}");
            assert_eq!(engine.parts[0].name,"question","state transitions must not replace the visibility gate");
        }
        let frame=Frame{body:true,effects:true};
        for alpha in [0.5,0.,0.5,1.] {
            frame.fade([Some(&body),None,Some(&effect)],alpha).unwrap();
            assert_eq!(body.visibility_state(),effect.visibility_state());
            assert_eq!(effect.visibility_state(),(alpha>0.,(alpha*255.).round() as u8));
        }
        Frame::move_by([Some(&body),None,Some(&effect)],-100.,0.,&[screen],1.).unwrap();
        assert_eq!(body.visibility_state(),(false,0));
        assert_eq!(effect.visibility_state(),(false,0));
        frame.present([Some(&body),None,Some(&effect)],[Some((at.0,at.1,true)),None,Some((at.0,at.1,true))],1.).unwrap();
        // An independently hidden body cannot resurrect effects on uncover.
        body.hide();frame.fade([Some(&body),None,Some(&effect)],1.).unwrap();
        assert_eq!(effect.visibility_state(),(false,0));
        for state in [State::Idle,State::Wait] {
            engine.tick(0.25,state,"wait_0",0.,0.,&mut rng,true,false);
            frame.fade([Some(&body),None,Some(&effect)],1.).unwrap();
            assert_eq!(effect.visibility_state(),(false,0));
        }
        Frame::hide([Some(&body),None,Some(&effect)]);
        assert_eq!(body.visibility_state(),(false,0));
    }}

    #[test]
    #[ignore]
    fn harmony_wait_visibility() { unsafe {
        use crate::{companion::{fx,model::State,motion::Motion,render},gfx::promo_kit as kit};
        let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        let cv=kit::Canvas::at(320.,320.,1.);
        let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/wait-visibility");
        std::fs::create_dir_all(&root).unwrap();
        let side=320u32;
        let mut raster=render::Raster::new(cv.dc.GetDevice().unwrap().CreateDeviceContext(windows::Win32::Graphics::Direct2D::D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap(),side,side).unwrap();
        for light in [false,true] {for size in [0.28,0.56] {
            let mut sheet=vec![0u8;(side*side*4*8) as usize];
            let bg=if light{240u8}else{30};
            for p in sheet.as_chunks_mut::<4>().0{p.copy_from_slice(&[bg,bg,bg,255]);}
            let mut engine=fx::Engine::default();let mut rng=fx::Rng(1);
            let mut motion=Motion::new(160.);motion.set_state(State::Wait);let p=motion.step(0.25,0.,320.);
            engine.pose(&p,160.,260.,size);
            engine.tick(0.25,State::Wait,"wait_0",160.,260.,&mut rng,true,false);
            for index in 0..8 {
                let mut body=vec![0u8;(side*side*4) as usize];let mut effects=body.clone();
                raster.draw(&p,160.,260.,size,1.,false,0.,false,light,&mut body).unwrap();
                raster.draw_particles(&engine.parts,160.,260.,size,1.,(0.,0.),light,&mut effects).unwrap();
                if index==1||index==4 {body.fill(0);}
                if index==2 {for (i,p) in body.as_chunks_mut::<4>().0.iter_mut().enumerate(){if i/side as usize>=(260.-260.*size) as usize {p.fill(0);}}}
                let origin=if index==5{(500,0)}else{(0,0)};
                let screen=RECT{left:0,top:0,right:side as i32,bottom:side as i32};
                let frame=Frame::from_pixels(&body,side,origin,&[screen],matches!(index,1..=4));
                let alpha=if index==6{0.}else{1.};
                let mut effect_ink=0;
                for (layer,show) in [(&body,frame.body),(&effects,frame.effects)] {
                    if !show||alpha==0. {continue;}
                    for (i,p) in layer.as_chunks::<4>().0.iter().enumerate() {
                        let x=i%side as usize+(index%4)*side as usize;
                        let y=i/side as usize+(index/4)*side as usize;
                        let dst=&mut sheet[(y*(side*4) as usize+x)*4..][..4];
                        for c in 0..3{dst[c]=(p[c] as u16+dst[c] as u16*(255-p[3] as u16)/255).min(255) as u8;}
                        if std::ptr::eq(layer,&effects)&&p[3]>1 {effect_ink+=1;}
                    }
                }
                assert_eq!(effect_ink>0,index==0||index==7,"state {index}");
            }
            kit::save_png(&sheet,side*4,side*2,&root.join(format!("wait-{size}-{light}.png")));
        }}
    }}
    #[test] fn labels_have_no_input_to_the_policy_and_other_ui_only_yields() {
        assert_eq!(mode(false,false,false,false,false,false,false),Mode::Run);
        for index in 0..5 {
            let mut ui=[false;5];ui[index]=true;
            assert_eq!(mode(false,false,ui[0],ui[1],ui[2],ui[3],ui[4]),Mode::Yield);
        }
        assert_eq!(mode(true,false,true,true,true,true,true),Mode::Hide);
        assert_eq!(mode(false,true,false,false,false,false,false),Mode::Hide);
        assert!(rest_interval(true,false)>=34 && rest_interval(true,true)>=67);
        assert_eq!(rest_interval(false,false),500);
    }
    #[test] fn hide_and_return_take_120_and_180_ms_and_reverse_without_jumps() {
        let mut f=Fade::default();
        assert_eq!(f.step(true,0.,true),1.);
        assert!((f.step(true,0.060,true)-0.5).abs()<1e-5);
        assert_eq!(f.step(false,0.,true),0.5);
        assert!((f.step(false,0.090,true)-0.75).abs()<1e-5);
        assert_eq!(f.step(false,0.090,true),1.);
        f.step(true,0.,true);assert_eq!(f.step(true,0.120,true),0.);
        f.step(false,0.,true);assert_eq!(f.step(false,0.180,true),1.);
        assert_eq!(f.step(true,0.,false),0.);
        assert_eq!(f.step(false,0.,false),1.);
    }
}
