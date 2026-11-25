//! Motion and tones: the springs, curves and colors (pure).

use super::*;

// ---- motion (notes.md, section 8) -------------------------------------------------

/// One of the kit's springs (crate::motion, solved there), cut off at
/// `max_ms` (pinned on its target then).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Spring {
    pub(super) kit: crate::motion::Spring,
    pub(super) max_ms: i64,
}

/// SNAP: a button's return from a press, a slider's thickness; GLIDE:
/// the panel's opening (the kit's, as every panel; design system 7.1).
pub(super) const SPRING_A: Spring = Spring { kit: crate::motion::SPRING_SNAP, max_ms: crate::motion::SNAP_MAX_MS };
pub(super) const SPRING_C: Spring = Spring { kit: crate::motion::SPRING_GLIDE, max_ms: crate::motion::OPEN_MS };
/// Key frames a spring is sampled into (linear between them).
pub(super) const SPRING_FRAMES: usize = crate::motion::SAMPLES;

impl Spring {
    /// Its damping ratio, c / (2 sqrt(k m)).
    #[cfg(test)]
    pub(super) fn ratio(&self) -> f32 {
        self.kit.ratio()
    }

    /// The share of the way to its target after `t` seconds (0 at the
    /// start, from rest; 1 on the target).
    #[cfg(test)]
    pub(super) fn progress(&self, tn: f32) -> f32 {
        1.0 - self.kit.left(tn)
    }

    /// Its course as key frames: (share of max_ms, share of the way), the
    /// first after the start, the last pinned on the target.
    pub(super) fn frames(&self, n: usize) -> Vec<(f32, f32)> {
        self.kit.keys(self.max_ms.max(1), n).into_iter().skip(1).collect()
    }
}

/// The panel's fade in and out; album art's cross-fade (design system 6.3
/// and 7.1: every other length is crate::motion's).
pub(super) const OPEN_FADE_MS: i64 = crate::motion::PANEL_FADE_MS;
pub(super) const CLOSE_MS: i64 = crate::motion::CLOSE_MS;
pub(super) const ART_FADE_MS: i64 = crate::motion::CROSS_MS;
/// The panel's scale when it starts to open (the kit's, as every panel:
/// it grows from the button and only fades away).
pub(super) const OPEN_SCALE: f32 = crate::motion::OPEN_SCALE;
/// The curves (cubic Bezier control points): ease-out for things
/// arriving, the exit curve for things going.
pub(super) type Curve = ((f32, f32), (f32, f32));
pub(super) const EASE_OUT: Curve = crate::motion::EASE_OUT;
pub(super) const EASE_EXIT: Curve = crate::motion::EASE_IN;

/// A button's state, for its colors and scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) struct Look {
    pub(super) on: bool,
    pub(super) hover: bool,
    pub(super) pressed: bool,
}

/// Pure: how a button's colors go from was to now (6.3): a press at
/// once, a switch crossing (CROSS_MS), a hover coming on or a press let go
/// FADE_IN_MS easing out, a hover going FADE_OUT_MS easing in.
pub(super) fn tone_ms(was: Look, now: Look) -> (i64, Curve) {
    if now.pressed && !was.pressed {
        (0, EASE_OUT)
    } else if now.on != was.on {
        (crate::motion::CROSS_MS, EASE_OUT)
    } else if (now.hover && !was.hover) || (was.pressed && !now.pressed) {
        (crate::motion::FADE_IN_MS, EASE_OUT)
    } else {
        (crate::motion::FADE_OUT_MS, EASE_EXIT)
    }
}

/// Pure: the scale the panel starts opening from (it grows from the
/// button, like the kit's panels: crate::motion::open). With reduced
/// motion it only fades: 1.
pub(super) fn open_from(motion: bool) -> f32 {
    if motion {
        OPEN_SCALE
    } else {
        1.0
    }
}

// ---- tones (notes.md, section 2) --------------------------------------------------


/// Pure: `a` moved `t` (0..1) of the way to `b`, alpha too.
pub(super) fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Rgba { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b), a: m(a.a, b.a) }
}

pub(super) const WHITE: Rgba = Rgba { r: 255, g: 255, b: 255, a: 255 };
pub(super) const BLACK: Rgba = Rgba { r: 0, g: 0, b: 0, a: 255 };

