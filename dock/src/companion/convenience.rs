//! Shared actions for body tools, context menu, slot and optional shortcut.
use super::{model::{Config,Size},APP};
use crate::popup::Item;
use std::sync::Mutex;
use windows::Win32::Foundation::*;
#[derive(Clone,Copy,Debug,PartialEq)]
pub enum Action {
    Chat, NewNote,
    CancelShutdown(u64), Agenda(i64),
    Hide,Size(f32),Step(i32),Rest(u64),Snooze(u64),Wake,Detach,Ignore,Settings,Fan,
}
static QUEUE:Mutex<Vec<Action>>=Mutex::new(Vec::new());
thread_local! {
 static MENU_OPEN:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};
 static MENU_LEFT:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};
}
pub fn menu_open()->bool{MENU_OPEN.with(|v|v.get())}
pub(crate) fn menu_side(left:bool){MENU_LEFT.with(|v|v.set(left));super::refresh();}
pub(super) fn menu_frame()->&'static str {if MENU_LEFT.with(|v|v.get()){"look_3"}else{"look_5"}}

#[derive(Clone,Copy,Debug)]
pub(crate) struct MenuBody {pub rect:RECT,pub dpi:f32}
/// Include the opaque gaze silhouettes: looking up can widen a narrow pose
/// (notably look_2). Reserve its breathing envelope so it stays clear later.
#[cfg(test)]
fn gaze_envelope(mut rect:RECT,x:f32,y:f32,scale:f32,dpi:f32)->RECT {
 static BOUNDS:std::sync::OnceLock<[RECT;2]>=std::sync::OnceLock::new();
 let names=["look_3","look_5"];
 let bounds=BOUNDS.get_or_init(||names.map(|name|{
  let f=&super::clips::catalog().frames[name];
  let mut r=RECT{left:0,top:0,right:f.w as i32,bottom:f.h as i32};
  if let Some(im)=super::clips::png(name,"full").and_then(|b|crate::icons::decode_bytes_fit(b,4096)) {
   r.left=im.width as i32;r.top=im.height as i32;r.right=0;r.bottom=0;
   for (i,p) in im.pixels.as_chunks::<4>().0.iter().enumerate(){if p[3]>1 {
    let (x,y)=(i as i32%im.width as i32,i as i32/im.width as i32);
    r.left=r.left.min(x);r.top=r.top.min(y);r.right=r.right.max(x+1);r.bottom=r.bottom.max(y+1);
   }}
  }r
 }));
 for (name,r) in names.into_iter().zip(bounds) {
  for skew in [-1.4,1.4] {for tilt in [-1.,1.] {
   let mut p=super::Motion::new(x).pose();p.frame=name.into();p.sx=1.02;p.sy=1.02;p.skew=skew;p.tilt=tilt;
   let m=super::render::body_matrix(&p,x,y,scale);
   for (u,v) in [(r.left,r.top),(r.right,r.top),(r.left,r.bottom),(r.right,r.bottom)] {
    let px=(u as f32*m.M11+v as f32*m.M21+m.M31)*dpi;
    let py=(u as f32*m.M12+v as f32*m.M22+m.M32)*dpi;
    rect.left=rect.left.min(px.floor() as i32);rect.right=rect.right.max(px.ceil() as i32);
    rect.top=rect.top.min(py.floor() as i32);
   }
  }}
 }
 rect
}
impl MenuBody {
 pub(crate) fn room(self,work:RECT)->i32 {
  let gap=(8.*self.dpi).round() as i32;
  (work.right-self.rect.right-gap).max(self.rect.left-gap-work.left).max(1)
 }
}
/// Screen pixels, with the DIP gap resolved at the body's monitor scale.
pub(crate) fn menu_anchor(body:MenuBody,menu_size:SIZE,work:RECT)->(POINT,crate::popup::Align) {
 let gap=(8.*body.dpi).round() as i32;
 let right=work.right-body.rect.right-gap;
 let left=body.rect.left-gap-work.left;
 let on_right=right>=menu_size.cx || left<menu_size.cx && right>=left;
 (POINT{x:if on_right{body.rect.right+gap}else{body.rect.left-gap},y:body.rect.bottom},
  if on_right{crate::popup::Align::BottomLeft}else{crate::popup::Align::BottomRight})
}
pub(crate) struct MenuVisit(bool);
impl MenuVisit {pub(crate) fn new()->Self{let old=MENU_OPEN.with(|v|v.replace(true));super::refresh();Self(old)}}
impl Drop for MenuVisit {fn drop(&mut self){MENU_OPEN.with(|v|v.set(self.0));super::refresh();}}
pub fn now()->u64 {std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()}
fn morning_local(local:i64)->i64{let today=local.div_euclid(86400)*86400+5*3600;if local<today{today}else{today+86400}}
pub fn tomorrow(now:u64)->u64 {let local=crate::agenda::tz::local_from_utc(now as i64);crate::agenda::tz::utc_from_local(morning_local(local)).max(now as i64) as u64}
pub fn remaining(end:Option<u64>,now:u64)->Option<u32>{end.filter(|&e|e>now).map(|e|(e-now).div_ceil(60).min(u32::MAX as u64) as u32)}
// Hidden companions have no frame timer. Keep only the persisted wake deadline,
// including the last hide-trip frame and fullscreen/device suspension paths.
pub fn hidden_timer(c:Config,t:u64)->Option<u32>{
 c.snooze_until.filter(|_|c.enabled).map(|end|end.saturating_sub(t).saturating_mul(1000).clamp(1,0x7fff_ffff) as u32)
}
pub fn apply(c:&mut Config,a:Action,t:u64) {match a {
 Action::Hide=>c.set_visible(false),Action::Size(s)=>c.size=Size::new(s),Action::Step(n)=>c.size=c.size.wheel(n),
 Action::Snooze(end)=>{c.visible=false;c.snooze_until=Some(end.max(t+1));},Action::Wake=>c.set_visible(true),
 Action::Detach=>{if let Some(h)=&mut c.home{h.release_edge();}},
 Action::Ignore=>c.ignore_mouse = !c.ignore_mouse,_=>{}
}}
pub fn request(a:Action){QUEUE.lock().unwrap().push(a);super::refresh();}
fn snooze_action(c:Config,t:u64)->Action{if !c.visible||c.snooze_until.is_some(){Action::Wake}else{Action::Rest(t+1800)}}
pub fn snooze_toggle(){if let Some(c)=APP.with(|a|a.borrow().as_ref().map(|a|a.cfg)){request(snooze_action(c,now()));}}
pub fn drain(){let queued=std::mem::take(&mut *QUEUE.lock().unwrap());for action in queued {
 match action {
  Action::NewNote => { crate::notes::new_note(); },
  Action::Chat => super::chat::open(),
  Action::CancelShutdown(g) => { if crate::topbar::shutdown::cancel_generation(g) { super::cue(super::Cue::ShutdownCancelled); } }
  Action::Agenda(at) => crate::topbar::companion_agenda(at),
            Action::Settings=>crate::settings::launch_page("companion"),Action::Fan => APP.with(|a| {
                if let Some(a) = a.borrow_mut().as_mut() {
                    if !a.cfg.enabled || !a.cfg.visible || a.trip.is_some() { return; }
                    let now = a.start.elapsed().as_secs_f64();
                    if a.bubble.open() { a.bubble.close(now); }
                    else { a.fx.clear(); a.bubble.set_kind(super::grab::Kind::Mouse); a.bubble.show(now); }
                    a.immediate = true;
                }
            }),
  Action::Rest(end)=>{
   APP.with(|a|{let mut a=a.borrow_mut();if let Some(a)=a.as_mut(){a.bubble.close(a.start.elapsed().as_secs_f64());if a.shown&&a.cfg.animations&&a.animations&&a.peek_home().is_none() {a.rest=Some((std::time::Instant::now(),end));a.immediate=true;}}});
   // Persist before the yawn so fullscreen, session suspension or restart
   // cannot lose an already chosen break.
   if !crate::dock::companion_action(Action::Snooze(end)){request(Action::Snooze(end));}
  },
  _ => {
      if matches!(action, Action::Wake | Action::Hide) {
          APP.with(|a| {
              if let Some(a) = a.borrow_mut().as_mut() {
                  a.rest = None;
              }
          });
      }
      if !crate::dock::companion_action(action) {
          request(action);
      }
  }
 }
}}
pub fn wake_due(){let due=APP.with(|a| {
        a.borrow().as_ref().is_some_and(|a|a.cfg.enabled&&a.cfg.snooze_until.is_some_and(|t|now()>=t))
    });if due{let _=crate::dock::companion_action(Action::Wake);}}
