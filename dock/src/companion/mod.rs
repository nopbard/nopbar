//! The approved v8 desktop gnome and opt-in companion chat.
mod checkpoint;
pub(crate) mod anchors;
mod panels;
mod bubble;
pub mod chat;
pub(crate) mod remind;
pub(crate) mod device_battery;
pub mod clips;
mod comfort;
mod squishy;
mod connection_look;
pub(crate) mod convenience;
pub mod cues;
mod fan;
pub mod fx;
mod gestures;
pub mod grab;
mod interaction;
pub(crate) mod purpose;
mod assistant;
pub(crate) mod downloads;
pub(crate) mod routine;
mod card;
mod living;
mod pacer;
pub mod peek;
#[cfg(test)]
mod peek_snapshots;
#[cfg(test)]
mod squishy_snapshots;
mod rhythm;
mod size;
mod timing;
pub use convenience::{bar_items, bar_picked, snooze_toggle};
pub(crate) use convenience::MenuVisit;
pub use cues::{Calm, Cue};
#[cfg(test)]
mod harmony;
#[cfg(test)]
mod gnome_fixes_snapshots;
mod hotkey;
pub mod model;
pub mod motion;
pub mod render;
mod retry;
pub(crate) mod visibility;
pub(crate) mod window;
pub use hotkey::{apply_hotkey, apply_snooze_hotkey, release_hotkey};
pub use living::session_away;

use model::{Config, Model, State};
use motion::Motion;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicBool, AtomicIsize, Ordering},
    time::Instant,
};
use windows::Win32::{Foundation::*, UI::WindowsAndMessaging::*};


pub const WM_REFRESH: u32 = WM_APP + 130;
pub const WM_TOGGLE: u32 = WM_APP + 131;
pub const WM_APP_TICK: u32 = WM_APP + 132;
pub const TIMER: usize = 0x4347;
pub const TOGGLE_TIMER: usize = 0x4348;
pub const SNOOZE_HOTKEY_ID:i32=0x4349;
pub const HOTKEY_ID: i32 = 0x4347;
static OWNER: AtomicIsize = AtomicIsize::new(0);
static CAPTURE_ANCHOR: std::sync::Mutex<Option<(i32,i32)>>=std::sync::Mutex::new(None);
static CAPTURE_REACTION: AtomicBool=AtomicBool::new(false);
pub(crate) fn capture_target()->Option<(i32,i32)>{*CAPTURE_ANCHOR.lock().unwrap_or_else(|e|e.into_inner())}
fn set_capture_target(target: Option<(i32, i32)>) {
    *CAPTURE_ANCHOR.lock().unwrap_or_else(|e| e.into_inner()) = target;
}
pub(crate) fn capture_received(){CAPTURE_REACTION.store(true,Ordering::SeqCst);refresh();}
static PENDING: AtomicBool = AtomicBool::new(false);
static INPUT: std::sync::Mutex<std::collections::VecDeque<grab::Input>>=std::sync::Mutex::new(std::collections::VecDeque::new());
static CUES: std::sync::Mutex<Vec<(Cue,Instant)>> = std::sync::Mutex::new(Vec::new());
static CALM: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static UI_PAUSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static UI_COVERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
thread_local! {static APP:RefCell<Option<App>>=const {RefCell::new(None)};}

pub struct UiPause {cover:bool}
pub fn pause_cover()->UiPause {
    UI_COVERS.fetch_add(1,Ordering::SeqCst);
    refresh();UiPause{cover:true}
}
pub fn ui_covered()->bool {UI_COVERS.load(Ordering::SeqCst)>0}
pub fn pause_popup()->Option<UiPause>{(!convenience::menu_open()).then(pause_ui)}
pub fn pause_ui() -> UiPause {
    UI_PAUSES.fetch_add(1, Ordering::SeqCst);
    APP.with(|a| {
        if let Ok(mut a) = a.try_borrow_mut() {
            if let Some(a) = a.as_mut() {
                a.set_yielding(true);
            }
        }
    });
    refresh();
    UiPause{cover:false}
}
impl Drop for UiPause {
    fn drop(&mut self) {
        if self.cover {UI_COVERS.fetch_sub(1,Ordering::SeqCst);}else{UI_PAUSES.fetch_sub(1, Ordering::SeqCst);}
        refresh();
    }
}
pub fn ui_paused() -> bool {
    UI_PAUSES.load(Ordering::SeqCst) > 0
}

