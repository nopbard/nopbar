//! Harmony v2.1's interaction fills as visuals (design system 6.2, 6.3),
//! for the panels whose content is one picture (About this PC, search, a
//! stack's grid, PiP's controls): the picture is drawn once and what
//! answers the pointer and the keyboard is laid under it.
//!
//! - `Hover`: the pointer's fill on part of a surface (a row, a tile, a
//!   glyph button). It comes on over FADE_IN_MS and goes over FADE_OUT_MS
//!   (the one the pointer left fades where it is while the next comes on);
//!   a press is there at once, and a capsule also shrinks to press_scale on
//!   SNAP.
//! - `Mark`: the keyboard's cursor in a list, or the selection that follows
//!   it: the selected fill, there and gone at once.
//! - `Raised`: a button. The card fill at rest, lighter under the pointer,
//!   darker and a little smaller pressed.
//! - `Ring`: the progress ring, faded in.
//!
//! Every length comes from crate::motion (its fades are fade_now's: 60 ms
//! straight with reduced motion, where nothing changes size either). High
//! contrast has no fades and its states are outlines and inverted text:
//! these draw nothing then, and the screen paints the state into its
//! picture (theme::fill_of / outline_of / text_of) at once.
//!
//! Everything is in the units of the layer it is given (DIP under a scaled
//! root).

use windows::core::{Interface, Result, HSTRING};
use windows::Foundation::TimeSpan;
use windows::UI::Color;
use windows::UI::Composition::{CompositionBrush, CompositionColorBrush, CompositionRoundedRectangleGeometry, Compositor, ContainerVisual, SpriteVisual, VisualCollection};
use windows_numerics::{Vector2, Vector3};

use crate::motion::{self, Curve, EASE_IN, EASE_OUT, FADE_IN_MS, FADE_OUT_MS};
use crate::theme::{Rgba, Role, State, Theme};

/// A target's box and corner radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub rad: f32,
}

impl Shape {
    pub fn new(x: f32, y: f32, w: f32, h: f32, rad: f32) -> Shape {
        Shape { x, y, w, h, rad }
    }

    /// A capsule (a glyph button): its radius half its height.
    pub fn capsule(x: f32, y: f32, w: f32, h: f32) -> Shape {
        Shape { x, y, w, h, rad: h / 2.0 }
    }

    #[cfg(test)]
    pub fn holds(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

fn color(c: Rgba) -> Color {
    Color { A: c.a, R: c.r, G: c.g, B: c.b }
}

/// `brush`'s color to `to` over `ms` (at speed 1) on `curve` from the color
/// it shows, as motion::fade_now plays it; at once for 0.
fn color_to(c: &Compositor, brush: &CompositionColorBrush, to: Rgba, ms: i64, curve: Curve) {
    let prop = HSTRING::from("Color");
    let (ms, curve) = motion::fade_now(ms, curve);
    let run = || -> Result<()> {
        let a = c.CreateColorKeyFrameAnimation()?;
        a.InsertKeyFrameWithEasingFunction(1.0, color(to), &motion::easing(c, curve)?)?;
        a.SetDuration(TimeSpan { Duration: ms.max(1) * 10_000 })?;
        brush.StartAnimation(&prop, &a)
    };
    if ms <= 0 || run().is_err() {
        let _ = brush.StopAnimation(&prop);
        let _ = brush.SetColor(color(to));
    }
}

/// One rounded fill.
struct Fill {
    v: SpriteVisual,
    geom: CompositionRoundedRectangleGeometry,
    brush: CompositionColorBrush,
}

impl Fill {
    fn new(c: &Compositor, layer: &ContainerVisual) -> Result<Fill> {
        let v = c.CreateSpriteVisual()?;
        let geom = c.CreateRoundedRectangleGeometry()?;
        let brush = c.CreateColorBrush()?;
        v.SetBrush(&brush)?;
        v.SetClip(&c.CreateGeometricClipWithGeometry(&geom)?)?;
        v.SetOpacity(0.0)?;
        layer.Children()?.InsertAtTop(&v)?;
        Ok(Fill { v, geom, brush })
    }

    /// On `s` (a press's scale keeps to its middle).
    fn place(&self, s: Shape) {
        let rad = s.rad.clamp(0.0, (s.w.min(s.h) / 2.0).max(0.0));
        let _ = self.v.SetOffset(Vector3 { X: s.x, Y: s.y, Z: 0.0 });
        let _ = self.v.SetSize(Vector2 { X: s.w, Y: s.h });
        let _ = self.v.SetCenterPoint(Vector3 { X: s.w / 2.0, Y: s.h / 2.0, Z: 0.0 });
        let _ = self.geom.SetSize(Vector2 { X: s.w, Y: s.h });
        let _ = self.geom.SetCornerRadius(Vector2 { X: rad, Y: rad });
    }

    /// Its full size at once (a press that was under way is dropped).
    fn unscale(&self) {
        let _ = self.v.StopAnimation(&HSTRING::from("Scale.X"));
        let _ = self.v.StopAnimation(&HSTRING::from("Scale.Y"));
        let _ = self.v.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 });
    }

