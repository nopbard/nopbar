//! Motion, in one place (design system v2.1, section 7): three springs,
//! five fades, two curves.
//!
//! Opening: the surface grows from 0.96 to full size from the point it
//! opened at (a menu under a top bar item grows down from it, one over the
//! dock grows up from it) on GLIDE and fades in; its rim and its shadows
//! come up from nothing on the same GLIDE, so the plate fills first and the
//! light settles after it (7.2 rule 2). Closing: a quick fade and a small
//! step back to 0.98, light and plate together (rule 3). With reduced
//! motion (Windows' animation effects off, under any preset, or NopBar's
//! small animations off) only a short fade is left: nothing moves or
//! changes size (12.2). Every surface asks `now()` (or `full()`): one
//! answer for the dock, the bar, the panels, the island and Launchpad.
//!
//! Springs. Opening is solved here, once, into keyframes; Composition
//! plays them, so nothing runs per frame on the CPU. What has to keep its
//! speed when its target changes (rule 1) is a `SpringRun`: the closed
//! form knows where it is and how fast at any time, and `spring_scalar`
//! hands that to a Composition spring (spike S4: a spring started over a
//! running one keeps the value but starts at rest unless it is given the
//! velocity; spike S1: Composition's Period is 1 / w0, not a period). The
//! same `SpringRun` drives a frame loop where Composition cannot.
//!
//! Also here: `fade_to` (from wherever a visual is), `swap` (scrubbing
//! from one open surface to its neighbour), `press` and `press_scale`,
//! `origin` (where a surface grows from), `after` (a cancellable one-shot,
//! for hiding a surface once its fade is over) and `steps` (a fade the CPU
//! has to follow: DWM thumbnails, layered windows).

use windows::core::{Interface, HSTRING};
use windows::Foundation::TimeSpan;
use windows::UI::Composition::{CompositionGetValueStatus, Compositor, ContainerVisual, Visual};
use windows_numerics::{Vector2, Vector3};

/// m x'' + c x' + k (x - target) = 0, released at rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub mass: f32,
    pub stiffness: f32,
    pub damping: f32,
}

/// The springs (7.1), by what changes (7.4). SNAP: a small thing's state
/// (a switch's knob, a thumb, a press coming back, a knob's size). GLIDE:
/// a surface's place or size (opening, the rim's and shadows' strength,
/// the pill to card morph, a panel's height, pages). FOLLOW: what follows
/// the pointer (the dock's magnification, a drag). There are no others.
pub const SPRING_SNAP: Spring = Spring { mass: 1.0, stiffness: 600.0, damping: 48.0 };
pub const SPRING_GLIDE: Spring = Spring { mass: 1.0, stiffness: 400.0, damping: 38.0 };
#[allow(dead_code)]
pub const SPRING_FOLLOW: Spring = Spring { mass: 1.0, stiffness: 1600.0, damping: 80.0 };
/// The old names, for one release: A (560 / 44) and B (600 / 46) could
/// not be told apart and are SNAP; C was GLIDE already.
#[allow(dead_code)]
pub const SPRING_A: Spring = SPRING_SNAP;
#[allow(dead_code)]
pub const SPRING_B: Spring = SPRING_SNAP;
pub const SPRING_C: Spring = SPRING_GLIDE;
/// How long a spring played as keyframes may run (ms at speed 1; it ends
/// on its target).
#[allow(dead_code)]
pub const SNAP_MAX_MS: i64 = 220;
pub const GLIDE_MAX_MS: i64 = 280;

/// The fades (7.1). On: a hover coming on, a label or tooltip appearing, a
/// press coming back. Off: a hover going, closing, anything leaving.
/// Cross: content changing in place, a panel's opacity as it opens.
pub const FADE_IN_MS: i64 = 90;
pub const FADE_OUT_MS: i64 = 120;
pub const CROSS_MS: i64 = 180;
/// A sheet over the whole screen (Launchpad, the dim behind a folder):
/// linear, in and out.
#[allow(dead_code)]
pub const SCRIM_MS: i64 = 220;
#[allow(dead_code)]
pub const SCRIM_OUT_MS: i64 = CROSS_MS;
/// Light to dark, one finish to the other: the whole screen crosses,
/// linear.
#[allow(dead_code)]
pub const THEME_MS: i64 = 240;
/// A label or tooltip waits this long; within TIP_WARM_MS of one closing
/// the next shows at once.
#[allow(dead_code)]
pub const TIP_DELAY_MS: i64 = 300;
#[allow(dead_code)]
pub const TIP_WARM_MS: i64 = 500;
/// A submenu opens this long after its parent row is hovered (the
/// keyboard opens it at once).
#[allow(dead_code)]
pub const SUBMENU_DELAY_MS: i64 = CROSS_MS;
/// Scrubbing from an open surface to its neighbour: a cross this long at
/// full size (swap).
pub const SCRUB_MS: i64 = FADE_IN_MS;

/// Opening: the scale it starts from, how long the spring may run (ms at
/// speed 1; it ends on its target), and the fade for a menu and a panel.
pub const OPEN_SCALE: f32 = 0.96;
pub const OPEN_MS: i64 = GLIDE_MAX_MS;
pub const MENU_FADE_MS: i64 = FADE_OUT_MS;
pub const PANEL_FADE_MS: i64 = CROSS_MS;
/// Closing: a quick fade, and the scale it steps back to.
pub const CLOSE_MS: i64 = FADE_OUT_MS;
pub const CLOSE_SCALE: f32 = 0.98;
/// Reduced motion: a fade this long (ms), opening and closing alike. The
/// speed setting does not stretch it.
pub const REDUCED_MS: i64 = 60;
/// The old names of the fades, for one release.
pub const QUICK_IN_MS: i64 = FADE_IN_MS;
pub const QUICK_OUT_MS: i64 = FADE_OUT_MS;
#[allow(dead_code)]
pub const CROSSFADE_MS: i64 = CROSS_MS;
/// The dock's pressed icon (and its plate): a light dim, not a blackout
/// (visual audit 3.8).
pub const PRESS_DIM: f32 = 0.80;
/// A small surface under the pointer (the island's pill) grows this much.
pub const HOVER_SCALE: f32 = 1.02;
/// Keyframes the spring is sampled into.
pub const SAMPLES: usize = 10;
/// The standard curve (cubic-bezier(0.2, 0, 0, 1)) for fading in, and the
/// accelerating one (0.4, 0, 1, 1) for fading out. There are no others.
pub const EASE_OUT: ((f32, f32), (f32, f32)) = ((0.2, 0.0), (0.0, 1.0));
pub const EASE_IN: ((f32, f32), (f32, f32)) = ((0.4, 0.0), (1.0, 1.0));

/// popup::chrome_at's marks (a visual's Comment) on what carries a
/// surface's light: its rim and its shadows. `open` brings their opacity
/// (the light's strength) up apart from the plate's.
pub const TAG_RIM: &str = "nb-rim";
pub const TAG_SHADE: &str = "nb-shade";
/// `open`'s mark on what it opened: the scale it rests at (`close` steps
/// back from it).
const PROP_BASE: &str = "nbBase";

/// Pure: how much something `w` x `h` DIP shrinks while pressed (6.2): its
/// long side loses 2 DIP, a small thing stops at 0.94. One formula for
/// every screen (a 28 round button 0.94, a 28 x 80 button 0.975, a panel-
/// wide button 0.993, a card 0.986).
pub fn press_scale(w: f32, h: f32) -> f32 {
    let long = w.max(h);
    if !(w.is_finite() && h.is_finite() && long > 0.0) {
        return 1.0;
    }
    (1.0 - 2.0 / long).max(0.94)
}

impl Spring {
    /// ω0 = √(k / m), rad / s.
    pub fn w0(&self) -> f32 {
        (self.stiffness / self.mass).sqrt()
    }

    /// The undamped period 2π / ω0 in seconds: what the design's table
    /// calls the period (SwiftUI's `duration`).
    #[allow(dead_code)]
    pub fn period(&self) -> f32 {
        std::f32::consts::TAU / self.w0()
    }

    /// Composition's SpringNaturalMotionAnimation.Period for this spring,
    /// in seconds: 1 / ω0 (spike S1: neither the natural period nor the
    /// damped one; with either the spring ran 2π times too slow).
    pub fn composition_period(&self) -> f32 {
        1.0 / self.w0()
    }

    /// Where it is `t` seconds after being at `x0` with velocity `v0` on
    /// its way to `to`: (position, velocity). The closed form, so frame
    /// rate never matters.
    pub fn at(&self, x0: f32, v0: f32, to: f32, t: f32) -> (f32, f32) {
        if !(t.is_finite() && t > 0.0) {
            return (x0, v0);
        }
        let (w, z, y0) = (self.w0(), self.ratio(), x0 - to);
        let (y, v) = if z < 1.0 - 1e-4 {
            let wd = w * (1.0 - z * z).sqrt();
            let (a, b) = (y0, (v0 + z * w * y0) / wd);
            let (e, (sn, cs)) = ((-z * w * t).exp(), (wd * t).sin_cos());
            (e * (a * cs + b * sn), e * ((b * wd - z * w * a) * cs - (a * wd + z * w * b) * sn))
        } else if z <= 1.0 + 1e-4 {
            let (a, b) = (y0, v0 + w * y0);
            let e = (-w * t).exp();
            (e * (a + b * t), e * (b - w * (a + b * t)))
        } else {
            let sq = (z * z - 1.0).sqrt();
            let (r1, r2) = (-w * (z - sq), -w * (z + sq));
            let c1 = (v0 - r2 * y0) / (r1 - r2);
            let c2 = y0 - c1;
            let (e1, e2) = ((r1 * t).exp(), (r2 * t).exp());
            (c1 * e1 + c2 * e2, c1 * r1 * e1 + c2 * r2 * e2)
        };
        (to + y, v)
    }

    /// ζ = c / (2 √(k m)).impl Spring {
    /// ζ = c / (2 √(k m)).
    pub fn ratio(&self) -> f32 {
        self.damping / (2.0 * (self.stiffness * self.mass).sqrt())
    }