#[derive(Clone, Copy)]
pub struct SlotAnchor {
    pub rect: RECT,
    pub dpi: f32,
}
impl SlotAnchor {
    fn fallback(g: Ground) -> Self {
        Self {
            rect: RECT {
                left: g.monitor.right - (60.0 * g.dpi) as i32,
                right: g.monitor.right - (40.0 * g.dpi) as i32,
                top: g.monitor.top,
                bottom: g.monitor.top + (28.0 * g.dpi) as i32,
            },
            dpi: g.dpi,
        }
    }
    fn destination(self, source_dpi: f32) -> (f32, f32) {
        (
            (self.rect.left + self.rect.right) as f32 / 2.0 / source_dpi,
            (self.rect.bottom as f32 - 4.0 * self.dpi) / source_dpi,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ground {
    pub monitor: RECT,
    pub floor: f32,
    pub dpi: f32,
    pub covered: bool,
}
fn rebase_x(x: f32, old: Option<Ground>, new: Ground) -> f32 {
    match old {
        Some(old) if old.monitor == new.monitor => x * old.dpi / new.dpi,
        _ => (new.monitor.left + new.monitor.right) as f32 / 2.0 / new.dpi,
    }
}
struct Trip {
    at: Instant,
    out: bool,
}
struct App {
    gfx: Rc<crate::gfx::Gfx>,
    cfg: Config,
    model: Model,
    motion: Motion,
    squishy: squishy::Layer,
    comfort: comfort::Layer,
    connection: connection_look::Look,
    body_cache: connection_look::BodyCache,
    body_cache_origin: Option<(u32,u32,u32)>,
    body_clock: timing::BodyClock,
    wake: convenience::Wake,
    last: Instant,
    start: Instant,
    ground: Option<Ground>,
    trip: Option<Trip>,
    sprite: Option<window::Surface>,
    shadow: Option<window::Surface>,
    particles: Option<window::Surface>,
    fx: fx::Engine,
    rng: fx::Rng,
    animations: bool,
    reactions: cues::Reactions,
    purpose: purpose::Purpose,
    purpose_due: Instant,
    aside: purpose::Aside,
    rhythm: rhythm::Rhythm,
    pending_greet: Option<Option<u32>>,
    checkpoint: checkpoint::Checkpoint,
    grab: grab::Grab,
    drag_anchor: Option<grab::Point>,
    peek: peek::Runtime,
    peek_drag: Option<grab::Point>,
    last_draw: Option<interaction::Drawing>,
    rest: Option<(Instant,u64)>,
    immediate: bool,
    press: gestures::Press,

    bubble: bubble::Bubble,
    pending_save: Option<(Option<grab::Home>,bool)>,
    pending_size: Option<model::Size>,
    home_monitors: Vec<(u64,crate::monitors::Monitor)>,
    grounds: Vec<Ground>,
    dock_ground: Option<Ground>,
    ground_due: Instant,
    slot_shown: bool,
    slot_notice_until: Option<Instant>,
    slot_attention: bool,
    slot_tip: Option<String>,
    deferred: Option<model::Signal>,
    generation: u32,
    size: (u32, u32),
    timer: Option<u32>,
    pacer: Option<pacer::Pacer>,
    shown: bool,
    picture: Option<render::Stamp>,
    particle_picture: Option<render::ParticleStamp>,
    visible_frame: visibility::Frame,
    shadow_picture: Option<(i32, i32, u32, bool)>,
    retry: retry::Retry,
    battery: bool,
    light: bool,
    yielding: bool,
    fade: visibility::Fade,
    hidden_since: Option<Instant>,
    yield_from: Option<(Instant, motion::Pose)>,
}
pub fn init(gfx: &Rc<crate::gfx::Gfx>, owner: HWND, cfg: &crate::config::Config) {
    let _=clips::catalog(); // Precompute landmarks before input can render.
    OWNER.store(owner.0 as isize, Ordering::SeqCst);
    downloads::configure(cfg.companion.enabled&&cfg.companion.download_notifications);
    chat::session::subscribe(owner);
    chat::configure(&cfg.chat);
    unsafe {SetTimer(Some(owner), chat::cli::TIMER, 15*60*1000, None);}
    if cfg.chat.claude_mcp || cfg.chat.codex_mcp || cfg.chat.antigravity_mcp { chat::shim::maintain(); }
    let now = Instant::now();
    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            gfx: gfx.clone(),
            cfg: cfg.companion,
            model: Model::new(now),
            motion: Motion::new(0.0),
            squishy: squishy::Layer::default(),
            comfort: comfort::Layer::default(),
            connection: connection_look::Look::default(),
            body_cache:Default::default(),
            body_cache_origin:None,
            body_clock: timing::BodyClock::default(),
            wake: convenience::Wake::default(),
            last: now,
            start: now,
            ground: None,
            trip: None,
            sprite: None,
            shadow: None,
            particles: None,
            fx: fx::Engine::default(),
            rng: fx::Rng(0x736e6f6d65),
            animations: animations_enabled(),
            reactions: cues::Reactions::default(),
            purpose: purpose::Purpose::with_budget(cfg.companion.activity_budget), purpose_due: Instant::now(), aside: purpose::Aside::with_budget(cfg.companion.activity_budget),
            rhythm: rhythm::Rhythm::default(), pending_greet: None,
            checkpoint: checkpoint::Checkpoint {at:purpose::uptime() as u64,day:cfg.companion.routine.day},
            grab: grab::Grab::default(),drag_anchor:None,peek:peek::Runtime::default(),peek_drag:None,last_draw:None,rest:None,immediate:false,press:gestures::Press::default(),bubble:bubble::Bubble::default(),pending_save:None,pending_size:None,
            home_monitors:interaction::monitors(),grounds:Vec::new(),deferred:None,
            dock_ground:None,ground_due:now,slot_shown:cfg.companion.enabled&&cfg.companion.visible,slot_tip:None,slot_notice_until:None,slot_attention:false,
            generation: gfx.generation(),
            size: (0, 0),
            timer: None,
            pacer: match pacer::Pacer::new(owner) {Ok(p)=>Some(p),Err(e)=>{crate::log!("companion: high resolution pacer unavailable; legacy timer fallback: {e}");None}},
            shown: cfg.companion.visible,
            picture: None,
            particle_picture: None,
            visible_frame: visibility::Frame::default(),
            shadow_picture: None,
            retry: retry::Retry::default(),
            battery: !crate::sysstat::read_power().on_ac,
            light: crate::syslook::current().light,
            yielding: false, fade: visibility::Fade::default(), hidden_since: None, yield_from: None,
        })
    });
    APP.with(|a|{if let Some(a)=a.borrow_mut().as_mut(){if a.cfg.home.is_some(){a.restore_home();}}});
    crate::sysstat::companion_power();
    refresh();
}
pub fn refresh() {
    let h = OWNER.load(Ordering::SeqCst);
    if h != 0
        && !PENDING.swap(true, Ordering::SeqCst)
        && unsafe { PostMessageW(Some(HWND(h as *mut _)), WM_REFRESH, WPARAM(0), LPARAM(0)) }
            .is_err()
    {
        PENDING.store(false, Ordering::SeqCst);
    }
}
pub fn toggle() {
    if !APP.with(|a| a.try_borrow().map(|a| a.as_ref().is_some_and(|a| a.cfg.enabled)).unwrap_or(true)) {
        return;
    }
    let h = OWNER.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_TOGGLE, WPARAM(0), LPARAM(0));
        }
    }
}
pub fn apply_config(cfg: &crate::config::Config) {
    if !cfg.companion.enabled || !cfg.companion.visible {
        set_capture_target(None);
    }
    downloads::configure(cfg.companion.enabled&&cfg.companion.download_notifications);
    chat::configure(&cfg.chat);
    APP.with(|a| {
        if let Some(a) = a.borrow_mut().as_mut() {
            a.retry.reset();
            a.wake.configure(a.cfg,cfg.companion);
            if !cfg.companion.enabled {
                crate::dock::cancel_companion_toggle();
                a.grab=grab::Grab::default();
                a.trip = None;
                a.shown = false;
                a.stop();
            } else if a.cfg.visible != cfg.companion.visible
                || !a.cfg.enabled && cfg.companion.visible
            {
                a.grab = grab::Grab::default();
                a.trip = (a.rest.is_none()&&cfg.companion.animations&&a.animations&&a.peek_home().is_none()).then(||Trip {at:Instant::now(),out:cfg.companion.visible});
                a.motion.set_state(State::Idle);
                a.shown = cfg.companion.visible||a.trip.is_some()||a.rest.is_some();
            }
            if !cfg.companion.situations || !cfg.companion.enabled {a.reactions.clear();a.fx.clear();}
            if cfg.companion.ignore_mouse {a.bubble.dismiss();}
            if !cfg.companion.visible {a.bubble.close(a.start.elapsed().as_secs_f64());}
            let home_changed=a.cfg.home!=cfg.companion.home;
            // Runtime history may be newer than the last batched disk snapshot.
            let (routine,budget,greet)=(a.cfg.routine,a.cfg.activity_budget,a.cfg.last_greet);
            a.cfg = cfg.companion;
            a.cfg.routine=routine;a.cfg.activity_budget=budget;a.cfg.last_greet=greet;
            if home_changed&&!a.grab.busy(){a.restore_home();}
        }
    });
    refresh();
}
pub fn visible() -> bool {
    APP.with(|a| {
        a.try_borrow()
            .ok()
            .and_then(|a| a.as_ref().map(|a| a.cfg.enabled && a.shown))
            .unwrap_or(false)
    })
}
pub fn attention() -> bool {
    APP.with(|a| a.try_borrow().ok().is_some_and(|a| a.as_ref().is_some_and(|a| a.attention(Instant::now()))))
}
impl App {
    fn attention(&self, now: Instant) -> bool {
        self.cfg.enabled && slot_attention(&self.model.signal, self.slot_notice_until,
            self.cfg.situations && self.reactions.click(now, false).is_some(), now)
    }
}
fn slot_attention(signal: &model::Signal, until: Option<Instant>, actionable: bool, now: Instant) -> bool {
    !signal.waits.is_empty()
        || signal.result.is_some_and(|(at, _)| now.saturating_duration_since(at).as_secs_f32() < 4.)
        || until.is_some_and(|until| now < until)
        || actionable
}

/// When a hidden gnome's dot needs another look (ms): the nearest transient
/// deadline (a result's 4 s, a cue notice), else a slow check while a click
/// action is offered (those lapse on their own clock); None for agent waits
/// alone (a change of them refreshes).
fn slot_wake(signal: &model::Signal, until: Option<Instant>, actionable: bool, now: Instant) -> Option<u32> {
    let result = signal.result.map(|(at, _)| at + std::time::Duration::from_secs(4));
    let next = [result, until].into_iter().flatten().filter(|t| *t > now).min();
    match next {
        Some(t) => Some((t.saturating_duration_since(now).as_millis() as u32).saturating_add(20).clamp(50, 60_000)),
        None if actionable => Some(5_000),
        None => None,
    }
}