    /// Shown or gone at once.
    fn snap(&self, on: bool) {
        let _ = self.v.StopAnimation(&HSTRING::from("Opacity"));
        let _ = self.v.SetOpacity(if on { 1.0 } else { 0.0 });
    }
}

// ---- Flat v2's card light ---------------------------------------------------------

/// Flat v2's card light (Theme::card_fx) round a raised thing drawn as a
/// visual: `face`, a sprite clipped to the thing's shape and filled with
/// its fill. The drop (black, half a DIP down, the face's shape) lies
/// under the face in its layer; the light along the top edge (white, one
/// DIP: the face's shape less itself one DIP down) is the face's own
/// child, a light sprite under a cover in the face's fill one DIP down
/// (the face's clip cuts both). What gfx::parts::card_under / card_over
/// draw in D2D. Nothing shows where card_fx is None (a glass, high
/// contrast, the "on" fill): it is all clear then.
pub struct CardLight {
    drop: SpriteVisual,
    drop_geom: CompositionRoundedRectangleGeometry,
    drop_brush: CompositionColorBrush,
    light_brush: CompositionColorBrush,
    cover: SpriteVisual,
    cover_geom: CompositionRoundedRectangleGeometry,
    /// The state it shows (None: not shown yet).
    state: Option<State>,
}

impl CardLight {
    /// Round `face` (in `layer`; its fill `face_brush`, which the cover
    /// wears too: it fades with it).
    pub fn new(c: &Compositor, layer: &VisualCollection, face: &SpriteVisual, face_brush: &CompositionBrush) -> Result<CardLight> {
        let clear = color(Rgba { r: 0, g: 0, b: 0, a: 0 });
        let drop = c.CreateSpriteVisual()?;
        let drop_brush = c.CreateColorBrushWithColor(clear)?;
        drop.SetBrush(&drop_brush)?;
        let drop_geom = c.CreateRoundedRectangleGeometry()?;
        drop.SetClip(&c.CreateGeometricClipWithGeometry(&drop_geom)?)?;
        layer.InsertBelow(&drop, face)?;
        let light = c.CreateSpriteVisual()?;
        let light_brush = c.CreateColorBrushWithColor(clear)?;
        light.SetBrush(&light_brush)?;
        light.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        face.Children()?.InsertAtTop(&light)?;
        let cover = c.CreateSpriteVisual()?;
        cover.SetBrush(face_brush)?;
        let cover_geom = c.CreateRoundedRectangleGeometry()?;
        cover.SetClip(&c.CreateGeometricClipWithGeometry(&cover_geom)?)?;
        face.Children()?.InsertAtTop(&cover)?;
        Ok(CardLight { drop, drop_geom, drop_brush, light_brush, cover, cover_geom, state: None })
    }

    /// The face is `s` (in its layer's DIP).
    pub fn place(&self, s: Shape) {
        let rad = s.rad.clamp(0.0, (s.w.min(s.h) / 2.0).max(0.0));
        let _ = self.drop.SetOffset(Vector3 { X: s.x, Y: s.y + 0.5, Z: 0.0 });
        let _ = self.drop.SetSize(Vector2 { X: s.w, Y: s.h });
        let _ = self.drop.SetCenterPoint(Vector3 { X: s.w / 2.0, Y: s.h / 2.0, Z: 0.0 });
        let _ = self.drop_geom.SetSize(Vector2 { X: s.w, Y: s.h });
        let _ = self.drop_geom.SetCornerRadius(Vector2 { X: rad, Y: rad });
        let _ = self.cover.SetOffset(Vector3 { X: 0.0, Y: 1.0, Z: 0.0 });
        let _ = self.cover.SetSize(Vector2 { X: s.w, Y: s.h });
        let _ = self.cover_geom.SetSize(Vector2 { X: s.w, Y: s.h });
        let _ = self.cover_geom.SetCornerRadius(Vector2 { X: rad, Y: rad });
    }