    /// How much of the way is still left `t` seconds after letting go from
    /// rest (1 at the start, 0 at the target): the closed-form solution, so
    /// frame rate never matters.
    pub fn left(&self, t: f32) -> f32 {
        if !(t.is_finite() && t > 0.0) {
            return 1.0;
        }
        let w0 = (self.stiffness / self.mass).sqrt();
        let z = self.ratio();
        let x = if z < 1.0 - 1e-4 {
            let wd = w0 * (1.0 - z * z).sqrt();
            (-z * w0 * t).exp() * ((wd * t).cos() + (z * w0 / wd) * (wd * t).sin())
        } else if z <= 1.0 + 1e-4 {
            (-w0 * t).exp() * (1.0 + w0 * t)
        } else {
            let s = (z * z - 1.0).sqrt();
            let (r1, r2) = (-w0 * (z - s), -w0 * (z + s));
            (r2 * (r1 * t).exp() - r1 * (r2 * t).exp()) / (r2 - r1)
        };
        x.clamp(-1.0, 1.0)
    }

    /// Keyframes for a run of `ms`: (progress 0..1, fraction of the way
    /// done), `n` + 1 of them, the last exactly on the target.
    pub fn keys(&self, ms: i64, n: usize) -> Vec<(f32, f32)> {
        let n = n.max(1);
        (0..=n)
            .map(|i| {
                let p = i as f32 / n as f32;
                let done = if i == n { 1.0 } else { 1.0 - self.left(p * ms as f32 / 1000.0) };
                (p, done)
            })
            .collect()
    }
}

/// How much motion there is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Grow and fade.
    Full,
    /// A short fade only.
    Reduced,
}

/// Full motion needs animations on (crate::effects::motion: Windows'
/// animation effects) and NopBar's small animations on.
pub fn mode(motion: bool, small: bool) -> Mode {
    if motion && small {
        Mode::Full
    } else {
        Mode::Reduced
    }
}

/// The mode now.
pub fn now() -> Mode {
    mode(crate::effects::motion(), crate::effects::items().small)
}

/// Full motion now.
pub fn full() -> bool {
    now() == Mode::Full
}

/// The mode for config `c` in `env`, for a process that
/// does not hold the dock's effects (the settings window): the same
/// resolve the dock does, so the same answer.
pub fn mode_for(c: &crate::config::Config, env: crate::effects::Env) -> Mode {
    let r = crate::effects::resolve(c, env);
    mode(r.motion, r.items.small)
}

/// A fade of `ms` (at speed 1) in `mode`: as long with full motion, at
/// most REDUCED_MS with reduced motion.
pub fn fade_ms(mode: Mode, ms: i64) -> i64 {
    match mode {
        Mode::Full => ms,
        Mode::Reduced => ms.min(REDUCED_MS),
    }
}

/// A fade of `ms` now: as designed with full motion, REDUCED_MS at most
/// with reduced motion. Screens ask this (or `open`, `close`, `swap`,
/// `press`) and never branch on `now()` themselves.
#[allow(dead_code)]
pub fn len(ms: i64) -> i64 {
    fade_ms(now(), ms)
}

/// Pure: how long a change the pointer made takes in `mode` (6.3): coming
/// on (a hover, a label) FADE_IN_MS, going FADE_OUT_MS; REDUCED_MS at most
/// with reduced motion.
pub fn pointer_ms(mode: Mode, on: bool) -> i64 {
    fade_ms(mode, if on { FADE_IN_MS } else { FADE_OUT_MS })
}

/// Pure: a label or tooltip that closed `since` ago leaves the next one
/// without its wait (6.3: within TIP_WARM_MS). None: none was shown yet.
pub fn tip_warm(since: Option<std::time::Duration>) -> bool {
    since.is_some_and(|d| d.as_millis() < TIP_WARM_MS as u128)
}

/// Pure: an implicit animation's keyframe `done` of the way from where the
/// property was to where it was just set, as Composition's expression.
pub fn glide_key(done: f32) -> String {
    if done == 1.0 {
        "this.FinalValue".to_string()
    } else {
        format!("this.StartingValue + (this.FinalValue - this.StartingValue) * {done:.4}")
    }
}

/// Makes `v`'s Offset glide: every change of it runs on GLIDE from where
/// it is (7.4: a thing's place changes), as keyframes Composition plays
/// (the value is carried over, not the speed). `on` false, or reduced
/// motion: its place changes at once (12.2). A visual has one collection of
/// implicit animations: this replaces it.
pub fn glide_offset(c: &Compositor, v: &impl Interface, on: bool) -> windows::core::Result<()> {
    use windows::UI::Composition::{CompositionObject, ICompositionAnimationBase};
    let coll = c.CreateImplicitAnimationCollection()?;
    if on && full() {
        let lin = c.CreateLinearEasingFunction()?;
        let a = c.CreateVector3KeyFrameAnimation()?;
        for (p, done) in SPRING_GLIDE.keys(GLIDE_MAX_MS, SAMPLES).into_iter().skip(1) {
            a.InsertExpressionKeyFrameWithEasingFunction(p, &HSTRING::from(glide_key(done)), &lin)?;
        }
        a.SetDuration(span(GLIDE_MAX_MS))?;
        let key = HSTRING::from("Offset");
        a.SetTarget(&key)?;
        coll.Insert(&key, &a.cast::<ICompositionAnimationBase>()?)?;
    }
    v.cast::<CompositionObject>()?.SetImplicitAnimations(&coll)
}

/// An easing curve's two control points (cubic-bezier).
pub type Curve = ((f32, f32), (f32, f32));
/// The straight line (reduced motion's fades, the theme's cross, a scrim).
pub const LINEAR: Curve = ((0.0, 0.0), (1.0, 1.0));

/// Pure: how a fade of `ms` (at speed 1) on `curve` finally plays in
/// `mode` at animation speed `speed`: (ms of real time, curve). Full: as
/// designed, stretched by the speed setting. Reduced (12.2): REDUCED_MS
/// at most, straight, and the speed setting does not stretch it. The one
/// place a fade's length and curve are decided: `fade_to`, `open`,
/// `close`, `swap`, `light`, the dock's implicit fades and its press
/// coming back all ask it.
pub fn fade_plan(mode: Mode, ms: i64, curve: Curve, speed: f32) -> (i64, Curve) {
    if ms <= 0 {
        return (0, curve);
    }
    match mode {
        Mode::Full => (crate::effects::ms_at(ms, speed), curve),
        Mode::Reduced => (ms.min(REDUCED_MS), fade_curve(mode, curve)),
    }
}

/// Pure: the curve a fade designed on `curve` plays on in `mode` (reduced
/// motion: straight).
pub fn fade_curve(mode: Mode, curve: Curve) -> Curve {
    match mode {
        Mode::Full => curve,
        Mode::Reduced => LINEAR,
    }
}

/// `fade_plan` now (the mode and the speed in force).
pub fn fade_now(ms: i64, curve: Curve) -> (i64, Curve) {
    fade_plan(now(), ms, curve, crate::effects::current().speed)
}

/// Pure: a cubic Bézier easing curve (control points `curve`, ends at 0 and
/// 1) at progress `t` (0..1): how far along the value is.
pub fn bezier(curve: ((f32, f32), (f32, f32)), t: f32) -> f32 {
    if !t.is_finite() || t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let ((x1, y1), (x2, y2)) = curve;
    let at = |a: f32, b: f32, u: f32| 3.0 * a * u * (1.0 - u) * (1.0 - u) + 3.0 * b * u * u * (1.0 - u) + u * u * u;
    // x(u) is monotone for x1, x2 in 0..1: bisect for x(u) = t.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if at(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(y1, y2, (lo + hi) / 2.0).clamp(0.0, 1.0)
}

/// What opens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Menu,
    Panel,
}

/// `anchor` (where a surface of `size` opened, in its own coordinates)
/// kept inside it: a menu that opened downward from a point above it grows
/// from its top edge, one that opened upward from its bottom edge,
/// sideways likewise.
pub fn kept_inside(anchor: (f32, f32), size: (f32, f32)) -> (f32, f32) {
    let c = |v: f32, max: f32| if v.is_finite() { v.clamp(0.0, max.max(0.0)) } else { max.max(0.0) / 2.0 };
    (c(anchor.0, size.0), c(anchor.1, size.1))
}

/// Pure: the point a surface grows from (7.4), in the surface's own
/// coordinates: the point of its edge nearest to the center of what opened
/// it. `surface` and `anchor` are (x, y, w, h) in the same coordinates (a
/// top bar item, a dock icon, a submenu's parent row, a dropdown's
/// button). Under a bar item that is the top edge at the item's center x;
/// over a dock icon the bottom edge; beside a parent row the near side at
/// the row's center y. A surface the screen's edge pushed aside still
/// grows from the nearest point of its edge. Without an anchor (search, a
/// panel opened by a shortcut): its center.
#[allow(dead_code)]
pub fn origin(surface: (f32, f32, f32, f32), anchor: Option<(f32, f32, f32, f32)>) -> (f32, f32) {
    let (sx, sy, sw, sh) = surface;
    let (sw, sh) = (sw.max(0.0), sh.max(0.0));
    let center = (sw / 2.0, sh / 2.0);
    let Some((ax, ay, aw, ah)) = anchor else { return center };
    let (cx, cy) = (ax + aw / 2.0 - sx, ay + ah / 2.0 - sy);
    if !(cx.is_finite() && cy.is_finite()) {
        return center;
    }
    if cx > 0.0 && cx < sw && cy > 0.0 && cy < sh {
        // The anchor's center is under the surface: the nearest edge.
        let d = [cy, sh - cy, cx, sw - cx];
        let k = (0..4).min_by(|&a, &b| d[a].total_cmp(&d[b])).unwrap_or(0);
        return match k {
            0 => (cx, 0.0),
            1 => (cx, sh),
            2 => (0.0, cy),
            _ => (sw, cy),
        };
    }
    (cx.clamp(0.0, sw), cy.clamp(0.0, sh))
}

