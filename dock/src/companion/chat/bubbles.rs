//! Window-independent presentation state. Times are monotonic seconds.
use std::collections::VecDeque;

pub const LINE: f32 = 20.;
/// Text baseline inside a line (uniform spacing), and the bubble's insets.
pub const BASE: f32 = 15.;
pub const PAD_X: f32 = 10.;
pub const PAD_Y: f32 = 6.;
pub const RADIUS: f32 = 8.;
pub const WIDTH: f32 = 280.;
pub const GAP: f32 = 8.;
/// The whole stack's exit: each bubble drifts up and fades in FADE_ONE,
/// the topmost first, the next STAGGER later.
pub const FADE_ONE: f64 = 0.42;
pub const STAGGER: f64 = 0.09;
pub const FADE: f64 = FADE_ONE + 2. * STAGGER;

/// Absolute DIP offset, sampled from the same monotonic clock as the painter.
#[derive(Clone, Debug, Default)]
pub struct Scroll {
    from: f32,
    target: f32,
    at: f64,
}
impl Scroll {
    pub fn value(&self, now:f64, motion:bool)->f32 {
        if motion {self.from+(self.target-self.from)*ease((now-self.at)/0.08)} else {self.target}
    }
    pub fn wheel(&mut self, delta:f32, current:f32, max:f32, now:f64, motion:bool) {
        let target=(self.target+delta).clamp(0.,max);
        self.from=current.clamp(0.,max);self.target=target;self.at=now;
        if !motion {self.from=target;}
    }
    pub fn jump(&mut self, value:f32) {self.from=value;self.target=value;}
    pub fn clamp(&mut self,max:f32) {self.from=self.from.clamp(0.,max);self.target=self.target.clamp(0.,max);}
    pub fn active(&self,now:f64)->bool {now<self.at+0.08 && self.from!=self.target}
}

fn next_id()->u64 {
    static NEXT:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed)
}

