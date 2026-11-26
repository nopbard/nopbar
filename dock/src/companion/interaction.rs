//! Native input bridge; the injected grab model owns all gesture/physics rules.
use super::{
    grab::{self, Home, Input, Point, Release},
    motion::Pose,
    App, Ground,
};
use windows::Win32::{
    Foundation::POINT,
    Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST},
};
#[derive(Clone)]
pub(super) struct Drawing {
    pub pose: Pose,
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub dpi: f32,
}
pub(super) fn monitors() -> Vec<(u64, crate::monitors::Monitor)> {
    crate::displays::current()
        .into_iter()
        .filter_map(|d| Some((grab::monitor_key(&d.key), crate::monitors::info(d.handle)?)))
        .collect()
}
fn home_monitor(home: Option<Home>, monitors: &[(u64, crate::monitors::Monitor)], dock: Option<Ground>) -> Option<crate::monitors::Monitor> {
    home.and_then(|h| monitors.iter().find(|(key, _)| *key == h.monitor))
        .or_else(|| dock.and_then(|g| monitors.iter().find(|(_,m)|m.rect==g.monitor)))
        .map(|(_,m)|*m)
}
fn release_ground(fresh:Option<Ground>, last:Option<Ground>, dock:Option<Ground>)->Option<Ground> {
    fresh.or(last).or(dock)
}
fn dropped_home(g:Ground,x:f32,monitors:&[(u64,crate::monitors::Monitor)],previous:Option<Home>)->Home {
    let key=monitors.iter().find(|(_,m)|m.rect==g.monitor).map(|(key,_)|*key)
        .or(previous.map(|h|h.monitor)).unwrap_or(0);
    Home::at(key,x,g.monitor.left as f32,g.monitor.right as f32)
}
fn sampled_ground(m:crate::monitors::Monitor,dock:Option<Ground>,covered:bool)->Ground {
    Ground {monitor:m.rect,floor:dock.map_or(m.work.bottom as f32,|g|g.floor),dpi:m.scale(),covered}
}
fn accepts_ground(home:Option<Home>,monitors:&[(u64,crate::monitors::Monitor)],g:Ground,primary:bool,busy:bool)->bool {
    if busy{return false;}
    match home.and_then(|h|monitors.iter().find(|(key,_)|*key==h.monitor)) {
        Some((_,m))=>g.monitor==m.rect,
        None=>primary,
    }
}
pub(super) fn covered_stops(covered:bool, grab:&grab::Grab)->bool {
    covered && !grab.protect_from_cover()
}
impl App {
    pub(super) fn adopt_ground(&mut self, g:Ground) {
        if self.ground.is_some_and(|old|old.monitor!=g.monitor||old.dpi!=g.dpi) {
            self.squishy=super::squishy::Layer::default();
        }
        self.motion.x=super::rebase_x(self.motion.x,self.ground,g);
        self.ground=Some(g);
    }
    pub(super) fn peek_home(&self)->Option<(super::peek::Edge,windows::Win32::Foundation::RECT,f32)> {
        let h=self.cfg.home?;
        let screens:Vec<_>=self.home_monitors.iter().map(|(k,m)|(*k,m.rect)).collect();
        let dpi=self.home_monitors.iter().find(|(k,_)|*k==h.monitor).map_or(1.,|(_,m)|m.scale());
        super::peek::resolve(h,&screens,130.*self.cfg.size.scale()*dpi)
    }
    fn edge_candidate(&self,point:Point)->Option<Home> {
        let d=self.last_draw.as_ref()?;
        let screens:Vec<_>=self.home_monitors.iter().map(|(k,m)|(*k,m.rect)).collect();
        let body=if self.grab.phase==grab::Phase::Held {
            super::peek::bounds(&self.grab.pose(),point.x/d.dpi,point.y/d.dpi,d.scale,d.dpi)
        }else{super::peek::bounds(&d.pose,d.x,d.y,d.scale,d.dpi)};
        super::peek::candidate(&screens,body,point).filter(|h| {
            let dpi=self.home_monitors.iter().find(|(k,_)|*k==h.monitor).map_or(d.dpi,|(_,m)|m.scale());
            super::peek::resolve(*h,&screens,130.*self.cfg.size.scale()*dpi).is_some()
        })
    }
    pub(super) fn restore_home(&mut self) {
        let Some(home)=self.cfg.home else{return;};
        if home.edge.is_some() && self.peek_home().is_none() {
            // Display discovery can briefly be incomplete after resume. Keep
            // the saved anchor so reconnection can restore it automatically.
            if let Some(m)=self.home_monitors.iter().find(|(_,m)|m.primary).map(|(_,m)|*m) {
                self.ground=Some(self.ground_for(m));self.motion.x=(m.rect.left+m.rect.right) as f32/2./m.scale();
            }
            return;
        }
        if home.edge.is_some() && self.peek.next_look==0. {
            self.peek.next_look=self.start.elapsed().as_secs_f64()+9.;
        }
        if let Some(m)=home_monitor(Some(home), &self.home_monitors, self.dock_ground) {
            let g=self.ground_for(m);
            self.ground=Some(g);
            self.motion.x=home.x(m.rect.left as f32,m.rect.right as f32)/g.dpi;
        }
    }
    pub(super) fn accepts_ground(&self,g:Ground,primary:bool)->bool {
        accepts_ground(self.cfg.home,&self.home_monitors,g,primary,self.grab.busy())
    }
    pub(super) fn poll_ground(&mut self) {
        // No new timer: visible frames and the covered-state probe use TIMER.
        if let Some(grounds)=crate::dock::companion_grounds() {
            self.grounds=grounds.iter().map(|(g,_)|*g).collect();
            self.dock_ground=grounds.iter().find(|(_,primary)|*primary).map(|(g,_)|*g);
            if !self.grab.busy() {
                if let Some(home)=self.cfg.home {
                    let target=home_monitor(Some(home),&self.home_monitors,self.dock_ground);
                    if target.is_some_and(|m|self.ground.is_none_or(|g|g.monitor!=m.rect)) {self.restore_home();}
                } else if let Some(g)=self.dock_ground {self.adopt_ground(g);}
            }
        }
        if let Some((_,m))=self.home_monitors.iter().find(|(_,m)|self.ground.is_some_and(|g|g.monitor==m.rect)) {
            if let Some(m)=crate::monitors::info(m.handle) {
                self.adopt_ground(self.ground_for(m));
            }
        }
    }
    pub(super) fn ground_for(&self,m:crate::monitors::Monitor)->Ground {
        sampled_ground(m,self.grounds.iter().find(|g|g.monitor==m.rect).copied(),
            crate::dock::fullscreen_on_monitor(m.handle,m.rect))
    }
    pub(super) fn input(&mut self, event: Input) {
        if !self.cfg.enabled || !self.cfg.visible || self.trip.is_some() {
            return;
        }
        if self.cfg.ignore_mouse {
            let ctrl=unsafe{windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL.0 as i32)<0};
            if matches!(event,Input::Wheel{..})||matches!(event,Input::Down{..})&&!ctrl{return;}
        }
        let now=std::time::Instant::now().saturating_duration_since(self.start).as_secs_f64();
        if matches!(event,Input::Down{..}) {self.press.set(true,now);}
        if matches!(event,Input::Up{..}|Input::Cancel{..}) {self.press.set(false,now);}
        match event {
            Input::Wheel {steps}=>{
                let size=self.cfg.size.wheel(steps);self.cfg.size=size;self.pending_size=Some(size);
            }
            Input::Down { id, kind, point } => {
                self.peek_drag=self.peek_home().map(|_|point);
                self.bubble.set_kind(kind);
                if let Some(d) = &self.last_draw {
                    let pivot = super::render::grab_point(
                        &d.pose,
                        d.x,
                        d.y,
                        d.scale,
                        Point::new(point.x / d.dpi, point.y / d.dpi),
                    );
                    if self.grab.down(id, kind, point, pivot, d.scale * d.dpi) {self.drag_anchor=Some(point);}
                }
            }
            Input::Move { id, point } => {
                if !self.grab.accepts_release(id){return;}
                self.grab.moved(id, point);
                if let (Some(from),Some((edge,r,_)))=(self.peek_drag,self.peek_home()) {
                    if super::peek::detached(edge,r,from,point) {
                        self.peek.detach(now,edge);
                        if let Some(h)=&mut self.cfg.home {h.edge=None;}
                        self.peek_drag=None;self.pending_save=Some((self.cfg.home,true));
                    }
                }
                if self.grab.phase == grab::Phase::Held {
                    self.press.set(false,now);
                    let point = self.grab.point;
                    if let Some((_, m)) = self.home_monitors.iter().filter(|_|self.peek_home().is_none()).find(|(_, m)| {
                        point.x >= m.rect.left as f32
                            && point.x < m.rect.right as f32
                            && point.y >= m.rect.top as f32
                            && point.y < m.rect.bottom as f32
                    }) {
                        if self.ground.is_none_or(|g|g.monitor!=m.rect) {
                            let ground = self.ground_for(*m);
                            self.close_panels_on_monitor_change(ground.monitor);
                            self.ground = Some(ground);
                            self.grab.rescale(self.cfg.size.scale() * ground.dpi);
                            if let Some(p)=&self.pacer{p.refresh_display();}
                        }
                    }
                }
            }
            Input::Up { id, point } => self.release(id, point, false),
            Input::Cancel { id } => self.release(id, self.grab.point, true),
        }
    }
    fn close_panels_on_monitor_change(&mut self,monitor:windows::Win32::Foundation::RECT) {
        if self.ground.is_some_and(|g|g.monitor!=monitor) {
            super::chat::close_panels();self.bubble.dismiss();
        }
    }
    pub(super) unsafe fn move_drag_window(&mut self)->windows::core::Result<()> {
        if self.grab.phase!=grab::Phase::Held || self.peek_home().is_some() {return Ok(());}
        let point=self.grab.point;
        if let Some(old)=self.drag_anchor {
            let (dx,dy)=(point.x-old.x,point.y-old.y);
            let screens:Vec<_>=self.home_monitors.iter().map(|(_,m)|m.rect).collect();
            self.visible_frame=super::visibility::Frame::move_by(
                [self.sprite.as_ref(),self.shadow.as_ref(),self.particles.as_ref()].map(|s|s.map(|s|&s.window)),dx,dy,&screens,self.fade.opacity)?;
            self.shadow_picture=None;
            if let Some(d)=&mut self.last_draw{d.x+=dx/d.dpi;d.y+=dy/d.dpi;}
        }
        self.drag_anchor=Some(point);
        self.arm(Some(if self.battery{34}else{17}));Ok(())
    }
    fn release(&mut self, id: u32, point: Point, cancel: bool) {
        if !self.grab.accepts_release(id){return;}
        let slot = !cancel && crate::topbar::companion_slot_at(POINT{x:point.x as i32,y:point.y as i32});
        if !cancel {self.grab.moved(id,point);}
        if !cancel {
            if let (Some(from),Some((edge,r,_)))=(self.peek_drag,self.peek_home()) {
                if super::peek::detached(edge,r,from,point) {
                    self.peek.detach(self.start.elapsed().as_secs_f64(),edge);
                    if let Some(h)=&mut self.cfg.home {h.edge=None;}
                    self.peek_drag=None;self.pending_save=Some((self.cfg.home,true));
                }
            }
        }
        if self.grab.phase==grab::Phase::Held {
            if self.peek_drag.is_some() && self.peek_home().is_some() {
                self.grab.abort();self.peek_drag=None;self.drag_anchor=None;return;
            }
            if !cancel && !slot {
                if let Some(home)=self.edge_candidate(point) {
                    self.cfg.home=Some(home);self.cfg.visible=true;self.shown=true;
                    self.grab.abort();self.drag_anchor=None;self.peek_drag=None;
                    self.fx.clear();
                    self.peek.enter(self.start.elapsed().as_secs_f64(),super::fx::Random::unit(&mut self.rng));
                    if let Some((_,m))=self.home_monitors.iter().find(|(key,_)|*key==home.monitor){self.close_panels_on_monitor_change(m.rect);}
                    self.restore_home();self.pending_save=Some((Some(home),true));return;
                }
            }
        }
        self.peek_drag=None;
        let m = unsafe {
            crate::monitors::info(
                MonitorFromPoint(
                    POINT {
                        x: point.x as i32,
                        y: point.y as i32,
                    },
                    MONITOR_DEFAULTTONEAREST,
                )
                .0 as isize,
            )
        };
        let Some(g)=release_ground(m.map(|m|self.ground_for(m)), self.ground,self.dock_ground) else {
            self.grab.abort();
            return;
        };
        let margin = self.cfg.size.margin() * g.dpi;
        let left = g.monitor.left as f32 + margin;
        let right = (g.monitor.right as f32 - margin).max(left);
        let result = if cancel {
            self.grab.cancel(id, g.floor, left, right)
        } else {
            self.grab.up(id, point, slot, g.floor, left, right)
        };
        match result {
            Release::Tap=>self.tap(),
            Release::Hide|Release::Fall=>{
                self.close_panels_on_monitor_change(g.monitor);
                self.ground=Some(g);
                self.grab.rescale(self.cfg.size.scale()*g.dpi);
                let x=self.grab.point.x.clamp(left,right);
                self.motion.x=x/g.dpi;
                let home=dropped_home(g,x,&self.home_monitors,self.cfg.home);
                self.cfg.home=Some(home);
                let visible=result==Release::Fall;
                self.cfg.visible=visible;
                self.shown=visible;
                if !visible {self.stop();}
                self.pending_save=Some((Some(home),visible));
            }
            Release::None=>{}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::RECT;
    #[test]
    fn restart_selects_saved_monitor_and_missing_display_falls_back_to_dock() {
        let primary = crate::monitors::Monitor {
            handle: 1,
            primary: true,
            dpi: 96,
            rect: RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            work: RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1040,
            },
        };
        let other = crate::monitors::Monitor {
            handle: 2,
            primary: false,
            dpi: 144,
            rect: RECT {
                left: -2560,
                top: -200,
                right: 0,
                bottom: 1240,
            },
            ..primary
        };
        let home = Home::at(22, -1920., -2560., 0.);
        let mut config = super::super::model::Config::default();
        config.home = Some(home);
        let saved: super::super::model::Config =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        let monitors = [(11, primary), (22, other)];
        assert_eq!(home_monitor(saved.home, &monitors, None), Some(other));
        assert_eq!(saved.home.unwrap().x(-2560., 0.), -1920.);
        assert_eq!(home_monitor(saved.home, &monitors[..1], Some(Ground{monitor:primary.rect,floor:1040.,dpi:1.,covered:false})), Some(primary));
        assert_eq!(home_monitor(None, &monitors, None), None);
        assert_eq!(home_monitor(saved.home, &[], None), None);
        // The dock may live on a secondary display. OS primary is irrelevant.
        let dock=sampled_ground(other,None,false);
        assert_eq!(home_monitor(Some(Home::at(99,0.,0.,1.)),&monitors,Some(dock)),Some(other));
        assert!(accepts_ground(None,&monitors,dock,true,false));
        assert!(!accepts_ground(None,&monitors,dock,false,false));
        assert!(accepts_ground(saved.home,&monitors,dock,false,false));
        assert!(!accepts_ground(saved.home,&monitors,sampled_ground(primary,None,false),true,false));
        assert!(accepts_ground(saved.home,&monitors[..1],sampled_ground(primary,None,false),true,false));
        assert!(!accepts_ground(None,&monitors,dock,true,true));
        let dropped=dropped_home(dock,-1280.,&monitors,Some(Home::at(11,100.,0.,1920.)));
        let restored:Home=serde_json::from_str(&serde_json::to_string(&dropped).unwrap()).unwrap();
        assert_eq!(home_monitor(Some(restored),&monitors,None),Some(other));
        assert_eq!(restored.x(other.rect.left as f32,other.rect.right as f32)/dock.dpi,-1280./1.5);
    }
    #[test]
    fn review_cover_refresh_and_drop_finish_without_a_busy_hidden_grab() {
        let mut m=crate::monitors::Monitor{handle:2,primary:false,dpi:144,
            rect:RECT{left:1920,top:0,right:3840,bottom:1080},
            work:RECT{left:1920,top:0,right:3840,bottom:1040}};
        let mut grab=grab::Grab::default();
        assert!(!covered_stops(sampled_ground(m,None,false).covered,&grab));
        m.work.bottom=1000;
        let covered=sampled_ground(m,None,true);
        assert_eq!(covered.floor,1000.);
        assert!(covered_stops(covered.covered,&grab));
        grab.abort();
        assert!(!grab.busy());
        assert!(!covered_stops(sampled_ground(m,None,false).covered,&grab));
        grab.down(7,grab::Kind::Mouse,Point::new(2200.,0.),Point::new(0.5,0.),0.55);
        grab.moved(7,Point::new(2300.,0.));
        assert!(!covered_stops(true,&grab));
        // A failed native monitor lookup still cancels into a real fall.
        let fallback=release_ground(None,Some(covered),None).unwrap();
        assert_eq!(grab.cancel(7,fallback.floor,2000.,3700.),Release::Fall);
        let mut landed=false;
        for _ in 0..1000 {
            if covered_stops(true,&grab) {break;}
            landed|=grab.step(1./120.).landed;
        }
        assert!(landed);
        assert_eq!(grab.phase,grab::Phase::Peek);
        assert!(covered_stops(true,&grab));
        grab.abort();
        assert!(!grab.busy());
        assert!(!covered_stops(false,&grab));
        assert_eq!(release_ground(None,None,Some(covered)),Some(covered));
        assert_eq!(release_ground(None,None,None),None);
        let dock=Ground{floor:950.,covered:false,..covered};
        assert_eq!(sampled_ground(m,Some(dock),true).floor,950.);
        assert!(sampled_ground(m,Some(dock),true).covered);
    }
}