pub(super) fn slot_items(c:Config)->Vec<Item>{use crate::i18n::t;vec![
        Item::new(1,t("companion.menu_hide")),Item::new(0,t("companion.size")).submenu([("companion.tiny",10),("companion.small",11),("companion.normal",12),("companion.large",13),
            ].into_iter().map(|(s,id)|Item::new(id,t(s))).collect(),
        ),
 Item::new(0,t("companion.rest")).submenu(rest_items()),Item::new(31,t("companion.ignore_mouse.title")).checked(c.ignore_mouse),Item::new(33,t("companion.settings")),
    ]}
pub(super) fn morning_label(t:u64)->&'static str {let local=crate::agenda::tz::local_from_utc(t as i64);if local.rem_euclid(86400)<5*3600{"companion.rest_morning_today"}else{"companion.rest_morning"}}
fn rest_items()->Vec<Item>{["companion.rest30","companion.rest60",morning_label(now())].into_iter().enumerate().map(|(i,k)|Item::new(20+i as u32,crate::i18n::t(k))).collect()}
fn slot_picked(id:u32,t:u64)->Option<Action>{Some(match id {
        1=>Action::Hide,10=>Action::Size(0.34),11=>Action::Size(0.42),12=>Action::Size(0.56),13=>Action::Size(0.70),20=>Action::Rest(t+1800),21=>Action::Rest(t+3600),22=>Action::Rest(tomorrow(t)),31=>Action::Ignore,33=>Action::Settings,_=>return None,
    })}