    /// Shows state `s` (Theme::card_fx): the drop at once, the light gone
    /// at once pressed and back over `ms` on `curve` (with the fill).
    pub fn show(&mut self, c: &Compositor, theme: &Theme, s: State, ms: i64, curve: Curve) {
        let clear = Rgba { r: 0, g: 0, b: 0, a: 0 };
        let fx = theme.card_fx(s);
        let (light, drop) = fx.map_or((clear, clear), |fx| (fx.light, fx.drop));
        // No light (a glass, high contrast, a theme's own fill): no cover
        // either. It wears the face's fill, and a see-through fill laid
        // twice would show twice as thick (the flat v2 fills are opaque).
        let _ = self.cover.SetIsVisible(fx.is_some());
        let first = self.state.is_none();
        self.state = Some(s);
        color_to(c, &self.drop_brush, drop, 0, curve);
        color_to(c, &self.light_brush, light, if first || light.a == 0 { 0 } else { ms }, curve);
    }

    /// The face's press (motion::press): the drop goes with it.
    pub fn press(&self, c: &Compositor, size: (f32, f32), down: bool) {
        let _ = motion::press(c, &self.drop, size, down);
    }

    /// Its full size at once (as Fill::unscale).
    pub fn unscale(&self) {
        let _ = self.drop.StopAnimation(&HSTRING::from("Scale.X"));
        let _ = self.drop.StopAnimation(&HSTRING::from("Scale.Y"));
        let _ = self.drop.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 });
    }

    /// Shown or gone at once (with its face).
    pub fn snap(&self, on: bool) {
        let _ = self.drop.SetOpacity(if on { 1.0 } else { 0.0 });
    }

    /// Whether the cover over the face shows (tests).
    #[cfg(test)]
    pub fn covers(&self) -> bool {
        self.cover.IsVisible().unwrap()
    }

    /// What it shows now (tests): the drop's and the light's colors.
    #[cfg(test)]
    pub fn shows(&self) -> (Rgba, Rgba) {
        let rd = |b: &CompositionColorBrush| {
            let c = b.Color().unwrap();
            Rgba { r: c.R, g: c.G, b: c.B, a: c.A }
        };
        (rd(&self.drop_brush), rd(&self.light_brush))
    }
}

// ---- the pointer's fill ---------------------------------------------------------

/// What the pointer's fill shows: its target, pressed or not, and the
/// target's shape as last drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lit<T> {
    pub t: T,
    pub down: bool,
    pub shape: Shape,
}

/// What a hover fill has to do to show what is wanted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// It shows it already.
    Stay,
    /// Another target (or none): the one shown fades, the other fill comes
    /// on.
    Other,
    /// The same target: pressed or let go, and/or drawn somewhere else (the
    /// list under it scrolled).
    Same { press: bool, moved: bool },
}

/// Pure: the step from `now` to `want`.
pub fn step<T: PartialEq>(now: Option<&Lit<T>>, want: Option<&Lit<T>>) -> Step {
    match (now, want) {
        (None, None) => Step::Stay,
        (Some(a), Some(b)) if a.t == b.t => {
            let (press, moved) = (a.down != b.down, a.shape != b.shape);
            if press || moved {
                Step::Same { press, moved }
            } else {
                Step::Stay
            }
        }
        _ => Step::Other,
    }
}

/// Pure: how long the fill's change takes (ms at speed 1) and on which
/// curve (6.3): a press at once; the pointer coming on, or a press let go,
/// FADE_IN_MS; the pointer leaving FADE_OUT_MS.
pub fn pointer_fade(on: bool, down: bool) -> (i64, Curve) {
    if down {
        (0, EASE_OUT)
    } else if on {
        (FADE_IN_MS, EASE_OUT)
    } else {
        (FADE_OUT_MS, EASE_IN)
    }
}

/// The pointer's fill on parts of a surface (Role::Row).
pub struct Hover<T> {
    fills: [Fill; 2],
    cur: usize,
    on: Option<Lit<T>>,
    /// Its targets are capsules: a press also shrinks them (6.2). Rows,
    /// tiles and date cells keep their size.
    shrink: bool,
}

