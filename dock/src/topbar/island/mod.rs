//! The Live Island: a small pill in the middle of the primary top bar
//! showing what is happening right now (a copy or download, what is
//! playing, the charger, the camera or microphone in use), which opens into
//! a card on hover or click (card.rs). The pill and the card are one
//! thing: the card grows out of the pill in place and goes back into it
//! (morph.rs; design system v2.1, 7.2 rule 4).
//!
//! - model.rs: what is live, its order, the pill's words and place, the
//!   hidden -> pill -> expanded states (pure, tested);
//! - morph.rs: the one plate that is the pill at one end and the card at
//!   the other, and the shadow patch and contact ring both wear;
//! - privacy.rs: camera / microphone / screen capture from Windows' privacy
//!   records (a `service` registry watch, no polling);
//! - media: control.rs's media session listener (MediaWatch), run only
//!   while the island shows what is playing;
//! - progress: the dock's taskbar progress per app (set_jobs, from
//!   dock/badges.rs), with the dock's own icon brushes;
//! - battery: the bar's WM_POWERBROADCAST reading (a short notice on
//!   plug-in or when the battery falls under a warning level);
//! - the focus timer (crate::focus_timer): its phase and countdown, read
//!   from its state on every refresh. Its ring is a compositor animation
//!   (the remaining arc's TrimEnd runs to 0 over the time left), started
//!   once per deadline; only the seconds' text is drawn again, once a
//!   second while it shows (TICK_TIMER, aligned to the countdown).
//!
//! Idle cost: nothing runs while nothing is live. The hover, leave and
//! notice timers are one-shot and exist only around those moments; the
//! music bars are a compositor animation that runs only while music plays
//! and the pill is on screen. Motion goes through crate::motion (its
//! springs and fades; reduced with "effects.small_animations" off or
//! Windows' animations off): the pill's size under the pointer and its
//! press ride SNAP, its coming, going and width GLIDE; its fill comes on in
//! FADE_IN_MS and goes in FADE_OUT_MS, a press at once. High contrast draws
//! it in the system colors.

pub mod model;
pub mod privacy;
mod card;
mod morph;
mod pause;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;

use windows::core::{Interface, Result, HSTRING};
use windows::Foundation::TimeSpan;
use windows::UI::Composition::{
    AnimationIterationBehavior, CompositionBrush, CompositionColorBrush, CompositionColorGradientStop, CompositionEllipseGeometry, CompositionGeometricClip,
    CompositionLinearGradientBrush, CompositionMappingMode, CompositionObject, CompositionRoundedRectangleGeometry, CompositionSpriteShape, CompositionStretch, CompositionStrokeCap, Compositor, ContainerVisual, ICompositionAnimationBase, ShapeVisual,
    SpriteVisual,
};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D_SIZE_F};
use windows::Win32::Graphics::Direct2D::{D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_STROKE_STYLE_PROPERTIES1, D2D1_SWEEP_DIRECTION_CLOCKWISE};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, KillTimer, PostMessageW, SetTimer, WM_APP};
use windows_numerics::{Vector2, Vector3};

use super::control::{Art, MediaWatch};
use super::TopBar;
use crate::glyphs::Glyph;
use crate::theme::Rgba;
use model::{Activity, Content, Enabled, Event, Inputs, Job, Kind, Phase, Power, Shot, Timer, Tone};

/// Posted to the primary bar by the media listener (what is playing changed).
pub(super) const WM_ISLAND_MEDIA: u32 = WM_APP + 48;
/// Posted to the primary bar: a card action or a feed change (wparam: ACT_*).
pub(super) const WM_ISLAND_ACT: u32 = WM_APP + 49;
pub(super) use privacy::WM_ISLAND_PRIVACY;

pub(super) const ACT_REFRESH: usize = 0;
pub(super) const ACT_DISMISS: usize = 1;
pub(super) const ACT_LEAVE: usize = 2;
pub(super) const ACT_PREV: usize = 3;
pub(super) const ACT_TOGGLE: usize = 4;
pub(super) const ACT_NEXT: usize = 5;
/// The card's position bar: seek (lparam: model::pack_seek, the track's
/// generation and the position in ms).
pub(super) const ACT_SEEK: usize = 6;
/// The card's picture-in-picture button: what plays in a PiP (crate::pip).
pub(super) const ACT_PIP: usize = 7;
/// A PiP's play / pause: only for session lparam (Track::session).
pub(super) const ACT_PIP_TOGGLE: usize = 8;
/// A click on the card itself (not on one of its buttons) while it is out
/// by hovering: it stays, as a click on the pill made it stay (the card
/// stands on the pill now).
pub(super) const ACT_PIN: usize = 9;
/// The screenshot's buttons: ACT_SHOT + 1 save, + 2 copy, + 4 edit
/// (crate::shot::act).
pub(super) const ACT_SHOT: usize = 40;
/// The focus timer's buttons: ACT_TIMER + 1 pause / resume, + 2 skip,
/// + 3 "+5 min", + 4 stop.
pub(super) const ACT_TIMER: usize = 50;

/// One-shot bar timers.
pub(super) const HOVER_TIMER: usize = 21;
pub(super) const LEAVE_TIMER: usize = 22;
pub(super) const NOTICE_TIMER: usize = 23;
/// The focus timer's seconds (one-shot, at the next whole second).
pub(super) const TICK_TIMER: usize = 24;
/// One shot only while a paused media session is in its five-second grace.
pub(super) const PAUSE_TIMER: usize = 26;
/// The pointer may cross the gap between pill and card this long.
const LEAVE_MS: u32 = 160;

/// Inside the pill (DIP; the padding and gaps are model::PAD and GAP): a
/// glyph's box, album art and thumbnails (their corners concentric with
/// the capsule: lead_radius), the title (12 semibold: the compact
/// bar's pill is 20 tall, a 13 title filled it; the mock-up's 11.5) and
/// smaller words (Footnote; numbers tabular).
const LEAD: f32 = 16.0;
const ART: f32 = 20.0;
const TEXT_SIZE: f32 = crate::theme::ramp::FOOTNOTE.size;
const SMALL_SIZE: f32 = crate::theme::ramp::FOOTNOTE.size;
/// The music bars: count, width, gap, height.
const EQ_BARS: usize = 4;
const EQ_W: f32 = 2.0;
const EQ_GAP: f32 = 2.0;
const EQ_H: f32 = 12.0;
/// The pill under the pointer: a little larger (and its fill lighter), on
/// SNAP (crate::motion: a small thing's state; 7.2 rule 5: the pill is one
/// of the two things that change size under the pointer).
const HOVER_SCALE: f32 = crate::motion::HOVER_SCALE;
/// The pill coming and going: from (and to) the kit's opening scale, on
/// GLIDE, fading over the panels' fade.
const HIDDEN_SCALE: f32 = crate::motion::OPEN_SCALE;
/// Shared lengths (crate::motion, ms at speed 1): a move or a size on
/// GLIDE, a content cross-fade, the pill's fade.
const MOVE_MS: i64 = crate::motion::OPEN_MS;
const SWAP_MS: i64 = crate::motion::CROSS_MS;
const FADE_MS: i64 = crate::motion::CROSS_MS;
const RING: f32 = 12.0;
/// Between the timer's ring and its time.
const RING_GAP: f32 = crate::theme::metrics::space::S2;
/// The ring's stroke.
const RING_STROKE: f32 = 2.0;
/// The ring is started again when it is this far from the time left.
const RING_DRIFT_MS: i64 = 1500;

/// What the ring's animation was started for: a new deadline ("+5 min",
/// a resume, a new phase), a new whole, pausing or the motion setting
/// starts it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RingKey {
    deadline: i64,
    total: i64,
    running: bool,
    animated: bool,
}

/// Lead glyphs (crate::glyphs).
const G_MUSIC: Glyph = Glyph::Music;
const G_BOLT: Glyph = Glyph::Bolt;
const G_BATTERY_LOW: Glyph = Glyph::Battery(0);
const G_TIMER: Glyph = Glyph::Timer;
const G_SHOT: Glyph = Glyph::Camera;
const G_DOWNLOAD: Glyph = Glyph::Download;
const G_COPY: Glyph = Glyph::Copy;

/// The primary bar, for feeds that come from elsewhere (the dock).
static BAR: AtomicIsize = AtomicIsize::new(0);
/// The display the island shows on with "Only on the display I'm using"
/// (the monitor of the window in front, if a bar is there; 0 = the main
/// display's bar).
static ISLAND_MON: AtomicIsize = AtomicIsize::new(0);

/// Which display's island shows, with "Same on every display" on: `active`
/// is the monitor of the window in front (None: not known, or the desktop
/// on no monitor), `bars` each bar's (monitor, is the main display). The
/// main display's when no bar is on the active one.
pub fn island_monitor(active: Option<isize>, bars: &[(isize, bool)]) -> Option<isize> {
    active.filter(|a| bars.iter().any(|b| b.0 == *a)).or_else(|| bars.iter().find(|b| b.1).map(|b| b.0)).or_else(|| bars.first().map(|b| b.0))
}

/// Whether the bar on monitor `bar_mon` (`primary`: the main display's)
/// shows the island now. Without "Same on every display" only the main
/// bar has one; with it, every bar (`every`) or only the bar on
/// `island_mon` (island_monitor).
pub fn island_on(everywhere: bool, every: bool, primary: bool, bar_mon: isize, island_mon: Option<isize>) -> bool {
    if !everywhere {
        return primary;
    }
    every || island_mon.map_or(primary, |m| m == bar_mon)
}

/// The window in front moved to another display (or another one came to
/// the front): the island's display is now `m`. True when that changed
/// where the island shows.
pub(super) fn set_island_monitor(m: Option<isize>) -> bool {
    let v = m.unwrap_or(0);
    ISLAND_MON.swap(v, Ordering::SeqCst) != v
}

thread_local! {
    /// What the main bar's island found last: an island made later on
    /// another display starts from it (not empty until the next notice).
    static LAST_MIRROR: RefCell<Option<Mirror>> = const { RefCell::new(None) };
}

/// Keeps the main bar's latest Mirror (None: its island is gone).
pub(super) fn keep_mirror(m: Option<Mirror>) {
    LAST_MIRROR.with(|l| *l.borrow_mut() = m);
}

/// What the main bar's island tells the islands of the other bars (they
/// run no listeners of their own).
#[derive(Clone)]
pub(super) struct Mirror {
    track: Option<model::Track>,
    pause: pause::Grace,
    uses: Vec<model::Use>,
    art: Option<(Arc<Art>, CompositionBrush)>,
}

/// What other parts of NopBar told the island (UI thread).
#[derive(Default)]
struct Feed {
    /// Per dock window (each display's dock may show other apps).
    jobs: HashMap<isize, Vec<Job>>,
    icons: HashMap<isize, HashMap<String, CompositionBrush>>,
    shot: Option<Shot>,
}

thread_local! {
    static FEED: RefCell<Feed> = RefCell::new(Feed::default());
}

/// One app with taskbar progress, from the dock (dock/badges.rs).
pub struct JobFeed {
    pub key: String,
    pub name: String,
    /// The app's exe path, if known.
    pub exe: Option<String>,
    pub ratio: f32,
    pub hwnd: isize,
    /// The dock icon's brush (shared, nothing drawn again).
    pub icon: Option<CompositionBrush>,
}

/// Dock `source`'s apps with taskbar progress now (any order; empty =
/// none). Every dock reports its own; the island shows them together.
pub fn set_jobs(source: isize, list: Vec<JobFeed>) {
    let jobs: Vec<Job> = list
        .iter()
        .map(|f| Job {
            key: f.key.clone(),
            name: f.name.clone(),
            files: f.exe.as_deref().is_some_and(|e| e.rsplit(['\\', '/']).next().is_some_and(|n| n.eq_ignore_ascii_case("explorer.exe"))),
            ratio: f.ratio,
            hwnd: f.hwnd,
        })
        .collect();
    let changed = FEED.with(|f| {
        let Ok(mut f) = f.try_borrow_mut() else { return false };
        let icons: HashMap<String, CompositionBrush> = list.into_iter().filter_map(|j| Some((j.key, j.icon?))).collect();
        // New brushes (the dock drew its icons again) show too.
        let old = f.icons.get(&source);
        let same_icons = icons.len() == old.map_or(0, |o| o.len()) && icons.iter().all(|(k, b)| old.and_then(|o| o.get(k)).is_some_and(|o| o.as_raw() == b.as_raw()));
        if f.jobs.get(&source).map_or(jobs.is_empty(), |j| *j == jobs) && same_icons {
            return false;
        }
        if jobs.is_empty() {
            f.jobs.remove(&source);
            f.icons.remove(&source);
        } else {
            f.jobs.insert(source, jobs);
            f.icons.insert(source, icons);
        }
        true
    });
    if changed {
        poke();
    }
}

/// The focus timer changed (any thread): the island reads it again.
pub fn refresh() {
    poke();
}

/// A screenshot was taken (crate::shot), or it went (None).
pub fn set_screenshot(s: Option<Shot>) {
    let changed = FEED.with(|f| f.try_borrow_mut().map(|mut f| std::mem::replace(&mut f.shot, s.clone()) != s).unwrap_or(false));
    if changed {
        poke();
    }
}

/// The island's card is out (crate::shot keeps its screenshot meanwhile).
pub fn card_open() -> bool {
    card::is_open()
}

/// The screenshot's state changed (saved, copied): the card shows it.
pub fn poke_screenshot() {
    poke();
}

/// Tells the primary bar to look again (posted: the caller may be inside
/// the bars).
fn poke() {
    let h = BAR.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_ISLAND_ACT, WPARAM(ACT_REFRESH), LPARAM(0));
        }
    }
}

fn feed_inputs(inp: &mut Inputs) {
    inp.timer = crate::focus_timer::view(crate::focus_timer::now_ms()).map(Timer::of_focus);
    FEED.with(|f| {
        if let Ok(mut f) = f.try_borrow_mut() {
            // A dock that is gone (its display went away) reports no more.
            f.jobs.retain(|h, _| unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(*h as *mut _))) }.as_bool());
            let alive: Vec<isize> = f.jobs.keys().copied().collect();
            f.icons.retain(|h, _| alive.contains(h));
            // In a steady order (by dock), each app once (model::live).
            let mut docks: Vec<&isize> = f.jobs.keys().collect();
            docks.sort();
            inp.jobs = docks.into_iter().flat_map(|h| f.jobs[h].iter().cloned()).collect();
            inp.shot = f.shot.clone();
        }
    });
}

/// Play / pause media session `session` (Track::session) if it is still
/// the one playing, from outside the bars (a PiP's button): the main bar
/// holds the player.
pub fn media_toggle(session: u32) {
    let main = BAR.load(Ordering::SeqCst);
    if main != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(main as *mut _)), WM_ISLAND_ACT, WPARAM(ACT_PIP_TOGGLE), LPARAM(session as isize));
        }
    }
}

/// What a PiP is told of the track.
fn pip_media(t: &model::Track) -> crate::pip::Media {
    crate::pip::Media { app: t.app.clone(), title: t.title.clone(), session: t.session, playing: t.playing, can_toggle: t.can_toggle }
}

/// The main bar's island went (switched off, or the bar destroyed): its
/// listeners stop, and a PiP hears that nothing plays any more (it goes
/// unless a new main bar's listener says otherwise in time).
fn main_island_gone() {
    privacy::stop();
    crate::shot::set_enabled(false);
    BAR.store(0, Ordering::SeqCst);
    keep_mirror(None);
    crate::pip::media(None);
}

fn icon_of(key: &str) -> Option<CompositionBrush> {
    FEED.with(|f| {
        let f = f.try_borrow().ok()?;
        let mut docks: Vec<&isize> = f.icons.keys().collect();
        docks.sort();
        docks.into_iter().find_map(|h| f.icons[h].get(key).cloned())
    })
}

// ---------------------------------------------------------------- motion

/// The island moves (full motion, crate::motion).
fn motion() -> bool {
    crate::motion::full()
}

#[derive(Clone, Copy)]
enum Ch {
    Scalar,
    V2,
    V3,
}

/// The kit's ease-out (crate::motion::EASE_OUT).
fn ease(c: &Compositor) -> Result<windows::UI::Composition::CompositionEasingFunction> {
    let ((x1, y1), (x2, y2)) = crate::motion::EASE_OUT;
    c.CreateCubicBezierEasingFunction(Vector2 { X: x1, Y: y1 }, Vector2 { X: x2, Y: y2 })?.cast()
}

/// Pure: Spring C's course over `ms` as expression key frames from where
/// the value starts to `to` (an expression: "this.FinalValue" for an
/// implicit animation, a vector for an explicit one): (progress, text).
/// The kit's spring (crate::motion), nearly critical: no overshoot.
fn spring_keys(ms: i64, to: &str) -> Vec<(f32, String)> {
    crate::motion::SPRING_GLIDE
        .keys(ms, crate::motion::SAMPLES)
        .into_iter()
        .skip(1)
        .map(|(p, done)| (p, format!("this.StartingValue + ({to} - this.StartingValue) * {done}")))
        .collect()
}