#[cfg(test)]
#[test]
fn home_notice_clears_when_waits_or_transient_notifications_end() {
    use std::time::Duration;
    let now = Instant::now();
    let mut signal = model::Signal::default();
    assert!(!slot_attention(&signal, None, false, now));
    signal.waits.push(("agent".into(), now));
    assert!(slot_attention(&signal, None, false, now));
    signal.waits.clear();
    assert!(!slot_attention(&signal, None, false, now));
    signal.result = Some((now, false));
    assert!(slot_attention(&signal, None, false, now + Duration::from_secs(3)));
    assert!(!slot_attention(&signal, None, false, now + Duration::from_secs(4)));
    signal.result = None;
    let end = now + Duration::from_secs(4);
    assert!(slot_attention(&signal, Some(end), false, now));
    assert!(!slot_attention(&signal, Some(end), false, end));
    assert!(slot_attention(&signal, None, true, end));
}
pub fn monitor() -> Option<RECT> {
    APP.with(|a| {
        a.try_borrow()
            .ok()
            .and_then(|a| a.as_ref().and_then(|a| a.ground.map(|g| g.monitor)))
    })
}
pub fn publish_ground(g: Ground, primary: bool) {
    let changed = APP.with(|a| {
        let mut a = a.borrow_mut();
        let Some(a) = a.as_mut() else { return false };
        if let Some(old)=a.grounds.iter_mut().find(|old|old.monitor==g.monitor){*old=g;}else{a.grounds.push(g);}
        if primary {a.dock_ground=Some(g);}
        if !a.accepts_ground(g,primary)||a.ground==Some(g){return false;}
        if a.cfg.home.is_some() && a.ground.is_none_or(|old|old.monitor!=g.monitor) {a.restore_home();}
        else {a.adopt_ground(g);}
        true
    });
    if changed {
        refresh();
    }
}
pub fn environment_changed() {
    crate::sysstat::companion_power();
    APP.with(|a| {
        if let Some(a) = a.borrow_mut().as_mut() {
            a.battery = !crate::sysstat::read_power().on_ac;
            a.light = crate::syslook::current().light;
            a.bubble.dismiss();
            a.animations = animations_enabled();
            if let Some(p)=&a.pacer{p.set_awake(!crate::dock::session_idle());}
        }
    });
    refresh();
}
pub fn paced_tick(generation:u64) {
    let ticket=APP.with(|a|a.borrow().as_ref().and_then(|a|a.pacer.as_ref().map(|p|p.ticket(generation))));
    if ticket.as_ref().is_some_and(|t|t.active){handle();}
    // Ticket drops after handle, including any nested refresh or rearm.
}
pub fn foreground_changed() { crate::notification_state::changed(); window::desktop_changed(); refresh(); }
pub fn notifications_quiet() -> bool { CALM.load(Ordering::SeqCst) & 2 != 0 || crate::notification_state::quiet() }
pub fn handle() {
    let quiet = notifications_quiet();
    remind::poll();
    assistant::poll();
    routine::poll();
    downloads::poll();
    convenience::wake_due();
    convenience::drain();
    let refreshed = PENDING.swap(false, Ordering::SeqCst);
    let now = Instant::now();
    let signal = if quiet { model::Signal::default() } else { crate::agents::companion_signal(now) };
    let inputs=std::mem::take(&mut *INPUT.lock().unwrap_or_else(|e|e.into_inner()));
    let queued=cues::take_ready(&mut CUES.lock().unwrap_or_else(|e|e.into_inner()), quiet);
    let scheduled=crate::topbar::shutdown::remaining().map(|(g,_)|g);
    let slot = crate::topbar::companion_anchor();
    let mode = crate::dock::companion_mode();
    let (bounce, changed) = APP.with(|a| {
        let mut a = a.borrow_mut();
        let Some(a) = a.as_mut() else {
            return (false, false);
        };
        a.observe_purpose(now, &signal, mode);
        if CAPTURE_REACTION.swap(false,Ordering::SeqCst){a.reactions.accept(Cue::BreakStart,now,a.cfg.visible);if a.peek_home().is_some(){a.peek.notify(a.start.elapsed().as_secs_f64());}}
        if a.cfg.enabled && (a.cfg.visible||a.trip.is_some()) && (refreshed||now>=a.ground_due) {
            a.poll_ground();a.ground_due=now+std::time::Duration::from_millis(500);
        }
        let mode=if a.ground.is_some_and(|g|interaction::covered_stops(g.covered,&a.grab)){visibility::Mode::Hide}else{mode};
        a.set_yielding(mode==visibility::Mode::Yield);
        if mode==visibility::Mode::Run {for event in inputs {a.input(event);}}
        let edge_event=signal.waits.iter().any(|w|!a.model.signal.waits.contains(w))
            || signal.result.is_some()&&signal.result!=a.model.signal.result;
        if edge_event&&a.peek_home().is_some()&&(!a.grab.busy()||a.grab.phase==grab::Phase::Pressed){a.peek.notify(a.start.elapsed().as_secs_f64());}
        let new_wait=if a.grab.busy()&&a.grab.phase!=grab::Phase::Pressed{a.deferred=Some(signal);false}else{if a.deferred.take().is_some(){a.model.resume_after_grab(signal,now)}else{a.model.observe(signal,now)}};
        if new_wait && a.motion.state == State::Notify {
            a.motion.restart();
        }
        let mut bounce = new_wait && a.cfg.enabled && !a.cfg.visible;
        let calm=CALM.load(Ordering::SeqCst);
        a.reactions.calm(Calm::Focus,quiet || a.cfg.situations&&calm&1!=0);
        a.reactions.calm(Calm::Meeting,a.cfg.situations&&calm&2!=0);
        a.reactions.suppress(quiet);
        a.reactions.retain_shutdown(scheduled);
        let mut cue_notice = false;
        if a.cfg.enabled&&a.cfg.situations {for (cue,_at) in queued {cue_notice|=a.reactions.accept(cue,now,a.cfg.visible);a.purpose.notice(a.ground.is_some_and(|g|a.motion.x>(g.monitor.left+g.monitor.right) as f32/(2.*g.dpi)));}}

        bounce |= cue_notice;
        if cue_notice { a.slot_notice_until = Some(now + std::time::Duration::from_secs(4)); }
        let before = (a.slot_shown, a.slot_attention);
        if let Err(e) = unsafe { a.draw(now, slot, mode) } {
            crate::log!("companion: render failed: {e}");
            a.stop();
            a.particles = None;
            a.body_cache = Default::default();
            a.sprite = None;
            a.shadow = None;
            let delay = a.retry.failed(now);
            if delay.is_none() {
                crate::log!(
                    "companion: render retry exhausted; waiting for config/device recovery"
                );
            }
            a.arm(delay);
        }
        {
            a.slot_shown = a.cfg.enabled && a.shown;
            a.slot_attention = a.attention(now);
            // A hidden gnome has no animation timer: wake once when a transient
            // dot is due to go (not every 250 ms; agent waits refresh on change).
            if a.cfg.enabled && !a.shown && a.slot_attention {
                let actionable = a.cfg.situations && a.reactions.click(now, false).is_some();
                if let Some(ms) = slot_wake(&a.model.signal, a.slot_notice_until, actionable, now) { a.arm(Some(ms)); }
            }
            (bounce, before != (a.slot_shown, a.slot_attention))
        }
    });
    flush_state(false);
    let save=APP.with(|a|a.borrow_mut().as_mut().and_then(|a|a.pending_save.take()));
    if let Some((home,visible))=save {if !crate::dock::save_companion_position(home,visible){APP.with(|a|{if let Some(a)=a.borrow_mut().as_mut(){a.pending_save=Some((home,visible));a.arm(Some(50));}});}}

    let greet=APP.with(|a|a.borrow_mut().as_mut().and_then(|a|a.pending_greet.take()));
    if let Some(date)=greet {if !crate::dock::save_companion_greet(date) {APP.with(|a|{if let Some(a)=a.borrow_mut().as_mut(){a.pending_greet=Some(date);}});}}
    let size=APP.with(|a|a.borrow_mut().as_mut().and_then(|a|a.pending_size.take()));
    if let Some(size)=size {if !crate::dock::set_companion_size(size){APP.with(|a|{if let Some(a)=a.borrow_mut().as_mut(){a.pending_size=Some(size);}});}}
    if bounce || changed {
        crate::topbar::companion_changed(bounce);
    }
    let tip=tooltip();
    let tip_changed=APP.with(|a|{let mut a=a.borrow_mut();let Some(a)=a.as_mut()else{return false;};
        let changed=a.slot_tip!=tip;a.slot_tip=tip.clone();changed});
    if tip_changed {crate::topbar::companion_tooltip_changed();}
}
impl App {
    fn set_yielding(&mut self, on: bool) {
        if on && !self.yielding { self.yield_motion(); }
        if !on && self.yielding { self.model.settled(Instant::now()); }
        self.yielding=on;
    }
    fn yield_motion(&mut self) {
        let mut x=self.motion.x;
        if let Some(d)=&self.last_draw {
            if self.trip.is_none() {x=d.x;}
            self.yield_from=Some((Instant::now(),d.pose.clone()));
        }
        self.motion.yield_at(x);
        self.press=gestures::Press::default();self.grab.abort();self.drag_anchor=None;
        self.bubble.dismiss();
        self.yielding=true;
    }
    fn arm(&mut self, interval: Option<u32>) {
        if self.pacer.as_ref().is_some_and(|p|!p.healthy()) {self.pacer=None;self.timer=None;}
        if let Some(pacer)=&self.pacer {
            let vsync=interval==Some(17)&&(self.grab.fast()||self.trip.is_some())
                &&timing::motion(self.cfg.animations,self.animations,crate::effects::motion())&&!self.battery;
            pacer.arm(interval,vsync);self.timer=interval;return;
        }
        // Only used if the high-resolution kernel timer could not be created.
        if self.timer==interval{return;}
        let h=HWND(OWNER.load(Ordering::SeqCst) as *mut _);
        unsafe {let _=KillTimer(Some(h),TIMER);if let Some(ms)=interval{SetTimer(Some(h),TIMER,ms,None);}}
        self.timer=interval;
    }
    fn stop(&mut self) {
        set_capture_target(None);
        card::close();
        self.hidden_since=None;
        self.fade=visibility::Fade::default();
        self.visible_frame=visibility::Frame::default();
        visibility::Frame::hide([self.sprite.as_ref(),self.shadow.as_ref(),self.particles.as_ref()].map(|s|s.map(|s|&s.window)));
        if self.grab.busy() {self.model.settled(Instant::now());}
        self.grab.abort();
        self.aside.clear();
        self.drag_anchor=None;
        self.press=gestures::Press::default();
        self.rest=None;
        self.comfort=comfort::Layer::default();
        self.body_clock=timing::BodyClock::default();
        self.squishy=squishy::Layer::default();
        self.fx.clear();
        self.bubble.dismiss();
        self.wake=convenience::Wake::default();
        self.last_draw = None;
        self.picture = None;
        self.particle_picture = None;
        self.shadow_picture = None;
        self.arm(convenience::hidden_timer(self.cfg,convenience::now()));
        if let Some(s) = &mut self.sprite {
            s.window.cancel_input();
        }
    }
    unsafe fn draw(
        &mut self,
        now: Instant,
        slot: Option<SlotAnchor>,
        mode: visibility::Mode,
    ) -> windows::core::Result<()> {
        let mut dt = now.saturating_duration_since(self.last).as_secs_f32();

        if chat::busy() && card::restore_allowance(card::interrupt_for_chat(),&mut self.cfg) {
            self.pending_greet=Some(None);
        }

        // Drop device resources even while hidden, before any early return.
        if self.generation != self.gfx.generation() {
            self.bubble.dismiss();
            self.particles = None;
            self.body_cache = Default::default();
            self.sprite = None;
            self.shadow = None;
            self.picture = None;
            self.particle_picture = None;
            self.shadow_picture = None;
            self.generation = self.gfx.generation();
            self.retry.reset();
        }
        let Some(g) = self.ground else {
            self.stop();
            return Ok(());
        };
        let hidden = interaction::covered_stops(g.covered,&self.grab) || mode==visibility::Mode::Hide;
        let animated=timing::motion(self.cfg.animations,self.animations,crate::effects::motion());
        if !self.cfg.enabled || !self.cfg.visible && self.trip.is_none() && self.rest.is_none() {
            self.fade=visibility::Fade::default();self.hidden_since=None;self.stop();return Ok(());
        }
        self.set_yielding(mode==visibility::Mode::Yield);
        // Freeze the existing raster, location, and animation clocks for a cover.
        // No stop(): that would destroy the pose and life/gesture state.
        let alpha=self.fade.step(hidden,dt,animated);
        if hidden || self.hidden_since.is_some() {
            set_capture_target(None);
            let at=*self.hidden_since.get_or_insert(now);
            self.last=now;
            self.visible_frame.fade([self.sprite.as_ref(),self.shadow.as_ref(),self.particles.as_ref()].map(|s|s.map(|s|&s.window)),alpha)?;
            self.bubble.dismiss();
            if !hidden && !self.fade.moving() {
                let elapsed=now.saturating_duration_since(at);
                self.start+=elapsed;
                if let Some(t)=&mut self.trip {t.at+=elapsed;}
                if let Some((t,_))=&mut self.rest {*t+=elapsed;}
                self.hidden_since=None;
                self.arm(Some(visibility::rest_interval(animated,self.battery)));
            } else {
                let interval=if self.fade.moving() {Some(visibility::rest_interval(animated,self.battery))}
                    else if g.covered && !crate::dock::session_idle() {Some(500)}
                    else {convenience::hidden_timer(self.cfg,convenience::now())};
                self.arm(interval);
            }
            return Ok(());
        }
        if self.yielding {dt=dt.min(0.034);}
        if self.yielding {
            let elapsed=now.saturating_duration_since(self.last);
            if let Some(t)=&mut self.trip {t.at+=elapsed;}
            if let Some((t,_))=&mut self.rest {*t+=elapsed;}
        }
        let suppressed=notifications_quiet();
        let hour=windows::Win32::System::SystemInformation::GetLocalTime().wHour;
        let night=self.cfg.situations&&hour<6;
        let edge_home=self.peek_home();
        let pinned=edge_home.is_some()&&self.trip.is_none();
        // Quiet assistant: legacy settings cannot restart patrol or sleep chores.
        let base_cfg=self.cfg.quiet_base();
        let state = if self.yielding || suppressed { State::Idle } else { self.model.update_with_sleep(now, base_cfg, cues::sleep_after(hour,self.cfg.situations),
            ) };
        if !self.retry.ready(now) {
            self.arm(self.retry.remaining(now));
            return Ok(());
        }
        if self.grab.phase==grab::Phase::Held {
            // The move path (including queued reentrant moves) presents first.
            self.move_drag_window()?;
            if !timing::drag_due(now.saturating_duration_since(self.last).as_secs_f64(),self.battery) {
                self.arm(Some(if self.battery{34}else{17}));return Ok(());
            }
        }
        let immediate=self.immediate;
        self.immediate=false;
        self.last=now;
        let grab_events=self.grab.step(dt);
        if interaction::covered_stops(g.covered,&self.grab) {
            self.stop();self.arm(Some(500));return Ok(());
        }
        if grab_events.finished {self.model.settled(now);self.motion.x=self.grab.point.x/g.dpi;}
        let reaction=if pinned {cues::Reaction{state,effects:&[],quiet:true,active:false,started:false}}else if self.yielding||self.trip.is_some()||(self.grab.busy()&&self.grab.phase!=grab::Phase::Pressed){cues::Reaction{state:State::Idle,effects:&[],quiet:self.reactions.quiet(),active:false,started:false}}else{self.reactions.tick(now,dt,state,!self.model.signal.waits.is_empty())};
        let mut state=reaction.state;
        if reaction.started {self.motion.restart();}
        self.motion.set_state(if self.trip.is_some() {
            State::Idle
        } else {
            state
        });
        let s = self.cfg.size.scale();
        let margin = self.cfg.size.margin();
        let left = g.monitor.left as f32 / g.dpi + margin;
        let right = (g.monitor.right as f32 / g.dpi - margin).max(left);
        let living=if pinned||self.yielding {purpose::Output::default()}else{self.living(state,reaction.active)};
        let mut living=living;
        if !pinned && !self.yielding && !self.grab.busy() && !reaction.active && self.trip.is_none() {
            if let Some((next,flip))=self.aside.step(self.motion.x,dt) {
                living.clip=Some(("walk",purpose::uptime() as f32));living.frame=Some(clips::catalog().clip("walk").frame(purpose::uptime() as f32).into());living.flip=flip;living.x=Some(next);
            }
        }
        if living.frame.is_some() {state=State::Idle;self.motion.set_state(State::Idle);}
        self.motion.x = self.motion.x.clamp(left, right);
        let mut p = self.motion.step(if pinned || self.grab.busy() || self.yielding && !animated {0.}else{dt}, left, right);
        if let Some((at,old))=&self.yield_from {
            let t=now.saturating_duration_since(*at).as_secs_f32()/0.12;
            if t<1. && animated {p.blend=Some((old.frame.clone(),1.-t));}else{self.yield_from=None;}
        }
        if let Some(frame)=&living.frame {p.frame=frame.clone();p.flip=living.flip;p.blend=living.blend.clone();}
        if let Some((clip,time))=living.clip {p.clip=clip.into();p.clip_time=time;}
        if let Some(x)=living.x {self.motion.x=x;p.x=x;}
        if convenience::menu_open() {p.frame=convenience::menu_frame().into();p.blend=None;p.flip=false;}
        else if self.bubble.open() {p.frame="look_4".into();p.blend=None;}
        else if self.bubble.visible() {p.frame="look_0".into();p.blend=None;}
        if !self.yielding {if let Some((at,_end))=self.rest {let clip=clips::catalog().clip("yawn");let t=now.saturating_duration_since(at).as_secs_f32();p.frame=clip.frame(t).into();p.blend=None;if t>=clip.duration(){self.rest=None;self.trip=Some(Trip{at:now,out:false});}}}
        let mut x = p.x;
        let mut y = g.floor / g.dpi;
        let mut draw_scale = s;
        let mut travelling = false;
        if let Some(trip) = &self.trip.as_ref().filter(|_|!self.yielding) {
            let elapsed = now.saturating_duration_since(trip.at).as_secs_f32();
            let journey = motion::journey(trip.out, elapsed);
            p.frame = journey.frame;
            p.clip = journey.clip.into();
            p.clip_time = journey.clip_time;
            p.flip = false;
            if let Some(progress) = journey.progress {
                travelling = true;
                let anchor = slot.unwrap_or_else(|| SlotAnchor::fallback(g));
                // Interpolate in source-monitor DIPs; the destination padding
                // and icon scale use the destination monitor's own DPI.
                let dest = anchor.destination(g.dpi);
                let sf = &clips::catalog().frames[clips::catalog().clip("slot").frame(0.0)];
                (x, y, draw_scale) = (if animated {motion::spring_travel}else{motion::travel})(
                    progress,
                    trip.out,
                    (x, y),
                    dest,
                    s,
                    20.0 * anchor.dpi / g.dpi / sf.h as f32,
                );
            }
            if journey.finished {
                let out = trip.out;
                self.trip = None;
                self.shown = out;
                if out {
                    self.model.landed(now);
                } else {
                    self.stop();
                    return Ok(());
                }
            }
        }
        if self.grab.phase==grab::Phase::Pressed && living.frame.is_none() && !reaction.active && matches!(state,State::Idle|State::Sit|State::Walk|State::Sleep) {
            if let Some(d)=&self.last_draw{p=d.pose.clone();p.sx=1.;p.sy=1.;p.skew=0.;p.hat_tilt=0.;p.jy=0.;x=d.x;y=d.y;draw_scale=d.scale;}
        }else if self.grab.busy() && self.grab.phase!=grab::Phase::Pressed {p=self.grab.pose();x=self.grab.point.x/g.dpi;y=self.grab.point.y/g.dpi;}
        if self.yielding && self.trip.is_some() {if let Some(d)=&self.last_draw {x=d.x;y=d.y;draw_scale=d.scale;}}
        let seconds_f64=now.saturating_duration_since(self.start).as_secs_f64();
        let motion_enabled=timing::motion(self.cfg.animations,self.animations,crate::effects::motion());
        let wake_ready=!self.yielding && self.trip.is_none() && !self.grab.busy() && !reaction.active
            && matches!(state,State::Idle|State::Sit|State::Walk|State::Sleep|State::Peek);
        if !self.bubble.visible() {if let Some(frame)=self.wake.frame(seconds_f64,motion_enabled,wake_ready) {p.frame=frame;p.blend=None;}}
        let bob=if !self.grab.busy() && self.trip.is_none() {
            self.comfort.apply(&mut p,state,seconds_f64,s,g.dpi,motion_enabled&&!self.bubble.open())
        }else{0.};
        let walking=!self.grab.busy() && self.trip.is_none()
            && (p.frame.starts_with("walk_")||p.frame.starts_with("carry_"));
        let bob=squishy::ground_bob(bob,motion_enabled);
        y+=bob;
        self.press.apply(&mut p,now.saturating_duration_since(self.start).as_secs_f64(),self.cfg.animations&&self.animations,
        );
        if let Some(frame) = chat::frame().or_else(chat::mcp::frame) {
            p.frame = frame;
            p.blend = None;
            p.flip = false;
        }
        if !motion_enabled&&!self.bubble.open()&&!p.frame.starts_with("look_") {
            if let Some((name,_))=p.frame.rsplit_once('_') {if let Some(clip)=clips::catalog().clips.get(name){p.frame=clip.frames[0].clone();p.blend=None;}}
        }
        let awake_frame=self.connection.update(chat::awake(),seconds_f64,motion_enabled&&!pinned,wake_ready&&!self.bubble.visible());
        if let Some(frame)=awake_frame{p.frame=frame;p.blend=None;}
        if let Some((edge,r,along))=edge_home.filter(|_|pinned) {
            let unit=fx::Random::unit(&mut self.rng);
            let frame=self.peek.edge_frame(edge,seconds_f64,motion_enabled,unit);
            let notified=self.peek.active_notification(seconds_f64);
            let reaction_time=if motion_enabled {(seconds_f64-self.peek.notification.unwrap_or(seconds_f64)) as f32}else{0.};
            let frame=if notified && edge==peek::Edge::Bottom {match state {State::Notify=>clips::catalog().clip("notify").frame(reaction_time),State::Done=>clips::catalog().clip("done").frame(reaction_time),State::Error=>clips::catalog().clip("error").frame(reaction_time),_=>frame}}else{frame};
            (p,x,y)=self.peek.animated_pose(edge,r,along,frame,s,g.dpi,seconds_f64,motion_enabled);
            if !notified {state=State::Idle;}
            self.fx.clear();
        } else if self.grab.phase==grab::Phase::Held && motion_enabled {
            let screens:Vec<_>=self.home_monitors.iter().map(|(k,m)|(*k,m.rect)).collect();
            if let Some(h)=peek::candidate(&screens,peek::bounds(&p,x,y,s,g.dpi),self.grab.point) {
                p.tilt=(p.tilt+match h.edge {Some(peek::Edge::Left)=>-8.,Some(peek::Edge::Right)=>8.,_=>5.}).clamp(-grab::MAX_HELD_TILT,grab::MAX_HELD_TILT);
            }
        }
        if !pinned {self.peek.apply_detach(&mut p,seconds_f64,motion_enabled);}
        if !pinned {
            // Final pose selection includes chat, wake-up, direct input and
            // journeys. Reused frame families retain their semantic clip.
            let frame_clip=p.frame.rsplit_once('_').map_or(p.frame.as_str(),|(c,_)|c);
            let matching=clips::catalog().clips.get(&p.clip).is_some_and(|c|c.frames.contains(&p.frame))
                || matches!(p.clip.as_str(),"grab"|"fall");
            let clip=if matching {p.clip.clone()}else{frame_clip.into()};
            let time=if matching {p.clip_time}else{f32::NAN};
            let direction=if p.flip || clip=="turn_left" {-1.}else{1.};
            x=self.squishy.apply(&mut p,(x,y,draw_scale),dt,squishy::Sample{clip:&clip,time,direction},motion_enabled);
        } else {self.squishy=squishy::Layer::default();}
        // A compact window follows the gnome, including during the jump;
        // never allocate a monitor-sized surface for a tiny character.
        let reference=&clips::catalog().frames["idle_0"];
        let frames=&clips::catalog().frames;
        let width=frames.values().map(|f|f.w).max().unwrap_or(250) as f32+reference.hatw*1.4;
        let height=frames.values().map(|f|f.h).max().unwrap_or(361) as f32+reference.ground*0.45;
        let side = if self.grab.fast(){(2.*self.grab.radius()/g.dpi+96.*self.cfg.size.ratio()).ceil()}else{(width.max(height) * s + 48.0*self.cfg.size.ratio()).ceil()};
        let pixels = (side * g.dpi).ceil() as u32;
        if self.size != (pixels, pixels) || self.generation != self.gfx.generation() {
            if let Some(sprite)=&mut self.sprite {sprite.resize(pixels,pixels)?;}
            if let Some(fx)=&mut self.particles {fx.resize(pixels,pixels)?;}
            if let Some(shadow)=&mut self.shadow {shadow.resize(pixels,(20.*g.dpi).ceil() as u32)?;}
            self.picture = None;
            self.particle_picture = None;
            self.shadow_picture = None;
            self.size = (pixels, pixels);
            self.generation = self.gfx.generation();
        }
        if self.sprite.is_none() {
            self.sprite = Some(window::Surface::new(&self.gfx, pixels, pixels, false)?);
            let surface=self.sprite.as_mut().unwrap();
            surface.raster.painter.warm_inputs(&surface.raster.dc,s,g.dpi)?;
        }
        self.sprite.as_ref().unwrap().window.ignore(self.cfg.ignore_mouse);
        let cx = side / 2.0;
        let cy = if pinned||self.grab.fast(){side/2.}else{side-24.};
        let cache_origin=(pixels,cx.to_bits(),cy.to_bits());
        if self.body_cache_origin!=Some(cache_origin) {
            self.body_cache=Default::default();self.body_cache_origin=Some(cache_origin);self.picture=None;
        }
        let origin = ((x - cx) * g.dpi, (y - cy) * g.dpi);
        let seconds = now.duration_since(self.start).as_secs_f32();
        let boil = self.cfg.boil && p.frame != "sidepeek_0";
        let picture = render::Stamp::new(&p, draw_scale, g.dpi, boil, self.light);
        self.fx.motion(motion_enabled);
        self.fx.pose(&p,x,y,draw_scale);
        self.fx.breath_phase(seconds);
        if self.yielding {self.fx.yield_tick(dt,animated);}else{self.fx.tick(dt,if self.rest.is_some()||self.bubble.open()||self.grab.busy()||living.frame.is_some(){State::Idle}else{state},&p.frame,x,y,&mut self.rng,self.cfg.animations&&!reaction.quiet,night);}
        if !self.yielding&&self.cfg.animations&&!reaction.quiet {for effect in reaction.effects {self.fx.burst(effect,&mut self.rng,x,y,night);}}
        if grab_events.landed && !self.yielding && self.cfg.animations && !reaction.quiet {
            self.fx.burst("dust",&mut self.rng,x,y,false);
        }
        let mut interval=grab::interval(fx::interval(model::interval(
            state,
            self.cfg.visible||self.rest.is_some(),
            suppressed && !reaction.active,
            self.trip.is_some(),
            self.battery,
        ),&self.fx.parts,self.battery),self.press.active(now.saturating_duration_since(self.start).as_secs_f64())||self.grab.fast()||(self.grab.phase==grab::Phase::Pressed),self.battery);
        if state==State::Idle && !motion_enabled && !reaction.active && !self.cfg.boil
            && !self.grab.busy() && self.trip.is_none() && self.fx.parts.is_empty()
            && !self.press.active(seconds_f64) && !self.wake.active() {
            interval=Some(250);
        }
        if walking && motion_enabled && !self.battery {interval=Some(17);}
        let body_due=self.body_clock.due(seconds_f64,walking,self.battery,immediate||self.picture.is_none());
        let sprite = self.sprite.as_mut().unwrap();
        let dirty = self.picture.as_ref().is_none_or(|old| !old.same_bitmap(&picture)) && body_due || pinned;
        if dirty && !self.body_cache.restore(&picture,sprite.window.pixels_mut()) {
            sprite.raster.draw(
                &p,
                cx,
                cy,
                draw_scale,
                g.dpi,
                boil,
                seconds,
                false,
                self.light,
                sprite.window.pixels_mut(),
            )?;
            self.body_cache.store(&picture,sprite.window.pixels_mut());
        }
        if dirty {
            if draw_scale<=0.4 && !pinned {sprite.window.pad_input(g.dpi);}
        }
        if let Some((_, monitor, _)) = edge_home.filter(|_| pinned) {
            // Restore the uncut cache above on every pinned tick, then clip for
            // this position. Notification movement can reveal previously hidden pixels.
            peek::clip_pixels(sprite.window.pixels_mut(), pixels,
                (origin.0.round() as i32, origin.1.round() as i32), monitor);
        }
        let screens:Vec<_>=self.home_monitors.iter().map(|(_,m)|m.rect).collect();
        self.visible_frame=visibility::Frame::from_pixels(sprite.window.pixels_mut(),pixels,
            (origin.0.round() as i32,origin.1.round() as i32),&screens,
            pinned||self.rest.is_some()||self.trip.is_some());
        let mut shadow_update=None;
        if !pinned&&!travelling&&!matches!(self.grab.phase,grab::Phase::Held|grab::Phase::Fall) {
            // The shadow window is only 20 DIP tall and wholly click-through.
            let sh = (20.0 * g.dpi).ceil() as u32;
            if self.shadow.is_none() {
                self.shadow = Some(window::Surface::new(&self.gfx, pixels, sh, true)?);
            }
            let shadow_key = (
                origin.0.round() as i32,
                ((y - bob - 10.0) * g.dpi).round() as i32,
                pixels,
                self.light,
            );
            if self.shadow_picture != Some(shadow_key) {
                let shadow = self.shadow.as_mut().unwrap();
                let dirty = self
                    .shadow_picture
                    .is_none_or(|old| old.2 != pixels || old.3 != self.light);
                if dirty {
                    shadow.raster.draw(
                        &p,
                        cx,
                        10.0,
                        s,
                        g.dpi,
                        false,
                        0.0,
                        true,
                        self.light,
                        shadow.window.pixels_mut(),
                    )?;
                }
                shadow_update=Some((shadow_key.0,shadow_key.1,dirty));
                self.shadow_picture = Some(shadow_key);
            } else {shadow_update=Some((shadow_key.0,shadow_key.1,false));}
        } else {
            self.shadow_picture = None;
        }
        let mut particle_update=None;
        if self.visible_frame.effects && !self.fx.parts.is_empty() {
            if self.particles.is_none() {self.particles=Some(window::Surface::new(&self.gfx,pixels,pixels,true)?);}
            let fx=self.particles.as_mut().unwrap();
            let stamp=render::ParticleStamp::new(&self.fx.parts,x,y,draw_scale,g.dpi,(x-cx,y-cy),self.light);
            let dirty=self.particle_picture.as_ref()!=Some(&stamp);
            if dirty {fx.raster.draw_particles(&self.fx.parts,x,y,draw_scale,g.dpi,(x-cx,y-cy),self.light,fx.window.pixels_mut())?;}
            particle_update=Some((origin.0.round() as i32,origin.1.round() as i32,dirty));
            if dirty {self.particle_picture=Some(stamp);}
        } else {self.particle_picture=None;}
        let front = self.grab.busy() || chat::is_open() || pinned
            || reaction.active && !reaction.quiet
            || (!suppressed && (matches!(state, State::Notify | State::Done | State::Error) || chat::busy()));
        for w in [Some(&sprite.window), self.shadow.as_ref().map(|s| &s.window), self.particles.as_ref().map(|s| &s.window)].into_iter().flatten() { w.front(front); }
        self.visible_frame.present([Some(&sprite.window),self.shadow.as_ref().map(|s|&s.window),self.particles.as_ref().map(|s|&s.window)],
            [Some((origin.0.round() as i32,origin.1.round() as i32,dirty)),shadow_update,particle_update],self.fade.opacity)?;
        if self.bubble.visible() {
            let body_tip=anchors::Anchors::body_tip(&p);
            let tip=fan::Point{x:x+body_tip.0*draw_scale,y:y+body_tip.1*draw_scale};
            let f=&clips::catalog().frames[&p.frame];
            let body=RECT{left:((x-f.ax*draw_scale)*g.dpi) as i32,top:((y-f.ground*draw_scale)*g.dpi) as i32,right:((x+(f.w as f32-f.ax)*draw_scale)*g.dpi) as i32,bottom:(y*g.dpi) as i32};
            let work=self.home_monitors.iter().find(|(_,m)|m.rect==g.monitor).and_then(|(_,m)|crate::monitors::info(m.handle)).map_or(g.monitor,|m|m.work);
            let mut forbidden=crate::dock::companion_obstacles().unwrap_or_else(||vec![RECT{top:g.floor as i32,..g.monitor}]);
            forbidden.extend(crate::topbar::companion_obstacles().unwrap_or_else(||vec![RECT{bottom:g.monitor.top+(48.*g.dpi) as i32,..g.monitor}]));
            let theme=crate::dock::companion_theme().unwrap_or_else(crate::theme::Theme::graphite);
            self.bubble.update(&self.gfx,&theme,&p,x,y,draw_scale,tip,body,work,&forbidden,now.saturating_duration_since(self.start).as_secs_f64(),g.dpi,motion_enabled,pinned)?;
            let fan_interval=self.bubble.interval(seconds_f64,motion_enabled);
            interval=Some(if self.battery&&fan_interval==17{34}else{fan_interval});
        }
        if chat::is_open() || chat::mcp::frame().is_some() || self.connection.animating() {
            interval = Some(interval.map_or(50, |old|old.min(50)));
        }
        let work = self
            .home_monitors
            .iter()
            .find(|(_, m)| m.rect == g.monitor)
            .and_then(|(_, m)| crate::monitors::info(m.handle))
            .map_or(g.monitor, |m| m.work);
        let body_tip = anchors::Anchors::body_tip(&p);
        let body_anchor = (
            ((x + body_tip.0 * draw_scale) * g.dpi) as i32,
            ((y + body_tip.1 * draw_scale) * g.dpi) as i32,
        );
        set_capture_target((self.cfg.visible && self.trip.is_none()).then_some(body_anchor));
        chat::anchor(
            work,
            g.dpi,
            body_anchor,
            anchors::Anchors::spinner_foot(&p,x,y,draw_scale),
            draw_scale,
            motion_enabled,
            edge_home.map(|(edge,_,_)|edge),
        );
        if !chat::busy()
            && card::anchor(work, g.dpi, (x+body_tip.0*draw_scale,y+body_tip.1*draw_scale), self.light, motion_enabled) == Some(card::Kind::Morning)
        {
            let date = living::local_calendar().date;
            self.cfg.last_greet = Some(date);
            self.pending_greet = Some(Some(date));
        }
        chat::above_sprite(sprite.window.hwnd);
        if pinned {interval=Some(self.peek.interval(seconds_f64,motion_enabled,self.battery));}
        if self.yielding {interval=Some(visibility::rest_interval(animated,self.battery));}
        if !pinned && !self.yielding {interval=squishy::interval(interval,motion_enabled&&self.squishy.moving(),self.battery);}
        if let Some(card_interval) = card::interval() {
            interval = Some(interval.map_or(card_interval, |old| old.min(card_interval)));
        }
        self.arm(interval);
        self.last_draw=Some(interaction::Drawing{pose:p,x,y,scale:draw_scale,dpi:g.dpi});
        if dirty {self.picture = Some(picture);}
        if self.grab.busy(){self.drag_anchor=Some(self.grab.point);}
        self.retry.reset();
        Ok(())
    }
}
/// Saves numeric usage/caps without replaying UI configuration changes.
pub(crate) fn flush_state(force: bool) {
    let now=purpose::uptime() as u64;
    let state=APP.with(|a| {
        let a=a.borrow();let a=a.as_ref()?;
        let b=a.purpose.budget(&a.aside);
        a.checkpoint.due(now,a.cfg.routine.day,a.cfg.routine.dirty||b!=a.cfg.activity_budget,force)
            .then_some((a.cfg.routine,b))
    });
    if let Some((mut r,b))=state {
        r.dirty=false;
        if crate::dock::save_companion_state(r,b) {
            APP.with(|a|{if let Some(a)=a.borrow_mut().as_mut(){
                a.cfg.routine.dirty=false;a.cfg.activity_budget=b;
                a.checkpoint=checkpoint::Checkpoint{at:now,day:r.day};
            }});
        }
    }
}
pub fn shutdown() {
    flush_state(true);
    crate::worker::wait_for_saves(std::time::Duration::from_secs(2));
    downloads::configure(false);
    card::close();
    chat::shutdown();
    unsafe {
        let _ = KillTimer(
            Some(HWND(OWNER.load(Ordering::SeqCst) as *mut _)),
            chat::mcp::TIMER,
        );
        let _ = KillTimer(Some(HWND(OWNER.load(Ordering::SeqCst) as *mut _)), chat::cli::TIMER);
    }
    APP.with(|a| {
        if let Some(mut a) = a.borrow_mut().take() {
            a.stop();
            a.arm(None);
        }
    });
    OWNER.store(0, Ordering::SeqCst);
    PENDING.store(false, Ordering::SeqCst);
    release_hotkey();
}