/// Opening `ms` and the fade's length for `kind` in `mode` (at speed 1).
pub fn open_lengths(mode: Mode, kind: Kind) -> (i64, i64) {
    match (mode, kind) {
        (Mode::Full, Kind::Menu) => (OPEN_MS, MENU_FADE_MS),
        (Mode::Full, Kind::Panel) => (OPEN_MS, PANEL_FADE_MS),
        (Mode::Reduced, _) => (0, REDUCED_MS),
    }
}

/// Closing's fade (ms at speed 1).
pub fn close_ms(mode: Mode) -> i64 {
    match mode {
        Mode::Full => CLOSE_MS,
        Mode::Reduced => REDUCED_MS,
    }
}

/// `ms` (at speed 1) stretched by the speed setting: for full motion's
/// springs only (a fade's length comes from `fade_plan`).
fn span(ms: i64) -> TimeSpan {
    real(crate::effects::ms(ms))
}

/// `ms` of real time, as is.
fn real(ms: i64) -> TimeSpan {
    TimeSpan { Duration: ms * 10_000 }
}

/// Composition's easing for `curve` (LINEAR: its linear function).
pub fn easing(c: &Compositor, curve: Curve) -> windows::core::Result<windows::UI::Composition::CompositionEasingFunction> {
    if curve == LINEAR {
        return c.CreateLinearEasingFunction()?.cast();
    }
    c.CreateCubicBezierEasingFunction(Vector2 { X: curve.0 .0, Y: curve.0 .1 }, Vector2 { X: curve.1 .0, Y: curve.1 .1 })?.cast()
}

/// Where an animation of a property starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Start<T> {
    /// Where the property is shown now (a running animation's value: one
    /// under way turns back smoothly). No key frame at 0, and nothing is
    /// assigned first: an assigned value would be the start (an animation
    /// with no key frame at 0 starts from the property's value, and
    /// assigning it cuts a running animation short).
    Current,
    /// At this value (a key frame at 0).
    At(T),
}

/// How a property gets to a new value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Move<T> {
    /// Animated from `from` to `to`. The property is not assigned `to`
    /// first: an animation, once over, leaves it there.
    Animate { from: Start<T>, to: T },
    /// Assigned at once (no motion, or no time for it).
    Set(T),
}

/// Pure: how a property goes to `to` from `from` in `ms` (0 or less: at
/// once).
pub fn plan<T>(from: Start<T>, to: T, ms: i64) -> Move<T> {
    if ms > 0 {
        Move::Animate { from, to }
    } else {
        Move::Set(to)
    }
}

/// A property Composition animates (one visual's Opacity, the island's
/// Scale): what `apply` drives.
pub trait Prop {
    type V: Copy;
    /// Assigns `v` at once (what animates it stops).
    fn set(&self, v: Self::V) -> windows::core::Result<()>;
    /// Starts an animation from `from` to `to`. Never assigns the
    /// property first (Start::Current).
    fn start(&self, from: Start<Self::V>, to: Self::V) -> windows::core::Result<()>;
}

/// Plays `m` on `p`: an animation only starts (the target is never
/// assigned before it); without one, or if it could not start, `p` is
/// assigned its target (and the error comes back). True if it animates.
pub fn apply<P: Prop>(p: &P, m: Move<P::V>) -> windows::core::Result<bool> {
    match m {
        Move::Animate { from, to } => match p.start(from, to) {
            Ok(()) => Ok(true),
            Err(e) => {
                let _ = p.set(to);
                Err(e)
            }
        },
        Move::Set(to) => p.set(to).map(|_| false),
    }
}

/// A visual's Opacity, faded over `ms` of real time on `curve` (what
/// `fade_plan` decided).
struct Opacity<'a> {
    c: &'a Compositor,
    v: &'a Visual,
    ms: i64,
    curve: Curve,
}

impl Prop for Opacity<'_> {
    type V = f32;

    fn set(&self, v: f32) -> windows::core::Result<()> {
        let _ = self.v.StopAnimation(&HSTRING::from("Opacity"));
        self.v.SetOpacity(v)
    }

    fn start(&self, from: Start<f32>, to: f32) -> windows::core::Result<()> {
        let ease = easing(self.c, self.curve)?;
        let a = self.c.CreateScalarKeyFrameAnimation()?;
        if let Start::At(x) = from {
            a.InsertKeyFrame(0.0, x)?;
        }
        a.InsertKeyFrameWithEasingFunction(1.0, to, &ease)?;
        a.SetDuration(real(self.ms))?;
        self.v.StartAnimation(&HSTRING::from("Opacity"), &a)
    }
}

/// Fades `v` from `from` to `to` over `ms` (at speed 1) on `curve` (see
/// `apply`), as `fade_now` plays it (reduced motion: REDUCED_MS at most,
/// straight, whatever the speed). Returns how long it takes (ms of real
/// time).
fn fade(c: &Compositor, v: &Visual, from: Start<f32>, to: f32, ms: i64, curve: Curve) -> windows::core::Result<i64> {
    let (ms, curve) = fade_now(ms, curve);
    apply(&Opacity { c, v, ms, curve }, plan(from, to, ms))?;
    Ok(ms)
}

/// The visuals under `v` that carry its light (popup::chrome_at marks its
/// rim and its shadows): `v`'s children and theirs.
fn lights(v: &Visual) -> Vec<Visual> {
    fn tagged(x: &Visual) -> bool {
        x.Comment().is_ok_and(|c| c == TAG_RIM || c == TAG_SHADE)
    }
    fn kids(x: &Visual) -> Vec<Visual> {
        let Ok(c) = x.cast::<ContainerVisual>() else { return Vec::new() };
        let Ok(list) = c.Children() else { return Vec::new() };
        list.into_iter().collect()
    }
    let mut out = Vec::new();
    for k in kids(v) {
        if tagged(&k) {
            out.push(k);
            continue;
        }
        out.extend(kids(&k).into_iter().filter(tagged));
    }
    out
}

/// Opens `v` (`size` in its own units, before its scale; `anchor` where
/// it opened, in the same units: `origin`'s point): it grows from there on
/// GLIDE and fades in, and its light (the rim and the shadows
/// popup::chrome_at built under it) comes up from nothing on the same
/// GLIDE. `base` is the scale it rests at (a visual scaled for the
/// display's DPI keeps that). With reduced motion nothing grows: plate and
/// light fade in together. Implicit animations on its Opacity or Scale
/// must not be attached.
pub fn open(c: &Compositor, v: &impl Interface, size: (f32, f32), anchor: (f32, f32), kind: Kind, base: f32) -> windows::core::Result<()> {
    let v: Visual = v.cast()?;
    let mode = now();
    let (grow, fade_ms) = open_lengths(mode, kind);
    let lit = lights(&v);
    if grow > 0 {
        let (ox, oy) = kept_inside(anchor, size);
        // Scaling about a center point C moves the content by C * (1 - S):
        // with a DPI scale S at rest that shift would stay after the
        // animation (drawn and hit-tested places would disagree). The
        // offset takes it back, so at rest the visual sits exactly where
        // it did with the center at 0.
        let off = v.Offset()?;
        let (dx, dy) = rest_shift((ox, oy), base);
        v.SetOffset(Vector3 { X: off.X - dx, Y: off.Y - dy, Z: off.Z })?;
        v.SetCenterPoint(Vector3 { X: ox, Y: oy, Z: 0.0 })?;
        let lin = c.CreateLinearEasingFunction()?;
        let a = c.CreateVector3KeyFrameAnimation()?;
        let keys = SPRING_GLIDE.keys(grow, SAMPLES);
        for &(p, done) in &keys {
            let s = base * (OPEN_SCALE + (1.0 - OPEN_SCALE) * done);
            a.InsertKeyFrameWithEasingFunction(p, Vector3 { X: s, Y: s, Z: 1.0 }, &lin)?;
        }
        a.SetDuration(span(grow))?;
        v.SetScale(Vector3 { X: base, Y: base, Z: 1.0 })?;
        v.StartAnimation(&HSTRING::from("Scale"), &a)?;
        // What `close` steps back from.
        if let Ok(props) = v.Properties() {
            let _ = props.InsertScalar(&HSTRING::from(PROP_BASE), base);
        }
        // The light: 0 to 1 on the same keys as the size.
        if !lit.is_empty() {
            let strength = c.CreateScalarKeyFrameAnimation()?;
            for &(p, done) in &keys {
                strength.InsertKeyFrameWithEasingFunction(p, done.clamp(0.0, 1.0), &lin)?;
            }
            strength.SetDuration(span(grow))?;
            for l in &lit {
                let _ = l.StartAnimation(&HSTRING::from("Opacity"), &strength);
            }
        }
    } else {
        for l in &lit {
            let _ = l.StopAnimation(&HSTRING::from("Opacity"));
            let _ = l.SetOpacity(1.0);
        }
    }
    fade(c, &v, Start::At(0.0), 1.0, fade_ms, EASE_OUT).map(|_| ())
}

/// A surface's light (its rim, its shadows: `v` is one of them) coming
/// up or going down by itself, for a surface that is not opened by `open`
/// (the dock back from auto-hide: 7.2 rules 2 and 3). Up: from where it is
/// to full on GLIDE (the keys `open` gives the light of what it opens).
/// Down: to nothing over CLOSE_MS on the ease-in. With reduced motion a
/// REDUCED_MS fade either way. Returns how long it takes (ms, at the
/// chosen speed).
pub fn light(c: &Compositor, v: &impl Interface, on: bool) -> windows::core::Result<i64> {
    let v: Visual = v.cast()?;
    if now() == Mode::Reduced {
        // (fade: REDUCED_MS, straight, the speed left out.)
        return fade(c, &v, Start::Current, if on { 1.0 } else { 0.0 }, REDUCED_MS, LINEAR);
    }
    if !on {
        return fade(c, &v, Start::Current, 0.0, CLOSE_MS, EASE_IN);
    }
    let lin = c.CreateLinearEasingFunction()?;
    let a = c.CreateScalarKeyFrameAnimation()?;
    for (p, done) in SPRING_GLIDE.keys(OPEN_MS, SAMPLES).into_iter().skip(1) {
        a.InsertExpressionKeyFrameWithEasingFunction(p, &HSTRING::from(light_key(done)), &lin)?;
    }
    a.SetDuration(span(OPEN_MS))?;
    v.StartAnimation(&HSTRING::from("Opacity"), &a)?;
    Ok(crate::effects::ms(OPEN_MS))
}