/// The panel's achromatic tones, derived from the theme's tokens: the
/// tiles' fills at rest, hovered and pressed (Theme::card_states: the
/// fill_tile washes, opaque on Flat's panel, see-through with Glass), the
/// track (fill_track), text (F), secondary text (S, translucent) and the
/// panel tint (P). "On" is the text's lightness (A = F) with the panel's,
/// deepened, on it (AI). Nothing here takes a hue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Tones {
    pub(super) light: bool,
    pub(super) tile: Rgba,
    pub(super) tile_hover: Rgba,
    pub(super) tile_press: Rgba,
    pub(super) ink: Rgba,
    pub(super) dim: Rgba,
    pub(super) on: Rgba,
    pub(super) on_ink: Rgba,
    pub(super) on_hover: Rgba,
    pub(super) on_press: Rgba,
    /// The mixer slider's and the position bar's fill, and the bar's track.
    pub(super) fill: Rgba,
    pub(super) bar: Rgba,
    /// Tertiary text (a list's headings).
    pub(super) faint: Rgba,
    /// A chip on a card (rest, hover, pressed).
    pub(super) chip: Rgba,
    pub(super) chip_hover: Rgba,
    pub(super) chip_press: Rgba,
    /// A row's or glyph button's fill (none at rest): hover, pressed.
    pub(super) row_hover: Rgba,
    pub(super) row_press: Rgba,
    /// The slider track before its filled portion.
    pub(super) groove: Rgba,
}

/// Pure: the tones for a light or dark theme with tile fills `tiles`
/// (rest, hover, pressed: Theme::card_states), track `track`, text `ink`,
/// secondary text `dim` and panel tint `panel`; `accent`: the accent and
/// its ink when it has a color (Theme::accent_rgba), for what is on (the
/// discs, the sliders' fill); None: the achromatic "on".
pub(super) fn tones(light: bool, tiles: (Rgba, Rgba, Rgba), track: Rgba, ink: Rgba, dim: Rgba, panel: Rgba, accent: Option<(Rgba, Rgba)>) -> Tones {
    let down = if light { WHITE } else { BLACK };
    let panel = Rgba { a: 255, ..panel };
    let (tile, tile_hover, tile_press) = tiles;
    let t = Tones {
        light,
        tile,
        tile_hover,
        tile_press,
        ink,
        dim,
        on: ink,
        // #171717 on the dark theme's disc, #F7F7F7-ish on the light one's.
        on_ink: mix(panel, if light { WHITE } else { BLACK }, 0.35),
        on_hover: mix(ink, WHITE, if light { 0.05 } else { 0.4 }),
        on_press: if light { mix(ink, WHITE, 0.12) } else { mix(ink, BLACK, 0.12) },
        fill: if light { mix(ink, down, 0.07) } else { mix(ink, BLACK, 0.052) },
        bar: track,
        faint: dim,
        chip: wash(light, if light { 0.06 } else { 0.10 }),
        chip_hover: wash(light, if light { 0.10 } else { 0.16 }),
        chip_press: wash(light, if light { 0.12 } else { 0.07 }),
        row_hover: wash(light, if light { crate::theme::fills::ROW_HOVER.1 } else { crate::theme::fills::ROW_HOVER.0 }),
        row_press: wash(light, if light { crate::theme::fills::ROW_PRESSED.1 } else { crate::theme::fills::ROW_PRESSED.0 }),
        groove: track,
    };
    match accent {
        // A color: on, its ink, the fill. Hover and press move away from
        // the ink (deeper under white, lighter under black), so they never
        // read worse than the color itself (4.5 in every state).
        Some((on, on_ink)) => Tones { on, on_ink, on_hover: away(on, on_ink, ON_HOVER), on_press: away(on, on_ink, ON_PRESS), fill: on, ..t },
        None => t,
    }
}

/// The ink's wash at a (black on light, white on dark).
pub(super) fn wash(light: bool, a: f32) -> Rgba {
    Rgba { a: (a * 255.0).round() as u8, ..if light { BLACK } else { WHITE } }
}

/// How far an accent "on" moves on hover and when pressed (away()).
pub(super) const ON_HOVER: f32 = 0.1;
pub(super) const ON_PRESS: f32 = 0.2;
/// What a glyph on a disc keeps against what is under it (WCAG).
pub(super) const DISC_CONTRAST: f32 = 4.5;

/// Pure: `c` moved `k` of the way away from `ink`: toward black under a
/// lighter ink, toward white under a darker one. Its contrast with `ink`
/// only grows.
pub(super) fn away(c: Rgba, ink: Rgba, k: f32) -> Rgba {
    let to = if crate::theme::luminance(ink) > crate::theme::luminance(c) { BLACK } else { WHITE };
    mix(c, to, k)
}

/// The panel's tones for `theme` (resolved): the tile states and track
/// as they sit on the panel (opaque on Flat, see-through with Glass), the
/// opaque ink (it is also the achromatic "on" disc) and the translucent
/// secondary text (it keeps 4.5:1 over Glass whatever is under it).
pub(super) fn tones_of(theme: &Theme) -> Tones {
    let accent = theme.accent_rgba.map(|_| (theme.accent(), theme.on_accent()));
    let track = if theme.glass() { theme.fill_track() } else { theme.track() };
    let tiles = theme.card_states();
    let panels = panel_extremes(theme);
    let dim = readable_dim(theme.text_secondary(), theme.text_primary().a, &panels, &[tiles.0, tiles.1, tiles.2, track]);
    let faint = readable_dim(theme.text_tertiary(), theme.text_primary().a, &panels, &[tiles.0]);
    Tones { faint, groove: theme.fill_track(), ..tones(theme.is_light, tiles, track, theme.ink(), dim, theme.popup_tint(), accent) }
}