#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn review_pointer_moves_coalesce_without_waking_the_ui() {
        use grab::{Input,Kind,Point};
        let mut q=std::collections::VecDeque::new();
        assert!(queue_input(&mut q,Input::Down{id:3,kind:Kind::Mouse,point:Point::default()}));
        for x in 0..1000 {assert!(!queue_input(&mut q,Input::Move{id:3,point:Point::new(x as f32,0.)}));}
        assert_eq!(q.len(),2);
        assert!(matches!(q.back(),Some(Input::Move{point,..}) if point.x==999.));
        assert!(queue_input(&mut q,Input::Up{id:3,point:Point::new(999.,0.)}));
        assert!(matches!(q.front(),Some(Input::Down{id:3,..})));
        assert!(matches!(q.back(),Some(Input::Up{id:3,..})));
        // Multi-pointer storms remain bounded and leave space for the release edge.
        for id in 0..1000 {queue_input(&mut q,Input::Move{id,point:Point::default()});}
        assert_eq!(q.len(),128);
        assert!(queue_input(&mut q,Input::Cancel{id:3}));
        assert_eq!(q.len(),128);
        assert!(matches!(q.back(),Some(Input::Cancel{id:3})));
    }
    #[test]
    fn dpi_change_preserves_physical_position_and_slot_padding() {
        let a = Ground {
            monitor: RECT {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1080,
            },
            floor: 1000.0,
            dpi: 1.0,
            covered: false,
        };
        let b = Ground { dpi: 1.5, ..a };
        assert_eq!(rebase_x(-900.0, Some(a), b), -600.0);
        let slot = SlotAnchor {
            rect: RECT {
                left: 2100,
                top: 0,
                right: 2130,
                bottom: 48,
            },
            dpi: 1.5,
        };
        assert_eq!(slot.destination(1.0), (2115.0, 42.0));
        assert_eq!(slot.destination(1.5), (1410.0, 28.0));
        let hidden = SlotAnchor::fallback(b);
        assert_eq!(hidden.destination(b.dpi), (-50.0, 24.0));
    }
}