/// Pure: `light`'s keyframe `done` of the way from the strength it has to
/// full (never past it), as Composition's expression.
pub fn light_key(done: f32) -> String {
    format!("this.StartingValue + (1 - this.StartingValue) * {:.4}", done.clamp(0.0, 1.0))
}

/// Scrubbing (7.4): a surface is open and its neighbour takes over (the
/// next top bar menu, another submenu). `old` fades out and `new` in over
/// SCRUB_MS, at full size: nothing grows from 0.96, and the light comes
/// with the plate. Returns how long it takes (ms of real time).
#[allow(dead_code)]
pub fn swap(c: &Compositor, old: Option<&Visual>, new: &impl Interface) -> windows::core::Result<i64> {
    let new: Visual = new.cast()?;
    let ms = len(SCRUB_MS);
    if let Some(o) = old {
        let _ = fade(c, o, Start::Current, 0.0, ms, EASE_IN);
    }
    for l in lights(&new) {
        let _ = l.StopAnimation(&HSTRING::from("Opacity"));
        let _ = l.SetOpacity(1.0);
    }
    fade(c, &new, Start::At(0.0), 1.0, ms, EASE_OUT)
}

/// The speed setting as a stretch of time (1 at speed 1, 2 at half speed).
fn stretch() -> f32 {
    crate::effects::ms(1000) as f32 / 1000.0
}

/// A spring on its way: where it started (`from`, with `velocity`), where
/// it goes and since when. Pure: `state` is the closed form, so the value
/// and the speed are known at any time without a frame loop. Retargeted
/// with `toward`, it leaves from where it is at the speed it has (7.2
/// rule 1: nothing starts over).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringRun {
    pub spring: Spring,
    pub from: f32,
    /// Units per second (of real time).
    pub velocity: f32,
    pub to: f32,
    pub since: std::time::Instant,
    /// Time's stretch when it started (the speed setting).
    pub stretch: f32,
}

#[allow(dead_code)]
impl SpringRun {
    /// At rest at `at`.
    pub fn still(spring: Spring, at: f32, now: std::time::Instant) -> SpringRun {
        SpringRun { spring, from: at, velocity: 0.0, to: at, since: now, stretch: 1.0 }
    }

    /// (value, velocity per second) at `now`.
    pub fn state(&self, now: std::time::Instant) -> (f32, f32) {
        let k = if self.stretch > 0.0 { self.stretch } else { 1.0 };
        let t = now.saturating_duration_since(self.since).as_secs_f32() / k;
        let (x, v) = self.spring.at(self.from, self.velocity * k, self.to, t);
        (x, v / k)
    }

    /// The same spring now heading to `to`: from where it is at `now`, at
    /// the speed it has (time stretched by `stretch`).
    pub fn toward(&self, to: f32, now: std::time::Instant, stretch: f32) -> SpringRun {
        let (x, v) = self.state(now);
        SpringRun { spring: self.spring, from: x, velocity: v, to, since: now, stretch }
    }

    /// Within `eps` of its target and as good as still.
    pub fn settled(&self, now: std::time::Instant, eps: f32) -> bool {
        let (x, v) = self.state(now);
        (x - self.to).abs() <= eps && v.abs() <= eps * self.spring.w0()
    }
}

/// Sends scalar property `prop` of `v` ("Opacity", "Offset.X",
/// "Scale.X") to `to` on `run`'s spring, from wherever it is at the speed
/// it has; returns the run to keep for the next change. Composition plays
/// it (a spring animation given the velocity: spike S4). With reduced
/// motion the property is set at once.
#[allow(dead_code)]
pub fn spring_scalar(c: &Compositor, v: &impl Interface, prop: &str, run: &SpringRun, to: f32) -> windows::core::Result<SpringRun> {
    spring_prop(c, v, prop, run, to)
}

/// `spring_scalar` for any composition object: a visual, a geometry, or a
/// property set's own scalar (one progress that expressions on many
/// properties follow: the island's pill to card morph).
pub fn spring_prop(c: &Compositor, obj: &impl Interface, prop: &str, run: &SpringRun, to: f32) -> windows::core::Result<SpringRun> {
    use windows::Foundation::{IReference, PropertyValue};
    let v: windows::UI::Composition::CompositionObject = obj.cast()?;
    let now_t = std::time::Instant::now();
    if !full() {
        let _ = v.StopAnimation(&HSTRING::from(prop));
        let props = c.CreatePropertySet()?;
        props.InsertScalar(&HSTRING::from("v"), to)?;
        let e = c.CreateExpressionAnimationWithExpression(&HSTRING::from("p.v"))?;
        e.SetReferenceParameter(&HSTRING::from("p"), &props)?;
        // (An expression of a constant: the one way to assign a scalar
        // sub-property by name.)
        v.StartAnimation(&HSTRING::from(prop), &e)?;
        return Ok(SpringRun::still(run.spring, to, now_t));
    }
    let k = stretch();
    let next = run.toward(to, now_t, k);
    let a = c.CreateSpringScalarAnimation()?;
    a.SetPeriod(TimeSpan { Duration: (next.spring.composition_period() * k * 1e7) as i64 })?;
    a.SetDampingRatio(next.spring.ratio())?;
    let target: IReference<f32> = PropertyValue::CreateSingle(to)?.cast()?;
    a.SetFinalValue(&target)?;
    // The retarget lands with the next commit: the speed it will have
    // then (about a frame on).
    let ahead = run.state(now_t + std::time::Duration::from_millis(16)).1;
    a.SetInitialVelocity(ahead)?;
    v.StartAnimation(&HSTRING::from(prop), &a)?;
    Ok(next)
}

/// Pressing something raised or capsule-shaped, `size` DIP (6.2): its
/// fill changes at once (the caller's brush); its size goes to
/// press_scale on SNAP, and back when let go. With reduced motion the size
/// does not change.
#[allow(dead_code)]
pub fn press(c: &Compositor, v: &impl Interface, size: (f32, f32), down: bool) -> windows::core::Result<()> {
    scale_to(c, v, size, if down { press_scale(size.0, size.1) } else { 1.0 })
}

/// `v` (`size` DIP) to `to` of its size about its middle, on SNAP from
/// the size it has (6.3: a press, a slider's knob held at 1.12). With
/// reduced motion its size does not change (12.2).
pub fn scale_to(c: &Compositor, v: &impl Interface, size: (f32, f32), to: f32) -> windows::core::Result<()> {
    use windows::Foundation::{IReference, PropertyValue};
    let v: Visual = v.cast()?;
    v.SetCenterPoint(Vector3 { X: size.0 / 2.0, Y: size.1 / 2.0, Z: 0.0 })?;
    if !full() {
        let _ = v.StopAnimation(&HSTRING::from("Scale"));
        return v.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 });
    }
    let a = c.CreateSpringScalarAnimation()?;
    a.SetPeriod(TimeSpan { Duration: (SPRING_SNAP.composition_period() * stretch() * 1e7) as i64 })?;
    a.SetDampingRatio(SPRING_SNAP.ratio())?;
    let target: IReference<f32> = PropertyValue::CreateSingle(to)?.cast()?;
    a.SetFinalValue(&target)?;
    v.StartAnimation(&HSTRING::from("Scale.X"), &a)?;
    v.StartAnimation(&HSTRING::from("Scale.Y"), &a)
}

/// What scaling by `scale` about `center` adds to every point, beyond
/// scaling about 0 (Composition: Offset + C + S * (x - C)).
fn rest_shift(center: (f32, f32), scale: f32) -> (f32, f32) {
    (center.0 * (1.0 - scale), center.1 * (1.0 - scale))
}

/// Fades `v` to `to` from wherever it is now (a fade under way turns
/// back smoothly) over `ms` (at speed 1) on `curve`. Returns how long it
/// takes (ms of real time).
pub fn fade_to(c: &Compositor, v: &impl Interface, to: f32, ms: i64, curve: Curve) -> windows::core::Result<i64> {
    fade_from(c, v, Start::Current, to, ms, curve)
}

/// Fades `v` from `from` to `to` over `ms` (at speed 1) on `curve`; as
/// `fade_to`, with an explicit start when the caller knows it (a layer
/// that comes in from nothing).
pub fn fade_from(c: &Compositor, v: &impl Interface, from: Start<f32>, to: f32, ms: i64, curve: Curve) -> windows::core::Result<i64> {
    let v: Visual = v.cast()?;
    fade(c, &v, from, to, ms, curve)
}

/// Pure: closing's fade, from `from` (where the surface is shown now, or
/// a level the caller knows) to nothing over `ms`.
pub fn closing(from: Start<f32>, ms: i64) -> Move<f32> {
    plan(from, 0.0, ms)
}

/// Fades `v` out from where it is now (one still fading in fades out from
/// there, not from full). Returns how long it takes (ms of real time):
/// keep what draws it alive that long (see `retire`).
pub fn close(c: &Compositor, v: &impl Interface) -> windows::core::Result<i64> {
    close_from(c, v, Start::Current)
}

/// Fades `v` out from `from` (see `close`). What `open` opened also steps
/// back to CLOSE_SCALE on the same curve, from the size it has now (its
/// light goes with it: the rim and the shadows are its children). With
/// reduced motion its size stays.
pub fn close_from(c: &Compositor, v: &impl Interface, from: Start<f32>) -> windows::core::Result<i64> {
    let v: Visual = v.cast()?;
    let mode = now();
    let (ms, curve) = fade_plan(mode, close_ms(mode), EASE_IN, crate::effects::current().speed);
    if mode == Mode::Full {
        if let Some(base) = opened_base(&v) {
            let _ = step_back(c, &v, base, ms);
        }
    }
    apply(&Opacity { c, v: &v, ms, curve }, closing(from, ms))?;
    Ok(ms)
}

