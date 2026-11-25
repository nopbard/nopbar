//! The capsule behind a bar cell (design system v2.1, 6.1 to 6.3). A bar
//! cell is part of its surface ("면의 일부", theme::Role::Row) whose own
//! shape is a capsule: no fill at rest; the row's hover fill under the
//! pointer (on over FADE_IN_MS, off over FADE_OUT_MS); pressed, the row's
//! pressed fill at once and its size to motion::press_scale on SNAP; and
//! while the menu or panel it opened is up it keeps the pressed fill at
//! full size ("open"), which goes over FADE_OUT_MS when that surface starts
//! to close. Scrubbing to a neighbour moves the open fill at once (7.4).
//! High contrast (12.3): an inner Highlight outline under the pointer, a
//! Highlight fill with HighlightText content pressed and open, both at
//! once. Everything a state looks like comes from the theme's fill_of /
//! outline_of / text_of; every length from crate::motion.
//!
//! On screen the capsules are compositor shapes (`Capsules`); the design
//! audit's pictures draw the same `Look` (bar_harmony.rs).

use windows::core::{Interface, Result, HSTRING};
use windows::UI::Composition::{CompositionColorBrush, CompositionRoundedRectangleGeometry, CompositionSpriteShape, Compositor, ShapeVisual, VisualCollection};
use windows_numerics::{Vector2, Vector3};

use super::{baseline, color};
use crate::motion::{self, Mode};
use crate::theme::{Level, Rgba, Role, State, Theme};

/// Pure: what cell `i` is doing (6.2): disabled; open (what it opened is
/// up); pressed (the button went down on it and the pointer is still on
/// it); under the pointer; or at rest.
pub(super) fn cell_state(i: usize, hovered: Option<usize>, pressed: Option<usize>, open: Option<usize>, enabled: bool) -> State {
    if !enabled {
        State::Disabled
    } else if open == Some(i) {
        State::Open
    } else if pressed == Some(i) && hovered == Some(i) {
        State::Pressed
    } else if hovered == Some(i) {
        State::Hover
    } else {
        State::Rest
    }
}

/// The cells of a bar that are lit: the one whose surface is up, and the
/// one under the pointer with its state (Hover or Pressed), if another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Lit {
    pub open: Option<usize>,
    pub under: Option<(usize, State)>,
}

/// Pure: the lit cells of a bar from where the pointer is (`hovered`),
/// where the button went down (`pressed`) and whose surface is up (`open`).
pub(super) fn lit(hovered: Option<usize>, pressed: Option<usize>, open: Option<usize>) -> Lit {
    let under = hovered.map(|h| (h, cell_state(h, hovered, pressed, open, true))).filter(|(_, s)| matches!(s, State::Hover | State::Pressed));
    Lit { open, under }
}

/// Pure: opening from `next` while `open` (the cell whose surface is up)
/// is another cell is scrubbing (7.4): the open fill moves there at once
/// and the surfaces cross at full size. The first open, or the same cell
/// again, is not. (Cells by kind for the bar, by index for its capsules.)
pub(super) fn scrub<T: PartialEq>(open: Option<T>, next: T) -> bool {
    open.is_some_and(|c| c != next)
}

/// What a cell's capsule and content look like in a state: the fill, an
/// outline (color, DIP, inside the edge: high contrast only), the color
/// its content takes instead of the bar's text color (None: the bar's),
/// and the scale it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Look {
    pub fill: Rgba,
    pub outline: Option<(Rgba, f32)>,
    pub ink: Option<Rgba>,
    pub scale: f32,
}

/// Pure: the look of a cell `w` x `h` DIP in state `s` on a bar whose
/// pressed and open fill is `mark` (look::Ground::mark). The
/// Top bar: nothing at rest or under the pointer, `mark`
/// pressed and open, at full size (it does not shrink). High contrast
/// (`mark` None) keeps 6.2 and 12.3: the row's fills and outlines, the
/// content in the color the state asks, pressed at motion::press_scale
/// (full size with reduced motion).
pub(super) fn look(theme: &Theme, mark: Option<Rgba>, s: State, w: f32, h: f32, mode: Mode) -> Look {
    let ink = theme.text_of(Level::Primary, s);
    if let Some(m) = mark {
        let none = Rgba { a: 0, ..m };
        return Look {
            fill: if matches!(s, State::Pressed | State::Open) { m } else { none },
            outline: None,
            ink: (s == State::Disabled).then_some(ink),
            scale: 1.0,
        };
    }
    Look {
        fill: theme.fill_of(Role::Row, s),
        outline: theme.outline_of(Role::Row, s),
        ink: (ink != theme.text_of(Level::Primary, State::Rest)).then_some(ink),
        scale: if s == State::Pressed && mode == Mode::Full { motion::press_scale(w, h) } else { 1.0 },
    }
}