/// Implicit animations on `obj` (each property eases to every new value);
/// none when the island's motion is off.
fn implicit(c: &Compositor, obj: &impl Interface, props: &[(&str, Ch, i64, bool)]) -> Result<()> {
    implicit_if(c, obj, props, motion())
}

/// Implicit animations on `obj` when `on` (else none: new values apply at
/// once).
fn implicit_if(c: &Compositor, obj: &impl Interface, props: &[(&str, Ch, i64, bool)], on: bool) -> Result<()> {
    let coll = c.CreateImplicitAnimationCollection()?;
    if on {
        for &(prop, ch, ms, sprung) in props {
            let d = TimeSpan { Duration: crate::effects::ms(ms) * 10_000 };
            let key = HSTRING::from(prop);
            let e = ease(c)?;
            let lin: windows::UI::Composition::CompositionEasingFunction = c.CreateLinearEasingFunction()?.cast()?;
            // Sprung (moves and sizes): Spring C's course, linear between
            // its samples; else the kit's ease-out to the new value.
            let keys: Vec<(f32, String)> = if sprung { spring_keys(ms, "this.FinalValue") } else { vec![(1.0, "this.FinalValue".to_string())] };
            let e = if sprung { &lin } else { &e };
            macro_rules! keyed {
                ($a:expr) => {{
                    let a = $a;
                    for (p, k) in &keys {
                        a.InsertExpressionKeyFrameWithEasingFunction(*p, &HSTRING::from(k.as_str()), e)?;
                    }
                    a.SetDuration(d)?;
                    a.SetTarget(&key)?;
                    a.cast()?
                }};
            }
            let anim: ICompositionAnimationBase = match ch {
                Ch::Scalar => keyed!(c.CreateScalarKeyFrameAnimation()?),
                Ch::V2 => keyed!(c.CreateVector2KeyFrameAnimation()?),
                Ch::V3 => keyed!(c.CreateVector3KeyFrameAnimation()?),
            };
            coll.Insert(&key, &anim)?;
        }
    }
    obj.cast::<CompositionObject>()?.SetImplicitAnimations(&coll)?;
    Ok(())
}

/// A quick "pop" (grows into place) on `v`'s Scale: the kit's Spring A,
/// no overshoot bezier.
fn pop(c: &Compositor, v: &SpriteVisual) {
    if !motion() {
        return;
    }
    const POP_FROM: f32 = 0.6;
    const POP_MS: i64 = 260;
    let run = || -> Result<()> {
        let a = c.CreateVector3KeyFrameAnimation()?;
        let lin = c.CreateLinearEasingFunction()?;
        for (p, done) in crate::motion::SPRING_SNAP.keys(POP_MS, crate::motion::SAMPLES) {
            let k = POP_FROM + (1.0 - POP_FROM) * done;
            a.InsertKeyFrameWithEasingFunction(p, Vector3 { X: k, Y: k, Z: 1.0 }, &lin)?;
        }
        a.SetDuration(TimeSpan { Duration: crate::effects::ms(POP_MS) * 10_000 })?;
        v.StartAnimation(&HSTRING::from("Scale"), &a)?;
        Ok(())
    };
    let _ = run();
}

/// What the pointer is doing to the pill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Touch {
    hovered: bool,
    pressed: bool,
}

/// Pure: the pill's scale: hidden, the kit's opening scale; pressed, the
/// one press formula (crate::motion::press_scale: its long side 2 DIP
/// shorter); under the pointer, HOVER_SCALE; else 1.
fn scale_target(shown: bool, t: Touch, w: f32, h: f32) -> f32 {
    if !shown {
        HIDDEN_SCALE
    } else if t.pressed {
        crate::motion::press_scale(w, h)
    } else if t.hovered {
        HOVER_SCALE
    } else {
        1.0
    }
}

/// Pure: the spring the pill's scale rides: coming and going GLIDE (a
/// surface's size), hover and press SNAP (a small thing's state).
fn scale_spring(coming_or_going: bool) -> crate::motion::Spring {
    if coming_or_going {
        crate::motion::SPRING_GLIDE
    } else {
        crate::motion::SPRING_SNAP
    }
}

/// Which fill the pill wears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    Rest,
    Hover,
    Pressed,
}

/// Pure: the pill's fill for `t` and how long it takes to get there (ms
/// at speed 1; 6.3): a press at once, the pointer coming on (or a press
/// let go) FADE_IN_MS, the pointer leaving FADE_OUT_MS.
fn fill_plan(t: Touch) -> (Fill, i64) {
    if t.pressed {
        (Fill::Pressed, 0)
    } else if t.hovered {
        (Fill::Hover, crate::motion::FADE_IN_MS)
    } else {
        (Fill::Rest, crate::motion::FADE_OUT_MS)
    }
}

/// Pure: how a color change designed as `ms` (at speed 1) on EASE_OUT
/// plays in `mode` at animation speed `speed`: (ms of real time, curve).
/// The fades' own plan (crate::motion::fade_plan), decided once: reduced
/// motion is REDUCED_MS at most, straight, whatever the speed.
fn color_plan(mode: crate::motion::Mode, ms: i64, speed: f32) -> (i64, crate::motion::Curve) {
    crate::motion::fade_plan(mode, ms, crate::motion::EASE_OUT, speed)
}

/// `brush`'s color to `to` over `ms` (at speed 1; reduced motion: the
/// short straight fade) from the color it shows; at once for 0.
pub(super) fn color_to(c: &Compositor, brush: &CompositionColorBrush, to: Rgba, ms: i64) {
    if let Ok(obj) = brush.cast::<CompositionObject>() {
        tint_to(c, &obj, to, ms, |col| brush.SetColor(col));
    }
}

/// color_to on a fall's two stops (top, bottom): the pill's fill (Flat v2
/// falls from top to bottom; elsewhere both stops are one color).
fn fall_to(c: &Compositor, stops: &[CompositionColorGradientStop; 2], (top, bottom): (Rgba, Rgba), ms: i64) {
    for (stop, to) in stops.iter().zip([top, bottom]) {
        if let Ok(obj) = stop.cast::<CompositionObject>() {
            tint_to(c, &obj, to, ms, |col| stop.SetColor(col));
        }
    }
}

/// `obj`'s Color to `to` over `ms` (color_to), `set` putting it at once.
fn tint_to(c: &Compositor, obj: &CompositionObject, to: Rgba, ms: i64, set: impl Fn(windows::UI::Color) -> Result<()>) {
    let prop = HSTRING::from("Color");
    // Real time and curve, once (the speed is not applied again).
    let (ms, curve) = color_plan(crate::motion::now(), ms, crate::effects::current().speed);
    let run = || -> Result<()> {
        let a = c.CreateColorKeyFrameAnimation()?;
        a.InsertKeyFrameWithEasingFunction(1.0, color(to), &crate::motion::easing(c, curve)?)?;
        a.SetDuration(TimeSpan { Duration: ms.max(1) * 10_000 })?;
        obj.StartAnimation(&prop, &a)
    };
    if ms <= 0 || run().is_err() {
        let _ = obj.StopAnimation(&prop);
        let _ = set(color(to));
    }
}