/// The scale `open` left on `v` (None: it was not opened by `open`).
fn opened_base(v: &Visual) -> Option<f32> {
    let mut base = 0.0f32;
    let status = v.Properties().ok()?.TryGetScalar(&HSTRING::from(PROP_BASE), &mut base).ok()?;
    (status == CompositionGetValueStatus::Succeeded && base > 0.0).then_some(base)
}

/// `v`'s Scale to CLOSE_SCALE of `base` over `ms` of real time, from
/// where it is (no key frame at 0: one still opening turns back from its
/// size now).
fn step_back(c: &Compositor, v: &Visual, base: f32, ms: i64) -> windows::core::Result<()> {
    let ease = easing(c, EASE_IN)?;
    let a = c.CreateVector3KeyFrameAnimation()?;
    let s = base * CLOSE_SCALE;
    a.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: s, Y: s, Z: 1.0 }, &ease)?;
    a.SetDuration(real(ms))?;
    v.StartAnimation(&HSTRING::from("Scale"), &a)
}

struct Retired {
    hwnd: isize,
    timer: usize,
    _keep: Box<dyn std::any::Any>,
    /// Run once the window is gone (never if the thread's loop ends first).
    then: Option<Box<dyn FnOnce()>>,
}

thread_local! {
    static RETIRED: std::cell::RefCell<Vec<Retired>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A window that is fading out on its way to being destroyed. `retire`
/// already makes it click-through for every process; its WM_NCHITTEST
/// answers HTTRANSPARENT too (for this thread's windows under it).
pub fn retired(hwnd: windows::Win32::Foundation::HWND) -> bool {
    RETIRED.with(|r| r.borrow().iter().any(|x| x.hwnd == hwnd.0 as isize))
}

/// The windows fading out now (retired, not gone yet), oldest first: for
/// a window that takes the foreground back under them (Launchpad after its
/// menu) to keep them above itself while they fade.
pub fn retired_windows() -> Vec<windows::Win32::Foundation::HWND> {
    RETIRED.with(|r| r.borrow().iter().map(|x| windows::Win32::Foundation::HWND(x.hwnd as *mut _)).collect())
}

/// Closes window `hwnd` whose content is `root`: fades it out, then
/// destroys it, keeping `keep` (its composition target and visuals) alive
/// until then. While it fades every click goes through it, to whatever
/// window of any app is under it (click_through). Without a fade (it
/// failed) or without click-through it goes at once.
pub fn retire(hwnd: windows::Win32::Foundation::HWND, c: &Compositor, root: &impl Interface, keep: Box<dyn std::any::Any>) {
    retire_from(hwnd, c, root, Start::Current, keep);
}

/// `retire`, its fade starting at `from` (a surface whose opacity the CPU
/// drives: the level it has now, so what fades beside it, a DWM
/// thumbnail, starts at the same one).
pub fn retire_from(hwnd: windows::Win32::Foundation::HWND, c: &Compositor, root: &impl Interface, from: Start<f32>, keep: Box<dyn std::any::Any>) {
    if !unsafe { click_through(hwnd) } {
        end(hwnd, None);
        return;
    }
    let Ok(ms) = close_from(c, root, from) else {
        end(hwnd, None);
        return;
    };
    hold(hwnd, (ms + 16) as u32, keep, None);
}

/// Closes window `hwnd` that is already fading out with its own animation
/// (Launchpad's): like `retire`, click-through for every process while it
/// fades, then destroyed, keeping `keep` alive until then. It goes when
/// its fade says it is over (`faded`, from the animation's completion), or
/// after `within` ms at the latest. `then` runs once it is gone (at once if
/// it could not be made click-through: it goes at once too).
pub fn retire_faded(hwnd: windows::Win32::Foundation::HWND, within: u32, keep: Box<dyn std::any::Any>, then: Option<Box<dyn FnOnce()>>) {
    if !unsafe { click_through(hwnd) } {
        end(hwnd, then);
        return;
    }
    hold(hwnd, within, keep, then);
}

/// The fade of retired window `hwnd` is over before its timer: it goes now
/// (nothing if it is not retired, or gone already).
pub fn faded(hwnd: windows::Win32::Foundation::HWND) {
    let id = hwnd.0 as isize;
    finish(|x| x.hwnd == id);
}

/// Keeps retired `hwnd` (and `keep`) until a thread timer of `ms` (this
/// thread's message loop calls `gone`) or `faded`. Without a timer it goes
/// at once.
fn hold(hwnd: windows::Win32::Foundation::HWND, ms: u32, keep: Box<dyn std::any::Any>, then: Option<Box<dyn FnOnce()>>) {
    use windows::Win32::UI::WindowsAndMessaging::SetTimer;
    let timer = unsafe { SetTimer(None, 0, ms, Some(gone)) };
    if timer == 0 {
        drop(keep);
        end(hwnd, then);
        return;
    }
    RETIRED.with(|r| r.borrow_mut().push(Retired { hwnd: hwnd.0 as isize, timer, _keep: keep, then }));
}

/// Destroys `hwnd`, then runs `then`.
fn end(hwnd: windows::Win32::Foundation::HWND, then: Option<Box<dyn FnOnce()>>) {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
    }
    if let Some(f) = then {
        f();
    }
}

/// Makes `hwnd` transparent to the mouse for every process, not only for
/// this thread (HTTRANSPARENT from WM_NCHITTEST only passes a click to
/// windows of the same thread). The system skips a window that is both
/// WS_EX_LAYERED and WS_EX_TRANSPARENT when it looks for the window under
/// the pointer. A layered window needs its attributes set before it shows
/// again: full alpha (LWA_ALPHA 255) leaves the picture as it was. These
/// windows have no redirection bitmap (WS_EX_NOREDIRECTIONBITMAP): their
/// picture is the composition target's visuals, which layering at full
/// alpha does not touch (the shadow, genie and split overlays and the
/// Launchpad are made with the same styles and draw this way), and the
/// fade keeps playing on the visuals. An empty window region would not do:
/// it clips the composition content too, so the fade would not be seen.
/// False if it could not be done (the caller then destroys the window).
pub unsafe fn click_through(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, LWA_ALPHA, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, WS_EX_LAYERED, WS_EX_TRANSPARENT,
    };
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let want = ex | (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as isize;
    if want != ex {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
    }
    if SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA).is_err() {
        return false;
    }
    // The new extended style takes effect now (not at the next move).
    let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
    true
}

/// Undoes `click_through`: `hwnd` takes the mouse again (the island's card,
/// opened again while it was going back into its pill). It stays layered
/// at full alpha: its picture is the composition target's either way.
pub unsafe fn click_solid(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, WS_EX_TRANSPARENT,
    };
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let want = ex & !(WS_EX_TRANSPARENT.0 as isize);
    if want != ex {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
    }
}

unsafe extern "system" fn gone(_: windows::Win32::Foundation::HWND, _: u32, id: usize, _: u32) {
    // (Stopped even if nothing is left to finish with it.)
    let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(None, id);
    finish(|x| x.timer == id);
}

/// The retired windows `which` picks go: out of the list first (their
/// window procedures no longer see them as retired), timer stopped, window
/// destroyed, what was kept dropped, then their `then`.
fn finish(which: impl Fn(&Retired) -> bool) {
    use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, KillTimer};
    let done: Vec<Retired> = RETIRED.with(|r| {
        let mut r = r.borrow_mut();
        let (done, keep): (Vec<Retired>, Vec<Retired>) = r.drain(..).partition(|x| which(x));
        *r = keep;
        done
    });
    for mut x in done {
        unsafe {
            let _ = KillTimer(None, x.timer);
            let _ = DestroyWindow(windows::Win32::Foundation::HWND(x.hwnd as *mut _));
        }
        let then = x.then.take();
        drop(x);
        if let Some(f) = then {
            f();
        }
    }
}

// ---------------------------------------------------------------- later

/// What `after` runs, by its timer.
type Later = Vec<(usize, Box<dyn FnOnce()>)>;

thread_local! {
    static LATER: std::cell::RefCell<Later> = const { std::cell::RefCell::new(Vec::new()) };
    static STEPS: std::cell::RefCell<Vec<Steps>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Runs `f` on this thread in `ms` (its message loop runs it), unless
/// `cancel` stops it first: a surface hidden once its fade is over, and
/// shown again meanwhile. Returns the id to cancel it with; None if no
/// timer could be had (`f` then ran at once).
pub fn after(ms: u32, f: Box<dyn FnOnce()>) -> Option<usize> {
    use windows::Win32::UI::WindowsAndMessaging::SetTimer;
    let id = unsafe { SetTimer(None, 0, ms.max(1), Some(later)) };
    if id == 0 {
        f();
        return None;
    }
    LATER.with(|l| l.borrow_mut().push((id, f)));
    Some(id)
}

/// Stops what `after` would run (true if it had not run yet).
pub fn cancel(id: usize) -> bool {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(None, id);
    }
    LATER.with(|l| {
        let mut l = l.borrow_mut();
        let before = l.len();
        l.retain(|(i, _)| *i != id);
        l.len() != before
    })
}

unsafe extern "system" fn later(_: windows::Win32::Foundation::HWND, _: u32, id: usize, _: u32) {
    let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(None, id);
    // (Out of the list before it runs: it may call `after` or `cancel`.)
    let f = LATER.with(|l| {
        let mut l = l.borrow_mut();
        l.iter().position(|(i, _)| *i == id).map(|k| l.remove(k).1)
    });
    if let Some(f) = f {
        f();
    }
}

/// A fade the CPU follows (see `steps`).
struct Steps {
    timer: usize,
    start: std::time::Instant,
    ms: i64,
    /// The compositor's curve for the fade it follows (`fade_curve`).
    curve: Curve,
    step: Option<Box<dyn FnMut(f32)>>,
    then: Option<Box<dyn FnOnce()>>,
}

/// Pure: how much of a fade-out `ms` long is left `elapsed` ms in (1 at
/// the start, 0 at the end), on `curve` (the kit's exit curve, EASE_IN;
/// straight with reduced motion: fade_curve), as the compositor plays it.
pub fn out_level(elapsed: f32, ms: i64, curve: Curve) -> f32 {
    if ms <= 0 {
        return 0.0;
    }
    1.0 - bezier(curve, elapsed / ms as f32)
}