/// How the capsule under the pointer gets from one state to the next
/// (6.3), ms at speed 1 (0: at once; None: that part does not happen).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Change {
    /// The capsule that was lit goes over this long.
    pub out_ms: Option<i64>,
    /// A capsule comes on at the new cell over this long.
    pub in_ms: Option<i64>,
    /// On the same cell the fill changes over this long.
    pub fill_ms: Option<i64>,
}

/// Pure: the change from `from` to `to` (cell, state) in `mode`. The
/// pointer's changes: on over FADE_IN_MS, off over FADE_OUT_MS; a press at
/// once, its release back over FADE_IN_MS. With reduced motion every fade
/// is REDUCED_MS; `at_once` (high contrast: an outline or an inverted fill
/// must not pass through in-between colors) makes them all 0.
pub(super) fn change(from: Option<(usize, State)>, to: Option<(usize, State)>, mode: Mode, at_once: bool) -> Change {
    let ms = |on: bool| if at_once { 0 } else { motion::pointer_ms(mode, on) };
    match (from, to) {
        (a, b) if a == b => Change::default(),
        (Some((a, _)), Some((b, s))) if a == b => Change { fill_ms: Some(if s == State::Pressed { 0 } else { ms(true) }), ..Change::default() },
        (old, Some((_, s))) => Change { out_ms: old.map(|_| ms(false)), in_ms: Some(if s == State::Pressed { 0 } else { ms(true) }), fill_ms: None },
        (Some(_), None) => Change { out_ms: Some(ms(false)), ..Change::default() },
        (None, None) => Change::default(),
    }
}

/// Pure: how long the open fill takes to go once its surface starts to
/// close (6.2, 7.4: FADE_OUT_MS). It comes on, and moves while scrubbing,
/// at once.
pub(super) fn open_off_ms(mode: Mode, at_once: bool) -> i64 {
    if at_once {
        0
    } else {
        motion::pointer_ms(mode, false)
    }
}

/// Pure: a keyboard focus ring around a bar cell's capsule `r` (x, y, w,
/// h) with corners `radius`: its centerline's (x, y, w, h), its corner and
/// its width. The ring is the kit's width (FOCUS_W) but sits against the
/// capsule, not FOCUS_GAP out of it: a bar cell has only BAR_INSET (2)
/// between it and the bar's edge, which is the bar window's edge, so a
/// ring 2 further out would be cut away along the top and the bottom.
/// (The bar never takes the keyboard today, access.rs: nothing shows it on
/// screen yet; it is drawn in the design audit's pictures.)
#[cfg(test)]
pub(super) fn focus_ring(r: (f32, f32, f32, f32), radius: f32) -> ((f32, f32, f32, f32), f32, f32) {
    let w = crate::theme::metrics::size::FOCUS_W;
    ((r.0 - w / 2.0, r.1 - w / 2.0, r.2 + w, r.3 + w), radius + w / 2.0, w)
}

/// One capsule on screen: a shape visual holding its fill and, inside its
/// edge, high contrast's outline.
struct Cap {
    v: ShapeVisual,
    geom: CompositionRoundedRectangleGeometry,
    fill: CompositionColorBrush,
    edge_geom: CompositionRoundedRectangleGeometry,
    edge: CompositionSpriteShape,
    edge_brush: CompositionColorBrush,
    /// Its size now (DIP), for the press.
    size: (f32, f32),
}