impl<T: Copy + PartialEq> Hover<T> {
    pub fn new(c: &Compositor, layer: &ContainerVisual, shrink: bool) -> Result<Hover<T>> {
        Ok(Hover { fills: [Fill::new(c, layer)?, Fill::new(c, layer)?], cur: 0, on: None, shrink })
    }

    /// Shows `want` (None: nothing under the pointer). High contrast: it
    /// shows nothing (the picture draws the state).
    pub fn show(&mut self, c: &Compositor, theme: &Theme, want: Option<Lit<T>>) {
        if theme.hc.is_some() {
            self.clear();
            return;
        }
        let fill = |down: bool| theme.fill_of(Role::Row, if down { State::Pressed } else { State::Hover });
        match step(self.on.as_ref(), want.as_ref()) {
            Step::Stay => {}
            Step::Other => {
                if self.on.take().is_some() {
                    let (ms, curve) = pointer_fade(false, false);
                    let _ = motion::fade_to(c, &self.fills[self.cur].v, 0.0, ms, curve);
                    self.cur = 1 - self.cur;
                }
                let Some(on) = want else { return };
                let f = &self.fills[self.cur];
                f.unscale();
                f.place(on.shape);
                color_to(c, &f.brush, fill(on.down), 0, EASE_OUT);
                let (ms, curve) = pointer_fade(true, on.down);
                let _ = motion::fade_to(c, &f.v, 1.0, ms, curve);
                if on.down && self.shrink {
                    let _ = motion::press(c, &f.v, (on.shape.w, on.shape.h), true);
                }
                self.on = Some(on);
            }
            Step::Same { press, moved } => {
                let Some(on) = want else { return };
                let f = &self.fills[self.cur];
                if moved {
                    f.place(on.shape);
                }
                if press {
                    let (ms, curve) = pointer_fade(true, on.down);
                    color_to(c, &f.brush, fill(on.down), ms, curve);
                    if self.shrink {
                        let _ = motion::press(c, &f.v, (on.shape.w, on.shape.h), on.down);
                    }
                }
                self.on = Some(on);
            }
        }
    }

    /// Gone at once (the keyboard took over: a key leaves no trail; the
    /// list was rebuilt under it).
    pub fn clear(&mut self) {
        self.on = None;
        for f in &self.fills {
            f.snap(false);
            f.unscale();
        }
    }
}

// ---- the keyboard's cursor --------------------------------------------------------

/// The keyboard's cursor in a list (or the selection that follows it): the
/// selected fill under one row, moved at once (6.3: what a key changes has
/// no fade).
pub struct Mark {
    fill: Fill,
    on: Option<Shape>,
}

impl Mark {
    pub fn new(c: &Compositor, layer: &ContainerVisual) -> Result<Mark> {
        Ok(Mark { fill: Fill::new(c, layer)?, on: None })
    }

    /// On `want` (None: no cursor). High contrast: nothing (the picture
    /// draws the Highlight fill and the text over it).
    pub fn show(&mut self, theme: &Theme, want: Option<Shape>) {
        let want = want.filter(|_| theme.hc.is_none());
        if want == self.on {
            return;
        }
        self.on = want;
        match want {
            Some(s) => {
                self.fill.place(s);
                let _ = self.fill.brush.StopAnimation(&HSTRING::from("Color"));
                let _ = self.fill.brush.SetColor(color(theme.fill_of(Role::Row, State::Cursor)));
                self.fill.snap(true);
            }
            None => self.fill.snap(false),
        }
    }

    /// The theme changed: shown again in its colors at the next `show`.
    pub fn forget(&mut self) {
        self.on = None;
        self.fill.snap(false);
    }
}

// ---- a button ---------------------------------------------------------------------

/// Pure: a raised thing's state from what the pointer does.
pub fn raised_state(hover: bool, pressed: bool) -> State {
    if pressed {
        State::Pressed
    } else if hover {
        State::Hover
    } else {
        State::Rest
    }
}

/// Pure: how long a raised thing's fill takes from `from` to `to` (ms at
/// speed 1) and on which curve (6.3): pressed at once; let go, or the
/// pointer coming on, FADE_IN_MS; the pointer leaving FADE_OUT_MS.
pub fn raised_fade(from: State, to: State) -> (i64, Curve) {
    match (from, to) {
        (_, State::Pressed) => (0, EASE_OUT),
        (_, State::Rest) if from != State::Rest => (FADE_OUT_MS, EASE_IN),
        (a, b) if a == b => (0, EASE_OUT),
        _ => (FADE_IN_MS, EASE_OUT),
    }
}