fn rgba_d2d(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

/// A tone's color (high contrast: the system highlight).
pub(super) fn tone_rgba(t: Tone, theme: &crate::theme::Theme) -> Rgba {
    if let Some(h) = theme.hc.filter(|_| theme.high_contrast) {
        return if t == Tone::Ink { h.text } else { h.highlight };
    }
    let (r, g, b) = match t {
        Tone::Ink => return pill_colors(theme).1,
        Tone::Green => (0x30, 0xD1, 0x58),
        Tone::Orange => (0xFF, 0x9F, 0x0A),
        Tone::Purple => (0xBF, 0x5A, 0xF2),
        Tone::Red => (0xFF, 0x45, 0x3A),
    };
    Rgba { r, g, b, a: 255 }
}

/// The pill's own colors: (fill, text, dim text, inner border). Its fill
/// is material M4 (the name labels' and tooltips': a step lighter than
/// anything it can sit on, opaque on Flat and nearly so on Glass, no
/// backdrop), so its text reads over any bar. No edge but high contrast's
/// (Flat and Glass are borderless: the contact shadow and E1 mark it).
fn pill_colors(theme: &crate::theme::Theme) -> (Rgba, Rgba, Rgba, Rgba) {
    match theme.hc.filter(|_| theme.high_contrast) {
        Some(h) => (h.window, h.text, h.text, h.text),
        None => (theme.material(crate::theme::Material::Tip), theme.ink(), theme.text_dim(false), theme.tile_border()),
    }
}

/// `top` over `under` (both straight alpha), opaque as `under`.
fn over(top: Rgba, under: Rgba) -> Rgba {
    let k = top.a as f32 / 255.0;
    let mix = |a: u8, b: u8| (a as f32 * k + b as f32 * (1.0 - k)).round() as u8;
    Rgba { r: mix(top.r, under.r), g: mix(top.g, under.g), b: mix(top.b, under.b), a: under.a }
}

/// The pill's fill in state `f`: the row fills (hover, pressed) laid over
/// its material. High contrast: Window, and Highlight while pressed (12.3).
fn pill_fill(theme: &crate::theme::Theme, f: Fill) -> Rgba {
    let (fill, ..) = pill_colors(theme);
    if let Some(h) = theme.hc.filter(|_| theme.high_contrast) {
        return if f == Fill::Pressed { h.highlight } else { fill };
    }
    match f {
        Fill::Rest => fill,
        Fill::Hover => over(theme.hover(false), fill),
        Fill::Pressed => over(theme.hover(true), fill),
    }
}

/// The pill's fill in state `f` from its top to its bottom: Flat v2's M4
/// fall (Theme::flat_fill) with the row fills laid over each end; one
/// color elsewhere (pill_fill: a glass, a theme file, high contrast).
fn pill_fall(theme: &crate::theme::Theme, f: Fill) -> (Rgba, Rgba) {
    let one = pill_fill(theme, f);
    match theme.flat_fill(crate::theme::Material::Tip).filter(|_| !theme.high_contrast) {
        Some((top, bottom)) => match f {
            Fill::Rest => (top, bottom),
            Fill::Hover => (over(theme.hover(false), top), over(theme.hover(false), bottom)),
            Fill::Pressed => (over(theme.hover(true), top), over(theme.hover(true), bottom)),
        },
        None => (one, one),
    }
}

/// Pure: the pill's rim's stops (Theme::rim_of M4): Flat v2's (its edge
/// light folded in) and a glass's own light, as on every other glass
/// surface (the dock, panels, menus, tips). On a glass the pill is nearly
/// the bar's lightness, and over some wallpapers the same: the light along
/// its edge is what parts the two (2026-11-11 final sheet 2: a glass's
/// dark pill met the bar with no edge but the contact ring). None on a Flat without its coat (a theme file) and in
/// high contrast (its outline is drawn apart: Island::edges).
pub(super) fn pill_rim(theme: &crate::theme::Theme) -> Option<[Rgba; 3]> {
    let m = crate::theme::Material::Tip;
    if theme.glass() || theme.flat_coat(m).is_some() {
        theme.rim_of(m)
    } else {
        None
    }
}

/// Pure: the border's geometry in the pill `f` for a stroke `w` DIP wide:
/// half of it in from the pill's edge (model::frame's f.edge is a 1 DIP
/// stroke's, 0.5 in).
fn edge_for(f: &model::Frame, w: f32) -> model::Rect {
    let d = w / 2.0 - f.edge.x;
    model::Rect { x: f.edge.x + d, y: f.edge.y + d, w: (f.edge.w - 2.0 * d).max(0.0), h: (f.edge.h - 2.0 * d).max(0.0) }
}

/// Pure: what the pill wears at rest on a display scaled `s` (morph::Coat:
/// the card's plate at p = 0 wears the same, Look::ends): its fall, its rim
/// (pill_rim, Theme::rim_width wide: Flat v2's 1 DIP, a glass's one device
/// pixel), no grain (M4 has none).
pub(super) fn pill_coat(theme: &crate::theme::Theme, s: f32) -> morph::Coat {
    let m = crate::theme::Material::Tip;
    let rim = pill_rim(theme).map(|r| (r, theme.rim_width(m, s)));
    morph::Coat { fill: pill_fall(theme, Fill::Rest), rim, grain: 0.0 }
}

/// The music bars' color: the pill's ink (they only inform; the accent is
/// for what is on or picked).
fn eq_color(theme: &crate::theme::Theme) -> Rgba {
    pill_colors(theme).1
}

/// Pure: the pill's shadows: E1, both layers, as (x, y, blur, color);
/// none in high contrast or with the "Shadows" effect off.
fn pill_shadows(theme: &crate::theme::Theme, shadows: bool) -> Vec<(f32, f32, f32, Rgba)> {
    if theme.high_contrast || !shadows {
        return Vec::new();
    }
    theme.elevation(crate::theme::Elevation::E1)
}

/// Pure: a leading picture's corners in a pill `h` tall: concentric with
/// the capsule (its radius less the picture's inset), never past a circle.
fn lead_radius(side: f32, h: f32) -> f32 {
    let inset = ((h - side) / 2.0).max(0.0);
    crate::theme::concentric(crate::theme::capsule(h), inset).min(side / 2.0)
}

fn color(c: Rgba) -> windows::UI::Color {
    windows::UI::Color { A: c.a, R: c.r, G: c.g, B: c.b }
}

// ---------------------------------------------------------------- visuals

/// One copy of the pill's content (two, to cross-fade between activities).
struct Layer {
    root: ContainerVisual,
    lead: SpriteVisual,
    /// The lead's rounded clip (album art, a thumbnail), sized in layout.
    lead_geom: CompositionRoundedRectangleGeometry,
    lead_clip: CompositionGeometricClip,
    text: SpriteVisual,
    trail: SpriteVisual,
}

impl Layer {
    fn new(c: &Compositor, parent: &ContainerVisual) -> Result<Self> {
        let root = c.CreateContainerVisual()?;
        parent.Children()?.InsertAtTop(&root)?;
        let lead = c.CreateSpriteVisual()?;
        let text = c.CreateSpriteVisual()?;
        let trail = c.CreateSpriteVisual()?;
        for v in [&lead, &text, &trail] {
            root.Children()?.InsertAtTop(v)?;
            implicit(c, v, &[("Offset", Ch::V3, MOVE_MS, true)])?;
        }
        root.SetOpacity(0.0)?;
        implicit(c, &root, &[("Opacity", Ch::Scalar, SWAP_MS, false)])?;
        let lead_geom = c.CreateRoundedRectangleGeometry()?;
        let lead_clip = c.CreateGeometricClipWithGeometry(&lead_geom)?;
        Ok(Layer { root, lead, lead_geom, lead_clip, text, trail })
    }
}

/// What the pill draws, measured (DIP).
struct Drawn {
    key: String,
    kind: Kind,
    lead: Option<(CompositionBrush, f32)>,
    /// Round the lead (album art).
    lead_round: bool,
    text: Option<(CompositionBrush, f32, f32)>,
    /// The title's baseline from its top (DIP).
    baseline: f32,
    /// The text before cutting to fit, the most it may take (and its
    /// width there), and the width it is cut to now.
    words: String,
    full_max: f32,
    full_w: f32,
    text_max: f32,
    trail: Trail,
    dots: Vec<Tone>,
}

enum Trail {
    None,
    /// A drawn surface (a ring with the percent, a time).
    Surface(CompositionBrush, f32, f32),
    /// The music bars.
    Eq,
    /// The focus timer: the ring (a shape visual of its own) and the time
    /// drawn after it (brush, text width, height).
    Ring(CompositionBrush, f32, f32),
}

impl Trail {
    fn width(&self) -> f32 {
        match self {
            Trail::None => 0.0,
            Trail::Surface(_, w, _) => *w,
            Trail::Eq => EQ_BARS as f32 * EQ_W + (EQ_BARS - 1) as f32 * EQ_GAP,
            Trail::Ring(_, w, _) => RING + RING_GAP + *w,
        }
    }
}

/// Where the pill's parts were put last (island_layout): what the card's
/// plate copies while it stands in for the pill (island_pill).
struct Placed {
    frame: model::Frame,
    /// The trailing text's and the timer ring's x, the ring's y.
    text_at: f32,
    ring_at: f32,
    ring_y: f32,
}

pub(super) struct Island {
    c: Compositor,
    pub(super) root: ContainerVisual,
    /// Everything of the pill, under the root: hidden while the card's
    /// plate stands in for it (card.rs: the pill and the card are one
    /// thing; the plate at p = 0 is drawn exactly over this).
    shell: ContainerVisual,
    /// The background (model::frame): a plain fill rectangle and the inner
    /// border, both under the content's clip (the pill's rounded shape cuts
    /// them, so they follow it as it eases), and the shadow's own visual
    /// (no brush; a pill-shaped mask shapes the shadow, as the menus do).
    /// No LayerVisual: its shadow came out as a rectangle over the layer's
    /// bounds while its shape visual's fill never showed.
    fill_v: SpriteVisual,
    /// The fill's brush: a fall from the pill's top to its bottom (in the
    /// fill's own DIP: the fill is taller than the pill), its two stops
    /// (Flat v2's M4 top and bottom; one color elsewhere).
    fill: CompositionLinearGradientBrush,
    fill_stops: [CompositionColorGradientStop; 2],
    /// The rim on the border (pill_coat: Flat v2's, a glass's; None: the
    /// border's own color), its fall set to the pill's height in layout.
    rim: Option<CompositionLinearGradientBrush>,
    /// The rim's width (DIP; 1: high contrast's outline at rest): the
    /// border's stroke lies half of it in from the pill's edge.
    rim_w: f32,
    /// The border's visual (its shape at positive x from the pill's left
    /// end: a shape is drawn only inside its visual) and its geometry.
    edge_v: ShapeVisual,
    edge_geom: CompositionRoundedRectangleGeometry,
    edge: CompositionSpriteShape,
    /// E1 under the pill: a nine-grid patch of both layers (morph.rs: the
    /// card's plate wears the same one at p = 0), on a visual E1's reach
    /// larger than the pill.
    shadow_v: SpriteVisual,
    /// The contact shadow: a ring just outside the pill.
    contact_v: ShapeVisual,
    contact_geom: CompositionRoundedRectangleGeometry,
    contact: CompositionSpriteShape,
    /// What the patch and the ring were made for: (height, scale, light,
    /// high contrast, the "Shadows" effect).
    shade_for: Option<(u32, u32, bool, bool, bool)>,
    /// The root's scale on its way (hover, press, coming and going).
    scale: crate::motion::SpringRun,
    placed: Option<Placed>,
    /// The timer ring's colors: (track, arc).
    ring_colors: (Rgba, Rgba),
    /// High contrast: the outline's colors at rest and under the pointer
    /// (WindowText 1 DIP, Highlight 2 DIP: 12.3). None: no outline.
    edges: Option<(Rgba, Rgba)>,
    /// The content's clip (the pill, around the root's x 0).
    clip_geom: CompositionRoundedRectangleGeometry,
    layers: [Layer; 2],
    cur: usize,
    eq: ContainerVisual,
    bars: Vec<SpriteVisual>,
    eq_running: bool,
    dots: Vec<SpriteVisual>,
    /// The focus timer's ring: a track and the arc still to go.
    ring: ShapeVisual,
    ring_track: CompositionSpriteShape,
    ring_arc: CompositionSpriteShape,
    ring_geom: CompositionEllipseGeometry,
    /// What the ring runs for now: (deadline, whole, animated). None: off.
    ring_key: Option<RingKey>,
    /// When the running ring's animation started (monotonic) and the time
    /// left then: a refresh that finds the wall clock elsewhere (the clock
    /// was set, a sleep) starts it again.
    ring_started: Option<(std::time::Instant, i64)>,
    /// TICK_TIMER is set.
    ticking: bool,
    /// The focus timer's phase last drawn (a new one pops).
    timer_phase: Option<u8>,
    pause: pause::Grace,
    pause_timer: Option<std::time::Instant>,
    pub(super) phase: Phase,
    inputs: Inputs,
    on: Enabled,
    live: Vec<Activity>,
    drawn: Option<Drawn>,
    /// Where the pill is (DIP on the bar): center x, top, width, height.
    at: Option<(f32, f32, f32, f32)>,
    hovered: bool,
    /// The fill at rest, under the pointer and pressed, top and bottom
    /// (set when drawn; indexed by Fill).
    fills: Option<[(Rgba, Rgba); 3]>,
    hover_pending: bool,
    /// The left button went down on the pill.
    pressed: bool,
    media: Option<MediaWatch>,
    art: Option<(Arc<Art>, CompositionBrush)>,
    battery: Option<(u8, bool)>,
    tone_brushes: RefCell<HashMap<(Tone, u32), CompositionBrush>>,
    /// The scale and theme things were drawn for.
    drawn_for: (u32, bool, bool),
}

impl Island {
    fn new(c: &Compositor, parent: &ContainerVisual) -> Result<Self> {
        let root = c.CreateContainerVisual()?;
        parent.Children()?.InsertAtTop(&root)?;
        let shell = c.CreateContainerVisual()?;
        root.Children()?.InsertAtTop(&shell)?;
        // E1 under the pill and the contact ring round it: visuals of
        // their own below the rest (their brushes are set in layout).
        let shadow_v = c.CreateSpriteVisual()?;
        shell.Children()?.InsertAtTop(&shadow_v)?;
        let contact_v = c.CreateShapeVisual()?;
        let contact_geom = c.CreateRoundedRectangleGeometry()?;
        let contact = c.CreateSpriteShapeWithGeometry(&contact_geom)?;
        contact_v.Shapes()?.Append(&contact)?;
        shell.Children()?.InsertAtTop(&contact_v)?;
        // Everything else under the content's clip (the pill).
        let body = c.CreateContainerVisual()?;
        let clip_geom = c.CreateRoundedRectangleGeometry()?;
        let clip: CompositionGeometricClip = c.CreateGeometricClipWithGeometry(&clip_geom)?;
        body.SetClip(&clip)?;
        shell.Children()?.InsertAtTop(&body)?;
        // The fill: a rectangle the clip cuts to the pill.
        let fill_v = c.CreateSpriteVisual()?;
        let fill = c.CreateLinearGradientBrush()?;
        // (Absolute: the fill is taller than the pill; layout puts the
        // fall on the pill.)
        fill.SetMappingMode(CompositionMappingMode::Absolute)?;
        let clear = windows::UI::Color { A: 0, R: 0, G: 0, B: 0 };
        let fill_stops = [c.CreateColorGradientStopWithOffsetAndColor(0.0, clear)?, c.CreateColorGradientStopWithOffsetAndColor(1.0, clear)?];
        for s in &fill_stops {
            fill.ColorStops()?.Append(s)?;
        }
        fill_v.SetBrush(&fill)?;
        body.Children()?.InsertAtTop(&fill_v)?;
        // A 1-DIP border just inside the pill's edge.
        let edge_v = c.CreateShapeVisual()?;
        let edge_geom = c.CreateRoundedRectangleGeometry()?;
        let edge = c.CreateSpriteShapeWithGeometry(&edge_geom)?;
        edge.SetStrokeThickness(1.0)?;
        edge_v.Shapes()?.Append(&edge)?;
        body.Children()?.InsertAtTop(&edge_v)?;
        let layers = [Layer::new(c, &body)?, Layer::new(c, &body)?];
        let eq = c.CreateContainerVisual()?;
        body.Children()?.InsertAtTop(&eq)?;
        let mut bars = Vec::new();
        for _ in 0..EQ_BARS {
            let b = c.CreateSpriteVisual()?;
            eq.Children()?.InsertAtTop(&b)?;
            bars.push(b);
        }
        let mut dots = Vec::new();
        for _ in 0..model::MAX_DOTS {
            let d = c.CreateSpriteVisual()?;
            d.SetSize(Vector2 { X: model::DOT, Y: model::DOT })?;
            d.SetOpacity(0.0)?;
            body.Children()?.InsertAtTop(&d)?;
            dots.push(d);
        }
        // The focus timer's ring: the full track, and the remaining arc from
        // the top, clockwise (the circle's path starts at the right).
        let ring = c.CreateShapeVisual()?;
        ring.SetSize(Vector2 { X: RING, Y: RING })?;
        let circle = |c: &Compositor| -> Result<CompositionEllipseGeometry> {
            let g = c.CreateEllipseGeometry()?;
            g.SetCenter(Vector2 { X: RING / 2.0, Y: RING / 2.0 })?;
            g.SetRadius(Vector2 { X: RING / 2.0 - 1.2, Y: RING / 2.0 - 1.2 })?;
            Ok(g)
        };
        let ring_track = c.CreateSpriteShapeWithGeometry(&circle(c)?)?;
        ring_track.SetStrokeThickness(2.0)?;
        let ring_geom = circle(c)?;
        let ring_arc = c.CreateSpriteShapeWithGeometry(&ring_geom)?;
        ring_arc.SetStrokeThickness(2.0)?;
        ring_arc.SetStrokeStartCap(CompositionStrokeCap::Round)?;
        ring_arc.SetStrokeEndCap(CompositionStrokeCap::Round)?;
        ring_arc.SetCenterPoint(Vector2 { X: RING / 2.0, Y: RING / 2.0 })?;
        ring_arc.SetRotationAngleInDegrees(-90.0)?;
        ring.Shapes()?.Append(&ring_track)?;
        ring.Shapes()?.Append(&ring_arc)?;
        ring.SetOpacity(0.0)?;
        body.Children()?.InsertAtTop(&ring)?;
        root.SetOpacity(0.0)?;
        root.SetScale(Vector3 { X: HIDDEN_SCALE, Y: HIDDEN_SCALE, Z: 1.0 })?;
        let mut me = Island {
            c: c.clone(),
            root,
            shell,
            fill_v,
            fill,
            fill_stops,
            rim: None,
            rim_w: 1.0,
            edge_v,
            edge_geom,
            edge,
            shadow_v,
            contact_v,
            contact_geom,
            contact,
            shade_for: None,
            scale: crate::motion::SpringRun::still(crate::motion::SPRING_GLIDE, HIDDEN_SCALE, std::time::Instant::now()),
            placed: None,
            ring_colors: (Rgba { r: 0, g: 0, b: 0, a: 0 }, Rgba { r: 0, g: 0, b: 0, a: 0 }),
            edges: None,
            clip_geom,
            layers,
            cur: 0,
            eq,
            bars,
            eq_running: false,
            dots,
            ring,
            ring_track,
            ring_arc,
            ring_geom,
            ring_key: None,
            ring_started: None,
            ticking: false,
            timer_phase: None,
            pause: pause::Grace::default(),
            pause_timer: None,
            phase: Phase::Hidden,
            inputs: Inputs::default(),
            on: Enabled::ALL,
            live: Vec::new(),
            drawn: None,
            at: None,
            hovered: false,
            fills: None,
            hover_pending: false,
            pressed: false,
            media: None,
            art: None,
            battery: None,
            tone_brushes: RefCell::new(HashMap::new()),
            drawn_for: (0, false, false),
        };
        me.attach_motion();
        Ok(me)
    }

    /// (Re)sets the implicit animations (the effect or Windows' animation
    /// setting may have changed).
    fn attach_motion(&mut self) {
        let c = &self.c;
        // (Scale is a spring of its own: scale_to. The fill's color is
        // sent by color_to: on and off take different times.)
        let _ = implicit(c, &self.root, &[("Opacity", Ch::Scalar, FADE_MS, false)]);
        for l in &self.layers {
            let _ = implicit(c, &l.root, &[("Opacity", Ch::Scalar, SWAP_MS, false)]);
        }
        // The ring runs again with the new motion setting.
        self.ring_key = None;
        self.layout_motion(motion());
    }

    /// The geometry's implicit animations: on, every new place eases from
    /// the last; off, it is set at once. Off for a first placement
    /// (model::layout_animates): the background's geometry and the
    /// content's clip start from nothing, not from a shared place.
    fn layout_motion(&self, on: bool) {
        let c = &self.c;
        // The width eases to each new one: the content's clip (it shapes
        // the fill), the border and the shadow together, on one curve.
        for g in [&self.edge_geom, &self.clip_geom, &self.contact_geom] {
            let _ = implicit_if(c, g, &[("Size", Ch::V2, MOVE_MS, true), ("Offset", Ch::V2, MOVE_MS, true)], on);
        }
        let _ = implicit_if(c, &self.edge_v, &[("Offset", Ch::V3, MOVE_MS, true)], on);
        let _ = implicit_if(c, &self.contact_v, &[("Offset", Ch::V3, MOVE_MS, true)], on);
        let _ = implicit_if(c, &self.shadow_v, &[("Size", Ch::V2, MOVE_MS, true), ("Offset", Ch::V3, MOVE_MS, true)], on);
        let _ = implicit_if(c, &self.eq, &[("Offset", Ch::V3, MOVE_MS, true), ("Opacity", Ch::Scalar, SWAP_MS, false)], on);
        let _ = implicit_if(c, &self.ring, &[("Offset", Ch::V3, MOVE_MS, true), ("Opacity", Ch::Scalar, SWAP_MS, false)], on);
        for d in &self.dots {
            let _ = implicit_if(c, d, &[("Offset", Ch::V3, MOVE_MS, true), ("Opacity", Ch::Scalar, SWAP_MS, false)], on);
        }
        for l in &self.layers {
            for v in [&l.lead, &l.text, &l.trail] {
                let _ = implicit_if(c, v, &[("Offset", Ch::V3, MOVE_MS, true)], on);
            }
        }
    }

    /// What the pointer does to the pill now (a press counts while the
    /// pointer is still on it).
    fn touch(&self) -> Touch {
        Touch { hovered: self.hovered, pressed: self.pressed && self.hovered }
    }

    /// The pill's look for what the pointer does: its size (scale_target,
    /// on scale_spring) and its fill (fill_plan). `coming_or_going`: it
    /// just appeared or left.
    fn apply_touch(&mut self, coming_or_going: bool) {
        let shown = self.phase != Phase::Hidden;
        let t = if shown { self.touch() } else { Touch::default() };
        let (w, h) = self.at.map(|a| (a.2, a.3)).unwrap_or((0.0, 0.0));
        self.scale_to(scale_target(shown, t, w, h), scale_spring(coming_or_going));
        if let Some(fills) = self.fills {
            let (f, ms) = fill_plan(t);
            fall_to(&self.c, &self.fill_stops, fills[f as usize], ms);
        }
        // High contrast answers the pointer with its outline, at once.
        if let Some((rest, over)) = self.edges {
            let (c, w) = if t.hovered && !t.pressed { (over, 2.0) } else { (rest, 1.0) };
            if let Ok(b) = self.c.CreateColorBrushWithColor(color(c)) {
                let _ = self.edge.SetStrokeBrush(&b);
                let _ = self.edge.SetStrokeThickness(w);
            }
        }
    }

    fn set_hover(&mut self, on: bool) {
        self.hovered = on;
        self.apply_touch(false);
    }

    /// The root's scale to `k` on `spring`, from where it is at the speed
    /// it has (crate::motion::spring_prop; at once with reduced motion).
    fn scale_to(&mut self, k: f32, spring: crate::motion::Spring) {
        let now = std::time::Instant::now();
        let (x, v) = self.scale.state(now);
        let seed = crate::motion::SpringRun { spring, from: x, velocity: v, to: x, since: now, stretch: self.scale.stretch };
        let mut next = None;
        for prop in ["Scale.X", "Scale.Y"] {
            if let Ok(run) = crate::motion::spring_prop(&self.c, &self.root, prop, &seed, k) {
                next = Some(run);
            }
        }
        self.scale = next.unwrap_or_else(|| {
            let _ = self.root.SetScale(Vector3 { X: k, Y: k, Z: 1.0 });
            crate::motion::SpringRun::still(spring, k, now)
        });
    }

    fn remove_from(&self, parent: &ContainerVisual) {
        self.stop_eq();
        let _ = self.ring_geom.StopAnimation(&HSTRING::from("TrimEnd"));
        if let Ok(ch) = parent.Children() {
            let _ = ch.Remove(&self.root);
        }
    }

    fn stop_eq(&self) {
        for b in &self.bars {
            let _ = b.StopAnimation(&HSTRING::from("Scale"));
            let _ = b.SetScale(Vector3 { X: 1.0, Y: 0.55, Z: 1.0 });
        }
    }

    /// The music bars move (compositor-driven, forever) or rest.
    fn set_eq(&mut self, run: bool) {
        if run == self.eq_running {
            return;
        }
        self.eq_running = run;
        if !run {
            self.stop_eq();
            return;
        }
        let c = &self.c;
        // Each bar its own beat, so they never line up.
        const BEATS: [(i64, f32); EQ_BARS] = [(620, 0.30), (470, 0.45), (780, 0.25), (540, 0.40)];
        for (b, (ms, lo)) in self.bars.iter().zip(BEATS) {
            let run = || -> Result<()> {
                let a = c.CreateVector3KeyFrameAnimation()?;
                let e = ease(c)?;
                a.InsertKeyFrame(0.0, Vector3 { X: 1.0, Y: lo, Z: 1.0 })?;
                a.InsertKeyFrameWithEasingFunction(0.5, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &e)?;
                a.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: lo, Z: 1.0 }, &e)?;
                a.SetDuration(TimeSpan { Duration: crate::effects::ms(ms) * 10_000 })?;
                a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                b.StartAnimation(&HSTRING::from("Scale"), &a)?;
                Ok(())
            };
            let _ = run();
        }
    }

    /// The focus timer's ring for `tm` (None, or `show` false: off and
    /// still). Running with motion: one compositor animation takes the arc
    /// to nothing by the deadline (started again only when the deadline or
    /// the whole changes: "+5 min", a resume, a new phase). Paused or
    /// without motion: the arc is set where it is (the seconds' tick moves
    /// it). Never per-frame work in our process.
    fn set_ring(&mut self, tm: Option<&Timer>, show: bool) {
        let prop = HSTRING::from("TrimEnd");
        let Some((tm, v)) = tm.filter(|_| show).and_then(|tm| Some((tm, tm.focus.clone()?))).filter(|(tm, _)| !tm.over()) else {
            if self.ring_key.take().is_some() {
                let _ = self.ring_geom.StopAnimation(&prop);
            }
            self.ring_started = None;
            let _ = self.ring.SetOpacity(0.0);
            return;
        };
        let frac = tm.ratio();
        let animate = v.running && motion();
        let key = RingKey { deadline: v.deadline, total: v.total_ms, running: v.running, animated: animate };
        // Where the running animation should be now, by the monotonic
        // clock; far from what the wall clock says: start it again.
        let drifted = self.ring_started.is_some_and(|(at, left0)| (left0 - at.elapsed().as_millis() as i64 - v.left_ms).abs() > RING_DRIFT_MS);
        if animate {
            if self.ring_key != Some(key) || drifted {
                self.ring_started = Some((std::time::Instant::now(), v.left_ms));
                let c = &self.c;
                let run = || -> Result<()> {
                    let a = c.CreateScalarKeyFrameAnimation()?;
                    let linear = c.CreateLinearEasingFunction()?;
                    a.InsertKeyFrame(0.0, frac)?;
                    a.InsertKeyFrameWithEasingFunction(1.0, 0.0, &linear)?;
                    // Real time (not scaled by the animation speed).
                    a.SetDuration(TimeSpan { Duration: v.left_ms.max(1) * 10_000 })?;
                    self.ring_geom.StartAnimation(&prop, &a)?;
                    Ok(())
                };
                if run().is_err() {
                    let _ = self.ring_geom.SetTrimEnd(frac);
                }
            }
        } else {
            self.ring_started = None;
            if self.ring_key.is_some_and(|k| k.animated) {
                let _ = self.ring_geom.StopAnimation(&prop);
            }
            let _ = self.ring_geom.SetTrimEnd(frac);
        }
        self.ring_key = Some(key);
        let _ = self.ring.SetOpacity(1.0);
    }
}