impl Cap {
    fn new(c: &Compositor, layer: &VisualCollection, above: &impl Interface) -> Result<Cap> {
        let v = c.CreateShapeVisual()?;
        let geom = c.CreateRoundedRectangleGeometry()?;
        let fill = c.CreateColorBrushWithColor(color(0, 0, 0, 0))?;
        let body = c.CreateSpriteShapeWithGeometry(&geom)?;
        body.SetFillBrush(&fill)?;
        let edge_geom = c.CreateRoundedRectangleGeometry()?;
        let edge = c.CreateSpriteShapeWithGeometry(&edge_geom)?;
        let edge_brush = c.CreateColorBrushWithColor(color(0, 0, 0, 0))?;
        edge.SetStrokeBrush(&edge_brush)?;
        v.Shapes()?.Append(&body)?;
        v.Shapes()?.Append(&edge)?;
        v.SetOpacity(0.0)?;
        layer.InsertAbove(&v, &above.cast::<windows::UI::Composition::Visual>()?)?;
        Ok(Cap { v, geom, fill, edge_geom, edge, edge_brush, size: (0.0, 0.0) })
    }

    /// At (`x`, `y`), `w` x `h` DIP, corners `r`, on whole device pixels.
    fn place(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, scale: f32) {
        self.size = (w, h);
        let _ = self.v.SetOffset(Vector3 { X: baseline::snap_px(x, scale), Y: baseline::snap_px(y, scale), Z: 0.0 });
        let _ = self.v.SetSize(Vector2 { X: w, Y: h });
        let _ = self.geom.SetSize(Vector2 { X: w, Y: h });
        let _ = self.geom.SetCornerRadius(Vector2 { X: r, Y: r });
    }

    /// The look's fill, over `ms` at speed 1 from the color it shows (0:
    /// at once), and its outline (at once).
    fn paint(&self, c: &Compositor, look: &Look, ms: i64) {
        let to = color(look.fill.a, look.fill.r, look.fill.g, look.fill.b);
        let _ = self.fill.StopAnimation(&HSTRING::from("Color"));
        let faded = ms > 0
            && (|| -> Result<()> {
                let a = c.CreateColorKeyFrameAnimation()?;
                let (p1, p2) = motion::EASE_OUT;
                let ease = c.CreateCubicBezierEasingFunction(Vector2 { X: p1.0, Y: p1.1 }, Vector2 { X: p2.0, Y: p2.1 })?;
                a.InsertKeyFrameWithEasingFunction(1.0, to, &ease)?;
                a.SetDuration(windows::Foundation::TimeSpan { Duration: crate::effects::ms(ms) * 10_000 })?;
                self.fill.StartAnimation(&HSTRING::from("Color"), &a)
            })()
            .is_ok();
        if !faded {
            let _ = self.fill.SetColor(to);
        }
        let (w, h) = self.size;
        let r = self.geom.CornerRadius().map(|v| v.X).unwrap_or(0.0);
        match look.outline {
            Some((oc, ow)) => {
                let _ = self.edge_brush.SetColor(color(oc.a, oc.r, oc.g, oc.b));
                let _ = self.edge.SetStrokeThickness(ow);
                let _ = self.edge_geom.SetOffset(Vector2 { X: ow / 2.0, Y: ow / 2.0 });
                let _ = self.edge_geom.SetSize(Vector2 { X: (w - ow).max(0.0), Y: (h - ow).max(0.0) });
                let _ = self.edge_geom.SetCornerRadius(Vector2 { X: (r - ow / 2.0).max(0.0), Y: (r - ow / 2.0).max(0.0) });
            }
            None => {
                let _ = self.edge_brush.SetColor(color(0, 0, 0, 0));
            }
        }
    }

    /// Its opacity to `to` over `ms` at speed 1 from where it is (0: at
    /// once); `from_nothing`: it comes in from 0.
    fn fade(&self, c: &Compositor, to: f32, ms: i64, from_nothing: bool) {
        let curve = if to > 0.0 { motion::EASE_OUT } else { motion::EASE_IN };
        let from = if from_nothing { motion::Start::At(0.0) } else { motion::Start::Current };
        if motion::fade_from(c, &self.v, from, to, ms, curve).is_err() {
            let _ = self.v.SetOpacity(to);
        }
    }

    /// Pressed (`down`): its size to motion::press_scale on SNAP; let go,
    /// back to full size on SNAP (nothing with reduced motion).
    fn press(&self, c: &Compositor, down: bool) {
        let _ = motion::press(c, &self.v, self.size, down);
    }

    /// At full size, now (a capsule taken for another cell).
    fn unpress(&self) {
        for p in ["Scale.X", "Scale.Y", "Scale"] {
            let _ = self.v.StopAnimation(&HSTRING::from(p));
        }
        let _ = self.v.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 });
    }
}