/// A button's fill (Role::Raised): the card fill at rest, lighter under
/// the pointer, darker pressed and smaller by press_scale (SNAP; its words
/// are the picture's and keep their size).
pub struct Raised {
    fill: Fill,
    /// Flat v2's card light round it (a button is raised: a card).
    card: CardLight,
    on: Option<(Shape, State)>,
}

impl Raised {
    pub fn new(c: &Compositor, layer: &ContainerVisual) -> Result<Raised> {
        let fill = Fill::new(c, layer)?;
        let card = CardLight::new(c, &layer.Children()?, &fill.v, &fill.brush.cast()?)?;
        card.snap(false);
        Ok(Raised { fill, card, on: None })
    }

    /// The button at `shape` in state `s` (None: there is no button now).
    /// High contrast: nothing (the picture draws its outline and fill).
    pub fn show(&mut self, c: &Compositor, theme: &Theme, want: Option<(Shape, State)>) {
        let want = want.filter(|_| theme.hc.is_none());
        let Some((shape, s)) = want else {
            if self.on.take().is_some() {
                self.fill.snap(false);
                self.fill.unscale();
                self.card.snap(false);
                self.card.unscale();
            }
            return;
        };
        let to = theme.fill_of(Role::Raised, s);
        match self.on {
            Some((was, from)) => {
                if was != shape {
                    self.fill.place(shape);
                    self.card.place(shape);
                }
                if from != s {
                    let (ms, curve) = raised_fade(from, s);
                    color_to(c, &self.fill.brush, to, ms, curve);
                    self.card.show(c, theme, s, ms, curve);
                    if (from == State::Pressed) != (s == State::Pressed) {
                        let _ = motion::press(c, &self.fill.v, (shape.w, shape.h), s == State::Pressed);
                        self.card.press(c, (shape.w, shape.h), s == State::Pressed);
                    }
                }
            }
            None => {
                self.fill.place(shape);
                self.fill.unscale();
                self.card.place(shape);
                self.card.unscale();
                color_to(c, &self.fill.brush, to, 0, EASE_OUT);
                self.card.show(c, theme, s, 0, EASE_OUT);
                self.fill.snap(true);
                self.card.snap(true);
                if s == State::Pressed {
                    let _ = motion::press(c, &self.fill.v, (shape.w, shape.h), true);
                    self.card.press(c, (shape.w, shape.h), true);
                }
            }
        }
        self.on = Some((shape, s));
    }

    /// The theme changed: shown again in its colors at the next `show`.
    pub fn forget(&mut self) {
        self.on = None;
        self.fill.snap(false);
        self.fill.unscale();
        self.card.snap(false);
        self.card.unscale();
    }

    /// Its card light now (tests): the drop's and the light's colors.
    #[cfg(test)]
    pub fn card_shows(&self) -> (Rgba, Rgba) {
        self.card.shows()
    }
}

// ---- the progress ring ------------------------------------------------------------

/// The progress ring (11.3) over a picture: crate::popup's turning ring,
/// coming on over FADE_IN_MS once something has taken TIP_DELAY_MS (the
/// caller's wait), gone at once when it is over. It turns only while it
/// shows (and with full motion): hidden, nothing of it keeps running.
pub struct Ring {
    v: SpriteVisual,
    on: bool,
    /// Its turn is running.
    turning: bool,
}

impl Ring {
    /// Hidden, standing still.
    pub fn new(gfx: &crate::gfx::Gfx, theme: &Theme, scale: f32, layer: &ContainerVisual) -> Result<Ring> {
        let v = crate::popup::parts::spinner(gfx, theme, scale)?;
        // (spinner turns what it makes at once: not this one yet.)
        crate::popup::parts::turn(&gfx.compositor, &v, false)?;
        v.SetOpacity(0.0)?;
        layer.Children()?.InsertAtTop(&v)?;
        Ok(Ring { v, on: false, turning: false })
    }

    /// Reduced motion switched on (or off) while it is up: it stops where
    /// it starts (or turns again). Cheap when nothing changed: the screen
    /// asks on its own ticks and on WM_SETTINGCHANGE.
    pub fn follow_motion(&mut self, c: &Compositor) {
        self.turn(c, ring_turns(motion::now()));
    }