// ---------------------------------------------------------------- the bar's side

impl TopBar {
    /// This bar has an island: the main bar, and with "Same on every
    /// display" every bar (whether it shows now is island_here's).
    fn island_wanted(&self) -> bool {
        self.cfg.island && (self.primary || self.cfg.everywhere())
    }

    /// The island shows on this bar now (see island_on).
    pub(super) fn island_here(&self) -> bool {
        let m = ISLAND_MON.load(Ordering::SeqCst);
        island_on(self.cfg.everywhere(), self.cfg.island_displays() == 1, self.primary, self.hmon, (m != 0).then_some(m))
    }

    /// The bar moves to the other screen edge: the island goes back to its
    /// pill (Dismiss, so the card closes the way Esc closes it) with
    /// nothing of the old place pending (the hover and leave timers, a
    /// press, the hover look), so hovering or the first click opens the
    /// card again from the new place.
    pub(super) fn island_bar_moved(&mut self) {
        let hwnd = self.hwnd;
        let before = {
            let Some(isl) = self.island.as_mut() else {
                self.island_close_card();
                return;
            };
            isl.hover_pending = false;
            isl.pressed = false;
            if isl.hovered {
                isl.set_hover(false);
            }
            unsafe {
                let _ = KillTimer(Some(hwnd), HOVER_TIMER);
                let _ = KillTimer(Some(hwnd), LEAVE_TIMER);
            }
            let before = isl.phase;
            isl.phase = model::after_bar_moved(before);
            before
        };
        if self.island.as_ref().is_some_and(|i| i.phase != before) {
            self.island_phase_changed(before);
        }
        // A card this bar opened while its own phase did not say so.
        self.island_close_card();
    }

    /// Closes the card if this bar opened it (the main bar's: any card).
    pub(super) fn island_close_card(&self) {
        if self.primary || card::opened_by(self.hwnd) {
            card::close();
        }
    }