/// A fade-out of `ms` (of real time: what `close` returns) for what
/// Composition cannot fade (a DWM thumbnail, a layered window's alpha):
/// `step` gets the level (1 to 0, out_level on the fade's curve now) about
/// every frame, 0 last; then `then` runs. Without a timer both run at once.
pub fn steps(ms: i64, mut step: Box<dyn FnMut(f32)>, then: Box<dyn FnOnce()>) {
    use windows::Win32::UI::WindowsAndMessaging::SetTimer;
    let timer = if ms > 0 { unsafe { SetTimer(None, 0, 15, Some(step_tick)) } } else { 0 };
    if timer == 0 {
        step(0.0);
        then();
        return;
    }
    step(1.0);
    let curve = fade_curve(now(), EASE_IN);
    STEPS.with(|s| s.borrow_mut().push(Steps { timer, start: std::time::Instant::now(), ms, curve, step: Some(step), then: Some(then) }));
}

unsafe extern "system" fn step_tick(_: windows::Win32::Foundation::HWND, _: u32, id: usize, _: u32) {
    step_now(id, std::time::Instant::now());
}

/// One frame of the stepped fade on timer `id` at `now`; at its end it
/// stops, steps to 0 and runs its `then`.
fn step_now(id: usize, now: std::time::Instant) {
    // Taken out while it runs (it may start or end other fades).
    let taken = STEPS.with(|s| {
        let mut s = s.borrow_mut();
        let k = s.iter().position(|x| x.timer == id)?;
        let x = &mut s[k];
        let lvl = out_level((now - x.start).as_secs_f32() * 1000.0, x.ms, x.curve);
        let done = lvl <= 0.0;
        let step = x.step.take();
        let then = if done { s.remove(k).then } else { None };
        Some((step, lvl, done, then))
    });
    let Some((step, lvl, done, then)) = taken else {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(None, id);
        }
        return;
    };
    if done {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(None, id);
        }
    }
    if let Some(mut step) = step {
        step(lvl.max(0.0));
        if !done {
            STEPS.with(|s| {
                if let Some(x) = s.borrow_mut().iter_mut().find(|x| x.timer == id) {
                    x.step = Some(step);
                }
            });
        }
    }
    if let Some(f) = then {
        f();
    }
}

/// A property as Composition keeps it, for tests of the order `apply`
/// does things in: assigning a value shows it at once (and cuts a running
/// animation short); an animation with no start of its own starts from
/// the value shown.
#[cfg(test)]
pub(crate) mod fake {
    use super::{Prop, Start};
    use std::cell::{Cell, RefCell};

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Call<T> {
        Set(T),
        /// An animation began: from (the value it really starts at), to.
        Start(T, T),
    }

    pub struct Fake<T: Copy> {
        pub shown: Cell<T>,
        pub calls: RefCell<Vec<Call<T>>>,
        pub fail: bool,
    }

    impl<T: Copy> Fake<T> {
        pub fn at(v: T) -> Self {
            Fake { shown: Cell::new(v), calls: RefCell::new(Vec::new()), fail: false }
        }

        pub fn calls(&self) -> Vec<Call<T>> {
            self.calls.borrow().clone()
        }

        /// What it starts at, if an animation began.
        pub fn started_at(&self) -> Option<T> {
            self.calls.borrow().iter().find_map(|c| if let Call::Start(f, _) = c { Some(*f) } else { None })
        }
    }

    impl<T: Copy> Prop for Fake<T> {
        type V = T;

        fn set(&self, v: T) -> windows::core::Result<()> {
            self.shown.set(v);
            self.calls.borrow_mut().push(Call::Set(v));
            Ok(())
        }

        fn start(&self, from: Start<T>, to: T) -> windows::core::Result<()> {
            if self.fail {
                return Err(windows::Win32::Foundation::E_FAIL.into());
            }
            let f = match from {
                Start::Current => self.shown.get(),
                Start::At(x) => x,
            };
            self.calls.borrow_mut().push(Call::Start(f, to));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Call, Fake};
    use super::*;

    /// 12.2: with reduced motion every fade is REDUCED_MS straight, and the
    /// speed setting does not stretch it; with full motion each is as
    /// designed, at the speed, on its curve. The final length and curve,
    /// at speeds 0.25, 1 and 4.
    #[test]
    fn reduced_fades_are_60ms_straight_at_any_speed() {
        let fades = [(FADE_IN_MS, EASE_OUT), (FADE_OUT_MS, EASE_IN), (CROSS_MS, EASE_OUT), (CLOSE_MS, EASE_IN), (SCRUB_MS, EASE_OUT), (THEME_MS, LINEAR), (REDUCED_MS, LINEAR), (OPEN_MS, EASE_OUT)];
        for speed in [0.25f32, 1.0, 4.0] {
            for (ms, curve) in fades {
                assert_eq!(fade_plan(Mode::Reduced, ms, curve, speed), (REDUCED_MS, LINEAR), "{ms} at {speed}");
                assert_eq!(fade_plan(Mode::Full, ms, curve, speed), (crate::effects::ms_at(ms, speed), curve), "{ms} at {speed}");
            }
            // Opening and closing (open, close, light) through it.
            for kind in [Kind::Menu, Kind::Panel] {
                assert_eq!(fade_plan(Mode::Reduced, open_lengths(Mode::Reduced, kind).1, EASE_OUT, speed), (REDUCED_MS, LINEAR));
            }
            assert_eq!(fade_plan(Mode::Reduced, close_ms(Mode::Reduced), EASE_IN, speed), (REDUCED_MS, LINEAR));
            // A shorter one keeps its length; none is none.
            assert_eq!(fade_plan(Mode::Reduced, 40, EASE_OUT, speed), (40, LINEAR));
            assert_eq!(fade_plan(Mode::Reduced, 0, EASE_OUT, speed).0, 0);
        }
        // The numbers, at full motion: a quarter of the speed is four times
        // as long, four times the speed a quarter.
        assert_eq!(fade_plan(Mode::Full, FADE_OUT_MS, EASE_IN, 0.25), (480, EASE_IN));
        assert_eq!(fade_plan(Mode::Full, FADE_OUT_MS, EASE_IN, 4.0), (30, EASE_IN));
        // Straight is straight (the dim coming back, a stepped close).
        for t in [0.1f32, 0.25, 0.5, 0.9] {
            assert!((bezier(LINEAR, t) - t).abs() < 1e-4, "{t}");
        }
        assert!((out_level(30.0, REDUCED_MS, fade_curve(Mode::Reduced, EASE_IN)) - 0.5).abs() < 1e-4);
        assert_eq!(fade_curve(Mode::Full, EASE_IN), EASE_IN);
    }

    #[test]
    fn an_animated_fade_never_assigns_its_target_first() {
        // From where it is: no key frame at 0, nothing assigned. (The old
        // fade_to assigned the target first: it started there, no fade.)
        let p = Fake::at(0.0f32);
        assert_eq!(plan(Start::Current, 1.0, CROSSFADE_MS), Move::Animate { from: Start::Current, to: 1.0 });
        assert!(apply(&p, plan(Start::Current, 1.0, CROSSFADE_MS)).unwrap());
        assert_eq!(p.calls(), vec![Call::Start(0.0, 1.0)]);
        // An explicit start: that one, whatever is shown.
        let p = Fake::at(1.0f32);
        assert!(apply(&p, plan(Start::At(0.0), 1.0, MENU_FADE_MS)).unwrap());
        assert_eq!(p.calls(), vec![Call::Start(0.0, 1.0)]);
        // Fading out from rest: from full.
        let p = Fake::at(1.0f32);
        apply(&p, plan(Start::Current, 0.0, QUICK_OUT_MS)).unwrap();
        assert_eq!(p.calls(), vec![Call::Start(1.0, 0.0)]);
    }

    #[test]
    fn a_fade_turned_back_starts_where_it_is() {
        // In, cut short at 0.3 by going out: out from 0.3; and back in
        // from 0.55 of the way out.
        let p = Fake::at(0.3f32);
        apply(&p, plan(Start::Current, 0.0, QUICK_OUT_MS)).unwrap();
        assert_eq!(p.started_at(), Some(0.3));
        let p = Fake::at(0.55f32);
        apply(&p, plan(Start::Current, 1.0, QUICK_IN_MS)).unwrap();
        assert_eq!(p.started_at(), Some(0.55));
    }

    #[test]
    fn closing_goes_from_where_the_surface_is() {
        // A close while it still fades in (0.4): from 0.4, not from full.
        assert_eq!(closing(Start::Current, CLOSE_MS), Move::Animate { from: Start::Current, to: 0.0 });
        let p = Fake::at(0.4f32);
        apply(&p, closing(Start::Current, close_ms(Mode::Full))).unwrap();
        assert_eq!(p.calls(), vec![Call::Start(0.4, 0.0)]);
        // A level the caller knows (the preview's open animation, which
        // its thumbnails follow too): from that one.
        let p = Fake::at(0.25f32);
        apply(&p, closing(Start::At(0.6), close_ms(Mode::Reduced))).unwrap();
        assert_eq!(p.calls(), vec![Call::Start(0.6, 0.0)]);
        // The DWM thumbnails' steps from that level match the fade: the
        // composition's from * (1 - ease) is out_level * from.
        let (from, ms) = (0.6f32, 120);
        for t in [0.0f32, 30.0, 60.0, 120.0] {
            let comp = from + (0.0 - from) * bezier(EASE_IN, t / ms as f32);
            assert!((comp - out_level(t, ms, EASE_IN) * from).abs() < 1e-5);
        }
    }