/// Where a cell's capsule goes on its bar, DIP: (x, y, width, height,
/// corner radius).
pub(super) type Spot = (f32, f32, f32, f32, f32);

/// The bar's capsules. Three shapes are enough: the open cell's, the one
/// under the pointer, and the one the pointer just left, still fading.
pub(super) struct Capsules {
    caps: Vec<Cap>,
    /// (capsule, cell) of the open cell.
    open: Option<(usize, usize)>,
    /// (capsule, cell, state) of the cell under the pointer.
    under: Option<(usize, usize, State)>,
    /// The theme changed: what is lit is painted again.
    stale: bool,
}

impl Capsules {
    /// Three hidden capsules just above `above` (the bar's tint), under
    /// the cells.
    pub(super) fn new(c: &Compositor, layer: &VisualCollection, above: &impl Interface) -> Result<Capsules> {
        let caps = (0..3).map(|_| Cap::new(c, layer, above)).collect::<Result<Vec<_>>>()?;
        Ok(Capsules { caps, open: None, under: None, stale: false })
    }

    /// The theme changed: the next `show` paints what is lit again.
    pub(super) fn restyle(&mut self) {
        self.stale = true;
    }

    /// A capsule neither the open cell nor the pointer holds, nor one of
    /// `not` (those just let go, still fading).
    fn spare(&self, not: [Option<usize>; 2]) -> usize {
        let taken = [self.open.map(|o| o.0), self.under.map(|u| u.0), not[0], not[1]];
        (0..self.caps.len()).find(|k| !taken.contains(&Some(*k))).unwrap_or(0)
    }

    /// Shows `lit` (cells by index; `spot`: where a cell's capsule goes,
    /// None for a cell not on show) as the interaction model has it. The
    /// open cell changing while a surface stays up is a scrub: its fill
    /// moves at once, at full size. Safe to call on every layout: only a
    /// change of state starts a fade.
    /// `mark`: the bar's open fill; `cell_mark`: a cell's own instead
    /// (a clear bar's cell in the other ink: look::CellLook::mark).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn show(&mut self, c: &Compositor, theme: &Theme, mark: Option<Rgba>, cell_mark: &dyn Fn(usize) -> Option<Rgba>, lit: Lit, scale: f32, spot: &dyn Fn(usize) -> Option<Spot>) {
        let mode = motion::now();
        // The Top bar's fill comes and goes at once, like high
        // contrast's (a menu opens on the press).
        let at_once = theme.high_contrast || mark.is_some();
        let stale = std::mem::take(&mut self.stale);
        let looks = |i: usize, s: State, sp: &Spot| look(theme, mark.map(|m| cell_mark(i).unwrap_or(m)), s, sp.2, sp.3, mode);

        // The open cell: on at once, moved at once, off over open_off_ms.
        let to_open = lit.open.and_then(|i| spot(i).map(|sp| (i, sp)));
        // The capsule the open cell just let go of (still fading out).
        let mut gone = None;
        match (self.open, to_open) {
            (Some((k, was)), Some((i, sp))) => {
                self.caps[k].place(sp.0, sp.1, sp.2, sp.3, sp.4, scale);
                let scrubbed = scrub(Some(was), i);
                if scrubbed {
                    self.caps[k].unpress();
                }
                if scrubbed || stale {
                    self.caps[k].paint(c, &looks(i, State::Open, &sp), 0);
                }
                self.open = Some((k, i));
            }
            (None, Some((i, sp))) => {
                // The pressed capsule under the pointer carries on as the
                // open one (its size springs back from where it is); else
                // a spare one, on at once at full size.
                let k = match self.under {
                    Some((k, u, _)) if u == i => {
                        self.under = None;
                        k
                    }
                    _ => {
                        let k = self.spare([None, None]);
                        self.caps[k].unpress();
                        k
                    }
                };
                self.caps[k].place(sp.0, sp.1, sp.2, sp.3, sp.4, scale);
                self.caps[k].paint(c, &looks(i, State::Open, &sp), 0);
                self.caps[k].fade(c, 1.0, 0, false);
                self.caps[k].press(c, false);
                self.open = Some((k, i));
            }
            (Some((k, _)), None) => {
                self.caps[k].fade(c, 0.0, open_off_ms(mode, at_once), false);
                self.open = None;
                gone = Some(k);
            }
            (None, None) => {}
        }