thread_local! { static MENU_PRIORITY: std::cell::RefCell<Option<super::cues::Click>> = const { std::cell::RefCell::new(None) }; }
/// The old home-cell controls live inside the bar's NopBar menu.
pub fn bar_items() -> Vec<Item> {
    let cfg = APP.with(|a| a.borrow().as_ref().map(|a| a.cfg));
    let Some(c) = cfg.filter(|c| c.enabled) else { return Vec::new(); };
    let mut rows = slot_items(c);
    rows[0] = Item::new(1, crate::i18n::t(if c.visible { "companion.hide" } else { "companion.show" }));
    let priority = if c.situations {
        APP.with(|a| a.borrow().as_ref().and_then(|a| a.reactions.click(std::time::Instant::now(), !a.model.signal.waits.is_empty())))
    } else { None };
    // What the menu shows is what id 2 does, even if the cue changes while it is open.
    MENU_PRIORITY.with(|p| *p.borrow_mut() = priority.clone());
    if let Some(action) = priority {
        let key = match action { super::cues::Click::CancelShutdown(_) => "topbar.system.schedule.cancel", super::cues::Click::Agenda(_) => "companion.next_event" };
        rows.insert(0, Item::new(2, crate::i18n::t(key)));
    }
    rows
}
pub fn bar_picked(id: u32) {
    if id == 2 { if let Some(action) = MENU_PRIORITY.with(|p| p.borrow_mut().take()) { super::slot_click_with(action); } }
    else if id == 1 {
        let visible = APP.with(|a| a.borrow().as_ref().is_some_and(|a| a.cfg.visible));
        request(if visible { Action::Hide } else { Action::Wake });
    } else if let Some(action) = slot_picked(id, now()) { request(action); }
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn looking_up_from_a_narrow_pose_stays_clear_of_the_menu() {
  unsafe {let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
  for dpi in [1.,1.25,1.5,2.] {for scale in [0.34,0.56,0.70] {
   let (x,y)=(500.,700.);let f=&super::super::clips::catalog().frames["look_2"];
   let old=RECT{left:((x-f.ax*scale)*dpi) as i32,right:((x+(f.w as f32-f.ax)*scale)*dpi) as i32,top:500,bottom:(y*dpi) as i32};
   let rect=gaze_envelope(old,x,y,scale,dpi);
   let body=MenuBody{rect,dpi};let size=SIZE{cx:240,cy:220};
   for work in [RECT{left:0,top:0,right:3000,bottom:2000},RECT{left:0,top:0,right:rect.right+7,bottom:2000}] {
    let (at,align)=menu_anchor(body,size,work);let (mx,_)=crate::popup::level_origin(align,None,false,at,size.cx,size.cy,work,dpi);
    for name in ["look_3","look_5"] {let f=&super::super::clips::catalog().frames[name];
     let l=(x-f.ax*scale)*dpi;let r=(x+(f.w as f32-f.ax)*scale)*dpi;
     assert!(mx as f32>=r || (mx+size.cx) as f32<=l,"{name} at {dpi} / {scale}");
    }
   }
  }}
 }
 #[test] fn menu_opens_beside_the_body_with_its_bottom_at_the_feet() {
  let work=RECT{left:-1920,top:0,right:0,bottom:1080};
  for dpi in [1.,1.25,1.5,2.] {for x in [-1600,-160] {
   let rect=RECT{left:x,top:850,right:x+100,bottom:1000};let body=MenuBody{rect,dpi};
   let size=SIZE{cx:240,cy:220};let (p,a)=menu_anchor(body,size,work);
   assert_eq!(p.y,rect.bottom);
   assert_eq!(a,if x==-160{crate::popup::Align::BottomRight}else{crate::popup::Align::BottomLeft});
   let (mx,my)=crate::popup::level_origin(a,None,false,p,size.cx,size.cy,work,dpi);
   assert!(mx>=rect.right+(8.*dpi) as i32 || mx+size.cx<=rect.left-(8.*dpi) as i32);
   assert_eq!(my+size.cy,rect.bottom);
   let (_,my)=crate::popup::level_origin(a,None,false,p,size.cx,1300,work,dpi);assert_eq!(my,work.top);
  }}
 }
 #[test] fn narrow_work_area_caps_menu_width_before_clamping() {
  let work=RECT{left:0,top:0,right:320,bottom:240};
  let body=MenuBody{rect:RECT{left:120,top:120,right:200,bottom:220},dpi:1.};
  let size=SIZE{cx:body.room(work),cy:240};let (at,align)=menu_anchor(body,size,work);
  let (x,_)=crate::popup::level_origin(align,None,false,at,size.cx,size.cy,work,1.);
  assert!(x>=body.rect.right+8 || x+size.cx<=body.rect.left-8);
 }
 #[test] fn context_menu_gaze_follows_its_side() {
  menu_side(false);assert_eq!(menu_frame(),"look_5");
  menu_side(true);assert_eq!(menu_frame(),"look_3");menu_side(false);
 }
 #[test] fn morning_deadline_uses_the_next_five_am(){for (hour,minute,day) in [(1,0,0),(4,59,0),(5,0,1),(23,0,1)]{assert_eq!(morning_local(hour*3600+minute*60),day*86400+5*3600);}}
 #[test] fn snooze_hotkey_restores_hidden_and_cancels_a_break(){let mut c=Config::default();assert_eq!(snooze_action(c,100),Action::Wake);c.set_visible(true);assert_eq!(snooze_action(c,100),Action::Rest(1900));apply(&mut c,Action::Snooze(1900),100);assert_eq!(snooze_action(c,100),Action::Wake);for v in [false,true]{c.snooze_until=Some(1900);c.set_visible(v);assert_eq!(c.snooze_until,None);assert_eq!(c.visible,v);}}
 #[test] fn own_menu_visit_is_scoped(){assert!(!menu_open());assert!(super::super::pause_popup().is_some());{let _v=MenuVisit::new();assert!(menu_open());assert!(super::super::pause_popup().is_none());{let _nested=MenuVisit::new();assert!(menu_open());}assert!(menu_open());}assert!(!menu_open());}
 #[test] fn hidden_timer_keeps_snooze_deadline_until_wake_or_manual_hide(){
  let mut c=Config::default();apply(&mut c,Action::Snooze(1900),100);
  assert_eq!(hidden_timer(c,100),Some(1_800_000));
  // Ending the yawn/hide sequence must retain a wake timer with no animation.
  c.animations=false;assert_eq!(hidden_timer(c,102),Some(1_798_000));
  c=serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
  assert_eq!(hidden_timer(c,1901),Some(1));
  c.enabled=false;assert_eq!(hidden_timer(c,102),None);c.enabled=true;
  apply(&mut c,Action::Wake,1901);assert_eq!(hidden_timer(c,1901),None);
  apply(&mut c,Action::Snooze(u64::MAX),1901);assert_eq!(hidden_timer(c,1901),Some(0x7fff_ffff));
  apply(&mut c,Action::Hide,1901);assert_eq!(hidden_timer(c,1901),None);
 }
 #[test] fn menu_actions_persist_and_snooze_resumes_across_restart(){let mut c=Config::default();apply(&mut c,Action::Snooze(1900),100);assert_eq!(remaining(c.snooze_until,100),Some(30));let mut c:Config=serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();assert!(!c.visible);assert_eq!(remaining(c.snooze_until,1900),None);apply(&mut c,Action::Wake,1900);assert!(c.visible&&c.snooze_until.is_none());for id in [10,11,12,13,31]{apply(&mut c,slot_picked(id,0).unwrap(),0);}assert!(c.ignore_mouse);assert!(slot_picked(30,0).is_none()&&slot_picked(32,0).is_none());assert_eq!(c.size.scale(),0.70);}
}

