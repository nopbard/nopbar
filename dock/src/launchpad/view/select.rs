//! What is lit under the pointer or the keyboard (design system v2.1, 6.2
//! and 6.3, an app icon's column): one thing at a time, whichever moved
//! last.
//!
//! The pointer: a ground behind the cell in the raised fill's hover (the
//! icon itself never changes size). It comes on over 90 ms and goes over
//! 120, in place: the one before fades where it was while the new one
//! comes up (three grounds take turns, so one is reused only when it has
//! long gone). The keyboard: the focus ring (6.4) round the icon, 2 out and
//! 2 wide, concentric with its squircle; at once, both ways, and what the
//! pointer had lit goes at once too. High contrast: the pointer's is an
//! inner outline 2 wide in Highlight, the ring is Highlight; nothing fades.

use windows::core::{Interface, Result, HSTRING};
use windows::UI::Composition::{CompositionRoundedRectangleGeometry, CompositionSpriteShape, Compositor, ContainerVisual, ShapeVisual, Visual};
use windows_numerics::{Vector2, Vector3};

use super::{color, rgba, Hit, Pad, Rect};
use crate::theme::metrics::size;
use crate::theme::{Role, State};

/// The grounds that take turns.
const GROUNDS: usize = 3;
/// High contrast's hover outline (12.3: a state's line is 2).
const HC_LINE: f32 = 2.0;

struct Piece {
    v: ShapeVisual,
    geom: CompositionRoundedRectangleGeometry,
    shape: CompositionSpriteShape,
}

impl Piece {
    fn new(c: &Compositor) -> Result<Self> {
        let v = c.CreateShapeVisual()?;
        let geom = c.CreateRoundedRectangleGeometry()?;
        let shape = c.CreateSpriteShapeWithGeometry(&geom)?;
        v.Shapes()?.Append(&shape)?;
        v.SetIsVisible(false)?;
        Ok(Piece { v, geom, shape })
    }

    fn detach(&self) {
        if let Ok(parent) = self.v.Parent() {
            let _ = parent.Children().and_then(|c| c.Remove(&self.v));
        }
    }

    /// Where it is drawn: a rounded rectangle `r` (corners `radius`) whose
    /// stroke, `line` wide, lies `inset` inside it (negative: outside).
    fn place(&self, r: Rect, radius: f32, inset: f32, pad: f32) {
        let _ = self.v.SetOffset(Vector3 { X: r.l - pad, Y: r.t - pad, Z: 0.0 });
        let _ = self.v.SetSize(Vector2 { X: r.w() + pad * 2.0, Y: r.h() + pad * 2.0 });
        let _ = self.geom.SetOffset(Vector2 { X: pad + inset, Y: pad + inset });
        let _ = self.geom.SetSize(Vector2 { X: (r.w() - inset * 2.0).max(0.0), Y: (r.h() - inset * 2.0).max(0.0) });
        let rad = (radius - inset).max(0.0);
        let _ = self.geom.SetCornerRadius(Vector2 { X: rad, Y: rad });
    }
}

/// What is lit: where (the view's key, as `Pad::sel_in`), the cell and
/// whether the keyboard put it there.
type Lit = (Option<usize>, Rect, bool);

/// Pure: the pointer on `h` lights it (an app, an app of the open folder,
/// a Library tile); anything else is nothing to light.
pub(super) fn lights(h: Hit) -> bool {
    matches!(h, Hit::App(_) | Hit::FolderApp(_) | Hit::LibTile(..))
}

/// What a pointer move does to what it lit (pointer_step).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PointerStep {
    /// It is on nothing that lights now (Pad::pointer_out).
    pub out: bool,
    /// What it lit goes (over 120 ms; high contrast at once).
    pub off: bool,
    /// It came back onto a tile: the tile lights even if it was the one
    /// selected (its ground went when the pointer left).
    pub again: bool,
}