#[cfg(test)]
mod quality;

fn animations_enabled()->bool {unsafe {let mut on=windows::core::BOOL(1);let _=SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION,0,Some(&mut on as *mut _ as _),SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));on.as_bool()}}


/// Feeds can call from any thread; rendering and decisions stay on the UI thread.
pub fn cue(cue:Cue){if OWNER.load(Ordering::SeqCst)==0{return;}let mut q=CUES.lock().unwrap_or_else(|e|e.into_inner());if q.len()<64{q.push((cue,Instant::now()));}drop(q);refresh();}
pub fn calm(reason:Calm,on:bool){let bit=match reason{Calm::Focus=>1,Calm::Meeting=>2};let old=if on{CALM.fetch_or(bit,Ordering::SeqCst)}else{CALM.fetch_and(!bit,Ordering::SeqCst)};if (old&bit!=0)!=on{refresh();}}
/// Runs `action` (a cell click or the menu's priority row, as it was shown).
pub fn slot_click_with(action:cues::Click){
    if !APP.with(|a|a.borrow().as_ref().is_some_and(|a|a.cfg.enabled&&a.cfg.situations)){return;}
    let action=Some(action);
    match &action {
        Some(cues::Click::CancelShutdown(g))=>{if crate::topbar::shutdown::cancel_generation(*g){cue(Cue::ShutdownCancelled);}}
        Some(cues::Click::Agenda(at))=>crate::topbar::companion_agenda(*at),
        None=>return,
    }
    APP.with(|a|{if let Some(a)=a.borrow_mut().as_mut(){a.reactions.clicked(action.as_ref().unwrap());}});refresh();
}
impl App {
 fn tap(&mut self) {
  if card::dismiss(){return;}
  chat::recheck_on_click();
  if !self.cfg.enabled||!self.cfg.visible||self.trip.is_some(){return;}
  let now=Instant::now();let seconds=now.saturating_duration_since(self.start).as_secs_f64();
  if self.peek_home().is_some() {
   self.bubble.close(seconds);
   if chat::connected(){convenience::request(convenience::Action::Chat);}else{
    // 빼꼼 중 클릭: 모자 들기만 다시 틀면 이미 들고 있을 때 반응이 없어 보인다(오너 0.1.87). 알림처럼 쏙 올라온다.
    self.peek.clicked=Some(seconds);self.peek.notify(seconds);self.peek.next_look=seconds+9.;
   }
   return;
  }
  // Input::Down already enforces ignore-mouse with the Ctrl exception.
  let priority = self.cfg.situations.then(|| self.reactions.click(now, !self.model.signal.waits.is_empty())).flatten();
  self.bubble.close(seconds);
  match gestures::click(priority, chat::connected()) {
   gestures::Click::Priority(action) => {
    self.reactions.clicked(&action);
    convenience::request(match action { cues::Click::CancelShutdown(g) => convenience::Action::CancelShutdown(g), cues::Click::Agenda(at) => convenience::Action::Agenda(at) });
   }
   gestures::Click::Chat => {self.fx.clear();convenience::request(convenience::Action::Chat);}
   gestures::Click::Confirm => {}
  }
 }
}
pub fn tooltip()->Option<String>{APP.with(|a|{let a=a.try_borrow().ok()?;let a=a.as_ref()?;if !a.cfg.enabled{return None;}if let Some(n)=convenience::remaining(a.cfg.snooze_until,convenience::now()){return Some(crate::i18n::t("companion.return_in").replace("{n}",&n.to_string()));}if !a.cfg.situations{return None;}match a.reactions.click(Instant::now(),!a.model.signal.waits.is_empty()){Some(cues::Click::CancelShutdown(_))=> {
                Some(crate::i18n::t("companion.cancel_shutdown").into())
            }
            _=>None,
        }})}