        // The cell under the pointer.
        let to = lit.under.and_then(|(i, s)| spot(i).map(|sp| (i, s, sp)));
        let ch = change(self.under.map(|(_, i, s)| (i, s)), to.map(|(i, s, _)| (i, s)), mode, at_once);
        match (self.under, to) {
            (Some((k, was, _)), Some((i, s, sp))) if was == i => {
                self.caps[k].place(sp.0, sp.1, sp.2, sp.3, sp.4, scale);
                if let Some(ms) = ch.fill_ms {
                    self.caps[k].paint(c, &looks(i, s, &sp), ms);
                    self.caps[k].press(c, s == State::Pressed);
                } else if stale {
                    self.caps[k].paint(c, &looks(i, s, &sp), 0);
                }
                self.under = Some((k, i, s));
            }
            (old, Some((i, s, sp))) => {
                if let Some((k, ..)) = old {
                    self.caps[k].fade(c, 0.0, ch.out_ms.unwrap_or(0), false);
                }
                self.under = None;
                let k = self.spare([old.map(|o| o.0), gone]);
                self.caps[k].unpress();
                self.caps[k].place(sp.0, sp.1, sp.2, sp.3, sp.4, scale);
                self.caps[k].paint(c, &looks(i, s, &sp), 0);
                self.caps[k].fade(c, 1.0, ch.in_ms.unwrap_or(0), true);
                if s == State::Pressed {
                    self.caps[k].press(c, true);
                }
                self.under = Some((k, i, s));
            }
            (Some((k, ..)), None) => {
                self.caps[k].fade(c, 0.0, ch.out_ms.unwrap_or(0), false);
                self.under = None;
            }
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Cell;
    use super::*;
    use crate::syslook::{HighContrast, SysLook};

    #[test]
    fn a_cell_is_at_rest_hovered_pressed_open_or_disabled() {
        // (hovered, pressed, open) -> cell 2's state.
        assert_eq!(cell_state(2, None, None, None, true), State::Rest);
        assert_eq!(cell_state(2, Some(1), None, None, true), State::Rest, "the pointer is on another cell");
        assert_eq!(cell_state(2, Some(2), None, None, true), State::Hover);
        assert_eq!(cell_state(2, Some(2), Some(2), None, true), State::Pressed);
        // Pressed, then dragged off it: at rest; back on it: pressed again.
        assert_eq!(cell_state(2, Some(3), Some(2), None, true), State::Rest);
        assert_eq!(cell_state(2, None, Some(2), None, true), State::Rest);
        // Its surface is up: open, wherever the pointer is, pressed or not.
        for hovered in [None, Some(2), Some(5)] {
            for pressed in [None, Some(2)] {
                assert_eq!(cell_state(2, hovered, pressed, Some(2), true), State::Open);
            }
        }
        // Another cell's surface is up: this one still answers the pointer.
        assert_eq!(cell_state(2, Some(2), None, Some(0), true), State::Hover);
        // Disabled: nothing else.
        assert_eq!(cell_state(2, Some(2), Some(2), Some(2), false), State::Disabled);
    }

    #[test]
    fn one_cell_is_open_and_one_under_the_pointer() {
        assert_eq!(lit(None, None, None), Lit::default());
        assert_eq!(lit(Some(3), None, None), Lit { open: None, under: Some((3, State::Hover)) });
        assert_eq!(lit(Some(3), Some(3), None), Lit { open: None, under: Some((3, State::Pressed)) });
        // The open cell is not also "under the pointer": one fill, not two.
        assert_eq!(lit(Some(3), Some(3), Some(3)), Lit { open: Some(3), under: None });
        assert_eq!(lit(Some(4), None, Some(3)), Lit { open: Some(3), under: Some((4, State::Hover)) });
        // Pressed on one cell and dragged to another: that one only hovers.
        assert_eq!(lit(Some(4), Some(3), None), Lit { open: None, under: Some((4, State::Hover)) });
    }

    #[test]
    fn another_cell_while_a_surface_is_up_is_a_scrub() {
        assert!(!scrub(None, Cell::App), "the first open is an ordinary open");
        assert!(scrub(Some(Cell::App), Cell::Menu(0)));
        assert!(scrub(Some(Cell::Menu(1)), Cell::Menu(0)));
        assert!(scrub(Some(Cell::Start), Cell::App));
        assert!(!scrub(Some(Cell::Clock), Cell::Clock), "the same cell again is not");
        // The capsules ask the same of cells by index.
        assert!(scrub(Some(1usize), 2) && !scrub(Some(2usize), 2) && !scrub(None, 2usize));
    }

    #[test]
    fn the_looks_come_from_the_themes_row() {
        for light in [false, true] {
            let t = Theme::graphite().resolve(&SysLook { light, ..Default::default() });
            let at = |s| look(&t, None, s, 60.0, 28.0, Mode::Full);
            // Rest: nothing. Hover, pressed: the row's fills, no outline,
            // the bar's own text color.
            assert_eq!(at(State::Rest), Look { fill: Rgba { r: 0, g: 0, b: 0, a: 0 }, outline: None, ink: None, scale: 1.0 });
            assert_eq!(at(State::Hover), Look { fill: t.fill_of(Role::Row, State::Hover), outline: None, ink: None, scale: 1.0 });
            assert!(at(State::Hover).fill.a > 0 && at(State::Pressed).fill.a > at(State::Hover).fill.a);
            // Pressed: smaller (its long side loses 2 DIP); open: the same
            // fill at full size.
            assert_eq!(at(State::Pressed).scale, motion::press_scale(60.0, 28.0));
            assert!((at(State::Pressed).scale - (1.0 - 2.0 / 60.0)).abs() < 1e-6);
            assert_eq!(at(State::Open), Look { scale: 1.0, ..at(State::Pressed) });
            // A 28 round cell stops at 0.94.
            assert_eq!(look(&t, None, State::Pressed, 28.0, 28.0, Mode::Full).scale, 0.94);
            // Reduced motion: no size change.
            assert_eq!(look(&t, None, State::Pressed, 60.0, 28.0, Mode::Reduced), Look { scale: 1.0, ..at(State::Pressed) });
            // Disabled: no fill, the disabled text color.
            assert_eq!(at(State::Disabled), Look { fill: Rgba { r: 0, g: 0, b: 0, a: 0 }, outline: None, ink: Some(t.text_disabled()), scale: 1.0 });
        }
    }

    #[test]
    fn the_mac_bar_lights_only_what_is_pressed_or_open() {
        let mode = Mode::Full;
        for (light, mark) in [(false, crate::topbar::look::MARK_ON_DARK), (true, crate::topbar::look::MARK_ON_LIGHT)] {
            let t = Theme::graphite().resolve(&SysLook { light, ..Default::default() });
            let at = |s| look(&t, Some(mark), s, 60.0, 22.0, mode);
            // Nothing at rest or under the pointer.
            for s in [State::Rest, State::Hover] {
                assert_eq!((at(s).fill.a, at(s).outline, at(s).ink), (0, None, None), "{s:?}");
            }
            // Pressed and open: the mark, at full size (no shrinking).
            for s in [State::Pressed, State::Open] {
                assert_eq!(at(s), Look { fill: mark, outline: None, ink: None, scale: 1.0 }, "{s:?}");
            }
            assert_eq!(at(State::Disabled).ink, Some(t.text_disabled()));
        }
        // White .18 on a dark bar, black .10 on a light one.
        assert_eq!((crate::topbar::look::MARK_ON_DARK.a, crate::topbar::look::MARK_ON_LIGHT.a), ((0.18f32 * 255.0).round() as u8, (0.10f32 * 255.0).round() as u8));
    }

    #[test]
    fn high_contrast_outlines_the_hover_and_inverts_pressed_and_open() {
        let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
        let hi = Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 };
        let hc = HighContrast { window: black, text: white, highlight: hi, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT };
        let t = Theme::graphite().resolve(&SysLook { high_contrast: Some(hc), ..Default::default() });
        let at = |s| look(&t, None, s, 60.0, 28.0, Mode::Full);
        // Under the pointer: no fill, a 2 DIP Highlight outline inside.
        assert_eq!(at(State::Hover), Look { fill: Rgba { r: 0, g: 0, b: 0, a: 0 }, outline: Some((hi, 2.0)), ink: None, scale: 1.0 });
        // Pressed and open: a Highlight fill, HighlightText content.
        for s in [State::Pressed, State::Open] {
            assert_eq!((at(s).fill, at(s).outline, at(s).ink), (hi, None, Some(black)), "{s:?}");
        }
        // Its fills change at once (no in-between colors).
        for (from, to) in [(None, Some((1, State::Hover))), (Some((1, State::Hover)), None), (Some((1, State::Pressed)), Some((1, State::Hover))), (Some((1, State::Hover)), Some((2, State::Hover)))] {
            let ch = change(from, to, Mode::Full, true);
            assert!([ch.out_ms, ch.in_ms, ch.fill_ms].iter().flatten().all(|ms| *ms == 0), "{from:?} -> {to:?}");
        }
        assert_eq!(open_off_ms(Mode::Full, true), 0);
    }

