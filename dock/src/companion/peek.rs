//! Pure outer-edge docking geometry and animation; no desktop IO.
use super::{
    anchors::Anchors,
    clips,
    grab::{Home, Point},
    motion::{Motion, Pose},
};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::RECT;
use std::f32::consts::PI;

const ENTER_SECONDS: f32 = 0.5;
const RETURN_SECONDS: f32 = 0.35;
const HAT_DELAY: f32 = 0.05;
#[path = "peek_idle.rs"]
mod idle;

/// Analytic damped spring: 12% first overshoot, then a 1.4% rebound.
/// Smoothly fade the subpixel tail so the exact endpoint is independent of FPS.
fn spring(t: f32, duration: f32) -> f32 {
    let u = (t / duration).clamp(0., 1.);
    let fade = ((1. - u) / 0.2).clamp(0., 1.);
    let fade = fade * fade * (3. - 2. * fade);
    (-6.36 * u).exp() * ((3. * PI * u).cos() + 6.36 / (3. * PI) * (3. * PI * u).sin()) * fade
}

fn squash(t: f32, duration: f32) -> f32 {
    if t < 0. || t >= duration { return 0.; }
    let residual = -spring(t, duration);
    let residual = if t < duration / 3. { residual.max(0.) } else { residual };
    (residual / 0.12 * 0.1).clamp(-0.05, 0.1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    Bottom,
    Left,
    Right,
}
impl Edge {
    pub fn inward(self) -> (f32, f32) {
        match self {
            Self::Bottom => (0., -1.),
            Self::Left => (1., 0.),
            Self::Right => (-1., 0.),
        }
    }
    pub fn distance(self, r: RECT, p: Point) -> f32 {
        match self {
            Self::Bottom => r.bottom as f32 - p.y,
            Self::Left => p.x - r.left as f32,
            Self::Right => r.right as f32 - p.x,
        }
    }
    pub fn along(self, p: Point) -> f32 {
        if self == Self::Bottom {
            p.x
        } else {
            p.y
        }
    }
    pub fn span(self, r: RECT) -> (f32, f32) {
        if self == Self::Bottom {
            (r.left as f32, r.right as f32)
        } else {
            (r.top as f32, r.bottom as f32)
        }
    }
}
/// Reject every other screen in the outward half-plane at this span, including
/// separated screens: the hidden body must never appear on another monitor.
pub fn exposed(edge: Edge, r: RECT, lo: f32, hi: f32, monitors: &[(u64, RECT)]) -> bool {
    monitors.iter().all(|(_, other)| {
        if *other == r {
            return true;
        }
        let (a, b) = edge.span(*other);
        let outward = match edge {
            Edge::Bottom => other.bottom > r.bottom,
            Edge::Left => other.left < r.left,
            Edge::Right => other.right > r.right,
        };
        !outward || hi <= a || lo >= b
    })
}
pub fn candidate(monitors: &[(u64, RECT)], body: RECT, cursor: Point) -> Option<Home> {
    let mut best: Option<(f32, Home)> = None;
    for &(key, r) in monitors {
        for edge in [Edge::Bottom, Edge::Left, Edge::Right] {
            let along = edge.along(cursor);
            let (a, b) = edge.span(r);
            if along < a || along > b {
                continue;
            }
            let (lo, hi) = edge.span(body);
            if !exposed(edge, r, lo.max(a), hi.min(b), monitors) {
                continue;
            }
            let size = if edge == Edge::Bottom {
                (body.bottom - body.top) as f32
            } else {
                (body.right - body.left) as f32
            };
            let outside = match edge {
                Edge::Bottom => (body.bottom - r.bottom) as f32,
                Edge::Left => (r.left - body.left) as f32,
                Edge::Right => (body.right - r.right) as f32,
            };
            let distance = edge.distance(r, cursor).abs();
            // Do not attract a body on an unrelated monitor across a desktop gap.
            let near_body = match edge {
                Edge::Bottom => body.top < r.bottom && body.bottom >= r.top,
                _ => body.left < r.right && body.right > r.left,
            };
            if near_body
                && (distance <= 6. || outside >= size * 0.4 && outside <= size)
                && best.as_ref().is_none_or(|(d, _)| distance < *d)
            {
                let mut home = Home::at(key, along, a, b);
                home.edge = Some(edge);
                best = Some((distance, home));
            }
        }
    }
    best.map(|(_, h)| h)
}
pub fn resolve(home: Home, monitors: &[(u64, RECT)], half: f32) -> Option<(Edge, RECT, f32)> {
    let edge = home.edge?;
    let r = monitors.iter().find(|(k, _)| *k == home.monitor)?.1;
    let (a, b) = edge.span(r);
    let (before, after) = if edge == Edge::Bottom {
        (half, half)
    } else {
        let f = &clips::catalog().frames["sidepeek_0"];
        (
            f.ground * half / 130.,
            (f.h as f32 - f.ground) * half / 130.,
        )
    };
    let fit = ((b - a) / (before + after)).min(1.);
    let along = home.x(a, b).clamp(a + before * fit, b - after * fit);
    exposed(edge, r, along - before, along + after, monitors).then_some((edge, r, along))
}
pub fn detached(edge: Edge, r: RECT, from: Point, to: Point) -> bool {
    edge.distance(r, to) - edge.distance(r, from) >= 30.
}

#[derive(Default)]
pub struct Runtime {
    pub entered: Option<f64>,
    pub notification: Option<f64>,
    pub next_look: f64,
    pub look_at: Option<f64>,
    pub clicked: Option<f64>,
    pub released: Option<(f64, Edge)>,
    life: idle::Life,
    edge: Option<Edge>,
}
impl Runtime {
    pub fn edge_frame(&mut self, edge: Edge, now: f64, motion: bool, unit: f32) -> &'static str {
        self.edge = Some(edge);
        if edge == Edge::Bottom {
            self.frame(now, motion, unit)
        } else {
            self.look_at = None;
            "sidepeek_0"
        }
    }
    pub fn edge_depth(&self, edge: Edge, now: f64, motion: bool) -> f32 {
        let entry = if motion { self.entered.map_or(0., |at| spring((now-at) as f32, ENTER_SECONDS)) } else { 0. };
        let notification = self.notification.map_or(0., |at| {
            let t = (now-at) as f32;
            if t < 3. {
                if motion {(t / 0.3).clamp(0., 1.)} else {1.}
            } else if motion {spring(t-3., RETURN_SECONDS)} else {0.}
        });
        1. + entry + notification * if edge == Edge::Bottom {0.6} else {0.4}
            + self.idle(edge,now,motion).depth
    }
    pub fn enter(&mut self, now: f64, unit: f32) {
        *self = Self {
            entered: Some(now),
            next_look: now + 6. + 6. * unit as f64,
            life: idle::Life::new(now,unit),
            ..Self::default()
        };
    }
    pub fn notify(&mut self, now: f64) {
        self.notification = Some(now);
        self.entered = None;
    }
    pub fn active_notification(&self, now: f64) -> bool {
        // Return in the ordinary edge frame, so the artwork does not switch
        // abruptly after the spring has already settled.
        self.notification.is_some_and(|at| now - at < 3.)
    }
    #[cfg(test)]
    pub fn depth(&self, now: f64, motion: bool) -> f32 {
        self.edge_depth(Edge::Bottom, now, motion)
    }
    fn deformation(&self, now: f64) -> f32 {
        self.entered.map_or(0., |at| squash((now-at) as f32, ENTER_SECONDS))
            + self.notification.map_or(0., |at| squash((now-at-3.) as f32, RETURN_SECONDS))
    }
    fn idle(&self, edge: Edge, now: f64, motion: bool) -> idle::Sample {
        let transitioning = self.entered.is_some_and(|at| ((now-at) as f32) < ENTER_SECONDS+HAT_DELAY)
            || self.notification.is_some_and(|at| ((now-at) as f32) < 3.+RETURN_SECONDS+HAT_DELAY);
        let mut expression = if transitioning {0.} else {1.};
        if edge == Edge::Bottom {
            // Ease out before a scheduled look, then ease back in. A look
            // never changes its existing 1->2->1->0 timing to fit a droop.
            if self.next_look > now {expression *= ((self.next_look-now)/0.15).min(1.) as f32;}
            if let Some(at)=self.look_at {expression *= ((now-at-1.)/0.15).clamp(0.,1.) as f32;}
        }
        let mut sample=self.life.sample(edge,now,motion,expression);
        // Resume breathing smoothly after the motion settles, without a
        // subpixel step at the end of the spring.
        if let Some(at)=self.entered {sample.breath *= ((now-at-(ENTER_SECONDS+HAT_DELAY) as f64)/0.35).clamp(0.,1.) as f32;}
        if let Some(at)=self.notification {sample.breath *= ((now-at-3.-(RETURN_SECONDS+HAT_DELAY) as f64)/0.35).clamp(0.,1.) as f32;}
        sample
    }
    /// Production and offscreen renders use this identical pose assembly.
    #[allow(clippy::too_many_arguments)]
    pub fn animated_pose(&self, edge: Edge, r: RECT, along: f32, frame: &str, s: f32, dpi: f32, now: f64, motion: bool) -> (Pose, f32, f32) {
        let idle=self.idle(edge,now,motion);
        let (mut p,mut x,mut y) = pose(edge,r,along,frame,s,dpi,self.edge_depth(edge,now,motion),0.);
        if motion {
            let amount = self.deformation(now);
            if edge == Edge::Bottom {p.sx *= 1.+amount; p.sy *= 1.-amount;}
            else {p.sx *= 1.-amount; p.sy *= 1.+amount;}
            // Squash around the visible tip/outer edge: otherwise the scale
            // change adds another 10% of apparent depth to the 12% spring.
            let visible = if edge == Edge::Bottom {cut(frame)-Anchors::load(frame).tip.1}
                else {let f=&clips::catalog().frames["sidepeek_0"]; f.w as f32-f.edge.unwrap()};
            let (nx,ny)=edge.inward();
            x+=nx*visible*s*amount;y+=ny*visible*s*amount;
            let hat = self.deformation(now-HAT_DELAY as f64)*70.+idle.hat;
            Self::bend_hat(&mut p,hat);
            x+=nx*idle.breath/dpi;y+=ny*idle.breath/dpi;
        }
        (p,x,y)
    }
    fn bend_hat(p: &mut Pose, angle: f32) {
        p.hat_tilt = angle;
    }
    pub fn detach(&mut self, now: f64, edge: Edge) {
        *self = Self { released: Some((now,edge)), ..Self::default() };
    }
    pub fn apply_detach(&self, p: &mut Pose, now: f64, motion: bool) {
        if let Some((at,edge)) = self.released.filter(|(at,_)| motion && (0. ..0.2).contains(&(now-at))) {
            let stretch = 0.06*(PI*(now-at) as f32/0.2).sin();
            if edge == Edge::Bottom {p.sy *= 1.+stretch; p.sx *= 1.-stretch;}
            else {p.sx *= 1.+stretch; p.sy *= 1.-stretch;}
            p.continuous = true;
        }
    }
    pub fn interval(&self, now: f64, motion: bool, battery: bool) -> u32 {
        if !motion {
            return 250;
        }
        let busy = self.entered.is_some_and(|at| ((now-at) as f32) < ENTER_SECONDS+HAT_DELAY)
            || self.notification.is_some_and(|at| {
                let t=(now-at) as f32;
                t < 3.+RETURN_SECONDS+HAT_DELAY
                    && (self.edge.unwrap_or(Edge::Bottom)==Edge::Bottom || !(0.3..3.).contains(&t))
            })
            || self.look_at.is_some_and(|at| now - at < 1.)
            || self.idle(self.edge.unwrap_or(Edge::Bottom),now,motion).busy;
        if busy {
            if battery {
                34
            } else {
                17
            }
        } else if battery {
            100
        } else {
            50
        }
    }
    pub fn frame(&mut self, now: f64, motion: bool, _unit: f32) -> &'static str {
        if !motion {
            return if self.clicked.is_some_and(|at| now - at < 1.) {
                "peek_2"
            } else {
                "peek_0"
            };
        }
        // A look requires a click or notification; elapsed time is not a reason.
        match self.look_at.map(|at| now - at) {
            Some(t) if t < 0.25 => "peek_1",
            Some(t) if t < 0.75 => "peek_2",
            Some(t) if t < 1. => "peek_1",
            _ => "peek_0",
        }
    }
}
/// Pixel landmark measured from the approved peach nose, including its bottom.
pub fn cut(frame: &str) -> f32 {
    Anchors::load(frame).nose_bottom + 4.
}
#[allow(clippy::too_many_arguments)]
pub fn pose(
    edge: Edge,
    r: RECT,
    along: f32,
    frame: &str,
    s: f32,
    dpi: f32,
    depth: f32,
    breath: f32,
) -> (Pose, f32, f32) {
    if edge != Edge::Bottom {
        let frame = "sidepeek_0";
        let f = &clips::catalog().frames[frame];
        let wall = Anchors::load(frame).edge.expect("sidepeek wall anchor");
        let mut p = Motion::new(0.).pose();
        p.frame = frame.into();
        p.flip = edge == Edge::Right;
        p.pivot = Some((wall / f.w as f32, f.ground / f.h as f32));
        p.continuous = true;
        // Breathe vertically about the soles; never move the saved foot height.
        p.sy = 1. + breath / f.ground;
        let extra = (f.w as f32 - wall) * s * (depth - 1.);
        let x = if p.flip {
            r.right as f32 / dpi - extra
        } else {
            r.left as f32 / dpi + extra
        };
        return (p, x, along / dpi);
    }
    let f = &clips::catalog().frames[frame];
    let cut = cut(frame);
    let tip = Anchors::load(frame).tip.1;
    let mut p = Motion::new(0.).pose();
    p.frame = frame.into();
    p.pivot = Some((f.ax / f.w as f32, cut / f.h as f32));
    p.continuous = true;
    let normal = if frame.starts_with("peek_") {
        cut - tip
    } else {
        self::cut("peek_0") - Anchors::load("peek_0").tip.1
    };
    let extra = (normal * depth - (cut - tip)) * s + breath;
    let (nx, ny) = edge.inward();
    let (x, y) = match edge {
        Edge::Bottom => (along / dpi, r.bottom as f32 / dpi),
        Edge::Left => (r.left as f32 / dpi, along / dpi),
        Edge::Right => (r.right as f32 / dpi, along / dpi),
    };
    (p, x + nx * extra, y + ny * extra)
}
/// Clear both rendered and hit-test pixels outside the selected monitor.
/// Input and output are the same premultiplied BGRA buffer used by the HWND.
pub fn clip_pixels(pixels: &mut [u8], width: u32, origin: (i32, i32), monitor: RECT) {
    for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = origin.0 + (i % width as usize) as i32;
        let y = origin.1 + (i / width as usize) as i32;
        if x < monitor.left || x >= monitor.right || y < monitor.top || y >= monitor.bottom {
            pixel.fill(0);
        }
    }
}
pub fn bounds(p: &Pose, x: f32, y: f32, s: f32, dpi: f32) -> RECT {
    let f = &clips::catalog().frames[&p.frame];
    let m = super::render::body_matrix(p, x, y, s);
    let pts = [
        (0., 0.),
        (f.w as f32, 0.),
        (0., f.h as f32),
        (f.w as f32, f.h as f32),
    ]
    .map(|(a, b)| {
        (
            (a * m.M11 + b * m.M21 + m.M31) * dpi,
            (a * m.M12 + b * m.M22 + m.M32) * dpi,
        )
    });
    RECT {
        left: pts.iter().map(|p| p.0).reduce(f32::min).unwrap().floor() as i32,
        right: pts.iter().map(|p| p.0).reduce(f32::max).unwrap().ceil() as i32,
        top: pts.iter().map(|p| p.1).reduce(f32::min).unwrap().floor() as i32,
        bottom: pts.iter().map(|p| p.1).reduce(f32::max).unwrap().ceil() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: i32, y: i32) -> RECT {
        RECT {
            left: x,
            top: y,
            right: x + 1000,
            bottom: y + 800,
        }
    }
    #[test]
    fn squash_hat_delay_detach_and_motion_off_are_time_driven() {
        for edge in [Edge::Bottom,Edge::Left,Edge::Right] {
            let mut clock=Runtime::default();clock.enter(0.,0.);
            let frame=if edge==Edge::Bottom {"peek_0"} else {"sidepeek_0"};
            let r=rect(-1000,-800);
            let sample=|t,motion|clock.animated_pose(edge,r,-400.,frame,0.56,1.5,t,motion);
            let p=sample(1./6.,true).0;
            if edge==Edge::Bottom {assert!((p.sy-0.9).abs()<0.002 && (p.sx-1.1).abs()<0.002);}
            else {assert!((p.sx-0.9).abs()<0.002 && (p.sy-1.1).abs()<0.002);}
            let mut body_peak=(0.,0.);let mut hat_peak=(0.,0.);
            for i in 0..550 {
                let t=i as f64/1000.;let p=sample(t,true).0;
                let deform=if edge==Edge::Bottom {1.-p.sy} else {1.-p.sx};
                if deform>body_peak.1 {body_peak=(t,deform);}
                if p.hat_tilt>hat_peak.1 {hat_peak=(t,p.hat_tilt);}
                let (off,x,y)=sample(t,false);
                let (base,bx,by)=sample(0.,false);
                assert_eq!((off.sx,off.sy,off.hat_tilt,x,y),(base.sx,base.sy,base.hat_tilt,bx,by));
            }
            assert!((hat_peak.0-body_peak.0-0.05).abs()<0.002);
            let settled=sample(0.6,true);let base=sample(0.6,false);
            assert!((settled.1-base.1).hypot(settled.2-base.2)*1.5<0.5);
            assert_eq!((settled.0.sx,settled.0.sy,settled.0.hat_tilt),(1.,1.,0.));
            clock.detach(1.,edge);
            for t in [1.,1.1,1.21] {
                let mut p=Motion::new(0.).pose();p.sx=1.;p.sy=1.;
                clock.apply_detach(&mut p,t,true);
                assert_eq!(p.pivot,None);
                if t==1.1 {assert!(p.sx!=1. && p.sy!=1.);} else {assert_eq!((p.sx,p.sy),(1.,1.));}
            }
        }
    }
    #[test]
    fn hidden_life_pacing_look_suppression_and_motion_off() {
        for edge in [Edge::Bottom,Edge::Left,Edge::Right] {
            let mut clock=Runtime::default();clock.enter(0.,0.);
            let peak=if edge==Edge::Bottom {9.3} else {3.5};
            let frame=clock.edge_frame(edge,peak,true,0.);
            // Explicit look starts keep the original frame sequence and pause the hat.
            if edge==Edge::Bottom {
                clock.look_at=None;clock.next_look=12.;
                assert!(clock.idle(edge,peak,true).hat>9.9);
                clock.look_at=Some(peak);
                assert_eq!(clock.idle(edge,peak+0.2,true).hat,0.);
                assert_eq!(clock.frame(peak+0.3,true,0.),"peek_2");
                clock.look_at=None;
            } else {assert!((clock.edge_depth(edge,peak,true)-1.).abs()<0.001);}
            assert_eq!(clock.interval(peak,true,false),17);
            assert_eq!(clock.interval(peak,true,true),34);
            assert_eq!(clock.interval(2.,true,false),50);
            assert_eq!(clock.interval(2.,true,true),100);
            assert_eq!(clock.interval(peak,false,false),250);
            let still=clock.animated_pose(edge,rect(0,0),400.,frame,0.28,1.,0.,false);
            for now in [0.2,9.3,17.3,35.] {
                let (p,x,y)=clock.animated_pose(edge,rect(0,0),400.,frame,0.28,1.,now,false);
                assert_eq!((p.sx,p.sy,p.hat_tilt,x,y),(1.,1.,0.,still.1,still.2));
            }
        }
    }
    #[test]
    fn actual_visible_depth_includes_squash_and_stays_near_twelve_percent() {
        for edge in [Edge::Bottom,Edge::Left,Edge::Right] {
            for (s,dpi) in [(0.28,1.),(0.56,1.5)] {
                let mut clock=Runtime::default();clock.enter(0.,0.);
                let frame=if edge==Edge::Bottom {"peek_0"} else {"sidepeek_0"};
                let r=rect(-1000,-800);
                let (p,x,y)=clock.animated_pose(edge,r,-400.,frame,s,dpi,1./6.,true);
                let f=&clips::catalog().frames[frame];
                let (point,normal)=if edge==Edge::Bottom {
                    let a=Anchors::load(frame);let tip=a.posed(&p,1.).tip;
                    (Point::new((x+tip.0*s)*dpi,(y+tip.1*s)*dpi),(cut(frame)-a.tip.1)*s*dpi)
                } else {
                    let m=super::super::render::body_matrix(&p,x,y,s);
                    (Point::new((f.w as f32*m.M11+f.ground*m.M21+m.M31)*dpi,y*dpi),(f.w as f32-f.edge.unwrap())*s*dpi)
                };
                let visible=edge.distance(r,point)/normal;
                assert!((0.87..0.89).contains(&visible),"{edge:?}: {visible}");
            }
        }
    }
    #[test]
    fn three_monitor_layouts_and_partial_seams() {
        for screens in [
            vec![(1, rect(0, 0)), (2, rect(1000, 0))],
            vec![(1, rect(0, 0)), (2, rect(0, 800))],
            vec![(1, rect(0, 0)), (2, rect(1000, 0)), (3, rect(0, 800))],
        ] {
            let r = screens[0].1;
            assert!(exposed(Edge::Left, r, 100., 200., &screens));
            assert_eq!(
                exposed(Edge::Right, r, 100., 200., &screens),
                screens.len() == 2 && screens[1].1.top == 800
            );
            assert_eq!(
                exposed(Edge::Bottom, r, 100., 200., &screens),
                screens.len() == 2 && screens[1].1.left == 1000
            );
        }
        let screens = [(1, rect(-1000, 0)), (2, rect(0, 400))];
        assert!(exposed(Edge::Right, screens[0].1, 100., 200., &screens));
        assert!(!exposed(Edge::Right, screens[0].1, 350., 450., &screens));
    }
    #[test]
    fn thresholds_restore_and_missing_monitor() {
        let r = rect(0, 0);
        let screens = [(7, r)];
        let body = RECT {
            left: 450,
            right: 550,
            top: 740,
            bottom: 840,
        };
        let h = candidate(&screens, body, Point::new(500., 760.)).unwrap();
        assert_eq!(h.edge, Some(Edge::Bottom));
        assert!(candidate(
            &screens,
            RECT {
                bottom: 839,
                top: 739,
                ..body
            },
            Point::new(500., 760.)
        )
        .is_none());
        assert!(candidate(
            &screens,
            RECT {
                bottom: 790,
                top: 690,
                ..body
            },
            Point::new(500., 794.)
        )
        .is_some());
        let json = serde_json::to_string(&h).unwrap();
        let restored = serde_json::from_str(&json).unwrap();
        assert_eq!(h, restored);
        assert_eq!(resolve(restored, &screens, 30.).unwrap().2, 500.);
        assert!(resolve(h, &[], 30.).is_none());
        assert!(!detached(
            Edge::Bottom,
            r,
            Point::new(500., 790.),
            Point::new(500., 761.)
        ));
        assert!(detached(
            Edge::Bottom,
            r,
            Point::new(500., 790.),
            Point::new(500., 760.)
        ));
        let old: Home = serde_json::from_str(r#"{"monitor":7,"ratio":0.5}"#).unwrap();
        assert!(old.edge.is_none());
    }
    #[test]
    fn notification_and_motion_off() {
        let mut t = Runtime::default();
        t.enter(0., 0.5);
        assert_eq!(t.depth(0., false), 1.);
        assert_eq!(t.frame(10., false, 0.), "peek_0");
        assert_eq!(t.frame(9., true, 0.5), "peek_0");
        t.look_at = Some(9.); // An explicit notification or user action.
        assert_eq!(t.frame(9., true, 0.5), "peek_1");
        assert_eq!(t.frame(9.4, true, 0.5), "peek_2");
        assert_eq!(t.frame(9.8, true, 0.5), "peek_1");
        assert_eq!(t.frame(10.1, true, 0.5), "peek_0");
        t.notify(12.);
        assert!((t.depth(12.3, true) - 1.6).abs() < 0.001);
        assert_eq!(t.depth(15.35, true), 1.);
        assert_eq!(t.depth(12., false), 1.6);
        assert_eq!(t.depth(15., false), 1.);
    }
    #[test]
    fn pinned_pose_has_no_roaming_and_scales_on_restore() {
        let r = rect(-1000, 0);
        let screens = [(8, r)];
        for edge in [Edge::Bottom, Edge::Left, Edge::Right] {
            let mut h = Home::at(8, 0.5, 0., 1.);
            h.edge = Some(edge);
            for scale in [0.28, 0.56] {
                for dpi in [1., 1.5] {
                    let (_, r, along) = resolve(h, &screens, 130. * scale * dpi).unwrap();
                    let mut clock = Runtime::default();
                    clock.enter(0., 0.5);
                    let (_, x, y) = pose(edge, r, along, "peek_0", scale, dpi, 1., 0.);
                    for now in [1., 61., 601., 3601.] {
                        let frame = clock.frame(now, true, 0.5);
                        let (_, px, py) = pose(
                            edge,
                            r,
                            along,
                            frame,
                            scale,
                            dpi,
                            clock.depth(now, true),
                            0.,
                        );
                        assert_eq!((x, y), (px, py));
                    }
                    let (p, x, y) = pose(edge, r, along, "peek_0", scale, dpi, 1., 0.);
                    let a = Anchors::load(&p.frame).posed(&p, 1.);
                    let tip = Point::new((x + a.tip.0 * scale) * dpi, (y + a.tip.1 * scale) * dpi);
                    assert!(edge.distance(r, tip) > 0.);
                }
            }
        }
    }
    #[test]
    fn entrance_spring_overshoots_all_edges_and_settles_by_point_six() {
        let mut t = Runtime::default();
        t.enter(0., 0.5);
        for edge in [Edge::Bottom, Edge::Left, Edge::Right] {
            assert_eq!(t.edge_depth(edge, 0., true), 2.);
            let minimum = (0..60).map(|i| t.edge_depth(edge, i as f64 / 120., true)).fold(2., f32::min);
            assert!((0.86..0.90).contains(&minimum), "{edge:?}: {minimum}");
            assert!(t.edge_depth(edge, 0.34, true) > 1.);
            assert!((t.edge_depth(edge, 0.6, true) - 1.).abs() * 300. < 0.5);
            for i in 0..60 {assert_eq!(t.edge_depth(edge, i as f64 / 120., false), 1.);}
        }
        let r = rect(0, 0);
        assert!(candidate(
            &[(1, r)],
            RECT {
                left: 400,
                right: 500,
                top: -50,
                bottom: 50
            },
            Point::new(450., 2.)
        )
        .is_none());
    }

    #[test]
    fn detach_button_releases_edge() {
        let mut h = Home::at(1, 0.5, 0., 1.);
        h.edge = Some(Edge::Left);
        let mut c = super::super::model::Config {
            home: Some(h),
            roam: false,
            ..Default::default()
        };
        super::super::convenience::apply(&mut c, super::super::convenience::Action::Detach, 0);
        assert!(!c.roam);
        assert!(c.home.unwrap().edge.is_none());
        assert_eq!(c.home.unwrap().ratio, 0.);
        let mut right = h;
        right.edge = Some(Edge::Right);
        right.release_edge();
        assert_eq!(right.ratio, 1.);
    }

    #[test]
    fn notification_visible_depth_uses_normal_peek_height() {
        let r = rect(0, 0);
        let edge = Edge::Bottom;
        for frame in ["notify_1", "done_1", "error_1"] {
            let (p, x, y) = pose(edge, r, 400., frame, 0.56, 1.5, 1.6, 0.);
            let tip = Anchors::load(frame).posed(&p, 1.).tip;
            let distance = edge.distance(
                r,
                Point::new((x + tip.0 * 0.56) * 1.5, (y + tip.1 * 0.56) * 1.5),
            );
            let expected = (cut("peek_0") - Anchors::load("peek_0").tip.1) * 0.56 * 1.5 * 1.6;
            assert!((distance - expected).abs() < 0.001);
        }
    }

    #[test]
    fn resting_pacer_and_static_click_feedback() {
        let mut clock = Runtime::default();
        clock.enter(0., 0.5);
        assert_eq!(clock.interval(0.1, true, false), 17);
        assert_eq!(clock.interval(2., true, false), 50);
        assert_eq!(clock.interval(2., true, true), 100);
        clock.clicked = Some(2.);
        assert_eq!(clock.frame(2.5, false, 0.5), "peek_2");
        assert_eq!(clock.frame(3.1, false, 0.5), "peek_0");
        clock.notify(4.);
        assert_eq!(clock.interval(4.2, true, false), 17);
        assert_eq!(clock.interval(7.4, true, false), 50);
        for edge in [Edge::Left,Edge::Right] {
            clock.edge_frame(edge,4.,true,0.5);
            assert_eq!(clock.interval(4.2,true,false),17);
            assert_eq!(clock.interval(5.,true,false),50);
            assert_eq!(clock.interval(5.,true,true),100);
            assert_eq!(clock.interval(7.1,true,false),17);
            assert_eq!(clock.interval(7.1,true,true),34);
        }
    }

    #[test]
    fn side_wall_mirrors_without_rotation_and_soles_stay_fixed() {
        let r = rect(-1200, -300);
        for edge in [Edge::Left, Edge::Right] {
            for s in [0.28, 0.56] {
                for dpi in [1., 1.5] {
                    for breath in [-0.3, 0., 0.3] {
                        let (p, x, y) = pose(edge, r, 250., "peek_2", s, dpi, 1., breath);
                        assert_eq!(p.frame, "sidepeek_0");
                        assert_eq!(p.flip, edge == Edge::Right);
                        assert_eq!(p.tilt, 0.);
                        let a = Anchors::load(&p.frame);
                        let f = &clips::catalog().frames[&p.frame];
                        let m = super::super::render::body_matrix(&p, x, y, s);
                        let wall = (a.edge.unwrap() * m.M11 + m.M31) * dpi;
                        let expected = if edge == Edge::Left { r.left } else { r.right };
                        assert!((wall - expected as f32).abs() < 0.001);
                        let foot = (f.ground * m.M22 + m.M32) * dpi;
                        assert!((foot - 250.).abs() < 0.001);
                    }
                }
            }
        }
    }

    #[test]
    fn side_is_one_frame_and_notification_returns_after_three_seconds() {
        for edge in [Edge::Left, Edge::Right] {
            for motion in [false, true] {
                let mut clock = Runtime::default();
                clock.enter(0., 0.5);
                clock.clicked = Some(10.);
                for now in [0., 9., 9.4, 10.5, 60., 3600.] {
                    assert_eq!(clock.edge_frame(edge, now, motion, 0.5), "sidepeek_0");
                    assert!(clock.look_at.is_none());
                    if now > 0. || !motion {assert_eq!(clock.edge_depth(edge, now, motion), 1.);}
                }
                clock.notify(12.);
                assert_eq!(
                    clock.edge_depth(edge, 12., motion),
                    if motion { 1. } else { 1.4 }
                );
                assert!(
                    (clock.edge_depth(edge, 12.15, motion) - if motion { 1.2 } else { 1.4 }).abs()
                        < 0.001
                );
                assert_eq!(clock.edge_depth(edge, 12.3, motion), 1.4);
                assert_eq!(clock.edge_depth(edge, 14.99, motion), 1.4);
                assert_eq!(
                    clock.edge_depth(edge, 15., motion),
                    if motion { 1.4 } else { 1. }
                );
                assert_eq!(clock.edge_depth(edge, 15.35, motion), 1.);
                let r = rect(0, 0);
                let f = &clips::catalog().frames["sidepeek_0"];
                let wall = f.edge.unwrap();
                let (p, x, y) = pose(edge, r, 400., "notify_1", 0.56, 1.5, 1.4, 0.);
                let m = super::super::render::body_matrix(&p, x, y, 0.56);
                let end = Point::new((f.w as f32 * m.M11 + m.M31) * 1.5, y * 1.5);
                assert!(
                    (edge.distance(r, end) - (f.w as f32 - wall) * 0.56 * 1.5 * 1.4).abs() < 0.001
                );
            }
        }
    }

    #[test]
    fn side_home_ratio_is_foot_height_and_clamps_the_whole_body() {
        let r = rect(-1000, -200);
        for edge in [Edge::Left, Edge::Right] {
            for ratio in [0., 0.5, 1.] {
                let h = Home {
                    monitor: 1,
                    ratio,
                    edge: Some(edge),
                };
                let (_, _, along) = resolve(h, &[(1, r)], 130. * 0.56 * 1.5).unwrap();
                if ratio == 0.5 {
                    assert_eq!(along, 200.);
                }
                let (p, x, y) = pose(edge, r, along, "sidepeek_0", 0.56, 1.5, 1., 0.);
                let b = bounds(&p, x, y, 0.56, 1.5);
                assert!(b.top >= r.top && b.bottom <= r.bottom);
            }
        }
    }

    #[test]
    fn clipping_clears_outside_alpha_and_preserves_visible_hit_pixels() {
        let monitor = RECT {
            left: -2,
            top: -1,
            right: 2,
            bottom: 2,
        };
        for origin in [(-4, -3), (-3, -2), (-1, 0), (1, 1)] {
            let mut pixels = vec![128; 6 * 6 * 4];
            clip_pixels(&mut pixels, 6, origin, monitor);
            for (i, p) in pixels.as_chunks::<4>().0.iter().enumerate() {
                let (x, y) = (origin.0 + i as i32 % 6, origin.1 + i as i32 / 6);
                let inside = x >= monitor.left
                    && x < monitor.right
                    && y >= monitor.top
                    && y < monitor.bottom;
                assert_eq!(p, if inside { &[128; 4] } else { &[0; 4] });
            }
        }
    }
}