    /// Its turn on or off (`on` and shown), if that changes.
    fn turn(&mut self, c: &Compositor, on: bool) {
        let want = on && self.on;
        if want != self.turning {
            self.turning = want;
            let _ = crate::popup::parts::turn(c, &self.v, want);
        }
    }

    /// Its turn is running (tests).
    #[cfg(test)]
    pub fn turning(&self) -> bool {
        self.turning
    }

    /// Its middle at (`cx`, `cy`).
    pub fn place(&self, cx: f32, cy: f32) {
        let half = crate::theme::metrics::size::SPINNER / 2.0;
        let _ = self.v.SetOffset(Vector3 { X: cx - half, Y: cy - half, Z: 0.0 });
    }

    /// Shown (it turns with full motion as it comes on) or gone at once
    /// (its turn stops with it).
    pub fn show(&mut self, c: &Compositor, on: bool) {
        if on == self.on {
            return;
        }
        self.on = on;
        if on {
            self.follow_motion(c);
            let _ = motion::fade_to(c, &self.v, 1.0, FADE_IN_MS, EASE_OUT);
        } else {
            self.turn(c, false);
            let _ = self.v.StopAnimation(&HSTRING::from("Opacity"));
            let _ = self.v.SetOpacity(0.0);
        }
    }
}