    #[test]
    fn the_pointer_fades_in_90_and_out_120_and_a_press_is_at_once() {
        let (h, p) = (State::Hover, State::Pressed);
        let full = |a, b| change(a, b, Mode::Full, false);
        // Nothing changed: nothing starts (safe on every layout).
        assert_eq!(full(Some((1, h)), Some((1, h))), Change::default());
        assert_eq!(full(None, None), Change::default());
        // On: 90. Off: 120. To a neighbour: the old goes over 120 as the
        // new comes over 90.
        assert_eq!(full(None, Some((1, h))), Change { in_ms: Some(90), ..Change::default() });
        assert_eq!(full(Some((1, h)), None), Change { out_ms: Some(120), ..Change::default() });
        assert_eq!(full(Some((1, h)), Some((2, h))), Change { out_ms: Some(120), in_ms: Some(90), fill_ms: None });
        // A press: the fill at once. Its release: back over 90.
        assert_eq!(full(Some((1, h)), Some((1, p))), Change { fill_ms: Some(0), ..Change::default() });
        assert_eq!(full(Some((1, p)), Some((1, h))), Change { fill_ms: Some(90), ..Change::default() });
        assert_eq!(full(None, Some((1, p))), Change { in_ms: Some(0), ..Change::default() });
        // Let go off the bar: it goes over 120.
        assert_eq!(full(Some((1, p)), None), Change { out_ms: Some(120), ..Change::default() });
        // Reduced motion: 60 either way; the press still at once.
        let less = |a, b| change(a, b, Mode::Reduced, false);
        assert_eq!(less(None, Some((1, h))), Change { in_ms: Some(60), ..Change::default() });
        assert_eq!(less(Some((1, h)), None), Change { out_ms: Some(60), ..Change::default() });
        assert_eq!(less(Some((1, p)), Some((1, h))), Change { fill_ms: Some(60), ..Change::default() });
        assert_eq!(less(Some((1, h)), Some((1, p))), Change { fill_ms: Some(0), ..Change::default() });
        // The open fill goes over 120 when its surface starts to close.
        assert_eq!((open_off_ms(Mode::Full, false), open_off_ms(Mode::Reduced, false)), (120, 60));
    }