/// Pure: the pointer moves onto something that lights (`on`) or not,
/// from `out` (it was on nothing that lights), with its ground lit
/// (`lit`: the pointer's, not the keyboard's ring) or not.
pub(super) fn pointer_step(out: bool, on: bool, lit: bool) -> PointerStep {
    PointerStep { out: !on, off: !on && lit, again: on && out }
}

/// Pure: the selection shows: the keyboard's ring always; the pointer's
/// ground only while the pointer is on something that lights.
/// Pure: the pointer's ground under a cell: the raised fill's hover; in
/// the open folder (`folder`) on a light glass, the row's hover (the
/// raised hover is white on that near-white panel).
pub(super) fn ground_fill(theme: &crate::theme::Theme, folder: bool) -> crate::theme::Rgba {
    if folder && theme.is_light && theme.glass() {
        theme.fill_of(Role::Row, State::Hover)
    } else {
        theme.fill_of(Role::Raised, State::Hover)
    }
}

pub(super) fn pointer_shows(by_key: bool, out: bool) -> bool {
    by_key || !out
}

/// Pure: how the selection shows (None: not at all): by the focus ring
/// (Some(true): the keyboard moved it, `by_key`), or by the pointer's
/// ground (Some(false): the pointer's, or typing chose it, `typed`: a
/// letter is not a key that moves the focus, crate::input), not while the
/// pointer is away (`out`) unless a key or typing chose it.
pub(super) fn sel_look(by_key: bool, typed: bool, out: bool) -> Option<bool> {
    pointer_shows(by_key || typed, out).then_some(by_key)
}

pub(super) struct Glow {
    grounds: Vec<Piece>,
    /// The ground lit (or last lit).
    turn: usize,
    ring: Piece,
    lit: Option<Lit>,
}

/// Pure: how far out of what it rings the focus ring's centerline runs:
/// FOCUS_GAP out and half its FOCUS_W (6.4). Its corner is the ringed
/// thing's plus this (concentric).
pub(super) fn ring_out() -> f32 {
    size::FOCUS_GAP + size::FOCUS_W / 2.0
}

impl Glow {
    pub(super) fn new(c: &Compositor) -> Result<Self> {
        let grounds = (0..GROUNDS).map(|_| Piece::new(c)).collect::<Result<Vec<_>>>()?;
        Ok(Glow { grounds, turn: 0, ring: Piece::new(c)?, lit: None })
    }
}

impl Pad {
    /// The grounds' fill and the ring's color, from the theme.
    pub(super) fn glow_restyle(&mut self) {
        let c = self.gfx.compositor.clone();
        let clear = windows::UI::Color { A: 0, R: 0, G: 0, B: 0 };
        let (fill, stroke, line) = match self.hc() {
            Some(hc) => (clear, color(rgba(hc.highlight)), HC_LINE),
            None => (color(rgba(self.theme.fill_of(Role::Raised, State::Hover))), clear, 0.0),
        };
        for g in &self.glow.grounds {
            if let (Ok(f), Ok(st)) = (c.CreateColorBrushWithColor(fill), c.CreateColorBrushWithColor(stroke)) {
                let _ = g.shape.SetFillBrush(&f);
                let _ = g.shape.SetStrokeBrush(&st);
                let _ = g.shape.SetStrokeThickness(line);
            }
        }
        if let Ok(b) = c.CreateColorBrushWithColor(color(rgba(crate::gfx::parts::focus_color(&self.theme)))) {
            let _ = self.glow.ring.shape.SetStrokeBrush(&b);
            let _ = self.glow.ring.shape.SetStrokeThickness(size::FOCUS_W);
        }
        // (Placed again with the new corners and line the next time.)
        self.sel_off(true);
    }