/// The panels' plate as text on it sees it over any backdrop, with the
/// contrast text keeps there (Theme::plate_seen_all: the glass's whole
/// stack, the top light and the grain at their bounds; 4.5:1 over
/// theme::BACKDROPS, the full colors' 3:1 exception).
pub(in crate::topbar) fn panel_extremes(theme: &Theme) -> Vec<(Rgba, f32)> {
    theme.plate_seen_all(crate::theme::Material::Panel)
}

/// Pure: secondary text `dim` (translucent: the ink at the secondary
/// level) that keeps its contrast on the panel (`panels`: every look its
/// plate takes with the contrast it needs: panel_extremes) and on each of
/// `fills` over it: the secondary level as it is, or raised toward the
/// primary's alpha `primary_a` only as far as that needs (a glass over a
/// white wallpaper; Flat keeps it as it is). An opaque `dim` (high
/// contrast, a theme's own color) is kept.
pub(in crate::topbar) fn readable_dim(dim: Rgba, primary_a: u8, panels: &[(Rgba, f32)], fills: &[Rgba]) -> Rgba {
    let unders: Vec<(Rgba, f32)> = panels
        .iter()
        .flat_map(|&(panel, need)| std::iter::once((panel, need)).chain(fills.iter().map(move |f| (crate::theme::over(*f, panel), need))))
        .collect();
    crate::theme::readable(dim, primary_a, &unders)
}

/// Pure: the "on" fill (moved on hover and pressed) and its ink: the
/// theme's ink where it reads on the "on" color, else white first
/// (on_color); one ink for every state, and the states move away from it,
/// so it keeps 4.5:1 in every state.
fn on_tone(tn: &Tones, l: Look) -> (Rgba, Rgba) {
    let reads = |ink: Rgba, c: Rgba| crate::theme::contrast(ink, c) >= DISC_CONTRAST;
    let ink = if reads(tn.on_ink, tn.on) { tn.on_ink } else { crate::theme::on_color(tn.on) };
    let (fill, k) = if l.pressed { (tn.on_press, ON_PRESS) } else if l.hover { (tn.on_hover, ON_HOVER) } else { (tn.on, 0.0) };
    if ink == tn.on_ink && reads(ink, fill) {
        return (fill, ink);
    }
    (away(tn.on, ink, k), ink)
}

/// Pure: a raised button's fill and glyph colors (a 1 x 1 tile, the
/// output button; 6.2): off, the card's rest, hover and pressed fills with
/// the ink; on, inverted.
pub(super) fn raised_tone(tn: &Tones, l: Look) -> (Rgba, Rgba) {
    if l.on {
        return on_tone(tn, l);
    }
    (if l.pressed { tn.tile_press } else if l.hover { tn.tile_hover } else { tn.tile }, tn.ink)
}

/// Pure: a chip's (on a card): off, the chip's fills; on, inverted.
pub(super) fn chip_tone(tn: &Tones, l: Look) -> (Rgba, Rgba) {
    if l.on {
        return on_tone(tn, l);
    }
    (if l.pressed { tn.chip_press } else if l.hover { tn.chip_hover } else { tn.chip }, tn.ink)
}

/// Pure: a glyph button's (part of the surface: none at rest, the row's
/// fills under the pointer).
pub(super) fn glyph_tone(tn: &Tones, l: Look) -> (Rgba, Rgba) {
    let clear = Rgba { a: 0, ..tn.row_hover };
    (if l.pressed { tn.row_press } else if l.hover { tn.row_hover } else { clear }, tn.ink)
}

/// Pure: a button in high contrast (12.3): on or pressed, Highlight with
/// HighlightText; hovered, an outline of Highlight 2 inside; at rest an
/// outline of WindowText 1 inside when it has a face (a tile or a chip; a
/// glyph button has none). (fill or outline color, outline width, ink.)
pub(super) fn hc_tone(hc: &crate::syslook::HighContrast, l: Look, face: bool) -> (Rgba, Option<f32>, Rgba) {
    if l.on || l.pressed {
        (hc.highlight, None, hc.highlight_text)
    } else if l.hover {
        (hc.highlight, Some(2.0), hc.text)
    } else if face {
        (hc.text, Some(1.0), hc.text)
    } else {
        (Rgba { a: 0, ..hc.text }, None, hc.text)
    }
}

// ---- the panel ---------------------------------------------------------------------