    #[test]
    fn a_focus_ring_fits_between_the_capsule_and_the_bars_edge() {
        use crate::theme::metrics::size;
        // A 28 capsule in a 32 bar: the ring fills the 2 DIP around it.
        let (r, radius, w) = focus_ring((100.0, 2.0, 60.0, 28.0), 14.0);
        assert_eq!((r, radius, w), ((99.0, 1.0, 62.0, 30.0), 15.0, size::FOCUS_W));
        // Its outer edge is the bar's: nothing is cut away.
        assert_eq!((r.1 - w / 2.0, r.1 + r.3 + w / 2.0), (0.0, 32.0));
        assert!(size::BAR_INSET >= size::FOCUS_W);
    }

    #[test]
    fn no_color_or_length_of_its_own() {
        // The states' colors are the theme's, the lengths the kit's.
        let src = include_str!("capsule.rs").replace('\r', "");
        let body = src.split("#[cfg(test)]\nmod tests").next().unwrap();
        for banned in ["high_contrast {", ".hc", "fills::", "_MS: i64", "wash("] {
            assert!(!body.contains(banned), "{banned}");
        }
        assert!(body.contains("theme.fill_of(Role::Row, s)") && body.contains("theme.outline_of(Role::Row, s)") && body.contains("theme.text_of(Level::Primary, s)"));
    }
}