    fn glow_fade(&self, v: &ShapeVisual, on: bool, instant: bool) {
        let c = &self.gfx.compositor;
        let opacity = HSTRING::from("Opacity");
        let to = if on { 1.0 } else { 0.0 };
        let (ms, curve) = super::motion::pointer(self.animate(), on, instant || self.hc().is_some(), crate::effects::current().speed);
        let started = (|| -> Result<()> {
            if ms <= 0 {
                return Err(windows::core::Error::from(windows::Win32::Foundation::E_FAIL));
            }
            let a = c.CreateScalarKeyFrameAnimation()?;
            if on {
                a.InsertKeyFrame(0.0, 0.0)?;
            }
            a.InsertKeyFrameWithEasingFunction(1.0, to, &crate::motion::easing(c, curve)?)?;
            a.SetDuration(windows::Foundation::TimeSpan { Duration: ms * 10_000 })?;
            v.StartAnimation(&opacity, &a)
        })();
        if started.is_err() {
            let _ = v.StopAnimation(&opacity);
            let _ = v.SetOpacity(to);
        }
    }

    /// The pointer's ground is lit (not the keyboard's ring).
    pub(super) fn pointer_lit(&self) -> bool {
        matches!(self.glow.lit, Some((_, _, false)))
    }

    /// Nothing is lit any more. `instant`: the keyboard did it (or what was
    /// lit is going away); else the pointer left: over 120 ms.
    pub(super) fn sel_off(&mut self, instant: bool) {
        let _ = self.glow.ring.v.SetIsVisible(false);
        if matches!(self.glow.lit.take(), Some((_, _, false))) || instant {
            let g = &self.glow.grounds[self.glow.turn];
            self.glow_fade(&g.v, false, instant);
        }
    }

    /// Lights cell `cell` of the view `container` (`key`: Pad::sel_in's):
    /// the pointer's ground behind the cell, or (the keyboard moved last:
    /// Pad::by_key) the ring round `icon`, whose corner is `icon_r`.
    pub(super) fn sel_on(&mut self, key: Option<usize>, container: &ContainerVisual, cell: Rect, icon: Rect, icon_r: f32) {
        let by_key = self.by_key;
        if self.glow.lit == Some((key, cell, by_key)) && self.sel_in == Some(key) {
            return;
        }
        // What was lit goes: at once when the keyboard takes over.
        self.sel_off(by_key);
        self.sel_in = Some(key);
        self.glow.lit = Some((key, cell, by_key));
        if by_key {
            let ring = &self.glow.ring;
            ring.detach();
            if container.Children().and_then(|c| c.InsertAtTop(&ring.v)).is_err() {
                return;
            }
            let out = ring_out();
            let line = Rect::new(icon.l - out, icon.t - out, icon.r + out, icon.b + out);
            ring.place(line, icon_r + out, 0.0, size::FOCUS_W);
            let _ = ring.v.SetIsVisible(true);
            return;
        }
        self.glow.turn = (self.glow.turn + 1) % GROUNDS;
        let g = &self.glow.grounds[self.glow.turn];
        // In the open folder on a light glass the raised hover (white) lay
        // on a near-white panel, unseen: there the row's hover, as a list's
        // rows have (states-issues 11).
        if self.hc().is_none() {
            let fill = ground_fill(&self.theme, key == Some(usize::MAX));
            if let Ok(b) = self.gfx.compositor.CreateColorBrushWithColor(color(rgba(fill))) {
                let _ = g.shape.SetFillBrush(&b);
            }
        }
        g.detach();
        if container.Children().and_then(|c| c.InsertAtBottom(&g.v)).is_err() {
            return;
        }
        let hc = self.hc().is_some();
        g.place(cell, self.theme.radii().card, if hc { HC_LINE / 2.0 } else { 0.0 }, 0.0);
        let _ = g.v.SetIsVisible(true);
        self.glow_fade(&g.v, true, false);
    }

    /// The lit things leave the view they are in (it is going away).
    pub(super) fn detach_sel(&mut self) {
        for g in &self.glow.grounds {
            g.detach();
            let _ = g.v.cast::<Visual>().and_then(|v| v.StopAnimation(&HSTRING::from("Opacity")));
            let _ = g.v.SetOpacity(0.0);
        }
        self.glow.ring.detach();
        let _ = self.glow.ring.v.SetIsVisible(false);
        self.glow.lit = None;
        self.sel_in = None;
    }
}