    /// Makes or drops the island for the config. The main bar's runs the
    /// listeners (what plays, camera and microphone, screenshots); the
    /// other bars' take what it found (island_follow).
    pub(super) fn island_setup(&mut self) {
        let want = self.island_wanted();
        if !want {
            if let Some(isl) = self.island.take() {
                isl.remove_from(&self.root);
                self.island_close_card();
                if self.primary {
                    main_island_gone();
                }
                for t in [HOVER_TIMER, LEAVE_TIMER, NOTICE_TIMER, TICK_TIMER, PAUSE_TIMER] {
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), t);
                    }
                }
            }
            if self.primary {if self.companion_situations {privacy::start(self.hwnd);}else{privacy::stop();}}
            return;
        }
        if self.island.is_none() {
            match Island::new(&self.gfx.compositor, &self.root) {
                Ok(mut isl) => {
                    isl.battery = self.battery;
                    self.island = Some(isl);
                    if self.primary {
                        BAR.store(self.hwnd.0 as isize, Ordering::SeqCst);
                    }
                }
                Err(e) => {
                    crate::log!("island: cannot create: {e}");
                    return;
                }
            }
        }
        let cfg = &self.cfg;
        let on = Enabled { progress: cfg.island_progress, media: cfg.island_media, battery: cfg.island_battery, privacy: cfg.island_privacy };
        let hwnd = self.hwnd;
        if !self.primary {
            // Another display's island: the main bar's listeners feed it,
            // starting from what they found so far.
            if let Some(isl) = self.island.as_mut() {
                isl.on = on;
                isl.attach_motion();
                if !on.media {
                    isl.inputs.track = None;
                    isl.art = None;
                }
                if !on.privacy {
                    isl.inputs.uses.clear();
                }
                if !on.battery {
                    isl.inputs.power = None;
                }
                isl.drawn = None;
            }
            if let Some(m) = LAST_MIRROR.with(|l| l.borrow().clone()) {
                self.island_take(&m);
            }
            self.island_refresh();
            return;
        }
        // New screenshots on the clipboard (opt-in): its own listener.
        crate::shot::set_enabled(cfg.island_screenshot);
        if let Some(isl) = self.island.as_mut() {
            isl.on = on;
            isl.attach_motion();
            // The listeners run only for what is switched on.
            if on.media && isl.media.is_none() {
                isl.media = Some(MediaWatch::start(hwnd, WM_ISLAND_MEDIA));
            } else if !on.media {
                if isl.media.take().is_some() {
                    // No one tells a PiP what plays any more.
                    crate::pip::media(None);
                }
                isl.inputs.track = None;
                isl.art = None;
            }
            if on.privacy||self.companion_situations {
                privacy::start(hwnd);
            } else {
                privacy::stop();
                isl.inputs.uses.clear();
            }
            if !on.battery {
                isl.inputs.power = None;
            }
            // Redraw with the new look.
            isl.drawn = None;
        }
        self.island_refresh();
    }

    /// The bar goes away.
    pub(super) fn island_close(&mut self) {
        if let Some(isl) = self.island.take() {
            isl.remove_from(&self.root);
            self.island_close_card();
            if self.primary {
                main_island_gone();
            }
        }
    }

    /// What this (the main) bar's island knows from its listeners, for
    /// the other bars' islands.
    pub(super) fn island_mirror(&self) -> Option<Mirror> {
        let isl = self.island.as_ref().filter(|_| self.primary)?;
        Some(Mirror { track: isl.inputs.track.clone(), pause: isl.pause.clone(), uses: isl.inputs.uses.clone(), art: isl.art.clone() })
    }

    /// Another display's island takes what the main bar's found.
    pub(super) fn island_follow(&mut self, m: &Mirror) {
        if self.primary {
            return;
        }
        let timeline_only=self.island.as_ref().is_some_and(|i|i.inputs.track!=m.track&&pause::same_pill(i.inputs.track.as_ref(),m.track.as_ref())&&i.inputs.uses==m.uses&&match (&i.art,&m.art){(None,None)=>true,(Some((a,_)),Some((b,_)))=>Arc::ptr_eq(a,b),_=>false});
        self.island_take(m);
        if timeline_only {self.island_timeline();return;}
        self.island_refresh();
    }

    /// Takes the main bar's findings (no redraw).
    fn island_take(&mut self, m: &Mirror) {
        let Some(isl) = self.island.as_mut() else { return };
        let same_art = match (&isl.art, &m.art) {
            (Some((a, _)), Some((b, _))) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same_art {
            isl.art = if isl.on.media { m.art.clone() } else { None };
            isl.drawn = None;
        }
        isl.inputs.track = if isl.on.media { m.track.clone() } else { None };
        isl.pause=if isl.on.media{m.pause.clone()}else{pause::Grace::default()};
        isl.inputs.uses = if isl.on.privacy { m.uses.clone() } else { Vec::new() };
    }

    /// Everything the island shows was drawn on a lost device, or the look
    /// changed: draw again (an open card closes; reopening draws it new).
    pub(super) fn island_redraw(&mut self) {
        let gfx = self.gfx.clone();
        if let Some(isl) = self.island.as_mut() {
            isl.drawn = None;
            isl.ring_key = None;
            isl.shade_for = None;
            isl.tone_brushes.borrow_mut().clear();
            // The art's pixels are kept: only its brush is made again.
            isl.art = isl.art.take().and_then(|(a, _)| {
                let s = gfx.surface_from_bgra(a.px, a.px, &a.pixels).ok()?;
                let b = gfx.compositor.CreateSurfaceBrushWithSurface(&s).ok()?;
                let _ = b.SetStretch(CompositionStretch::UniformToFill);
                Some((a, b.cast().ok()?))
            });
            if matches!(isl.phase, Phase::Expanded { .. }) {
                isl.phase = Phase::Pill;
                card::close();
            }
        }
        if self.primary {
            crate::pip::relook(&self.theme);
        }
        self.island_refresh();
    }

    /// A battery reading came (WM_POWERBROADCAST).
    pub(super) fn island_battery(&mut self) {
        let now = self.battery;
        let hwnd = self.hwnd;
        let Some(isl) = self.island.as_mut() else { return };
        let before = std::mem::replace(&mut isl.battery, now);
        let Some(now) = now else { return };
        if !isl.on.battery {
            return;
        }
        match model::battery_notice(before, now) {
            Some(p) => {
                isl.inputs.power = Some(p);
                isl.drawn = None;
                unsafe {
                    SetTimer(Some(hwnd), NOTICE_TIMER, model::notice_ms(p), None);
                }
            }
            None => {
                // Unplugged during the charging notice: it goes.
                if matches!(isl.inputs.power, Some(Power::Charging(_))) && !now.1 {
                    isl.inputs.power = None;
                }
            }
        }
        self.island_refresh();
    }

    /// Feeds and listeners: WM_ISLAND_MEDIA / _PRIVACY / _ACT (`lp`: the
    /// position of ACT_SEEK).
    pub(super) fn island_message(&mut self, msg: u32, wp: usize, lp: isize) {
        if self.island.is_none() {
            return;
        }
        match msg {
            WM_ISLAND_MEDIA => {
                let isl = self.island.as_mut().unwrap();
                let now = isl.media.as_ref().and_then(|m| m.now());
                let track = now.as_ref().map(|n| model::Track {
                    // As a person would write it (no "(1) " count, no
                    // " - YouTube"): the pill, the card and screen readers.
                    title: model::clean_title(&n.title, &n.app),
                    artist: model::clean_artist(&n.artist, &n.app),
                    playing: n.playing,
                    can_prev: n.prev,
                    can_next: n.next,
                    can_toggle: n.toggle,
                    app: n.app.clone(),
                    timeline: n.timeline,
                    gen: n.gen,
                    session: n.session,
                    seek_refused: n.seek_refused,
                });
                let art = now.and_then(|n| n.art);
                let same_art = match (&isl.art, &art) {
                    (Some((a, _)), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                };
                if !same_art {
                    isl.art = art.and_then(|a| {
                        let s = self.gfx.surface_from_bgra(a.px, a.px, &a.pixels).ok()?;
                        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&s).ok()?;
                        let _ = b.SetStretch(CompositionStretch::UniformToFill);
                        Some((a, b.cast().ok()?))
                    });
                    isl.drawn = None;
                }
                let timeline_only=same_art&&pause::same_pill(isl.inputs.track.as_ref(),track.as_ref());
                isl.pause.observe(isl.inputs.track.as_ref(),track.as_ref(),std::time::Instant::now());
                if timeline_only {isl.inputs.track=track;self.island_timeline();return;}
                // A PiP follows what plays (and goes when nothing does).
                crate::pip::media(track.as_ref().map(pip_media));
                isl.inputs.track = track;
            }
            WM_ISLAND_PRIVACY => {
                let isl = self.island.as_mut().unwrap();
                isl.inputs.uses = if isl.on.privacy { privacy::current() } else { Vec::new() };
            }
            WM_ISLAND_ACT => match wp {
                ACT_DISMISS => self.island_event(Event::Dismiss),
                ACT_LEAVE => unsafe {
                    SetTimer(Some(self.hwnd), LEAVE_TIMER, LEAVE_MS, None);
                },
                ACT_PREV | ACT_TOGGLE | ACT_NEXT | ACT_SEEK | ACT_PIP_TOGGLE if !self.primary => {
                    // Its card's buttons: the main bar holds the player.
                    let main = BAR.load(Ordering::SeqCst);
                    if main != 0 {
                        unsafe {
                            let _ = PostMessageW(Some(HWND(main as *mut _)), WM_ISLAND_ACT, WPARAM(wp), LPARAM(lp));
                        }
                    }
                }
                ACT_PREV | ACT_TOGGLE | ACT_NEXT | ACT_SEEK | ACT_PIP_TOGGLE => {
                    if let Some(m) = self.island.as_ref().and_then(|i| i.media.as_ref()) {
                        match wp {
                            ACT_PREV => m.prev(),
                            ACT_TOGGLE => m.toggle(),
                            ACT_PIP_TOGGLE => m.toggle_in(lp as u32),
                            ACT_SEEK => {
                                let (gen, ms) = model::unpack_seek(lp);
                                m.seek(gen, ms);
                            }
                            _ => m.next(),
                        }
                    }
                }
                ACT_PIP => self.island_pip(),
                ACT_PIN => {
                    if self.island.as_ref().is_some_and(|i| i.phase == (Phase::Expanded { pinned: false })) {
                        self.island_event(Event::Click);
                    }
                }
                n if (ACT_SHOT + 1..=ACT_SHOT + 4).contains(&n) => crate::shot::act(n - ACT_SHOT),
                n if (ACT_TIMER + 1..=ACT_TIMER + 4).contains(&n) => {
                    use crate::focus_timer::{command, Command};
                    command(match n - ACT_TIMER {
                        1 => Command::Toggle,
                        2 => Command::Skip,
                        3 => Command::Extend,
                        _ => Command::Stop,
                    });
                }
                _ => {}
            },
            _ => {}
        }
        self.island_refresh();
    }

    /// One of the island's timers.
    pub(super) fn island_timer(&mut self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), id);
        }
        match id {
            HOVER_TIMER => {
                let over = self.island_pointer_over();
                if let Some(isl) = self.island.as_mut() {
                    isl.hover_pending = false;
                }
                if over {
                    self.island_event(Event::Hover);
                }
            }
            LEAVE_TIMER => {
                if !self.island_pointer_over() && !card::pointer_inside() {
                    self.island_event(Event::Leave);
                }
            }
            NOTICE_TIMER => {
                if let Some(isl) = self.island.as_mut() {
                    isl.inputs.power = None;
                }
                self.island_refresh();
            }
            TICK_TIMER => {
                if let Some(isl) = self.island.as_mut() {
                    isl.ticking = false;
                }
                self.island_refresh();
            }
            PAUSE_TIMER => {
                if let Some(isl)=self.island.as_mut(){isl.pause_timer=None;}
                self.island_refresh();
            }
            _ => {}
        }
    }

    /// The pill's rect on screen (px), if it is shown.
    pub(in crate::topbar) fn island_screen_rect(&self) -> Option<RECT> {
        let isl = self.island.as_ref()?;
        if isl.phase == Phase::Hidden {
            return None;
        }
        let (cx, top, w, h) = isl.at?;
        let (left, top, right, bottom) = model::screen_rect((self.at.left, self.at.top), cx, top, w, h, self.scale);
        Some(RECT { left, top, right, bottom })
    }

    fn island_pointer_over(&self) -> bool {
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return false;
        }
        let inside = |r: RECT| pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom;
        self.island_screen_rect().is_some_and(inside)
    }

    /// Is bar point `x` (DIP) on the pill?
    pub(super) fn island_hit(&self, x: f32) -> bool {
        let Some(isl) = self.island.as_ref() else { return false };
        if isl.phase == Phase::Hidden {
            return false;
        }
        isl.at.is_some_and(|(cx, _, w, _)| x >= cx - w / 2.0 && x < cx + w / 2.0)
    }

    /// After sleep or unlock: the ring starts again from the timer's state
    /// (the compositor's clock and the wall clock may have parted).
    pub(super) fn island_resync(&mut self) {
        if let Some(isl) = self.island.as_mut() {
            isl.ring_key = None;
        }
        self.island_refresh();
    }

    /// The pointer moved on the bar (DIP x). True when it is on the pill.
    pub(super) fn island_move(&mut self, x: f32) -> bool {
        let over = self.island_hit(x);
        let hwnd = self.hwnd;
        let Some(isl) = self.island.as_mut() else { return false };
        if over != isl.hovered {
            isl.set_hover(over);
        }
        match isl.phase {
            Phase::Pill if over && !isl.hover_pending => {
                isl.hover_pending = true;
                unsafe {
                    SetTimer(Some(hwnd), HOVER_TIMER, model::HOVER_MS, None);
                }
            }
            Phase::Pill if !over && isl.hover_pending => {
                isl.hover_pending = false;
                unsafe {
                    let _ = KillTimer(Some(hwnd), HOVER_TIMER);
                }
            }
            Phase::Expanded { pinned: false } if !over => unsafe {
                SetTimer(Some(hwnd), LEAVE_TIMER, LEAVE_MS, None);
            },
            _ => {}
        }
        over
    }

    /// The pointer left the bar.
    pub(super) fn island_leave(&mut self) {
        let hwnd = self.hwnd;
        let Some(isl) = self.island.as_mut() else { return };
        if isl.hovered {
            isl.set_hover(false);
        }
        if isl.hover_pending {
            isl.hover_pending = false;
            unsafe {
                let _ = KillTimer(Some(hwnd), HOVER_TIMER);
            }
        }
        if isl.phase == (Phase::Expanded { pinned: false }) {
            unsafe {
                SetTimer(Some(hwnd), LEAVE_TIMER, LEAVE_MS, None);
            }
        }
    }

    /// The left button went down at bar x (DIP). True: on the pill (the
    /// island takes it).
    pub(super) fn island_down(&mut self, x: f32) -> bool {
        let on = self.island_hit(x);
        if let Some(isl) = self.island.as_mut() {
            isl.pressed = on;
            if on {
                // Pressed at once (6.3): the fill now, the size on SNAP.
                isl.apply_touch(false);
            }
        }
        on
    }

    /// The left button came up at bar x (DIP). True: the island took it
    /// (a click when it went down there too).
    pub(super) fn island_up(&mut self, x: f32) -> bool {
        let Some(isl) = self.island.as_mut() else { return false };
        if !std::mem::take(&mut isl.pressed) {
            return false;
        }
        isl.apply_touch(false);
        if self.island_hit(x) {
            self.island_click();
        }
        true
    }

    /// A click on the pill (or Invoke from a screen reader).
    pub(super) fn island_click(&mut self) {
        if let Some(isl) = self.island.as_mut() {
            if isl.hover_pending {
                isl.hover_pending = false;
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), HOVER_TIMER);
                }
            }
        }
        self.island_event(Event::Click);
    }

    /// Expand or collapse from a screen reader.
    pub(super) fn island_expand(&mut self, open: bool) {
        let phase = self.island.as_ref().map(|i| i.phase);
        match (phase, open) {
            (Some(Phase::Pill), true) | (Some(Phase::Expanded { pinned: false }), true) => self.island_event(Event::Click),
            (Some(Phase::Expanded { .. }), false) => self.island_event(Event::Dismiss),
            _ => {}
        }
    }

    pub(super) fn island_event(&mut self, e: Event) {
        let Some(isl) = self.island.as_mut() else { return };
        let before = isl.phase;
        isl.phase = model::step(before, e);
        if isl.phase != before {
            self.island_phase_changed(before);
        }
    }

    /// Opens, pins or closes the card for the new phase.
    fn island_phase_changed(&mut self, before: Phase) {
        let Some(phase) = self.island.as_ref().map(|i| i.phase) else { return };
        match phase {
            Phase::Hidden | Phase::Pill => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), LEAVE_TIMER);
                }
                // A paused track was held only for the card: the pill as
                // it is without it first, so the card goes back into that
                // one (the same plate, backwards). Nothing to go back
                // into (nothing live any more, no room): it only fades.
                self.island_refresh();
                let pill = self.island.as_ref().is_some_and(|i| i.phase == Phase::Pill).then(|| self.island_pill(true)).flatten();
                match pill {
                    Some(pill) => card::close_into(pill),
                    None => card::close(),
                }
            }
            Phase::Expanded { pinned } => {
                if matches!(before, Phase::Expanded { .. }) && card::is_open() {
                    if pinned {
                        card::pin();
                    }
                } else {
                    self.island_open_card(pinned);
                }
            }
        }
        self.publish_a11y();
    }

    fn card_data(&self) -> Option<card::Data> {
        let isl = self.island.as_ref()?;
        // The PiP button: the playing app has a window to show.
        let pip = isl.live.iter().any(|a| matches!(a, Activity::Media(t) if crate::pip::source_for(&t.app, &t.title).is_some()));
        let live = isl.live.clone();
        let icons = isl.live.iter().filter_map(|a| if let Activity::Progress(j) = a { Some(j) } else { None }).flatten().filter_map(|j| Some((j.key.clone(), icon_of(&j.key)?))).collect();
        Some(card::Data { pip, live, icons, art: isl.art.as_ref().map(|(a, _)| a.clone()) })
    }

    /// What plays, in a picture-in-picture window on this bar's display
    /// (the card goes).
    fn island_pip(&mut self) {
        let Some(tr) = self.island.as_ref().and_then(|i| i.inputs.track.clone()) else { return };
        self.island_event(Event::Dismiss);
        let Some(source) = crate::pip::find(&tr.app, &tr.title) else { return };
        let media = Some(pip_media(&tr));
        crate::pip::open(crate::pip::Request { gfx: self.gfx.clone(), theme: self.theme.clone(), source, app: tr.app, monitor: self.hmon, media });
    }

    fn island_open_card(&mut self, pinned: bool) {
        let opened = match (self.island_pill(false), self.card_data()) {
            (Some(pill), Some(data)) => card::open(card::Request { gfx: self.gfx.clone(), theme: self.theme.clone(), scale: self.scale, pill, bar: self.hwnd, data, pinned }),
            // Not placed (no room on the bar): nothing to hang it from.
            _ => false,
        };
        if !opened {
            if let Some(isl) = self.island.as_mut() {
                isl.phase = Phase::Pill;
            }
        }
    }

    /// Timeline/capability changes update only the expanded controls. The
    /// pill does not redraw, lay out, or start a compositor transition.
    fn island_timeline(&mut self){
        if let Some(isl)=self.island.as_mut(){if let Some(track)=&isl.inputs.track {for a in &mut isl.live {if let Activity::Media(t)=a {*t=track.clone();}}}}
        if self.island.as_ref().is_some_and(|i|matches!(i.phase,Phase::Expanded{..})){if let Some(data)=self.card_data(){card::update(data,self.island_pill(true));}}
    }

    /// Something changed: what is live, the phase, the pill, the card.
    pub(super) fn island_refresh(&mut self) {
        // Hidden for a fullscreen app, or shown on another display now.
        let shown_bar = !self.hidden_full && self.island_here();
        let Some(isl) = self.island.as_mut() else { return };
        feed_inputs(&mut isl.inputs);
        let now=std::time::Instant::now();
        if !isl.on.media {isl.pause=pause::Grace::default();}
        let deadline=isl.pause.deadline(now);
        if isl.pause_timer!=deadline {
            unsafe {let _=KillTimer(Some(self.hwnd),PAUSE_TIMER);if let Some(until)=deadline {SetTimer(Some(self.hwnd),PAUSE_TIMER,until.saturating_duration_since(now).as_millis().min(u32::MAX as u128-1) as u32+1,None);}}
            isl.pause_timer=deadline;
        }
        let keep_paused = matches!(isl.phase, Phase::Expanded { .. })||isl.pause.keeps(now);
        isl.live = model::live(&isl.inputs, isl.on, keep_paused);
        let before = isl.phase;
        isl.phase = model::step(before, Event::Shown(shown_bar && !isl.live.is_empty()));
        let phase = isl.phase;
        if phase != before {
            if phase == Phase::Hidden {
                card::close();
                isl.set_hover(false);
                isl.hover_pending = false;
            }
            let _ = isl.root.SetOpacity(if phase == Phase::Hidden { 0.0 } else { 1.0 });
            isl.apply_touch(true);
        }
        self.island_draw();
        self.island_layout();
        let Some(isl) = self.island.as_mut() else { return };
        if matches!(isl.phase, Phase::Expanded { .. }) {
            if let Some(d) = self.card_data() {
                card::update(d, self.island_pill(true));
            }
        }
        // Names change with the numbers (free when no reader listens).
        self.publish_a11y();
    }

    /// Draws the pill's content for the top activity (when it changed).
    fn island_draw(&mut self) {
        let scale = self.scale;
        let look = (scale.to_bits(), self.theme.is_light, self.theme.high_contrast);
        let Some(isl) = self.island.as_ref() else { return };
        let Some(top) = isl.live.first() else { return };
        let (words, trail_words) = model::pill_words(top);
        let words = model::truncate(&words, 80);
        let dots: Vec<Tone> = isl.live.iter().skip(1).take(model::MAX_DOTS).map(model::shown_tone).collect();
        let lead_id = match top {
            Activity::Progress(j) => j.first().map(|j| format!("{}|{:?}", j.key, icon_of(&j.key).map(|b| b.as_raw()))).unwrap_or_default(),
            Activity::Media(_) => isl.art.as_ref().map(|(a, _)| format!("{:p}", Arc::as_ptr(a))).unwrap_or_default(),
            Activity::Screenshot(s) => s.path.clone(),
            _ => String::new(),
        };
        let pct = match top {
            Activity::Progress(j) => model::percent(model::overall(j)) as i32,
            Activity::Timer(tm) => tm.focus.as_ref().map(|v| v.phase as i32).unwrap_or(-1),
            _ => -1,
        };
        let timer_phase = match top {
            Activity::Timer(tm) => tm.focus.as_ref().map(|v| v.phase),
            _ => None,
        };
        let timer_tone = matches!(top, Activity::Timer(_)).then_some(Tone::Ink);
        let key = format!("{:?}|{words}|{trail_words}|{pct}|{lead_id}|{dots:?}", top.kind());
        if isl.drawn_for == look && isl.drawn.as_ref().is_some_and(|d| d.key == key) {
            return;
        }
        let kind = top.kind();
        let (fill, ink, dim, _) = pill_colors(&self.theme);
        // The leading picture.
        let mut lead_round = false;
        let glyph = |bar: &TopBar, g: Glyph, c: Rgba| bar.island_glyph(g, c);
        let lead: Option<(CompositionBrush, f32)> = match top {
            Activity::Progress(j) => match j.first().and_then(|j| icon_of(&j.key)) {
                Some(b) => Some((b, LEAD)),
                None => glyph(self, if j.first().is_some_and(|j| j.files) { G_COPY } else { G_DOWNLOAD }, ink),
            },
            Activity::Media(_) => match &isl.art {
                Some((_, b)) => {
                    lead_round = true;
                    Some((b.clone(), ART))
                }
                None => glyph(self, G_MUSIC, ink),
            },
            Activity::Battery(Power::Charging(_)) => glyph(self, G_BOLT, ink),
            Activity::Battery(Power::Low(_)) => glyph(self, G_BATTERY_LOW, tone_rgba(model::shown_tone(top), &self.theme)),
            Activity::Timer(_) => glyph(self, G_TIMER, ink),
            Activity::Privacy(u) => self.island_dot(model::privacy_tone(u), 8.0).map(|b| (b, 8.0)),
            Activity::Screenshot(_) => glyph(self, G_SHOT, ink),
        };
        // A screenshot leads with its own thumbnail (rounded).
        let lead = match top {
            Activity::Screenshot(s) => match self.island_shot_lead(&s.path) {
                Some(b) => {
                    lead_round = true;
                    Some((b, ART))
                }
                None => lead,
            },
            _ => lead,
        };
        let trail = match top {
            Activity::Media(_) => Trail::Eq,
            Activity::Timer(tm) if tm.focus.is_some() && !trail_words.is_empty() => match self.island_number(&trail_words, ink) {
                Some((b, w, h)) => Trail::Ring(b, w, h),
                None => Trail::None,
            },
            Activity::Progress(j) => match self.island_ring(model::overall(j), &trail_words) {
                Some((b, w, h)) => Trail::Surface(b, w, h),
                None => Trail::None,
            },
            _ if !trail_words.is_empty() => match self.island_text(&trail_words, SMALL_SIZE, false, dim, 200.0) {
                Some((b, w, h)) => Trail::Surface(b, w, h),
                None => Trail::None,
            },
            _ => Trail::None,
        };
        // The most the title may take: what the widest pill leaves it.
        let others = Content { lead: lead.as_ref().map(|l| l.1).unwrap_or(0.0), trail: trail.width(), dots: dots.len(), ..Default::default() };
        let text_max = model::text_room(&others, model::PILL_MAX_W);
        let title = if text_max > 0.0 { self.island_title(&words, ink, text_max) } else { None };
        let (text, baseline) = match title {
            Some((b, w, h, base)) => (Some((b, w, h)), base),
            None => (None, 0.0),
        };
        let full_w = text.as_ref().map(|t| t.1).unwrap_or(0.0);
        let fills = [Fill::Rest, Fill::Hover, Fill::Pressed].map(|f| pill_fall(&self.theme, f));
        let _ = fill;
        let border = pill_colors(&self.theme).3;
        let coat = pill_coat(&self.theme, self.scale);
        let isl = self.island.as_mut().unwrap();
        // Colors of the background, its border, and the bars (the shadow
        // and the contact ring are made in layout, for the pill's height).
        let c = isl.c.clone();
        isl.fills = Some(fills);
        let now = if isl.phase != Phase::Hidden { fill_plan(isl.touch()).0 } else { Fill::Rest };
        fall_to(&c, &isl.fill_stops, fills[now as usize], 0);
        // Its border: the rim (the light along its top: pill_coat, Flat
        // v2's with its edge light all round), else the border's own color
        // (high contrast's outline: apply_touch).
        isl.rim = None;
        isl.rim_w = coat.rim.map_or(1.0, |r| r.1);
        match coat.rim {
            Some((stops, w)) => {
                let rim = (|| -> Result<CompositionLinearGradientBrush> {
                    let b = c.CreateLinearGradientBrush()?;
                    b.SetMappingMode(CompositionMappingMode::Absolute)?;
                    for (at, col) in crate::theme::RIM_STOPS.iter().zip(stops) {
                        b.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(*at, color(col))?)?;
                    }
                    isl.edge.SetStrokeBrush(&b)?;
                    isl.edge.SetStrokeThickness(w)?;
                    Ok(b)
                })();
                isl.rim = rim.ok();
            }
            None => {
                if let Ok(b) = c.CreateColorBrushWithColor(color(border)) {
                    let _ = isl.edge.SetStrokeBrush(&b);
                    let _ = isl.edge.SetStrokeThickness(1.0);
                }
            }
        }
        isl.edges = self.theme.hc.filter(|_| self.theme.high_contrast).map(|h| (h.text, h.highlight));
        isl.shade_for = None;
        let bar_c = eq_color(&self.theme);
        if let Ok(b) = c.CreateColorBrushWithColor(color(bar_c)) {
            for v in &isl.bars {
                let _ = v.SetBrush(&b);
            }
        }
        // The ring: a faint track, the arc in the phase's color (the
        // pill's ink in high contrast).
        if let Some(t) = timer_tone {
            let arc = if self.theme.high_contrast { ink } else { tone_rgba(t, &self.theme) };
            let track = Rgba { a: if self.theme.high_contrast { 0x80 } else { 0x40 }, ..dim };
            if let (Ok(a), Ok(t)) = (c.CreateColorBrushWithColor(color(arc)), c.CreateColorBrushWithColor(color(track))) {
                let _ = isl.ring_arc.SetStrokeBrush(&a);
                let _ = isl.ring_track.SetStrokeBrush(&t);
            }
            isl.ring_colors = (track, arc);
        }
        let swap = isl.drawn.as_ref().map(|d| d.kind) != Some(kind) || isl.drawn_for != look;
        isl.drawn_for = look;
        let new_phase = timer_phase.is_some() && isl.timer_phase.is_some() && timer_phase != isl.timer_phase;
        isl.timer_phase = timer_phase;
        isl.drawn = Some(Drawn { key, kind, lead, lead_round, text, baseline, words, full_max: text_max, full_w, text_max, trail, dots });
        if swap {
            // Cross-fade to the other layer.
            let _ = isl.layers[isl.cur].root.SetOpacity(0.0);
            isl.cur = 1 - isl.cur;
            let _ = isl.layers[isl.cur].root.SetOpacity(1.0);
        }
        let drawn = isl.drawn.as_ref().unwrap();
        let l = &isl.layers[isl.cur];
        match &drawn.lead {
            // Its size (fitted to the pill's height) is set in layout.
            Some((b, _)) => {
                let _ = l.lead.SetBrush(b);
                let clip = if drawn.lead_round { l.lead_clip.cast::<windows::UI::Composition::CompositionClip>().ok() } else { None };
                let _ = l.lead.SetClip(clip.as_ref());
            }
            None => {
                let _ = l.lead.SetSize(Vector2 { X: 0.0, Y: 0.0 });
            }
        }
        match &drawn.text {
            Some((b, w, h)) => {
                let _ = l.text.SetBrush(b);
                let _ = l.text.SetSize(Vector2 { X: *w, Y: *h });
            }
            None => {
                let _ = l.text.SetSize(Vector2 { X: 0.0, Y: 0.0 });
            }
        }
        match &drawn.trail {
            Trail::Surface(b, w, h) | Trail::Ring(b, w, h) => {
                let _ = l.trail.SetBrush(b);
                let _ = l.trail.SetSize(Vector2 { X: *w, Y: *h });
            }
            _ => {
                let _ = l.trail.SetSize(Vector2 { X: 0.0, Y: 0.0 });
            }
        }
        // A notice arrives with a pop of its glyph; so does the focus
        // timer's next phase (its alert, with the chime).
        if (swap && matches!(kind, Kind::Battery | Kind::Screenshot)) || new_phase {
            pop(&c, &l.lead);
        }
        let tones: Vec<Tone> = drawn.dots.clone();
        for (i, d) in self.island.as_ref().unwrap().dots.iter().enumerate() {
            match tones.get(i) {
                Some(t) => {
                    if let Some(b) = self.island_dot(*t, model::DOT) {
                        let _ = d.SetBrush(&b);
                    }
                    let _ = d.SetOpacity(1.0);
                }
                None => {
                    let _ = d.SetOpacity(0.0);
                }
            }
        }
    }

    /// Places the pill between the cells (from layout()). The geometry is
    /// model::frame's (tested): the background, the content's clip and
    /// every part come from the same rectangles.
    pub(super) fn island_layout(&mut self) {
        let cells: Vec<(f32, f32)> = self.slots.iter().filter(|s| s.w > 0.0).map(|s| (s.x, s.w)).collect();
        let (width, height, rtl) = (self.width, self.height, crate::i18n::rtl());
        let Some(isl) = self.island.as_ref() else { return };
        let Some(d) = isl.drawn.as_ref() else {
            self.island_unplaced();
            return;
        };
        // The pill: the bar's height less 6 (model::PILL_INSET each side),
        // on whole device pixels (model::rest_height: its rim's every side
        // on a pixel's middle).
        let h = model::rest_height(height, self.scale);
        let mut content = Content {
            // Fitted to the pill's height (a short bar: smaller art).
            lead: d.lead.as_ref().map(|l| model::lead_fit(l.1, h)).unwrap_or(0.0),
            text: d.full_w,
            text_h: 0.0,
            trail: d.trail.width(),
            trail_h: 0.0,
            dots: d.dots.len(),
            baseline: None,
        };
        let want = model::pill_width(&content);
        let need = model::pill_need(&content);
        // No room: the pill waits (hidden) until there is.
        let Some((mut x, mut w)) = model::place(width, &cells, want, need) else {
            self.island_unplaced();
            return;
        };
        // Its width on whole device pixels too (model::rest_height).
        w = model::snap(w, self.scale);
        // Narrower than it wants: the title is cut to fit (and uncut when
        // the room comes back); too little room for it: left out, and the
        // pill as wide as the rest.
        let max = if w + 0.5 < want && d.full_w > 0.0 { model::text_room(&content, w) } else { d.full_max };
        if max <= 0.0 && d.full_w > 0.0 {
            let rest = model::pill_width(&Content { text: 0.0, ..content });
            if let Some((x2, w2)) = model::place(width, &cells, rest, need) {
                (x, w) = (x2, model::snap(w2, self.scale));
            }
        }
        if (max - d.text_max).abs() > 0.5 {
            let words = d.words.clone();
            let ink = pill_colors(&self.theme).1;
            let title = if max > 0.0 { self.island_title(&words, ink, max) } else { None };
            let isl = self.island.as_mut().unwrap();
            if let Some(d) = isl.drawn.as_mut() {
                d.text_max = max;
                let l = &isl.layers[isl.cur];
                match title {
                    Some((b, tw, th, base)) => {
                        let _ = l.text.SetBrush(&b);
                        let _ = l.text.SetSize(Vector2 { X: tw, Y: th });
                        d.text = Some((b, tw, th));
                        d.baseline = base;
                    }
                    None => {
                        let _ = l.text.SetSize(Vector2 { X: 0.0, Y: 0.0 });
                        d.text = None;
                    }
                }
            }
        }
        let isl = self.island.as_mut().unwrap();
        let Some(d) = isl.drawn.as_ref() else { return };
        // On whole pixels (its 1 px border and text crisp at 125 / 175 %),
        // its middle on a device pixel (not a whole DIP: 1.25 px at 125 %).
        let (cx, top, _, _) = model::rest_at(x, w, height, self.scale);
        let (text_w, text_h) = d.text.as_ref().map(|t| (t.1, t.2)).unwrap_or((0.0, 0.0));
        let trail_h = match &d.trail {
            Trail::Surface(_, _, th) | Trail::Ring(_, _, th) => *th,
            Trail::Eq => EQ_H,
            Trail::None => 0.0,
        };
        content.text = text_w;
        content.text_h = text_h;
        content.trail_h = trail_h;
        // The music bars stand on the title's baseline.
        content.baseline = matches!(d.trail, Trail::Eq).then_some(d.baseline);
        let f = model::frame(rtl, w, h, &content, self.scale);
        // Placed for the first time (new, or back from no room): set, not
        // eased; the animations come back for the next change.
        let animate = model::layout_animates(isl.at.is_some(), motion());
        if !animate {
            isl.layout_motion(false);
        }
        isl.at = Some((cx, top, w, h));
        let _ = isl.root.SetOpacity(if isl.phase == Phase::Hidden { 0.0 } else { 1.0 });
        let _ = isl.root.SetOffset(Vector3 { X: cx, Y: top, Z: 0.0 });
        let _ = isl.root.SetCenterPoint(Vector3 { X: 0.0, Y: h / 2.0, Z: 0.0 });
        // The pill's shape: the content's clip. Under it the fill (a
        // rectangle past any pill: the clip shapes it) and the border.
        let _ = isl.clip_geom.SetOffset(Vector2 { X: f.pill.x, Y: f.pill.y });
        let _ = isl.clip_geom.SetSize(Vector2 { X: f.pill.w, Y: f.pill.h });
        let _ = isl.clip_geom.SetCornerRadius(Vector2 { X: h / 2.0, Y: h / 2.0 });
        let _ = isl.fill_v.SetOffset(Vector3 { X: f.fill.x, Y: f.fill.y, Z: 0.0 });
        let _ = isl.fill_v.SetSize(Vector2 { X: f.fill.w, Y: f.fill.h });
        // The fall from the pill's top to its bottom (the fill's own DIP),
        // the rim's down the border's visual.
        let (fa, fb) = model::fall_span(&f);
        let _ = isl.fill.SetStartPoint(Vector2 { X: 0.0, Y: fa });
        let _ = isl.fill.SetEndPoint(Vector2 { X: 0.0, Y: fb });
        if let Some(rim) = &isl.rim {
            let _ = rim.SetStartPoint(Vector2 { X: 0.0, Y: f.pill.y - f.edge_box.y });
            let _ = rim.SetEndPoint(Vector2 { X: 0.0, Y: f.pill.y - f.edge_box.y + f.pill.h });
        }
        let _ = isl.edge_v.SetOffset(Vector3 { X: f.edge_box.x, Y: f.edge_box.y, Z: 0.0 });
        let _ = isl.edge_v.SetSize(Vector2 { X: f.edge_box.w, Y: f.edge_box.h });
        // (f.edge lies 0.5 DIP in: a 1 DIP stroke's middle. A thinner rim,
        // one device pixel, lies its own half in: on the pixel's middle.)
        let e = edge_for(&f, isl.rim_w);
        let _ = isl.edge_geom.SetOffset(Vector2 { X: e.x, Y: e.y });
        let _ = isl.edge_geom.SetSize(Vector2 { X: e.w, Y: e.h });
        let _ = isl.edge_geom.SetCornerRadius(Vector2 { X: e.h / 2.0, Y: e.h / 2.0 });
        // The light (2.6): E1 under the pill, a visual its reach larger on
        // every side filled with the shadows' patch, and the contact ring
        // just outside it. Made again for another height, scale or look.
        let reach = crate::theme::Elevation::E1.reach();
        let _ = isl.shadow_v.SetOffset(Vector3 { X: f.shadow.x - reach, Y: f.shadow.y - reach, Z: 0.0 });
        let _ = isl.shadow_v.SetSize(Vector2 { X: f.shadow.w + 2.0 * reach, Y: f.shadow.h + 2.0 * reach });
        let ((ox, oy), (cw, ch), cr, ck) = morph::contact_ring(f.pill.w, f.pill.h, h / 2.0);
        let _ = isl.contact_v.SetOffset(Vector3 { X: f.pill.x - 1.0, Y: f.pill.y - 1.0, Z: 0.0 });
        let _ = isl.contact_v.SetSize(Vector2 { X: f.edge_box.w + 2.0, Y: f.edge_box.h + 2.0 });
        let _ = isl.contact_geom.SetOffset(Vector2 { X: ox, Y: oy });
        let _ = isl.contact_geom.SetSize(Vector2 { X: cw, Y: ch });
        let _ = isl.contact_geom.SetCornerRadius(Vector2 { X: cr, Y: cr });
        let shadows = crate::effects::items().shadows;
        let key = (h.to_bits(), self.scale.to_bits(), self.theme.is_light, self.theme.high_contrast, shadows);
        if isl.shade_for != Some(key) {
            isl.shade_for = Some(key);
            let layers = pill_shadows(&self.theme, shadows);
            let patch: Option<CompositionBrush> = if layers.is_empty() { None } else { morph::shadow_patch(&self.gfx, &layers, h / 2.0, reach, self.scale).ok().and_then(|b| b.cast().ok()) };
            let _ = isl.shadow_v.SetBrush(patch.as_ref());
            let contact = self.theme.contact();
            let _ = isl.contact.SetStrokeThickness(ck);
            match isl.c.CreateColorBrushWithColor(color(contact)) {
                Ok(b) if contact.a > 0 => {
                    let _ = isl.contact.SetStrokeBrush(&b);
                }
                _ => {
                    let _ = isl.contact.SetStrokeBrush(None::<&CompositionBrush>);
                }
            }
        }
        // The parts.
        let l = &isl.layers[isl.cur];
        let lead = f.lead.w;
        let _ = l.lead.SetSize(Vector2 { X: lead, Y: lead });
        let _ = l.lead.SetCenterPoint(Vector3 { X: lead / 2.0, Y: lead / 2.0, Z: 0.0 });
        let _ = l.lead_geom.SetSize(Vector2 { X: lead, Y: lead });
        let r = lead_radius(lead, h);
        let _ = l.lead_geom.SetCornerRadius(Vector2 { X: r, Y: r });
        let _ = l.lead.SetOffset(Vector3 { X: f.lead.x, Y: f.lead.y, Z: 0.0 });
        let _ = l.text.SetOffset(Vector3 { X: f.text.x, Y: f.text.y, Z: 0.0 });
        let rx = f.trail.x;
        let trail_w = f.trail.w;
        let px = |v: f32| model::snap(v, self.scale);
        let y = |hh: f32| px((h - hh) / 2.0);
        // The timer: the ring at the trailing part's reading start, the
        // time after it (mirrored right to left).
        let (text_at, ring_at) = match &d.trail {
            Trail::Ring(_, tw, _) if rtl => (rx, rx + tw + RING_GAP),
            Trail::Ring(..) => (rx + RING + RING_GAP, rx),
            _ => (rx, rx),
        };
        let _ = l.trail.SetOffset(Vector3 { X: px(text_at), Y: f.trail.y, Z: 0.0 });
        let _ = isl.ring.SetOffset(Vector3 { X: px(ring_at), Y: y(RING), Z: 0.0 });
        let placed = Placed { frame: f.clone(), text_at: px(text_at), ring_at: px(ring_at), ring_y: y(RING) };
        // The music bars where the trailing part goes.
        let eq_on = matches!(d.trail, Trail::Eq);
        let _ = isl.eq.SetOpacity(if eq_on { 1.0 } else { 0.0 });
        let _ = isl.eq.SetOffset(Vector3 { X: rx, Y: f.trail.y, Z: 0.0 });
        for (i, b) in isl.bars.iter().enumerate() {
            let bx = i as f32 * (EQ_W + EQ_GAP);
            let bx = if rtl { trail_w - bx - EQ_W } else { bx };
            let _ = b.SetSize(Vector2 { X: EQ_W, Y: EQ_H });
            let _ = b.SetOffset(Vector3 { X: bx, Y: 0.0, Z: 0.0 });
            let _ = b.SetCenterPoint(Vector3 { X: EQ_W / 2.0, Y: EQ_H, Z: 0.0 });
            if !isl.eq_running {
                let _ = b.SetScale(Vector3 { X: 1.0, Y: 0.55, Z: 1.0 });
            }
        }
        // The dots after everything, at the end of the reading order.
        for (dot, r) in isl.dots.iter().zip(&f.dots) {
            let _ = dot.SetOffset(Vector3 { X: r.x, Y: r.y, Z: 0.0 });
        }
        // Music bars: only while music plays and the pill is placed and on
        // screen, with motion.
        let run = isl.phase != Phase::Hidden && eq_on && matches!(isl.live.first(), Some(Activity::Media(t)) if t.playing);
        // The focus timer's ring, while it leads the pill on screen.
        let shown = isl.phase != Phase::Hidden;
        let top_timer = match (isl.live.first(), &d.trail) {
            (Some(Activity::Timer(tm)), Trail::Ring(..)) => Some(tm.clone()),
            _ => None,
        };
        if !animate {
            isl.layout_motion(motion());
        }
        isl.placed = Some(placed);
        isl.set_eq(run && motion());
        isl.set_ring(top_timer.as_ref(), shown);
        self.island_tick();
    }

    /// Sets or drops the seconds' tick: while a running focus timer leads
    /// the pill on screen, or shows in the open card.
    fn island_tick(&mut self) {
        let hwnd = self.hwnd;
        let Some(isl) = self.island.as_mut() else { return };
        let card = matches!(isl.phase, Phase::Expanded { .. });
        let running = |a: &Activity| matches!(a, Activity::Timer(tm) if tm.running && tm.focus.is_some());
        let want = isl.phase != Phase::Hidden && isl.at.is_some() && (isl.live.first().is_some_and(running) || (card && isl.live.iter().any(running)));
        let left = isl.inputs.timer.as_ref().and_then(|t| t.focus.as_ref()).map(|v| v.left_ms).unwrap_or(0);
        if want && !isl.ticking {
            isl.ticking = true;
            unsafe {
                // Just past the second the shown time changes.
                SetTimer(Some(hwnd), TICK_TIMER, crate::focus_timer::model::ms_to_next_second(left) + 15, None);
            }
        } else if !want && isl.ticking {
            isl.ticking = false;
            unsafe {
                let _ = KillTimer(Some(hwnd), TICK_TIMER);
            }
        }
    }

    /// The pill has no place (no room, or nothing drawn): it is not on
    /// screen, so nothing of it runs, and a card hanging from it closes.
    fn island_unplaced(&mut self) {
        let hwnd = self.hwnd;
        let Some(isl) = self.island.as_mut() else { return };
        let _ = isl.root.SetOpacity(0.0);
        isl.at = None;
        isl.placed = None;
        isl.set_eq(false);
        isl.set_ring(None, false);
        if isl.ticking {
            isl.ticking = false;
            unsafe {
                let _ = KillTimer(Some(hwnd), TICK_TIMER);
            }
        }
        isl.hover_pending = false;
        if isl.hovered {
            isl.set_hover(false);
        }
        unsafe {
            let _ = KillTimer(Some(hwnd), HOVER_TIMER);
            let _ = KillTimer(Some(hwnd), LEAVE_TIMER);
        }
        if matches!(isl.phase, Phase::Expanded { .. }) {
            isl.phase = Phase::Pill;
            card::close();
        }
    }

    /// The pill as the card's plate shows it while it stands in for it
    /// (card.rs, morph.rs): where it is on screen, its parts where
    /// island_layout put them (the brushes are shared, nothing is drawn
    /// again) and the visual to hide meanwhile. `settled`: at the size it
    /// comes to rest at (the plate going back into it); else as it is
    /// shown this moment (a hover or a press still springing: the plate
    /// starts exactly on it).
    fn island_pill(&self, settled: bool) -> Option<card::Pill> {
        let isl = self.island.as_ref()?;
        let rect = self.island_screen_rect()?;
        let (d, p) = (isl.drawn.as_ref()?, isl.placed.as_ref()?);
        let (_, _, _, h) = isl.at?;
        let f = &p.frame;
        let rtl = crate::i18n::rtl();
        let mut parts = Vec::new();
        let picture = |b: &CompositionBrush, x: f32, y: f32, w: f32, h: f32, radius: f32| card::Part::Picture { brush: b.clone(), x, y, w, h, radius };
        if let Some((b, _)) = &d.lead {
            parts.push(picture(b, f.lead.x, f.lead.y, f.lead.w, f.lead.h, if d.lead_round { lead_radius(f.lead.w, h) } else { 0.0 }));
        }
        if let Some((b, w, th)) = &d.text {
            parts.push(picture(b, f.text.x, f.text.y, *w, *th, 0.0));
        }
        match &d.trail {
            Trail::Surface(b, w, th) | Trail::Ring(b, w, th) => parts.push(picture(b, p.text_at, f.trail.y, *w, *th, 0.0)),
            // The music bars, at rest (they are gone in a tenth of a second).
            Trail::Eq => {
                for i in 0..EQ_BARS {
                    let bx = i as f32 * (EQ_W + EQ_GAP);
                    let bx = if rtl { f.trail.w - bx - EQ_W } else { bx };
                    parts.push(card::Part::Fill { color: eq_color(&self.theme), x: f.trail.x + bx, y: f.trail.y + EQ_H * 0.45, w: EQ_W, h: EQ_H * 0.55 });
                }
            }
            Trail::None => {}
        }
        if let (Trail::Ring(..), Some(Activity::Timer(tm))) = (&d.trail, isl.live.first()) {
            parts.push(card::Part::Ring { x: p.ring_at, y: p.ring_y, side: RING, stroke: RING_STROKE, track: isl.ring_colors.0, arc: isl.ring_colors.1, share: tm.ratio() });
        }
        for (tone, r) in d.dots.iter().zip(&f.dots) {
            if let Some(b) = self.island_dot(*tone, model::DOT) {
                parts.push(picture(&b, r.x, r.y, r.w, r.h, 0.0));
            }
        }
        let scale = if settled { scale_target(true, Touch { hovered: isl.hovered, pressed: false }, f.pill.w, h) } else { isl.scale.state(std::time::Instant::now()).0 };
        Some(card::Pill { rect, scale, parts, shell: isl.shell.cast().ok()? })
    }

    // ---- drawing

    /// A glyph centered in a LEAD box.
    fn island_glyph(&self, g: Glyph, c: Rgba) -> Option<(CompositionBrush, f32)> {
        let s = self.scale;
        let side = (LEAD * s).round().max(1.0);
        let col = rgba_d2d(c);
        let surface = self
            .gfx
            .draw(side as u32, side as u32, |dc, ox, oy| unsafe { crate::glyphs::draw_device(dc, g, &crate::glyphs::fitted(g, ox, oy, side, 14.0 * s), col) })
            .ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        Some((b.cast().ok()?, side / s))
    }

    /// The screenshot's thumbnail as a brush (filling a square, centered).
    fn island_shot_lead(&self, path: &str) -> Option<CompositionBrush> {
        let (w, h, px) = crate::shot::thumb_for(path)?;
        let s = self.gfx.surface_from_bgra(w, h, &px).ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&s).ok()?;
        let _ = b.SetStretch(CompositionStretch::UniformToFill);
        b.cast().ok()
    }

    /// A filled circle `d` DIP wide in a tone (cached).
    fn island_dot(&self, t: Tone, d: f32) -> Option<CompositionBrush> {
        let k = (t, d.to_bits());
        if let Some(b) = self.island.as_ref().and_then(|i| i.tone_brushes.borrow().get(&k).cloned()) {
            return Some(b);
        }
        let s = self.scale;
        let px = (d * s).ceil().max(1.0);
        let col = rgba_d2d(tone_rgba(t, &self.theme));
        let surface = self
            .gfx
            .draw(px as u32, px as u32, |dc, ox, oy| unsafe {
                let b = dc.CreateSolidColorBrush(&col, None)?;
                dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: ox + px / 2.0, Y: oy + px / 2.0 }, radiusX: px / 2.0, radiusY: px / 2.0 }, &b);
                Ok(())
            })
            .ok()?;
        let b: CompositionBrush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?.cast().ok()?;
        if let Some(i) = self.island.as_ref() {
            i.tone_brushes.borrow_mut().insert(k, b.clone());
        }
        Some(b)
    }

    /// The one line all of the pill's text stands on (the title, the
    /// smaller words, the percent, the time: topbar-text review P2): a box
    /// as tall as the pill, the baseline that centers the title optically
    /// in it (as the bar's cells: topbar::baseline); smaller text keeps its
    /// size on the same baseline. Its text surfaces are the pill's height,
    /// so model::frame puts them at its top.
    fn pill_line(&self) -> crate::gfx::TextLine {
        super::baseline::line(model::rest_height(self.height, self.scale) * self.scale, &self.font_metrics(), TEXT_SIZE * self.scale * crate::fonts::scale())
    }

    /// One line of text at most `max` DIP wide (cut with "…").
    fn island_text(&self, text: &str, size: f32, bold: bool, c: Rgba, max: f32) -> Option<(CompositionBrush, f32, f32)> {
        if text.is_empty() {
            return None;
        }
        let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        let line = self.pill_line();
        let (surface, w, h, _) = self.gfx.text_fit_on(text, &self.theme.font.family, size, weight, rgba_d2d(c), self.scale, max, Some(line)).ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        Some((b.cast().ok()?, w, h))
    }

    /// A number that changes (the focus timer's time): semibold Footnote in
    /// tabular figures, so its width holds as it counts, on the pill's line.
    fn island_number(&self, text: &str, c: Rgba) -> Option<(CompositionBrush, f32, f32)> {
        if text.is_empty() {
            return None;
        }
        let line = self.pill_line();
        let (surface, w, h) = self.gfx.text_steady(text, text, &self.theme.font.family, SMALL_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, rgba_d2d(c), self.scale, 0.0, Some(line)).ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        Some((b.cast().ok()?, w, h))
    }

    /// The pill's title: one line at most `max` DIP wide, semibold. A cut
    /// one fades out over its last model::FADE DIP (a mask on its brush, nothing
    /// redrawn); high contrast cuts it with "…" instead. Returns the brush,
    /// width, height and baseline (DIP).
    fn island_title(&self, text: &str, c: Rgba, max: f32) -> Option<(CompositionBrush, f32, f32, f32)> {
        if text.is_empty() {
            return None;
        }
        let hc = self.theme.high_contrast;
        let line = self.pill_line();
        let (surface, w, h, base, cut) = self.gfx.text_line(text, &self.theme.font.family, TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, rgba_d2d(c), self.scale, max, hc, Some(line)).ok()?;
        let comp = &self.gfx.compositor;
        let b = comp.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        let brush: CompositionBrush = b.cast().ok()?;
        if !cut || hc {
            return Some((brush, w, h, base));
        }
        let fade = || -> Result<CompositionBrush> {
            let g = comp.CreateLinearGradientBrush()?;
            g.SetMappingMode(CompositionMappingMode::Absolute)?;
            // In the title's own coordinates: nothing after it is masked.
            let (from, to) = model::fade_span(w);
            g.SetStartPoint(Vector2 { X: from, Y: 0.0 })?;
            g.SetEndPoint(Vector2 { X: to, Y: 0.0 })?;
            let stops = g.ColorStops()?;
            stops.Append(&comp.CreateColorGradientStopWithOffsetAndColor(0.0, windows::UI::Color { A: 255, R: 255, G: 255, B: 255 })?)?;
            stops.Append(&comp.CreateColorGradientStopWithOffsetAndColor(1.0, windows::UI::Color { A: 0, R: 255, G: 255, B: 255 })?)?;
            let m = comp.CreateMaskBrush()?;
            m.SetSource(&brush)?;
            m.SetMask(&g)?;
            m.cast()
        };
        Some((fade().unwrap_or(brush.clone()), w, h, base))
    }

    /// A thin progress ring and the percent after it.
    fn island_ring(&self, ratio: f32, words: &str) -> Option<(CompositionBrush, f32, f32)> {
        let s = self.scale;
        let (_, ink, dim, _) = pill_colors(&self.theme);
        let accent = rgba_d2d(ink);
        let track = rgba_d2d(Rgba { a: if self.theme.high_contrast { 255 } else { 0x40 }, ..dim });
        let (layout, tw, th) = self.gfx.text_layout_tabular(words, &self.theme.font.family, SMALL_SIZE * s, DWRITE_FONT_WEIGHT_NORMAL).ok()?;
        let gap = RING_GAP * s;
        let ring = (RING * s).round();
        // As tall as the pill: the percent stands on the pill's one
        // baseline, the ring centered on whole pixels (model::frame puts the
        // surface at the pill's top: nothing left over to round).
        let line = self.pill_line();
        let (w, h) = ((ring + gap + tw).ceil(), line.box_h);
        let factory = self.gfx.d2d_factory().clone();
        let text_c = rgba_d2d(ink);
        let rtl = crate::i18n::rtl();
        let ty = super::baseline::origin_y(line.baseline, crate::gfx::first_baseline(&layout, th));
        let ring_y = super::baseline::centered_px(h, ring);
        let surface = self
            .gfx
            .draw(w as u32, h as u32, |dc, ox, oy| unsafe {
                // Right to left the ring comes after the number.
                let (ring_x, text_x) = if rtl { (ox + tw + gap, ox) } else { (ox, ox + ring + gap) };
                let c = Vector2 { X: ring_x + ring / 2.0, Y: oy + ring_y + ring / 2.0 };
                let stroke = RING_STROKE * s;
                let r = ring / 2.0 - stroke / 2.0;
                let tb = dc.CreateSolidColorBrush(&track, None)?;
                dc.DrawEllipse(&D2D1_ELLIPSE { point: c, radiusX: r, radiusY: r }, &tb, stroke, None);
                let ab = dc.CreateSolidColorBrush(&accent, None)?;
                let ratio = ratio.clamp(0.0, 1.0);
                if ratio >= 0.999 {
                    dc.DrawEllipse(&D2D1_ELLIPSE { point: c, radiusX: r, radiusY: r }, &ab, stroke, None);
                } else if ratio > 0.0 {
                    let a = ratio * std::f32::consts::TAU;
                    let start = Vector2 { X: c.X, Y: c.Y - r };
                    let end = Vector2 { X: c.X + r * a.sin(), Y: c.Y - r * a.cos() };
                    let path = factory.CreatePathGeometry()?;
                    let sink = path.Open()?;
                    sink.BeginFigure(start, D2D1_FIGURE_BEGIN_HOLLOW);
                    sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: end,
                        size: D2D_SIZE_F { width: r, height: r },
                        rotationAngle: 0.0,
                        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                        arcSize: if ratio > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                    });
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    sink.Close()?;
                    let style = factory.CreateStrokeStyle(
                        &D2D1_STROKE_STYLE_PROPERTIES1 {
                            startCap: D2D1_CAP_STYLE_ROUND,
                            endCap: D2D1_CAP_STYLE_ROUND,
                            ..Default::default()
                        },
                        None,
                    )?;
                    dc.DrawGeometry(&path, &ab, stroke, &style);
                }
                let fg = dc.CreateSolidColorBrush(&text_c, None)?;
                dc.DrawTextLayout(Vector2 { X: text_x, Y: oy + ty }, &layout, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE);
                Ok(())
            })
            .ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        Some((b.cast().ok()?, w / s, h / s))
    }

    /// The island for screen readers (a button on the menu bar).
    pub(super) fn island_node(&self) -> Option<crate::a11y::Node> {
        let isl = self.island.as_ref()?;
        if isl.phase == Phase::Hidden || self.hidden_full {
            return None;
        }
        let r = self.island_screen_rect()?;
        let name = isl.live.iter().map(model::spoken).collect::<Vec<_>>().join(crate::i18n::t("a11y.list_sep"));
        Some(
            crate::a11y::Node::new("island", crate::a11y::Role::Button, name, r)
                .help(crate::i18n::t("settings.topbar.island"))
                .invoke()
                .expand(matches!(isl.phase, Phase::Expanded { .. }))
                // A right-click on it opens the bar's menu (topbar::right_click).
                .menu(),
        )
    }
}