/// Both callbacks run before returning to the native message pump.
pub(super) fn deliver<S,R>(state:&mut S,event:Input,process:impl FnOnce(&mut S,Input),move_window:impl FnOnce(&mut S)->R,paint:impl FnOnce(&mut S)->R)->R {
 process(state,event);if matches!(event,Input::Move{..}){move_window(state)}else{paint(state)}
}
#[cfg(test)] mod immediate_tests {
 use super::*;
 #[test] fn high_rate_moves_only_present_and_paced_raster_respects_battery_cap() {
  struct Fake {grab:grab::Grab,moves:usize,rasters:usize,steps:usize,window:Point}
  for battery in [false,true] {for kind in [grab::Kind::Mouse,grab::Kind::Touch,grab::Kind::Pen] {
   let mut f=Fake{grab:Default::default(),moves:0,rasters:0,steps:0,window:Point::default()};
   f.grab.down(1,kind,Point::new(100.,100.),Point::new(0.5,0.1),0.56);
   let mut last=0.;
   for i in 1..=240 {
    let now=i as f64/240.;let point=Point::new(120.+i as f32,100.);
    deliver(&mut f,Input::Move{id:1,point},|f,event|{if let Input::Move{id,point}=event{f.grab.moved(id,point);}},
     |f|{f.window=f.grab.point;f.moves+=1;},|_|panic!("Move must not enter draw"));
    assert_eq!(f.window,point);
    // Simulate extra timer/refresh messages too: all use the production gate.
    if super::super::timing::drag_due(now-last,battery) {f.grab.step((now-last) as f32);last=now;f.steps+=1;f.rasters+=1;}
   }
   assert_eq!(f.moves,240);assert_eq!(f.steps,f.rasters);
   assert!(f.rasters<=if battery{31}else{61});assert_eq!(f.rasters,if battery{30}else{60});
   let theta=f.grab.theta;f.grab.step(if battery{1./30.}else{1./60.});assert_ne!(f.grab.theta,theta);
  }}
 }
 #[test] fn fake_down_and_release_paint_in_the_same_message_under_sixteen_ms(){
  use super::super::{gestures::{self,Press,Click},Motion};
  struct Fake {grab:grab::Grab,press:Press,events:Vec<&'static str>,chat:bool,t:f64}
  let mut f=Fake{grab:Default::default(),press:Default::default(),events:vec![],chat:false,t:10.};
  let start=std::time::Instant::now();
  for event in [Input::Down{id:1,kind:grab::Kind::Mouse,point:Point::new(100.,100.)},Input::Up{id:1,point:Point::new(100.,100.)}] {
   deliver(&mut f,event,|f,event|{f.events.push("input");match event {
    Input::Down{id,kind,point}=>{f.press.set(true,f.t);f.grab.down(id,kind,point,Point::new(0.5,0.5),0.56);},
    Input::Up{id,point}=>{f.press.set(false,f.t);if f.grab.up(id,point,false,500.,0.,1000.)==Release::Tap{f.chat=gestures::click(None,true)==Click::Chat;}},_=>{}}},
    |_|panic!("press/release must render"),|f|{f.events.push("render");let mut p=Motion::new(0.).pose();f.press.apply(&mut p,f.t,true);assert!(p.sy<1.||f.chat);});
   f.events.push("return");f.t+=0.1;
  }
  assert!(f.chat);assert_eq!(f.events,["input","render","return","input","render","return"]);
  assert!(start.elapsed()<std::time::Duration::from_millis(16));
 }
}