#[cfg(test)]
mod tests {
    /// states-issues 11: the open folder's ground on a light glass is the
    /// row's hover (dark wash), visible on the near-white panel.
    #[test]
    fn the_folders_ground_shows_on_a_light_glass() {
        use crate::syslook::SysLook;
        use crate::theme::{Finish, Theme};
        let mut raw = Theme::graphite();
        raw.finish = Finish::Glass;
        let light = raw.resolve_fx(&SysLook { light: true, ..Default::default() }, true);
        assert_eq!(ground_fill(&light, true), light.fill_of(Role::Row, State::Hover));
        assert_eq!(ground_fill(&light, false), light.fill_of(Role::Raised, State::Hover));
        let dark = raw.resolve_fx(&SysLook { light: false, ..Default::default() }, true);
        assert_eq!(ground_fill(&dark, true), dark.fill_of(Role::Raised, State::Hover));
    }

    /// Typing (a letter) chooses the first result, lit by the pointer's
    /// ground, never the ring; the arrows and Tab show the ring; a press
    /// of the pointer hides it (crate::input).
    #[test]
    fn typing_lights_the_first_result_without_the_ring() {
        // (by_key, typed, the pointer away)
        assert_eq!(sel_look(false, true, true), Some(false), "typed: the ground, the pointer away");
        assert_eq!(sel_look(true, true, true), Some(true), "a key moved it after: the ring");
        assert_eq!(sel_look(false, false, true), None, "nothing chose it, the pointer away");
        assert_eq!(sel_look(false, false, false), Some(false), "the pointer's");
        let view = include_str!("../view.rs").replace("\r\n", "\n");
        let code = &view[..view.find("#[cfg(test)]").unwrap_or(view.len())];
        assert!(code.contains("            self.typed = true;"), "typing sets typed");
        assert!(!code.contains("(Typed: Enter opens the first result, ringed.)"));
        assert!(code.contains("    fn on_down(&mut self, lp: LPARAM) {\n        self.pointer_takes_over();"), "a press hides the rings");
        let access = include_str!("access.rs");
        assert!(access.contains("self.reader_ring = crate::input::reader_focus(self.by_key || self.field_ring || self.reader_ring);"));
    }

    use super::*;

    #[test]
    fn the_pointers_ground_goes_when_it_leaves_and_comes_back_with_it() {
        use super::super::super::library::Part;
        for h in [Hit::App(0), Hit::FolderApp(2), Hit::LibTile(1, Part::Rest)] {
            assert!(lights(h), "{h:?}");
        }
        for h in [Hit::Empty, Hit::Field, Hit::Dot(1), Hit::Done, Hit::FolderName, Hit::FolderPanel, Hit::FolderOutside, Hit::Retry] {
            assert!(!lights(h), "{h:?}");
        }
        // On a tile, lit by the pointer: off onto empty space, it goes.
        let left = pointer_step(false, false, true);
        assert_eq!(left, PointerStep { out: true, off: true, again: false });
        assert!(!pointer_shows(false, left.out), "nothing lights it again meanwhile");
        // Moving on over empty space: nothing more to do.
        assert_eq!(pointer_step(true, false, false), PointerStep { out: true, off: false, again: false });
        // Back on (the same tile too): it lights again.
        let back = pointer_step(true, true, false);
        assert_eq!(back, PointerStep { out: false, off: false, again: true });
        assert!(pointer_shows(false, back.out));
        // From tile to tile: the new one lights as before (sel_on).
        assert_eq!(pointer_step(false, true, true), PointerStep { out: false, off: false, again: false });
        // The keyboard's ring: the pointer leaving leaves it.
        assert_eq!(pointer_step(false, false, false).off, false);
        assert!(pointer_shows(true, true));
    }

    #[test]
    fn the_ring_is_two_out_and_fits_a_cell() {
        // Its centerline 3 out (2 of gap and half its 2 of width): its
        // outer edge 4 out of the icon, inside a cell's padding.
        assert_eq!(ring_out(), 3.0);
        assert!(ring_out() + size::FOCUS_W / 2.0 <= crate::launchpad::grid::ITEM_PAD);
    }
}