/// A plain left click on the bar's NopBar cell while a shutdown warning is up:
/// cancel it, as the cell's tooltip says, instead of opening the stats panel.
pub fn take_bar_cancel()->bool{
    let pending=APP.with(|a|{let a=a.try_borrow().ok()?;let a=a.as_ref()?;if !a.cfg.enabled||!a.cfg.situations{return None;}match a.reactions.click(Instant::now(),!a.model.signal.waits.is_empty()){Some(cues::Click::CancelShutdown(g))=>Some(g),_=>None}});
    if let Some(g)=pending{convenience::request(convenience::Action::CancelShutdown(g));true}else{false}
}
pub fn show_tooltip(h:HWND){if let Some(text)=tooltip(){APP.with(|a|{let a=a.borrow();let Some(a)=a.as_ref()else{return;};let Some(g)=a.ground else{return;};unsafe {let mut rect=RECT::default();let _=GetWindowRect(h,&mut rect);crate::topbar::companion_tip(&a.gfx,g.dpi,&text,POINT{x:(rect.left+rect.right)/2,y:rect.top+48,
                    },g.monitor,
                );}});}}

fn queue_input(q:&mut std::collections::VecDeque<grab::Input>,event:grab::Input)->bool {
    if let grab::Input::Move{id,..}=event {
        if q.back().is_some_and(|old|matches!(old,grab::Input::Move{id:old,..} if *old==id)) {q.pop_back();}
    }
    if q.len()>=128 {
        if let Some(i)=q.iter().position(|old|matches!(old,grab::Input::Move{..})) {q.remove(i);}
        else {q.pop_front();}
    }
    q.push_back(event);
    !matches!(event,grab::Input::Move{..})
}
// Native down/up are processed and painted synchronously, never on a later timer.
pub fn input(event:grab::Input){
 let slot=crate::topbar::companion_anchor();let mode=crate::dock::companion_mode();
 if input_blocked() {refresh();return;}
 let handled=APP.with(|a| {let Ok(mut a)=a.try_borrow_mut() else{return false;};let Some(a)=a.as_mut() else{return true;};
  if let Err(e)=interaction::deliver(a,event,|a,event|{a.input(event);a.immediate=!matches!(event,grab::Input::Move{..});},|a|unsafe{a.move_drag_window()},|a|unsafe{a.draw(Instant::now(),slot,mode)}){crate::log!("companion input draw: {e}");a.stop();}
  true
 });
 if handled {if !matches!(event,grab::Input::Move{..}){refresh();}}else{if queue_input(&mut INPUT.lock().unwrap_or_else(|e|e.into_inner()),event){refresh();}}
}
pub(super) fn input_blocked()->bool {
    crate::dock::companion_mode()!=visibility::Mode::Run || APP.with(|a|a.try_borrow().map_or(true,|a|a.as_ref().is_some_and(|a|a.hidden_since.is_some() || a.yielding)))
}
pub fn displays_changed(){let monitors=interaction::monitors();APP.with(|a|{if let Ok(mut a)=a.try_borrow_mut(){if let Some(a)=a.as_mut(){a.home_monitors=monitors;a.grounds.clear();if let Some(p)=&a.pacer{p.refresh_display();}if !a.grab.busy()&&a.cfg.home.is_some(){a.restore_home();}a.ground_due=Instant::now();}}});refresh();}

#[cfg(test)]
mod slot_wake_tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn hidden_dot_wakes_at_its_deadline_not_every_quarter_second() {
        let now = Instant::now();
        let mut s = model::Signal::default();
        // Agent waits alone: refreshed on change, no polling.
        assert_eq!(slot_wake(&s, None, false, now), None);
        // A cue notice 3 s out: one wake just after it.
        let w = slot_wake(&s, Some(now + Duration::from_secs(3)), false, now).unwrap();
        assert!((3000..=3100).contains(&w), "{w}");
        // A result shown 1 s ago lapses at 4 s: about 3 s from now.
        s.result = Some((now - Duration::from_secs(1), true));
        let w = slot_wake(&s, None, false, now).unwrap();
        assert!((2900..=3100).contains(&w), "{w}");
        // Past deadlines and an offered click: a slow check, never 250 ms.
        s.result = Some((now - Duration::from_secs(10), true));
        assert_eq!(slot_wake(&s, Some(now - Duration::from_secs(1)), true, now), Some(5_000));
    }
}