/// A persisted break resumes through the normal show trip, then one stretch.
#[derive(Default)]
pub(super) struct Wake {pending:bool,at:Option<f64>}
impl Wake {
 pub fn configure(&mut self,old:Config,new:Config) {
  if !new.enabled||!new.visible {*self=Self::default();}
  else if old.snooze_until.is_some()&&!old.visible {self.pending=true;self.at=None;}
 }
 pub fn active(&self)->bool {self.pending}
 pub fn frame(&mut self,now:f64,animated:bool,ready:bool)->Option<String> {
  if !self.pending||!ready {return None;}
  if !animated {*self=Self::default();return None;}
  let t=(now-*self.at.get_or_insert(now)) as f32;
  let clip=super::clips::catalog().clip("stretch");
  if t>=clip.duration() {*self=Self::default();None}else{Some(clip.frame(t).into())}
 }
}
#[cfg(test)] mod polish_tests {
 use super::*;
 #[test] fn bar_slot_menu_retains_short_product_labels() {
  let _lock=crate::i18n::test_lock("ko");let menu=slot_items(Config::default());
  assert_eq!(menu.iter().map(|i|i.label.as_str()).collect::<Vec<_>>(),[
                        "숨기기","크기","잠깐 숨기기","클릭이 노움을 통과","노움 설정…"]);
  assert!(!menu[1].sub.is_empty()&&!menu[2].sub.is_empty());
  assert_eq!(crate::i18n::t("companion.rest"),"잠깐 숨기기");
 }
 #[test] fn break_return_stretches_once_after_landing_even_without_life() {
  for at in [1900,500] {
   let mut old=Config::default();old.enabled=true;old.life=false;apply(&mut old,Action::Snooze(1900),100);
   let mut new=old;apply(&mut new,Action::Wake,at);
   let mut wake=Wake::default();wake.configure(old,new);assert!(wake.active());
   assert!(wake.frame(2.,true,false).is_none());assert!(wake.frame(9.,true,false).is_none());
   assert!(wake.frame(10.,true,true).unwrap().starts_with("stretch_"));
   let end=10.+super::super::clips::catalog().clip("stretch").duration() as f64+0.01;
   assert!(wake.frame(end,true,true).is_none());assert!(!wake.active());assert!(wake.frame(end+1.,true,true).is_none());
   wake.configure(old,new);assert!(wake.frame(10.,false,true).is_none());
   wake.configure(old,new);new.visible=false;wake.configure(old,new);assert!(!wake.active());
  }
 }
}