    #[test]
    fn no_time_or_a_failed_start_assigns_the_target() {
        assert_eq!(plan(Start::Current, 1.0f32, 0), Move::Set(1.0));
        let p = Fake::at(0.0f32);
        assert!(!apply(&p, plan(Start::Current, 1.0, 0)).unwrap());
        assert_eq!(p.calls(), vec![Call::Set(1.0)]);
        // The animation could not start: it ends where it was going.
        let mut p = Fake::at(0.2f32);
        p.fail = true;
        assert!(apply(&p, plan(Start::Current, 0.0, CLOSE_MS)).is_err());
        assert_eq!(p.calls(), vec![Call::Set(0.0)]);
        assert_eq!(p.shown.get(), 0.0);
    }

    /// A hidden top-level window (never shown) of the system's static
    /// class, the kind of window a popup or the Launchpad is.
    unsafe fn hidden_window() -> windows::Win32::Foundation::HWND {
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, WS_EX_TOOLWINDOW, WS_POPUP};
        CreateWindowExW(WS_EX_TOOLWINDOW, windows::core::w!("STATIC"), windows::core::w!(""), WS_POPUP, 0, 0, 10, 10, None, None, None, None).unwrap()
    }

    #[test]
    fn a_faded_window_lets_clicks_through_until_it_goes() {
        use std::{cell::Cell, rc::Rc};
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, IsWindow, IsWindowVisible, GWL_EXSTYLE, WS_EX_LAYERED, WS_EX_TRANSPARENT};
        unsafe {
            let hwnd = hidden_window();
            let ran = Rc::new(Cell::new(0));
            let r = ran.clone();
            retire_faded(hwnd, 60_000, Box::new(()), Some(Box::new(move || r.set(r.get() + 1))));
            // Fading: layered and transparent (clicks go to whatever is
            // under it, in any app), still there, `then` not run yet.
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            assert_eq!(ex & (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0), WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0);
            assert!(retired(hwnd) && IsWindow(Some(hwnd)).as_bool() && !IsWindowVisible(hwnd).as_bool());
            assert_eq!(ran.get(), 0);
            // Its fade says it is over: gone, and `then` runs once.
            faded(hwnd);
            assert!(!retired(hwnd) && !IsWindow(Some(hwnd)).as_bool());
            assert_eq!(ran.get(), 1);
            faded(hwnd);
            assert_eq!(ran.get(), 1, "only once");
        }
    }

    #[test]
    fn a_window_that_cannot_let_clicks_through_goes_at_once() {
        use std::{cell::Cell, rc::Rc};
        // (No window: making it click-through fails.)
        let none = windows::Win32::Foundation::HWND::default();
        let ran = Rc::new(Cell::new(false));
        let r = ran.clone();
        retire_faded(none, 60_000, Box::new(()), Some(Box::new(move || r.set(true))));
        assert!(ran.get(), "what waits for it runs now");
        assert!(!retired(none), "nothing is kept");
    }

    #[test]
    fn a_dpi_scaled_visual_rests_where_it_was() {
        // A point x of a visual at offset O, scale S about center C, with
        // the offset corrected by rest_shift, lands where scaling about 0
        // put it (O + S * x).
        for (c, s) in [((120.0, 8.0), 1.5), ((0.0, 0.0), 2.0), ((300.0, 40.0), 1.0), ((64.0, 512.0), 1.25)] {
            let (o, x) = ((24.0f32, 24.0f32), (37.0f32, 91.0f32));
            let (dx, dy) = rest_shift(c, s);
            let got = (o.0 - dx + c.0 + s * (x.0 - c.0), o.1 - dy + c.1 + s * (x.1 - c.1));
            let want = (o.0 + s * x.0, o.1 + s * x.1);
            assert!((got.0 - want.0).abs() < 1e-3 && (got.1 - want.1).abs() < 1e-3, "{c:?} {s}");
        }
    }

    #[test]
    fn the_springs_are_the_three() {
        // Stiffness / damping, ratio, period (7.1's table).
        for (s, k, d, ratio, period_ms) in [(SPRING_SNAP, 600.0, 48.0, 0.98, 257.0), (SPRING_GLIDE, 400.0, 38.0, 0.95, 314.0), (SPRING_FOLLOW, 1600.0, 80.0, 1.0, 157.0)] {
            assert_eq!((s.mass, s.stiffness, s.damping), (1.0, k, d));
            assert!((s.ratio() - ratio).abs() < 0.001, "{s:?}");
            assert!((s.period() * 1000.0 - period_ms).abs() < 0.6, "{s:?}: {}", s.period() * 1000.0);
            // Composition's Period is 1 / w0 (spike S1), not the period.
            assert!((s.composition_period() * std::f32::consts::TAU - s.period()).abs() < 1e-6);
        }
        assert!((SPRING_GLIDE.composition_period() - 0.050).abs() < 1e-6);
        assert!((SPRING_SNAP.composition_period() - 0.040_825).abs() < 1e-5);
        assert!((SPRING_FOLLOW.composition_period() - 0.025).abs() < 1e-6);
        // The old names for one release.
        assert_eq!((SPRING_A, SPRING_B, SPRING_C), (SPRING_SNAP, SPRING_SNAP, SPRING_GLIDE));
    }

    #[test]
    fn the_fades_are_the_five() {
        assert_eq!((FADE_IN_MS, FADE_OUT_MS, CROSS_MS, SCRIM_MS, THEME_MS), (90, 120, 180, 220, 240));
        assert_eq!((SCRIM_OUT_MS, SUBMENU_DELAY_MS, SCRUB_MS), (180, 180, 90));
        assert_eq!((TIP_DELAY_MS, TIP_WARM_MS, REDUCED_MS), (300, 500, 60));
        assert_eq!((SNAP_MAX_MS, GLIDE_MAX_MS, OPEN_MS), (220, 280, 280));
        // Closing is quicker than opening, and steps back a little.
        assert!(CLOSE_MS * 2 < OPEN_MS && CLOSE_SCALE > OPEN_SCALE && CLOSE_SCALE < 1.0);
        assert_eq!((QUICK_IN_MS, QUICK_OUT_MS, CROSSFADE_MS, MENU_FADE_MS, PANEL_FADE_MS), (90, 120, 180, 120, 180));
    }

    #[test]
    fn the_pointers_changes_take_90_in_and_120_out() {
        // On / off with full motion; one short fade either way reduced.
        assert_eq!((pointer_ms(Mode::Full, true), pointer_ms(Mode::Full, false)), (FADE_IN_MS, FADE_OUT_MS));
        assert_eq!((pointer_ms(Mode::Reduced, true), pointer_ms(Mode::Reduced, false)), (REDUCED_MS, REDUCED_MS));
        // A tip that closed within TIP_WARM_MS leaves the next without its wait.
        use std::time::Duration;
        assert!(!tip_warm(None), "none shown yet: the wait");
        assert!(tip_warm(Some(Duration::ZERO)) && tip_warm(Some(Duration::from_millis(499))));
        assert!(!tip_warm(Some(Duration::from_millis(500))) && !tip_warm(Some(Duration::from_secs(60))));
        // A glide's keyframes go from where the property was to where it
        // was set, ending exactly there.
        assert_eq!(glide_key(1.0), "this.FinalValue");
        assert_eq!(glide_key(0.5), "this.StartingValue + (this.FinalValue - this.StartingValue) * 0.5000");
        let keys = SPRING_GLIDE.keys(GLIDE_MAX_MS, SAMPLES);
        assert_eq!(keys.last().copied(), Some((1.0, 1.0)));
    }

    #[test]
    fn a_press_takes_two_dip_off_the_long_side() {
        // A 28 round button, a 28 x 80 button, a panel-wide button, a card.
        let near = |a: f32, b: f32| (a - b).abs() < 0.0006;
        assert!(near(press_scale(28.0, 28.0), 0.94));
        assert!(near(press_scale(80.0, 28.0), 0.975));
        assert!(near(press_scale(304.0, 32.0), 0.993));
        assert!(near(press_scale(148.0, 92.0), 0.986));
        // The long side loses 2 DIP; small things stop at 0.94.
        assert!((304.0 * (1.0 - press_scale(304.0, 32.0)) - 2.0).abs() < 1e-3);
        assert_eq!(press_scale(12.0, 12.0), 0.94);
        assert_eq!((press_scale(0.0, 0.0), press_scale(f32::NAN, 4.0)), (1.0, 1.0));
    }

    #[test]
    fn a_surface_grows_from_the_edge_nearest_its_anchor() {
        let surface = (100.0, 40.0, 200.0, 300.0);
        // Under a top bar item: the top edge at the item's center x.
        assert_eq!(origin(surface, Some((130.0, 4.0, 60.0, 28.0))), (60.0, 0.0));
        // Over a dock icon: the bottom edge at the icon's center x.
        assert_eq!(origin(surface, Some((180.0, 360.0, 52.0, 52.0))), (106.0, 300.0));
        // Beside a parent row: the near side at the row's center y.
        assert_eq!(origin(surface, Some((40.0, 100.0, 56.0, 28.0))), (0.0, 74.0));
        assert_eq!(origin(surface, Some((304.0, 100.0, 56.0, 28.0))), (200.0, 74.0));
        // Pushed aside by the screen's edge (its anchor's center is past
        // its side): still the nearest point of its edge.
        assert_eq!(origin(surface, Some((330.0, 4.0, 60.0, 28.0))), (200.0, 0.0));
        // Pushed over its anchor: the nearest edge.
        assert_eq!(origin(surface, Some((150.0, 50.0, 20.0, 20.0))), (60.0, 0.0));
        // No anchor (search, a shortcut): its center.
        assert_eq!(origin(surface, None), (100.0, 150.0));
        assert_eq!(origin(surface, Some((f32::NAN, 0.0, 1.0, 1.0))), (100.0, 150.0));
    }

    #[test]
    fn a_spring_knows_where_it_is_and_how_fast() {
        // From rest it is `left`.
        for s in [SPRING_SNAP, SPRING_GLIDE, SPRING_FOLLOW, Spring { mass: 1.0, stiffness: 400.0, damping: 60.0 }, Spring { mass: 1.0, stiffness: 300.0, damping: 12.0 }] {
            for i in 0..40 {
                let t = i as f32 * 0.01;
                let (x, _) = s.at(0.0, 0.0, 1.0, t);
                assert!((1.0 - x - s.left(t)).abs() < 2e-4, "{s:?} at {t}: {x} vs {}", 1.0 - s.left(t));
            }
            // Its velocity is its position's slope.
            for t in [0.02f32, 0.07, 0.15] {
                let d = 0.0005;
                let slope = (s.at(0.2, 3.0, 1.0, t + d).0 - s.at(0.2, 3.0, 1.0, t - d).0) / (2.0 * d);
                let v = s.at(0.2, 3.0, 1.0, t).1;
                assert!((slope - v).abs() < 0.02 * (1.0 + v.abs()), "{s:?} at {t}: {slope} vs {v}");
            }
            // It starts where and as fast as it was told.
            assert_eq!(s.at(0.2, 3.0, 1.0, 0.0), (0.2, 3.0));
            let (x, v) = s.at(0.2, 3.0, 1.0, 3.0);
            assert!((x - 1.0).abs() < 1e-3 && v.abs() < 1e-2, "settles: {x} {v}");
        }
    }

    #[test]
    fn a_retargeted_spring_keeps_its_speed() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let run = SpringRun::still(SPRING_GLIDE, 0.0, t0).toward(1.0, t0, 1.0);
        assert_eq!(run.state(t0), (0.0, 0.0));
        // 70 ms in it is on its way, and fast.
        let t1 = t0 + Duration::from_millis(70);
        let (x1, v1) = run.state(t1);
        assert!(x1 > 0.2 && x1 < 0.6 && v1 > 3.0, "{x1} {v1}");
        // Turned back: from there, at that speed (it still rises a
        // moment before it turns: nothing starts over).
        let back = run.toward(0.0, t1, 1.0);
        assert_eq!(back.state(t1), (x1, v1));
        let (x2, _) = back.state(t1 + Duration::from_millis(20));
        assert!(x2 > x1, "still rising: {x2} vs {x1}");
        let (x3, v3) = back.state(t1 + Duration::from_millis(1500));
        assert!(x3.abs() < 1e-3 && v3.abs() < 1e-2);
        assert!(back.settled(t1 + Duration::from_millis(1500), 0.001) && !back.settled(t1, 0.001));
        // At half speed everything takes twice as long.
        let slow = SpringRun::still(SPRING_GLIDE, 0.0, t0).toward(1.0, t0, 2.0);
        let (xs, vs) = slow.state(t0 + Duration::from_millis(140));
        assert!((xs - x1).abs() < 1e-4 && (vs - v1 / 2.0).abs() < 1e-3);
    }

    #[test]
    fn spring_settles_without_bouncing_past() {
        let s = SPRING_C;
        assert_eq!(s.left(0.0), 1.0);
        // Monotone enough: never more than a hair past the target.
        let mut last = 1.0;
        for i in 1..=56 {
            let x = s.left(i as f32 * 0.005);
            assert!(x > -0.002, "overshoot at {i}: {x}");
            assert!(x <= last + 1e-4, "comes back at {i}");
            last = x;
        }
        // Done within its 280 ms: under the kit's 0.001 scale error (of 0.04).
        assert!(s.left(0.28).abs() * (1.0 - OPEN_SCALE) < 0.001);
        // Critical and over-damped springs are solved too.
        let crit = Spring { mass: 1.0, stiffness: 400.0, damping: 40.0 };
        let over = Spring { mass: 1.0, stiffness: 400.0, damping: 60.0 };
        assert!(crit.left(0.1) > 0.0 && crit.left(0.1) < 1.0 && over.left(0.1) > crit.left(0.1));
    }

    #[test]
    fn keys_start_at_rest_and_end_on_target() {
        let k = SPRING_C.keys(OPEN_MS, SAMPLES);
        assert_eq!(k.len(), SAMPLES + 1);
        assert_eq!(k[0], (0.0, 0.0));
        assert_eq!(*k.last().unwrap(), (1.0, 1.0));
        assert!(k.windows(2).all(|w| w[1].0 > w[0].0 && w[1].1 >= w[0].1 - 1e-4));
        // Half the way is done by 30 % of the time (a spring, not a linear
        // ramp's 30 %).
        assert!(k[3].1 > 0.45, "{:?}", k[3]);
    }

    #[test]
    fn growth_comes_from_where_it_opened() {
        // Down from a point above (top bar): the top edge.
        assert_eq!(kept_inside((40.0, -6.0), (200.0, 300.0)), (40.0, 0.0));
        // Up from a point below (the dock): the bottom edge.
        assert_eq!(kept_inside((120.0, 330.0), (200.0, 300.0)), (120.0, 300.0));
        // Right to left, past the far side: kept inside.
        assert_eq!(kept_inside((260.0, 0.0), (200.0, 300.0)), (200.0, 0.0));
        assert_eq!(kept_inside((f32::NAN, 10.0), (200.0, 300.0)), (100.0, 10.0));
    }

    /// Runs this thread's messages (timers) until `done` or `ms` pass.
    fn pump_until(ms: u64, done: &dyn Fn() -> bool) {
        use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, MSG, PM_REMOVE};
        let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while !done() && std::time::Instant::now() < end {
            let mut m = MSG::default();
            unsafe {
                while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&m);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn later_runs_once_unless_cancelled() {
        use std::{cell::Cell, rc::Rc};
        let ran = Rc::new(Cell::new(0));
        let r = ran.clone();
        let id = after(1, Box::new(move || r.set(r.get() + 1))).unwrap();
        pump_until(2000, &|| ran.get() > 0);
        assert_eq!(ran.get(), 1);
        assert!(!cancel(id), "already ran");
        // Shown again before its fade was over: the hide never comes.
        let r = ran.clone();
        let id = after(30, Box::new(move || r.set(r.get() + 10))).unwrap();
        assert!(cancel(id));
        pump_until(120, &|| false);
        assert_eq!(ran.get(), 1);
    }

    #[test]
    fn a_stepped_fade_ends_at_zero_then_lets_go() {
        use std::{cell::RefCell, rc::Rc};
        let seen = Rc::new(RefCell::new(Vec::new()));
        let ended = Rc::new(RefCell::new(false));
        let (s, e) = (seen.clone(), ended.clone());
        steps(40, Box::new(move |l| s.borrow_mut().push(l)), Box::new(move || *e.borrow_mut() = true));
        pump_until(2000, &|| *ended.borrow());
        let seen = seen.borrow();
        assert!(*ended.borrow(), "it ends");
        assert_eq!(seen.first(), Some(&1.0), "starts full");
        assert_eq!(seen.last(), Some(&0.0), "ends gone");
        assert!(seen.windows(2).all(|w| w[1] <= w[0]), "only fades: {seen:?}");
        assert!(STEPS.with(|x| x.borrow().is_empty()), "nothing kept");
        // No length: done at once.
        let done = Rc::new(RefCell::new(0));
        let d = done.clone();
        steps(0, Box::new(|_| {}), Box::new(move || *d.borrow_mut() += 1));
        assert_eq!(*done.borrow(), 1);
    }

    #[test]
    fn curves_run_from_0_to_1_and_the_exit_one_starts_slow() {
        for c in [EASE_OUT, EASE_IN] {
            assert_eq!(bezier(c, 0.0), 0.0);
            assert_eq!(bezier(c, 1.0), 1.0);
            let mut last = 0.0;
            for i in 1..=20 {
                let v = bezier(c, i as f32 / 20.0);
                assert!(v >= last - 1e-4, "monotone");
                last = v;
            }
        }
        assert!(bezier(EASE_OUT, 0.3) > 0.6, "ease-out arrives early");
        assert!(bezier(EASE_IN, 0.3) < 0.2, "ease-in leaves late");
        assert_eq!(out_level(0.0, 120, EASE_IN), 1.0);
        assert_eq!(out_level(120.0, 120, EASE_IN), 0.0);
        assert_eq!(out_level(5.0, 0, EASE_IN), 0.0);
    }

    #[test]
    fn reduced_motion_is_a_short_fade() {
        assert_eq!(mode(true, true), Mode::Full);
        assert_eq!(mode(false, true), Mode::Reduced);
        assert_eq!(mode(true, false), Mode::Reduced);
        assert_eq!(open_lengths(Mode::Reduced, Kind::Panel), (0, REDUCED_MS));
        assert!(REDUCED_MS <= 100 && close_ms(Mode::Reduced) <= 100);
        assert_eq!(open_lengths(Mode::Full, Kind::Menu), (OPEN_MS, MENU_FADE_MS));
        assert_eq!(close_ms(Mode::Full), CLOSE_MS);
        assert_eq!(fade_ms(Mode::Full, CROSSFADE_MS), CROSSFADE_MS);
        assert_eq!(fade_ms(Mode::Reduced, CROSSFADE_MS), REDUCED_MS);
        assert_eq!(fade_ms(Mode::Reduced, 40), 40);
        // Reduced: nothing grows or steps back (no size key frames at
        // all), whatever opens.
        for kind in [Kind::Menu, Kind::Panel] {
            assert_eq!(open_lengths(Mode::Reduced, kind).0, 0, "{kind:?}");
        }
    }

    #[test]
    fn windows_animations_off_reduce_every_preset() {
        use crate::effects::Env;
        let on = Env { animations: true, high_contrast: false };
        let off = Env { animations: false, high_contrast: false };
        for preset in ["ultra", "high", "medium", "custom"] {
            let mut c = crate::config::Config::default();
            crate::effects::pick_preset(&mut c, preset);
            assert_eq!(mode_for(&c, off), Mode::Reduced, "{preset}: Windows off always wins");
        }
        // NopBar's small animations off: reduced too, with Windows on.
        let mut c = crate::config::Config::default();
        crate::effects::pick_preset(&mut c, "custom");
        c.effects.small_animations = false;
        assert_eq!(mode_for(&c, on), Mode::Reduced);
        c.effects.small_animations = true;
        assert_eq!(mode_for(&c, on), Mode::Full);
        assert_eq!(mode_for(&crate::config::Config::default(), on), Mode::Full);
        // Low-spec mode: reduced, whatever the switches underneath say.
        c.effects.low_spec = true;
        assert_eq!(mode_for(&c, on), Mode::Reduced);
    }
}