/// The pill's picture for the promo film (tests only).
#[cfg(test)]
#[path = "pill_promo.rs"]
pub(super) mod promo;
#[cfg(test)]
pub(super) use card::paint_site_island;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_sessions_never_become_island_activities() {
        for st in [crate::agents::St::Work, crate::agents::St::Wait, crate::agents::St::Done] {
            crate::agents::with_test_session(st, || {
                let mut inputs = Inputs::default();
                feed_inputs(&mut inputs);
                assert!(model::live(&inputs, Enabled::ALL, false).is_empty(), "{st:?} must not open the island");
                inputs.track = Some(model::Track { title: "Song".into(), playing: true, ..Default::default() });
                let activities = model::live(&inputs, Enabled::ALL, false);
                assert_eq!(activities.iter().map(Activity::kind).collect::<Vec<_>>(), [Kind::Media]);
                assert_eq!(model::pill_words(&activities[0]).0, "Song");
                let signal = crate::agents::companion_signal(std::time::Instant::now());
                assert_eq!(signal.working, st == crate::agents::St::Work);
                assert_eq!(!signal.waits.is_empty(), st == crate::agents::St::Wait);
                assert_eq!(signal.result.is_some(), st == crate::agents::St::Done);
            });
        }
    }

    #[test]
    fn the_pill_sits_on_e1_with_ramp_type_and_concentric_art() {
        use crate::syslook::SysLook;
        for light in [false, true] {
            let t = crate::theme::Theme::graphite().resolve(&SysLook { light, ..Default::default() });
            // E1, both layers (it had one: Composition's DropShadow takes
            // one; the patch is drawn with both): Flat v2's near y 1 /
            // blur 2, far y 2 / blur 6 (Flat's short shadows).
            let sh = pill_shadows(&t, true);
            assert_eq!(sh.iter().map(|l| (l.1, l.2)).collect::<Vec<_>>(), [(1.0, 2.0), (2.0, 6.0)]);
            assert!(sh.iter().all(|l| l.0 == 0.0 && l.3.a > 0), "one light, straight down");
            assert!(pill_shadows(&t, false).is_empty(), "the Shadows effect off");
            let mut hc = t.clone();
            hc.high_contrast = true;
            assert!(pill_shadows(&hc, true).is_empty());
            // Its fill is material M4 (no alpha literal of its own): the
            // tooltips' tint, a step lighter than the bar and the panels.
            assert_eq!(pill_colors(&t).0, t.material(crate::theme::Material::Tip));
            assert!(pill_colors(&t).0.r > t.material(crate::theme::Material::Panel).r);
            // Hovered and pressed: the row fills over it, each a step on.
            let lum = |f: Fill| pill_fill(&t, f).r as i32;
            let step = if light { -1 } else { 1 };
            assert!((lum(Fill::Hover) - lum(Fill::Rest)) * step > 0 && (lum(Fill::Pressed) - lum(Fill::Hover)) * step > 0, "{light}");
            // Flat v2: at rest it is M4's fall (not its mean), with its rim
            // (1 DIP); hovered and pressed each end takes the row's fill,
            // a step on as the one color does.
            let m = crate::theme::Material::Tip;
            assert_eq!(pill_fall(&t, Fill::Rest), t.flat_fill(m).unwrap());
            for f in [Fill::Hover, Fill::Pressed] {
                let (top, bottom) = pill_fall(&t, f);
                let (rt, rb) = pill_fall(&t, Fill::Rest);
                assert!((top.r as i32 - rt.r as i32) * step > 0 && (bottom.r as i32 - rb.r as i32) * step > 0, "{light} {f:?}");
                assert!(top.r == bottom.r, "{f:?}: one grey (Flat since 2026-11-12)");
            }
            let coat = pill_coat(&t, 1.5);
            assert_eq!(coat.rim, Some((t.rim_of(m).unwrap(), 1.0)));
            assert_eq!((coat.fill, coat.grain), (pill_fall(&t, Fill::Rest), 0.0));
            // A glass's pill: one color, the glass's own rim, one device
            // pixel (a_pill_on_a_glass_wears_its_light).
            let mut raw = crate::theme::Theme::graphite();
            raw.finish = crate::theme::Finish::Glass;
            let g = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
            assert!(g.glass());
            let one = pill_fill(&g, Fill::Rest);
            assert_eq!(pill_coat(&g, 1.5), morph::Coat { fill: (one, one), rim: Some((g.rim_of(m).unwrap(), 1.0 / 1.5)), grain: 0.0 });
            // What only informs is ink; the music bars too.
            assert_eq!(tone_rgba(Tone::Ink, &t), pill_colors(&t).1);
            assert_eq!(eq_color(&t), pill_colors(&t).1);
        }
        // Type: the title 12 semibold, the rest Footnote 12.
        assert_eq!((TEXT_SIZE, SMALL_SIZE), (crate::theme::ramp::FOOTNOTE.size, crate::theme::ramp::FOOTNOTE.size));
        // The art's corners concentric with the capsule: a 20 picture in a
        // 28 pill is a circle (14 - 4 = 10), never past one.
        assert_eq!(lead_radius(ART, 28.0), 10.0);
        assert_eq!(lead_radius(ART, 44.0), 10.0);
        assert_eq!(lead_radius(16.0, 16.0), 8.0);
        for h in [16.0f32, 20.0, 24.0, 28.0, 32.0, 44.0] {
            let r = lead_radius(ART.min(h), h);
            assert!(r >= 0.0 && r <= ART.min(h) / 2.0, "{h}: {r}");
        }
        // Lengths on the grid (it had 2.5, 13, 5, 10, 9).
        for v in [LEAD, ART, EQ_W, EQ_GAP, EQ_H, RING, RING_GAP, RING_STROKE, model::PAD, model::GAP, model::DOT_STEP] {
            assert!((v / 2.0).fract() == 0.0, "{v}");
        }
    }

    const MAIN: isize = 0xA;
    const SIDE: isize = 0xB;
    const THIRD: isize = 0xC;

    #[test]
    fn the_island_moves_on_the_kits_springs() {
        // Moving and resizing: GLIDE over the kit's 280 ms, no overshoot.
        assert_eq!(MOVE_MS, 280);
        let keys = spring_keys(MOVE_MS, "this.FinalValue");
        assert_eq!(keys.len(), crate::motion::SAMPLES);
        assert_eq!(keys.last().map(|k| k.0), Some(1.0));
        assert!(keys.last().unwrap().1.ends_with("* 1"), "ends on its target: {:?}", keys.last());
        for (_, k) in &keys {
            let done: f32 = k.rsplit("* ").next().unwrap().parse().unwrap();
            assert!((0.0..=1.002).contains(&done), "{k}");
        }
        assert_eq!((SWAP_MS, FADE_MS), (180, 180));
        // Coming and going: GLIDE (a surface's size); hover and press: SNAP
        // (a small thing's state).
        assert_eq!(scale_spring(true), crate::motion::SPRING_GLIDE);
        assert_eq!(scale_spring(false), crate::motion::SPRING_SNAP);
    }

    /// Final sheet 2 (2026-11-11): a glass's dark pill was the bar's
    /// lightness and only the contact ring parted them. A glass's pill
    /// wears the rim every other glass surface wears (its own light), one
    /// device pixel on the pixel's middle; Flat and high contrast keep
    /// theirs.
    #[test]
    fn a_pill_on_a_glass_wears_no_line() {
        use crate::syslook::SysLook;
        use crate::theme::{Finish, Material};
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            for f in [Finish::Glass] {
                let mut raw = crate::theme::Theme::graphite();
                raw.finish = f;
                let t = raw.resolve_fx(&sys, true);
                assert!(t.glass(), "{f:?}");
                let what = format!("light {light} {f:?}");
                // The panels' light, the glass's own.
                let rim = pill_rim(&t).expect("a glass's pill has a rim");
                assert_eq!(Some(rim), t.rim(), "{what}: the rim of every glass surface");
                let white = |a: f32| Rgba { r: 255, g: 255, b: 255, a: (a * 255.0).round() as u8 };
                assert_eq!(rim, t.coat().unwrap().rim.map(white), "{what}");
                for s in [1.0, 1.5, 2.0] {
                    assert_eq!(pill_coat(&t, s).rim, Some((rim, 1.0 / s)), "{what} {s}: one device pixel");
                    // The morph's ends wear it as one rim.
                    let look = morph::Look::with(&t, true, true, s);
                    assert!(look.rim_carries(), "{what} {s}");
                    assert_eq!(look.ends().0.rim, pill_coat(&t, s).rim);
                }
                // Glass A (2026-11-12): no line on the pill, all clear.
                assert!(rim.iter().all(|c| c.a == 0), "{what}: {rim:?}");
            }
            // Flat v2 and high contrast as they were.
            let flat = crate::theme::Theme::graphite().resolve_fx(&sys, true);
            assert_eq!(pill_rim(&flat), flat.flat_coat(Material::Tip).and_then(|_| flat.rim_of(Material::Tip)));
            assert!(pill_rim(&flat).is_some());
            let mut hc = flat.clone();
            hc.high_contrast = true;
            assert_eq!(pill_rim(&hc), None);
        }
        // The stroke lies half its width in: a 1 DIP stroke where it was
        // (0.5), one device pixel at 150 % on that pixel's middle.
        let f = model::frame(false, 120.0, 28.0, &Content::default(), 1.5);
        assert_eq!(edge_for(&f, 1.0), f.edge);
        let e = edge_for(&f, 1.0 / 1.5);
        assert!((e.x - 1.0 / 3.0).abs() < 1e-5 && (e.y - 1.0 / 3.0).abs() < 1e-5, "{e:?}");
        assert!((e.w - (120.0 - 2.0 / 3.0)).abs() < 1e-4 && (e.h - (28.0 - 2.0 / 3.0)).abs() < 1e-4, "{e:?}");
    }

    /// Last-two review P2 (2026-11-11): a pill at rest is on whole device
    /// pixels every way (model::rest_at), so on screen each straight side
    /// of its rim lies where its stroke covers whole pixels (a one-pixel
    /// rim: on a pixel's middle), top and bottom, left and right, at every
    /// scale, bar height and title width, either way round; and the card's
    /// plate starts on it exactly (card::pill_shape of the rect
    /// island_screen_rect hands the card, morph::plate_at at p = 0).
    #[test]
    fn a_resting_pill_lies_on_whole_pixels() {
        let whole = |v: f32| (v - v.round()).abs() < 2e-3;
        // A bar's top left on screen (px): a second display's.
        let bar = (2561, 37);
        let mut seen = 0;
        for s in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            let width = 2559.0 / s;
            let cells = [(8.0, width * 0.21), (width * 0.79 - 8.0, width * 0.21)];
            for bar_h in [20.0f32, 24.0, 26.0, 28.0, 32.0, 48.0] {
                for text in [0.0f32, 37.3, 101.7, 213.45, 333.3] {
                    for rtl in [false, true] {
                        let what = format!("{s} bar {bar_h} text {text} rtl {rtl}");
                        let h0 = model::rest_height(bar_h, s);
                        let c = Content { lead: model::lead_fit(18.0, h0), text, text_h: 17.0, trail: 16.0, trail_h: 10.0, dots: 1, baseline: None };
                        // TopBar::island_layout's steps.
                        let (x, w) = model::place(width, &cells, model::pill_width(&c), model::pill_need(&c)).expect("room");
                        let (cx, top, w, h) = model::rest_at(x, model::snap(w, s), bar_h, s);
                        assert_eq!(h, h0, "{what}");
                        assert!((h - model::pill_height(bar_h)).abs() <= 0.5 / s + 1e-4, "{what}: {h}");
                        let f = model::frame(rtl, w, h, &c, s);
                        // On screen (px).
                        let sx = |v: f32| bar.0 as f32 + (cx + v) * s;
                        let sy = |v: f32| bar.1 as f32 + (top + v) * s;
                        // The fill's clip.
                        for v in [sx(f.pill.x), sx(f.pill.x + f.pill.w), sy(f.pill.y), sy(f.pill.y + f.pill.h)] {
                            assert!(whole(v), "{what}: the pill's edge at {v} px");
                        }
                        // The rims: a glass's one device pixel, Flat v2's
                        // 1 DIP where that is whole pixels (100, 200 %).
                        for rim in [1.0 / s, 1.0] {
                            if !whole(rim * s) {
                                continue;
                            }
                            let e = edge_for(&f, rim);
                            let (l, r) = (sx(f.edge_box.x + e.x), sx(f.edge_box.x + e.x + e.w));
                            let (t, b) = (sy(f.edge_box.y + e.y), sy(f.edge_box.y + e.y + e.h));
                            let k = rim * s / 2.0;
                            for (side, v) in [("left", l), ("right", r), ("top", t), ("bottom", b)] {
                                assert!(whole(v - k) && whole(v + k), "{what} rim {rim}: the {side} stroke's middle at {v} px");
                                if (rim * s - 1.0).abs() < 1e-4 {
                                    assert!(((v - v.floor()) - 0.5).abs() < 2e-3, "{what}: the {side} rim off a pixel's middle: {v}");
                                }
                            }
                            // The card's plate at p = 0: the same pill, the
                            // same rim (morph::Plate's edge: half its width in).
                            let (pl, pt, pr, pb) = model::screen_rect(bar, cx, top, w, h, s);
                            let pill = RECT { left: pl, top: pt, right: pr, bottom: pb };
                            let card_at = RECT { left: (pl + pr) / 2 - (180.0 * s) as i32, top: pt, right: (pl + pr) / 2 + (180.0 * s) as i32, bottom: pt + (240.0 * s) as i32 };
                            let start = card::pill_shape(pill, 1.0, card_at, s);
                            let card_shape = morph::Shape { x: 0.0, y: 0.0, w: 360.0, h: 240.0, r: 18.0 };
                            let p0 = morph::plate_at(start, card_shape, 0.0);
                            let (cl, ct) = (card_at.left as f32, card_at.top as f32);
                            let near = |a: f32, b: f32| (a - b).abs() < 2e-3;
                            assert!(near(cl + p0.x * s, sx(f.pill.x)) && near(ct + p0.y * s, sy(f.pill.y)), "{what}: the plate starts at {p0:?}");
                            assert!(near(p0.w * s, w * s) && near(p0.h * s, h * s) && near(p0.r, h / 2.0), "{what}: the plate starts {p0:?}");
                            let half = rim / 2.0;
                            let plate = [cl + (p0.x + half) * s, cl + (p0.x + p0.w - half) * s, ct + (p0.y + half) * s, ct + (p0.y + p0.h - half) * s];
                            for (a, b) in plate.into_iter().zip([l, r, t, b]) {
                                assert!(near(a, b), "{what} rim {rim}: the plate's rim at {a}, the pill's at {b}");
                            }
                        }
                        seen += 1;
                    }
                }
            }
        }
        assert_eq!(seen, 5 * 6 * 5 * 2);
    }

    #[test]
    fn the_pill_answers_the_pointer_by_the_interaction_table() {
        let t = |hovered, pressed| Touch { hovered, pressed };
        let (w, h) = (120.0, 28.0);
        // Size: hidden 0.96, at rest 1, under the pointer 1.02, pressed
        // the one press formula (the long side 2 DIP shorter).
        assert_eq!(scale_target(false, t(true, true), w, h), HIDDEN_SCALE);
        assert_eq!(scale_target(true, t(false, false), w, h), 1.0);
        assert!((scale_target(true, t(true, false), w, h) - 1.02).abs() < 1e-6);
        assert!((scale_target(true, t(true, true), w, h) - (1.0 - 2.0 / 120.0)).abs() < 1e-6);
        assert_eq!(scale_target(true, t(true, true), 20.0, 20.0), 0.94, "a small thing stops at 0.94");
        // Fill: a press at once, on in 90, off in 120.
        assert_eq!(fill_plan(t(true, true)), (Fill::Pressed, 0));
        assert_eq!(fill_plan(t(true, false)), (Fill::Hover, 90));
        assert_eq!(fill_plan(t(false, false)), (Fill::Rest, 120));
        // The indexes color_to reads the fills by.
        assert_eq!((Fill::Rest as usize, Fill::Hover as usize, Fill::Pressed as usize), (0, 1, 2));
    }

    #[test]
    fn color_fades_keep_reduced_motion_short_and_straight() {
        use crate::motion::{Mode, EASE_OUT, LINEAR, REDUCED_MS};
        // What color_to plays for each of the pill's and the card's fills
        // (fill_plan, the wash's press let go), in both modes and at the
        // slowest, normal and fastest speeds.
        for speed in [0.25f32, 1.0, 4.0] {
            for t in [Touch { hovered: true, pressed: false }, Touch { hovered: false, pressed: false }] {
                let ms = fill_plan(t).1;
                // Reduced (12.2): 60 ms at most, straight, whatever the speed.
                assert_eq!(color_plan(Mode::Reduced, ms, speed), (REDUCED_MS, LINEAR), "{ms} at {speed}");
                // Full: the speed applied once, on the fill's own curve.
                assert_eq!(color_plan(Mode::Full, ms, speed), (crate::effects::ms_at(ms, speed), EASE_OUT), "{ms} at {speed}");
            }
            // A press is at once in either mode.
            assert_eq!(color_plan(Mode::Reduced, 0, speed).0, 0);
            assert_eq!(color_plan(Mode::Full, 0, speed).0, 0);
        }
        // The review's case: reduced motion at 0.25 or 4 is not 240 or 15 ms.
        assert_eq!(color_plan(Mode::Reduced, crate::motion::FADE_IN_MS, 0.25).0, 60);
        assert_eq!(color_plan(Mode::Reduced, crate::motion::FADE_IN_MS, 4.0).0, 60);
        assert_eq!(color_plan(Mode::Full, crate::motion::FADE_OUT_MS, 4.0).0, 30);
    }

    #[test]
    fn the_pills_scale_turns_round_at_its_speed() {
        use std::time::{Duration, Instant};
        // Hover in, and out again half way: the spring leaves from where
        // it is at the speed it has (nothing starts over).
        let t0 = Instant::now();
        let up = crate::motion::SpringRun::still(scale_spring(false), 1.0, t0).toward(HOVER_SCALE, t0, 1.0);
        let mid = t0 + Duration::from_millis(40);
        let (x, v) = up.state(mid);
        assert!(x > 1.0 && x < HOVER_SCALE && v > 0.0, "{x} {v}");
        let back = up.toward(1.0, mid, 1.0);
        assert_eq!(back.state(mid), (x, v));
        assert!((back.state(t0 + Duration::from_millis(600)).0 - 1.0).abs() < 1e-3);
    }

    #[test]
    fn the_island_goes_to_the_display_in_use() {
        let bars = [(MAIN, true), (SIDE, false), (THIRD, false)];
        assert_eq!(island_monitor(Some(SIDE), &bars), Some(SIDE));
        assert_eq!(island_monitor(Some(MAIN), &bars), Some(MAIN));
        // No bar there (a display without one), or not known: the main.
        assert_eq!(island_monitor(Some(0xD), &bars), Some(MAIN));
        assert_eq!(island_monitor(None, &bars), Some(MAIN));
        // No main among the bars: the first one.
        assert_eq!(island_monitor(None, &[(SIDE, false), (THIRD, false)]), Some(SIDE));
        assert_eq!(island_monitor(Some(SIDE), &[]), None);
    }

    #[test]
    fn which_bars_show_the_island() {
        // Not the same on every display: the main bar only, wherever the
        // focus is.
        for at in [Some(MAIN), Some(SIDE), None] {
            assert!(island_on(false, false, true, MAIN, at));
            assert!(!island_on(false, false, false, SIDE, at));
            assert!(!island_on(false, true, false, SIDE, at), "the choice needs the switch");
        }
        // The display in use: exactly one bar, and it moves with the focus.
        let shows = |at: Option<isize>| [(MAIN, true), (SIDE, false), (THIRD, false)].iter().filter(|(m, p)| island_on(true, false, *p, *m, at)).map(|b| b.0).collect::<Vec<_>>();
        assert_eq!(shows(Some(SIDE)), vec![SIDE]);
        assert_eq!(shows(Some(THIRD)), vec![THIRD]);
        assert_eq!(shows(Some(MAIN)), vec![MAIN]);
        assert_eq!(shows(None), vec![MAIN], "not known yet: the main bar");
        // Every display: all of them.
        assert!([(MAIN, true), (SIDE, false)].iter().all(|(m, p)| island_on(true, true, *p, *m, Some(SIDE))));
    }

    #[test]
    fn a_focus_change_moves_it_only_when_the_display_changes() {
        set_island_monitor(Some(MAIN));
        assert!(!set_island_monitor(Some(MAIN)), "same display: nothing moves");
        assert!(set_island_monitor(Some(SIDE)));
        assert!(set_island_monitor(None));
        assert!(!set_island_monitor(None));
        set_island_monitor(None);
    }
}