#[derive(Clone, Debug)]
pub struct Message {
    pub id: u64,
    pub text: String,
    pub user: bool,
    pub scroll: Option<Scroll>,
}
impl Message {
    fn new(text:String,user:bool)->Self {Self{id:next_id(),text,user,scroll:None}}
    pub fn wheel(&mut self,delta:f32,current:f32,max:f32,now:f64,motion:bool) {
        let scroll=self.scroll.get_or_insert_with(||{let mut s=Scroll::default();s.jump(current);s});
        scroll.wheel(-delta*LINE,current,max,now,motion);
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Phase {
    #[default]
    Hidden,
    Input,
    Thinking,
    Reply,
}
#[derive(Default)]
pub struct Bubbles {
    pub phase: Phase,
    pub current: Option<Message>,
    /// Most recent first.
    pub previous: VecDeque<Message>,
    pub entered: f64,
    pub finished: Option<f64>,
    pub lifetime: f64,
    pub hovered: bool,
    pub hover_until: f64,
    pub agent: bool,
    pub reminder: bool,
    /// When the stack last moved one level up (archive).
    pub shifted: f64,
    /// The bubble pushed past the last level, and when.
    pub evicted: Option<(Message, f64)>,
}
impl Bubbles {
    fn archive(&mut self, now: f64) {
        if let Some(m) = self.current.take().filter(|m| !m.text.is_empty()) {
            self.previous.push_front(m);
            if self.previous.len() > 2 { self.evicted = self.previous.pop_back().map(|m| (m, now)); }
            self.shifted = now;
        }
    }
    /// When the stack starts its exit (None while it stays).
    pub fn deadline(&self) -> Option<f64> {
        self.finished.map(|at| (at + self.lifetime).max(self.hover_until + 0.5))
    }
    pub fn input(&mut self, now: f64) {
        self.reminder=false;
        self.archive(now);
        self.phase = Phase::Input;
        self.agent = false;
        self.entered = now;
        self.finished = None;
    }
    pub fn submit(&mut self, text: String, now: f64) {
        self.current = Some(Message::new(text, true));
        self.archive(now);
        self.phase = Phase::Thinking;
        self.agent = false;
        self.entered = now;
        self.finished = None;
    }
    pub fn delta(&mut self, text: &str, now: f64) {
        if text.is_empty() { return; }
        if self.phase != Phase::Reply {
            self.current = Some(Message::new(String::new(), false));
            self.phase = Phase::Reply;
            self.entered = now;
        }
        if let Some(m) = &mut self.current { m.text.push_str(text); }
    }
    pub fn finish(&mut self, now: f64) {
        self.finished = Some(now);
        self.lifetime = lifetime(self.current.as_ref().map_or("",|m|m.text.as_str()),false);
        self.hover_until = now;
        if self.phase == Phase::Thinking { self.phase = Phase::Hidden; }
    }
    pub fn say(&mut self, text: String, now: f64) {
        self.reminder=false;
        self.archive(now);
        self.phase = Phase::Reply;
        self.agent = true;
        self.current = Some(Message::new(text, false));
        self.entered = now;
        self.finished = Some(now);
        self.lifetime = lifetime(self.current.as_ref().unwrap().text.as_str(),true);
        self.hover_until = now;
    }
    pub fn hover(&mut self, inside: bool, now: f64) {
        self.hovered = inside;
        if inside { self.hover_until = now; }
    }
    pub fn alpha(&self, now: f64, motion: bool) -> f32 {
        if self.phase == Phase::Hidden { return 0.; }
        let entry = if motion { ((now - self.entered) / 0.18).clamp(0., 1.) as f32 } else { 1. };
        entry * self.opacity(now,motion)
    }
    pub fn opacity(&self,now:f64,motion:bool)->f32 {
        self.finished.map_or(1., |at| {
            if self.hovered { return 1.; }
            let deadline = (at + self.lifetime).max(self.hover_until + 0.5);
            if motion { (1. - (now - deadline) / FADE).clamp(0., 1.) as f32 }
            else { f32::from(now < deadline) }
        })
    }
    pub fn tick(&mut self, now: f64, motion: bool) {
        if self.evicted.as_ref().is_some_and(|(_, at)| !motion || now > at + 0.36) { self.evicted = None; }
        if self.finished.is_some() && self.alpha(now, motion) == 0. && now > self.entered + 0.18 {
            self.phase = Phase::Hidden;
            self.current = None;
            self.previous.clear();
            self.evicted = None;
        }
    }
    pub fn cancel(&mut self) { *self = Self::default(); }
    pub fn message(&self,id:u64)->Option<&Message> {self.current.iter().chain(&self.previous).find(|m|m.id==id)}
    pub fn message_mut(&mut self,id:u64)->Option<&mut Message> {self.current.iter_mut().chain(&mut self.previous).find(|m|m.id==id)}
    pub fn scrolling(&self,now:f64)->bool {self.current.iter().chain(&self.previous).any(|m|m.scroll.as_ref().is_some_and(|s|s.active(now)))}
}
pub fn lifetime(text:&str,agent:bool)->f64 {
    let n=text.chars().count() as f64;
    if agent {(6.+0.05*n).min(20.)} else {(8.+0.06*n).clamp(8.,30.)}
}
pub fn stack_style(depth: usize) -> (f32, f32) {
    match depth { 0 => (1., 1.), 1 => (0.94, 0.55), 2 => (0.88, 0.28), _ => (0.84, 0.) }
}
/// Ease-out cubic: fast start, soft landing (pop-ins, slides).
pub fn ease(t: f64) -> f32 { let t = t.clamp(0., 1.); (1. - (1. - t).powi(3)) as f32 }
/// All think frames share a foot-relative anchor, never the moving hat tip.
pub fn spinner(foot: (f32, f32), scale: f32) -> ((f32, f32), f32, f32) {
    ((foot.0 + 1.2 * scale, foot.1 - 206.7 * scale),
        (6.6 * scale / 0.49).max(5.), (1.9 * scale / 0.49).max(1.4))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stack_keeps_two_and_reply_archives_on_input() {
        let mut b = Bubbles::default();
        for i in 0..4 { b.say(i.to_string(), i as f64); }
        assert_eq!(b.previous.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), ["2", "1"]);
        assert_eq!(b.evicted.as_ref().map(|(m, at)| (m.text.as_str(), *at)), Some(("0", 3.)));
        b.tick(3.2, true); assert!(b.evicted.is_some()); b.tick(3.4, true); assert!(b.evicted.is_none());
        b.input(4.); assert_eq!(b.previous[0].text, "3");
        b.submit("hello".into(), 5.); b.delta("hi", 6.);
        assert_eq!(b.previous[0].text, "hello");
        assert_eq!(stack_style(1), (0.94, 0.55)); assert_eq!(stack_style(2), (0.88, 0.28)); assert_eq!(stack_style(3).1, 0.);
    }
    #[test]
    fn answer_eight_seconds_hover_and_say_six_seconds() {
        let mut b = Bubbles::default(); b.delta("reply", 0.); b.finish(1.);
        assert_eq!(b.alpha(8.99, true), 1.);
        b.hover(true, 9.); assert_eq!(b.alpha(20., true), 1.);
        b.hover(true, 20.); b.hover(false, 20.1);
        assert_eq!(b.alpha(20.49, true), 1.); b.tick(20.9, true); assert_eq!(b.phase, Phase::Reply); b.tick(21.25, true); assert_eq!(b.phase, Phase::Hidden);
        b.say("agent".into(), 30.); assert_eq!(b.alpha(36.24, false), 1.); b.tick(36.25, false); assert_eq!(b.phase, Phase::Hidden);
    }
    #[test]
    fn unicode_lifetimes_and_limits() {
        assert_eq!(lifetime("",false),8.);
        assert_eq!(lifetime(&"가".repeat(100),false),14.);
        assert_eq!(lifetime(&"가".repeat(1000),false),30.);
        assert_eq!(lifetime(&"🙂".repeat(100),true),11.);
        assert_eq!(lifetime(&"🙂".repeat(1000),true),20.);
    }
    #[test]
    fn scroll_eases_for_80ms_clamps_and_reverses_without_dead_wheels() {
        let mut m=Message::new("long".into(),false);
        m.wheel(1.,160.,160.,1.,true);
        let s=m.scroll.as_ref().unwrap();assert_eq!(s.value(1.,true),160.);
        assert!(s.value(1.04,true)<160. && s.value(1.04,true)>140.);
        assert_eq!(s.value(1.081,true),140.);
        m.wheel(100.,140.,160.,2.,true);
        m.wheel(100.,0.,160.,3.,true);
        m.wheel(-0.5,0.,160.,3.1,false);
        assert_eq!(m.scroll.as_ref().unwrap().value(3.1,false),10.);
    }
    #[test]
    fn manual_scroll_survives_streaming_archive_and_equal_text_has_distinct_ids() {
        let mut b=Bubbles::default();b.delta("same",0.);
        let id=b.current.as_ref().unwrap().id;
        b.message_mut(id).unwrap().wheel(1.,80.,80.,1.,false);
        b.delta(" appended",2.);
        assert_eq!(b.message(id).unwrap().scroll.as_ref().unwrap().value(2.,false),60.);
        b.finish(3.);b.input(4.);
        assert_eq!(b.previous[0].id,id);
        b.say("same appended".into(),5.);assert_ne!(b.current.as_ref().unwrap().id,id);
    }
    #[test]
    fn spinner_anchor_is_identical_across_all_think_frames() {
        let frames = &super::super::super::clips::catalog().frames;
        for scale in [0.28, 0.56] { for i in 0..8 {
            assert!(frames.contains_key(&format!("think_{i}")));
            let (p, r, dot) = spinner((200., 400.), scale);
            assert_eq!(p, (200. + 1.2 * scale, 400. - 206.7 * scale));
            assert!(r >= 5. && dot >= 1.4);
        }}
    }
}