/// Pure: the ring turns in `mode` (12.2: not with reduced motion).
pub fn ring_turns(mode: motion::Mode) -> bool {
    mode == motion::Mode::Full
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The review's P2 5 (2026-11-11): a raised button drawn as a visual
    /// (search's and a stack's "Try again") had the opaque card fill but
    /// not the card's light. It wears it now: the drop and the top edge's
    /// light, the light gone pressed; nothing on a glass or in high
    /// contrast.
    #[test]
    fn a_raised_button_wears_the_cards_light() {
        use crate::syslook::{HighContrast, SysLook};
        let stage = crate::gfx::promo_kit::stage();
        let c = &stage.gfx.compositor;
        let clear = Rgba { r: 0, g: 0, b: 0, a: 0 };
        let shape = Shape::new(10.0, 10.0, 96.0, 28.0, 8.0);
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let flat = Theme::graphite().resolve_fx(&sys, true);
            let fx = flat.card_fx(State::Rest).unwrap();
            let layer = c.CreateContainerVisual().unwrap();
            let mut b = Raised::new(c, &layer).unwrap();
            // Under its face in the layer: the drop, then the face.
            assert_eq!(layer.Children().unwrap().Count().unwrap(), 2);
            b.show(c, &flat, Some((shape, State::Rest)));
            assert_eq!(b.card_shows(), (fx.drop, fx.light), "light {light}: at rest");
            // (Flat's cards since 2026-11-12: plain, no light, no drop.)
            assert!(fx.drop.a == 0 && fx.light.a == 0);
            b.show(c, &flat, Some((shape, State::Pressed)));
            assert_eq!(b.card_shows(), (fx.drop, Rgba { a: 0, ..fx.light }), "pressed: no light, at once");
            // A glass: nothing of it.
            let mut raw = Theme::graphite();
            raw.finish = crate::theme::Finish::Glass;
            let glass = raw.resolve_fx(&sys, true);
            b.forget();
            b.show(c, &glass, Some((shape, State::Rest)));
            assert_eq!(b.card_shows(), (clear, clear), "a glass");
            // ... and no cover: the see-through glass fill is laid once.
            assert!(!b.card.covers(), "light {light}: no cover on a glass");
            b.forget();
            b.show(c, &flat, Some((shape, State::Rest)));
            assert!(b.card.covers(), "light {light}: the cover back on flat");
            // High contrast shows nothing at all (the picture draws it).
            let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
            let hc = Theme::graphite().resolve_fx(&SysLook { light, high_contrast: Some(HighContrast { window: black, text: white, highlight: white, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() }, true);
            assert!(hc.card_fx(State::Rest).is_none());
        }
    }

    #[test]
    fn the_pointers_fill_fades_and_a_press_is_at_once() {
        // 6.3: on 90 on the ease-out, off 120 on the ease-in, pressed now.
        assert_eq!(pointer_fade(true, false), (FADE_IN_MS, EASE_OUT));
        assert_eq!(pointer_fade(false, false), (FADE_OUT_MS, EASE_IN));
        assert_eq!(pointer_fade(true, true).0, 0);
        // Reduced motion: every one of them is fade_plan's 60 ms straight.
        for (ms, curve) in [pointer_fade(true, false), pointer_fade(false, false)] {
            assert_eq!(motion::fade_plan(motion::Mode::Reduced, ms, curve, 4.0), (motion::REDUCED_MS, motion::LINEAR));
        }
    }

    #[test]
    fn one_fill_follows_its_target_and_another_takes_the_next() {
        let a = Lit { t: 1u8, down: false, shape: Shape::new(8.0, 40.0, 100.0, 28.0, 12.0) };
        assert_eq!(step::<u8>(None, None), Step::Stay);
        assert_eq!(step(None, Some(&a)), Step::Other);
        assert_eq!(step(Some(&a), None), Step::Other);
        assert_eq!(step(Some(&a), Some(&a)), Step::Stay);
        // Pressed, and let go: the same fill.
        assert_eq!(step(Some(&a), Some(&Lit { down: true, ..a })), Step::Same { press: true, moved: false });
        // The list scrolled under the pointer.
        let moved = Lit { shape: Shape { y: 12.0, ..a.shape }, ..a };
        assert_eq!(step(Some(&a), Some(&moved)), Step::Same { press: false, moved: true });
        // Another row: the first fades where it is.
        assert_eq!(step(Some(&a), Some(&Lit { t: 2, ..a })), Step::Other);
    }

    #[test]
    fn a_button_lightens_darkens_and_comes_back() {
        assert_eq!(raised_state(false, false), State::Rest);
        assert_eq!(raised_state(true, false), State::Hover);
        assert_eq!(raised_state(true, true), State::Pressed);
        assert_eq!(raised_state(false, true), State::Pressed, "held while the pointer strays");
        assert_eq!(raised_fade(State::Rest, State::Hover), (FADE_IN_MS, EASE_OUT));
        assert_eq!(raised_fade(State::Hover, State::Rest), (FADE_OUT_MS, EASE_IN));
        assert_eq!(raised_fade(State::Hover, State::Pressed).0, 0);
        assert_eq!(raised_fade(State::Rest, State::Pressed).0, 0);
        // Let go: back to the hover fill over 90.
        assert_eq!(raised_fade(State::Pressed, State::Hover), (FADE_IN_MS, EASE_OUT));
        assert_eq!(raised_fade(State::Pressed, State::Rest), (FADE_OUT_MS, EASE_IN));
    }

    #[test]
    fn the_ring_stops_with_reduced_motion() {
        assert!(ring_turns(motion::Mode::Full));
        assert!(!ring_turns(motion::Mode::Reduced));
    }

    /// The panels' review (2026-11-11, P3): the ring turned for ever from
    /// the moment it was made, shown or not (search makes it with its
    /// chrome). Now its turn lives as long as it shows.
    #[test]
    fn the_ring_turns_only_while_it_shows() {
        let stage = crate::gfx::promo_kit::stage();
        let c = &stage.gfx.compositor;
        let theme = Theme::graphite().resolve(&crate::syslook::SysLook::default());
        let layer = c.CreateContainerVisual().unwrap();
        let mut r = Ring::new(&stage.gfx, &theme, 1.5, &layer).unwrap();
        assert!(!r.turning(), "made hidden: standing still");
        r.follow_motion(c);
        assert!(!r.turning(), "hidden: asking about the motion does not start it");
        r.show(c, true);
        assert_eq!(r.turning(), motion::full(), "shown: it turns with full motion");
        r.show(c, false);
        assert!(!r.turning(), "hidden: it stops");
        assert_eq!(r.v.RotationAngleInDegrees().unwrap(), 0.0, "where it starts");
        r.follow_motion(c);
        assert!(!r.turning());
        // Shown again: it asks the motion then.
        r.show(c, true);
        assert_eq!(r.turning(), motion::full());
        r.show(c, false);
        assert!(!r.turning());
    }

    #[test]
    fn a_capsule_is_half_its_height() {
        let s = Shape::capsule(4.0, 8.0, 28.0, 28.0);
        assert_eq!(s.rad, 14.0);
        assert!(s.holds(4.0, 8.0) && s.holds(31.9, 35.9) && !s.holds(32.0, 20.0) && !s.holds(10.0, 36.0));
    }
}

/// What the panels' design-audit pictures share (tests only).
#[cfg(test)]
#[path = "wash_harmony.rs"]
pub mod harmony;
